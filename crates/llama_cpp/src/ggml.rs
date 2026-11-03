//! Lifetime-bound metadata handles around core ggml graph APIs.
//!
//! A context owns tensor/graph metadata; every handle borrows that context.
//! Graph dependencies must belong to the same arena. These types are confined
//! to one thread. Invalid sizes, names, handles and native statuses are typed
//! errors. Backend allocation and execution remain explicitly unsafe because
//! this low-level API cannot own externally managed tensor buffers: callers
//! must retain them through all native operations that can use their addresses.
//!
//! This module provides the building blocks for creating and executing tensor computation
//! graphs using ggml backends. It's used for operations like `LoRA` merging, importance
//! matrix computation, and control vector generation.
//!
//! # Example
//!
//! ```no_run
//! use llama_cpp::ggml::GgmlContext;
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let ctx = GgmlContext::sized_for(3, 1, true)?;
//! let a = ctx.new_tensor_1d(llama_cpp_sys::GGML_TYPE_F32, 4)?;
//! let b = ctx.new_tensor_1d(llama_cpp_sys::GGML_TYPE_F32, 4)?;
//! let sum = ctx.add(&a, &b)?;
//! let mut graph = ctx.new_graph()?;
//! graph.build_forward(&sum)?;
//! # Ok(())
//! # }
//! ```

#![deny(clippy::undocumented_unsafe_blocks)]

use std::ffi::CStr;
use std::ptr::NonNull;

/// Re-export the raw ggml types for advanced usage.
pub use llama_cpp_sys::ggml_type;

/// Validation or native failure at the ggml boundary.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum GgmlError {
    /// A native operation did not return its required owned handle.
    #[error("ggml returned a null handle")]
    NullHandle,
    /// Dimensions, type, offset, index or pool arithmetic are invalid.
    #[error("invalid ggml dimensions, type, offset or index")]
    InvalidDimensions,
    /// An operation would retain metadata owned by another context.
    #[error("tensor and graph dependencies must belong to the same ggml context")]
    ForeignContext,
    /// The arena cannot fit the requested metadata and locally allocated data.
    #[error("ggml context memory pool has insufficient space")]
    InsufficientMemory,
    /// Names must fit the complete native buffer without truncating UTF-8.
    #[error("tensor name exceeds the native name buffer")]
    NameTooLong,
    /// Names cannot contain interior NUL bytes.
    #[error("invalid tensor name: {0}")]
    Nul(#[from] std::ffi::NulError),
    /// Native names should be UTF-8; preserve an error instead of a fallback.
    #[error("invalid UTF-8 from ggml: {0}")]
    Utf8(#[from] std::str::Utf8Error),
    /// Backend graph allocation failed.
    #[error("ggml could not allocate graph storage")]
    Allocation,
    /// Native graph computation did not succeed.
    #[error("ggml graph compute failed with status {0}")]
    Compute(i32),
}

/// A safe wrapper around `ggml_context`.
#[derive(Debug)]
pub struct GgmlContext {
    ctx: NonNull<llama_cpp_sys::ggml_context>,
    /// Whether tensor data is allocated outside this metadata arena.
    no_alloc: bool,
}

impl GgmlContext {
    /// Create a new ggml context.
    ///
    /// # Parameters
    /// - `mem_size`: Memory pool size in bytes for tensor metadata.
    /// - `no_alloc`: If true, tensor data is not allocated (use with backend allocation).
    ///
    /// # Sizing the pool
    ///
    /// Tensor/graph metadata and any local tensor data are carved from this
    /// fixed pool. Constructors check headroom before calling ggml, whose arena
    /// allocator can abort on exhaustion. [`Self::sized_for`] budgets metadata;
    /// callers using `no_alloc = false` must additionally budget tensor data.
    ///
    /// # Errors
    ///
    /// Returns [`GgmlError`] for invalid sizes or a null native result.
    pub fn new(mem_size: usize, no_alloc: bool) -> Result<Self, GgmlError> {
        if mem_size == 0 || mem_size > isize::MAX as usize - 32 {
            return Err(GgmlError::InvalidDimensions);
        }
        let params = llama_cpp_sys::ggml_init_params {
            mem_size,
            mem_buffer: std::ptr::null_mut(),
            no_alloc,
        };
        // SAFETY: scalars are checked and no external memory buffer is supplied.
        // ggml owns its allocation; this wrapper releases it exactly once.
        let ctx = unsafe { llama_cpp_sys::ggml_init(params) };
        Ok(Self {
            ctx: NonNull::new(ctx).ok_or(GgmlError::NullHandle)?,
            no_alloc,
        })
    }

    /// Create a context sized for `n_tensors` tensors and `n_graphs` graphs.
    ///
    /// Turns "pick a number and hope" into arithmetic: the pool holds metadata,
    /// whose per-item cost ggml reports through [`tensor_overhead`] and
    /// [`graph_overhead`]. A little slack is added for ggml's own bookkeeping.
    ///
    /// Views, reshapes and every intermediate an operation produces are tensors
    /// too — count graph *nodes*, not just the tensors you name.
    ///
    /// ```
    /// # use llama_cpp::ggml::GgmlContext;
    /// // Room for 64 tensors and one graph.
    /// let ctx = GgmlContext::sized_for(64, 1, true)?;
    /// assert!(ctx.mem_size() >= 64 * llama_cpp::ggml::tensor_overhead());
    /// # Ok::<(), llama_cpp::ggml::GgmlError>(())
    /// ```
    ///
    /// # Errors
    ///
    /// Returns [`GgmlError`] for invalid sizes or a null native result.
    pub fn sized_for(n_tensors: usize, n_graphs: usize, no_alloc: bool) -> Result<Self, GgmlError> {
        // The +1 tensor and the 1 KiB tail cover ggml's per-object headers and
        // alignment padding, which are not part of the published overheads.
        let mem_size = n_tensors
            .checked_add(1)
            .and_then(|n| n.checked_mul(tensor_overhead()))
            .and_then(|n| {
                n_graphs
                    .checked_mul(graph_overhead())
                    .and_then(|g| n.checked_add(g))
            })
            .and_then(|n| n.checked_add(1024))
            .ok_or(GgmlError::InvalidDimensions)?;
        Self::new(mem_size, no_alloc)
    }

    /// Bytes of the pool used so far.
    ///
    /// Compare with [`Self::mem_size`] while building a graph to see whether it
    /// will fit, rather than finding out by aborting.
    #[must_use]
    pub fn used_mem(&self) -> usize {
        // SAFETY: ctx is the live arena owned by this wrapper; this reads its size.
        unsafe { llama_cpp_sys::ggml_used_mem(self.ctx.as_ptr()) }
    }

    /// Total size of the pool, as passed to [`Self::new`].
    #[must_use]
    pub fn mem_size(&self) -> usize {
        // SAFETY: ctx is live arena-owned metadata and the call only reads its size.
        unsafe { llama_cpp_sys::ggml_get_mem_size(self.ctx.as_ptr()) }
    }

    /// Bytes still available in the pool.
    #[must_use]
    pub fn free_mem(&self) -> usize {
        self.mem_size().saturating_sub(self.used_mem())
    }

    /// Get the raw context pointer.
    #[must_use]
    pub fn as_ptr(&self) -> *mut llama_cpp_sys::ggml_context {
        self.ctx.as_ptr()
    }

    /// Validate that metadata references remain inside this arena.
    fn require_tensor(&self, tensor: &GgmlTensor<'_>) -> Result<(), GgmlError> {
        if !std::ptr::eq(self, tensor.1) {
            return Err(GgmlError::ForeignContext);
        }
        Ok(())
    }

    /// Check pool headroom before ggml's nonrecoverable arena allocator.
    fn reserve_tensor(&self, bytes: usize) -> Result<(), GgmlError> {
        let data = if self.no_alloc { 0 } else { bytes };
        let required = tensor_overhead()
            .checked_add(data)
            .and_then(|n| n.checked_add(32))
            .ok_or(GgmlError::InvalidDimensions)?;
        if required > self.free_mem() {
            return Err(GgmlError::InsufficientMemory);
        }
        Ok(())
    }

    /// Convert a native result without losing its context lifetime.
    fn tensor(
        &self,
        pointer: *mut llama_cpp_sys::ggml_tensor,
    ) -> Result<GgmlTensor<'_>, GgmlError> {
        Ok(GgmlTensor(
            NonNull::new(pointer).ok_or(GgmlError::NullHandle)?,
            self,
        ))
    }

    /// Create a one-dimensional tensor.
    ///
    /// # Errors
    /// Returns [`GgmlError`] for invalid dimensions/type or insufficient memory.
    pub fn new_tensor_1d(&self, typ: ggml_type, ne0: i64) -> Result<GgmlTensor<'_>, GgmlError> {
        self.new_tensor(typ, &[ne0])
    }

    /// Create a two-dimensional tensor.
    ///
    /// # Errors
    /// Returns [`GgmlError`] for invalid dimensions/type or insufficient memory.
    pub fn new_tensor_2d(
        &self,
        typ: ggml_type,
        ne0: i64,
        ne1: i64,
    ) -> Result<GgmlTensor<'_>, GgmlError> {
        self.new_tensor(typ, &[ne0, ne1])
    }

    /// Create a three-dimensional tensor.
    ///
    /// # Errors
    /// Returns [`GgmlError`] for invalid dimensions/type or insufficient memory.
    pub fn new_tensor_3d(
        &self,
        typ: ggml_type,
        ne0: i64,
        ne1: i64,
        ne2: i64,
    ) -> Result<GgmlTensor<'_>, GgmlError> {
        self.new_tensor(typ, &[ne0, ne1, ne2])
    }

    /// Create a four-dimensional tensor.
    ///
    /// # Errors
    /// Returns [`GgmlError`] for invalid dimensions/type or insufficient memory.
    pub fn new_tensor_4d(
        &self,
        typ: ggml_type,
        ne0: i64,
        ne1: i64,
        ne2: i64,
        ne3: i64,
    ) -> Result<GgmlTensor<'_>, GgmlError> {
        self.new_tensor(typ, &[ne0, ne1, ne2, ne3])
    }

    /// Copy a tensor's shape and type, without retaining the source tensor.
    ///
    /// # Errors
    /// Returns [`GgmlError`] for insufficient memory.
    pub fn dup_tensor(&self, src: &GgmlTensor<'_>) -> Result<GgmlTensor<'_>, GgmlError> {
        self.new_tensor(src.typ(), &src.ne())
    }

    /// Create a tensor with one to four positive dimensions.
    ///
    /// # Errors
    /// Returns [`GgmlError`] for invalid native types, nonpositive/overflowing
    /// dimensions, a partial quantization block, or insufficient arena memory.
    pub fn new_tensor(&self, typ: ggml_type, ne: &[i64]) -> Result<GgmlTensor<'_>, GgmlError> {
        let bytes = tensor_bytes(typ, ne)?;
        self.reserve_tensor(bytes)?;
        let dimensions = i32::try_from(ne.len()).map_err(|_| GgmlError::InvalidDimensions)?;
        // SAFETY: type, dimensions, products and pool headroom were checked.
        // ggml copies the dimension slice and retains no reference to it.
        self.tensor(unsafe {
            llama_cpp_sys::ggml_new_tensor(self.ctx.as_ptr(), typ, dimensions, ne.as_ptr())
        })
    }

    // ── Tensor operations (build graph nodes) ────────────────

    /// Element-wise addition: `a + b`
    ///
    /// # Errors
    ///
    /// Returns [`GgmlError`] for invalid input or insufficient arena memory.
    pub fn add(&self, a: &GgmlTensor<'_>, b: &GgmlTensor<'_>) -> Result<GgmlTensor<'_>, GgmlError> {
        self.require_tensor(a)?;
        self.reserve_tensor(a.nbytes())?;
        self.require_tensor(b)?;
        // SAFETY: both tensors are live metadata in this arena.
        if !unsafe { llama_cpp_sys::ggml_can_repeat(b.0.as_ptr(), a.0.as_ptr()) } {
            return Err(GgmlError::InvalidDimensions);
        }
        // SAFETY: same-arena operands, broadcast shape, and output pool headroom
        // are checked above. Dependencies cannot outlive their shared arena.
        let t = unsafe { llama_cpp_sys::ggml_add(self.ctx.as_ptr(), a.0.as_ptr(), b.0.as_ptr()) };
        self.tensor(t)
    }

    /// Matrix multiplication: `a @ b`
    ///
    /// # Errors
    ///
    /// Returns [`GgmlError`] for invalid input or insufficient arena memory.
    pub fn mul_mat(
        &self,
        a: &GgmlTensor<'_>,
        b: &GgmlTensor<'_>,
    ) -> Result<GgmlTensor<'_>, GgmlError> {
        self.require_tensor(a)?;
        self.require_tensor(b)?;
        let an = a.ne();
        let bn = b.ne();
        // Native matmul rejects transposed A and requires matching inner dims.
        // SAFETY: a and b are live arena-owned metadata.
        if an[0] != bn[0]
            || bn[2] % an[2] != 0
            || bn[3] % an[3] != 0
            // SAFETY: a is live metadata in this arena; this only reads strides.
            || unsafe { llama_cpp_sys::ggml_is_transposed(a.0.as_ptr()) }
        {
            return Err(GgmlError::InvalidDimensions);
        }
        let output_bytes =
            tensor_bytes(llama_cpp_sys::GGML_TYPE_F32, &[an[1], bn[1], bn[2], bn[3]])?;
        self.reserve_tensor(output_bytes)?;
        // SAFETY: checked matmul dimensions and output byte count, same arena
        // for both dependencies, and sufficient output pool headroom.
        let t =
            unsafe { llama_cpp_sys::ggml_mul_mat(self.ctx.as_ptr(), a.0.as_ptr(), b.0.as_ptr()) };
        self.tensor(t)
    }

    /// Scale tensor: `a * s`
    ///
    /// # Errors
    ///
    /// Returns [`GgmlError`] for invalid input or insufficient arena memory.
    pub fn scale(&self, a: &GgmlTensor<'_>, s: f32) -> Result<GgmlTensor<'_>, GgmlError> {
        self.require_tensor(a)?;
        self.reserve_tensor(a.nbytes())?;
        // SAFETY: a is live metadata; native scale requires contiguous rows.
        if !unsafe { llama_cpp_sys::ggml_is_contiguous_rows(a.0.as_ptr()) } {
            return Err(GgmlError::InvalidDimensions);
        }
        // SAFETY: the arena, contiguous input and output headroom are checked.
        let t = unsafe { llama_cpp_sys::ggml_scale(self.ctx.as_ptr(), a.0.as_ptr(), s) };
        self.tensor(t)
    }

    /// Cast tensor to a different type.
    ///
    /// # Errors
    ///
    /// Returns [`GgmlError`] for invalid input or insufficient arena memory.
    pub fn cast(&self, a: &GgmlTensor<'_>, typ: ggml_type) -> Result<GgmlTensor<'_>, GgmlError> {
        self.require_tensor(a)?;
        self.reserve_tensor(tensor_bytes(typ, &a.ne())?)?;
        // SAFETY: the target type/shape and output byte budget are validated;
        // the source and its dependent output share this live arena.
        let t = unsafe { llama_cpp_sys::ggml_cast(self.ctx.as_ptr(), a.0.as_ptr(), typ) };
        self.tensor(t)
    }

    /// Make tensor contiguous in memory.
    ///
    /// # Errors
    ///
    /// Returns [`GgmlError`] for invalid input or insufficient arena memory.
    pub fn cont(&self, a: &GgmlTensor<'_>) -> Result<GgmlTensor<'_>, GgmlError> {
        self.require_tensor(a)?;
        self.reserve_tensor(a.nbytes())?;
        // SAFETY: input and output belong to this live arena and sufficient
        // storage for a contiguous copy was reserved.
        let t = unsafe { llama_cpp_sys::ggml_cont(self.ctx.as_ptr(), a.0.as_ptr()) };
        self.tensor(t)
    }

    /// Transpose a tensor.
    ///
    /// # Errors
    ///
    /// Returns [`GgmlError`] for invalid input or insufficient arena memory.
    pub fn transpose(&self, a: &GgmlTensor<'_>) -> Result<GgmlTensor<'_>, GgmlError> {
        self.require_tensor(a)?;
        self.reserve_tensor(0)?;
        // SAFETY: the new view and its source borrow the same arena; creating
        // metadata does not take ownership of externally allocated data.
        let t = unsafe { llama_cpp_sys::ggml_transpose(self.ctx.as_ptr(), a.0.as_ptr()) };
        self.tensor(t)
    }

    /// Reshape to 1D.
    ///
    /// # Errors
    ///
    /// Returns [`GgmlError`] for invalid input or insufficient arena memory.
    pub fn reshape_1d(&self, a: &GgmlTensor<'_>, ne0: i64) -> Result<GgmlTensor<'_>, GgmlError> {
        self.require_tensor(a)?;
        self.reserve_tensor(0)?;
        if tensor_bytes(a.typ(), &[ne0])? != a.nbytes()
            // SAFETY: a is live metadata; native reshape requires contiguity.
            || !unsafe { llama_cpp_sys::ggml_is_contiguous(a.0.as_ptr()) }
        {
            return Err(GgmlError::InvalidDimensions);
        }
        // SAFETY: input is contiguous, shape preserves its byte count, and the
        // same live arena owns the input and the new view metadata.
        let t = unsafe { llama_cpp_sys::ggml_reshape_1d(self.ctx.as_ptr(), a.0.as_ptr(), ne0) };
        self.tensor(t)
    }

    /// Reshape to 2D.
    ///
    /// # Errors
    ///
    /// Returns [`GgmlError`] for invalid input or insufficient arena memory.
    pub fn reshape_2d(
        &self,
        a: &GgmlTensor<'_>,
        ne0: i64,
        ne1: i64,
    ) -> Result<GgmlTensor<'_>, GgmlError> {
        self.require_tensor(a)?;
        self.reserve_tensor(0)?;
        if tensor_bytes(a.typ(), &[ne0, ne1])? != a.nbytes()
            // SAFETY: a is live metadata; native reshape requires contiguity.
            || !unsafe { llama_cpp_sys::ggml_is_contiguous(a.0.as_ptr()) }
        {
            return Err(GgmlError::InvalidDimensions);
        }
        // SAFETY: shape preserves the contiguous input byte count and both
        // the source and the view metadata remain owned by this arena.
        let t =
            unsafe { llama_cpp_sys::ggml_reshape_2d(self.ctx.as_ptr(), a.0.as_ptr(), ne0, ne1) };
        self.tensor(t)
    }

    /// Create a 1D view of a tensor.
    ///
    /// # Errors
    ///
    /// Returns [`GgmlError`] for invalid input or insufficient arena memory.
    pub fn view_1d(
        &self,
        a: &GgmlTensor<'_>,
        ne0: i64,
        offset: usize,
    ) -> Result<GgmlTensor<'_>, GgmlError> {
        self.require_tensor(a)?;
        self.reserve_tensor(0)?;
        let bytes = tensor_bytes(a.typ(), &[ne0])?;
        if offset.checked_add(bytes).is_none_or(|end| end > a.nbytes()) {
            return Err(GgmlError::InvalidDimensions);
        }
        // SAFETY: checked offset plus view bytes stay within the source and
        // both metadata objects belong to this arena. External buffer validity
        // is part of the unsafe allocation API contract.
        let t =
            unsafe { llama_cpp_sys::ggml_view_1d(self.ctx.as_ptr(), a.0.as_ptr(), ne0, offset) };
        self.tensor(t)
    }

    // ── Graph creation ───────────────────────────────────────

    /// Create a new computation graph.
    ///
    /// # Errors
    ///
    /// Returns [`GgmlError`] for invalid input or insufficient arena memory.
    pub fn new_graph(&self) -> Result<GgmlGraph<'_>, GgmlError> {
        if graph_overhead() > self.free_mem() {
            return Err(GgmlError::InsufficientMemory);
        }
        // SAFETY: pool headroom covers the native default graph layout; the
        // returned graph metadata is retained by the context borrow.
        let g = unsafe { llama_cpp_sys::ggml_new_graph(self.ctx.as_ptr()) };
        Ok(GgmlGraph(
            NonNull::new(g).ok_or(GgmlError::NullHandle)?,
            self,
        ))
    }

    // ── Tensor iteration ─────────────────────────────────────

    /// Get the first tensor in this context.
    #[must_use]
    pub fn first_tensor(&self) -> Option<GgmlTensor<'_>> {
        // SAFETY: the live arena owns the linked metadata list.
        let t = unsafe { llama_cpp_sys::ggml_get_first_tensor(self.ctx.as_ptr()) };
        NonNull::new(t).map(|pointer| GgmlTensor(pointer, self))
    }

    /// Get the next tensor after `tensor` in this context.
    ///
    /// # Errors
    /// Returns [`GgmlError::ForeignContext`] for a tensor from another arena.
    pub fn next_tensor(
        &self,
        tensor: &GgmlTensor<'_>,
    ) -> Result<Option<GgmlTensor<'_>>, GgmlError> {
        self.require_tensor(tensor)?;
        // SAFETY: require_tensor proved membership in this arena, so the native
        // iterator never compares pointers from unrelated arena allocations.
        let t =
            unsafe { llama_cpp_sys::ggml_get_next_tensor(self.ctx.as_ptr(), tensor.0.as_ptr()) };
        Ok(NonNull::new(t).map(|pointer| GgmlTensor(pointer, self)))
    }
}

impl Drop for GgmlContext {
    fn drop(&mut self) {
        // SAFETY: this wrapper owns the arena exactly once; every safe metadata
        // handle borrows it and therefore cannot survive this destructor.
        unsafe { llama_cpp_sys::ggml_free(self.ctx.as_ptr()) }
    }
}

// ── Tensor ──────────────────────────────────────────────────

/// A wrapper around a `ggml_tensor` pointer.
///
/// Tensors are owned by their `GgmlContext` and must not outlive it.
/// This wrapper does NOT free the tensor on drop. Names are returned as owned
/// strings because another copy of the handle may rename the native buffer.
///
/// ```compile_fail
/// use llama_cpp::ggml::{GgmlContext, GgmlError};
/// fn cannot_escape() -> Result<(), GgmlError> {
///     let tensor = {
///         let ctx = GgmlContext::sized_for(1, 0, true)?;
///         ctx.new_tensor_1d(llama_cpp_sys::GGML_TYPE_F32, 4)?
///     };
///     let _ = tensor.nelements();
///     Ok(())
/// }
/// ```
#[derive(Clone, Copy)]
pub struct GgmlTensor<'ctx>(
    pub(crate) NonNull<llama_cpp_sys::ggml_tensor>,
    &'ctx GgmlContext,
);

impl GgmlTensor<'_> {
    /// Get the raw tensor pointer.
    #[must_use]
    pub fn as_ptr(&self) -> *mut llama_cpp_sys::ggml_tensor {
        self.0.as_ptr()
    }

    /// Set the complete tensor name without truncating UTF-8.
    ///
    /// # Errors
    /// Returns [`GgmlError::NameTooLong`] or [`GgmlError::Nul`] for invalid names.
    pub fn set_name(&self, name: &str) -> Result<(), GgmlError> {
        // SAFETY: the context borrow keeps the native fixed-size name array live.
        let name_capacity = unsafe { (*self.0.as_ptr()).name.len() };
        if name.len() >= name_capacity {
            return Err(GgmlError::NameTooLong);
        }
        let name = std::ffi::CString::new(name)?;
        // SAFETY: metadata is arena-owned and thread-confined. ggml copies the
        // checked complete name; no borrowed Rust string is retained.
        unsafe { llama_cpp_sys::ggml_set_name(self.0.as_ptr(), name.as_ptr()) };
        Ok(())
    }

    /// Number of elements in the live tensor metadata.
    #[must_use]
    pub fn nelements(&self) -> i64 {
        // SAFETY: the arena borrow retains this tensor; dimensions were checked
        // before creation, and element-count arithmetic fits the native i64.
        unsafe { llama_cpp_sys::ggml_nelements(self.0.as_ptr()) }
    }

    /// Get the total size in bytes.
    #[must_use]
    pub fn nbytes(&self) -> usize {
        // SAFETY: live arena metadata with a checked type, shape and strides.
        unsafe { llama_cpp_sys::ggml_nbytes(self.0.as_ptr()) }
    }

    /// Get the element size in bytes.
    #[must_use]
    pub fn element_size(&self) -> usize {
        // SAFETY: the live tensor contains a validated native type-table index.
        unsafe { llama_cpp_sys::ggml_element_size(self.0.as_ptr()) }
    }

    /// Get the tensor type.
    #[must_use]
    pub fn typ(&self) -> ggml_type {
        // SAFETY: the arena borrow retains this tensor metadata; the type is copied.
        unsafe { (*self.0.as_ptr()).type_ }
    }

    /// Get the tensor dimensions (ne).
    #[must_use]
    pub fn ne(&self) -> [i64; 4] {
        // SAFETY: the arena borrow retains the fixed-size dimensions; copy them out.
        unsafe { (*self.0.as_ptr()).ne }
    }

    /// Get the tensor name.
    #[must_use]
    pub fn name(&self) -> String {
        // SAFETY: the context borrow keeps metadata and its fixed-size,
        // NUL-terminated name buffer alive. Copy before any alias can rename it.
        let name = unsafe { CStr::from_ptr((*self.0.as_ptr()).name.as_ptr()) };
        name.to_string_lossy().into_owned()
    }
}

impl std::fmt::Debug for GgmlTensor<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let ne = self.ne();
        write!(
            f,
            "GgmlTensor({:?}, [{}, {}, {}, {}], {} bytes)",
            self.name(),
            ne[0],
            ne[1],
            ne[2],
            ne[3],
            self.nbytes()
        )
    }
}

// ── Graph ───────────────────────────────────────────────────

/// A wrapper around `ggml_cgraph`.
///
/// Graphs are owned by their `GgmlContext` and must not outlive it.
///
/// ```compile_fail
/// use llama_cpp::ggml::{GgmlContext, GgmlError};
/// fn cannot_escape() -> Result<(), GgmlError> {
///     let mut graph = {
///         let ctx = GgmlContext::sized_for(0, 1, true)?;
///         ctx.new_graph()?
///     };
///     let _ = graph.as_ptr();
///     Ok(())
/// }
/// ```
pub struct GgmlGraph<'ctx>(NonNull<llama_cpp_sys::ggml_cgraph>, &'ctx GgmlContext);

impl std::fmt::Debug for GgmlGraph<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GgmlGraph").finish()
    }
}

impl<'ctx> GgmlGraph<'ctx> {
    /// Get the raw graph pointer.
    pub fn as_ptr(&mut self) -> *mut llama_cpp_sys::ggml_cgraph {
        self.0.as_ptr()
    }

    /// Add a tensor and its dependencies to the forward computation graph.
    ///
    /// # Errors
    /// Returns [`GgmlError::ForeignContext`] for a dependency in another arena.
    pub fn build_forward(&mut self, tensor: &GgmlTensor<'_>) -> Result<(), GgmlError> {
        self.1.require_tensor(tensor)?;
        // SAFETY: graph and tensor dependencies are restricted to the same live
        // arena. Native traversal retains no external Rust references.
        unsafe { llama_cpp_sys::ggml_build_forward_expand(self.0.as_ptr(), tensor.0.as_ptr()) };
        Ok(())
    }

    /// Get a node (output tensor) by index. Use -1 for the last node.
    ///
    /// # Errors
    ///
    /// Returns [`GgmlError`] for invalid input or insufficient arena memory.
    pub fn node(&mut self, i: i32) -> Result<GgmlTensor<'ctx>, GgmlError> {
        // SAFETY: the graph's arena is retained by its context borrow.
        let count = unsafe { llama_cpp_sys::ggml_graph_n_nodes(self.0.as_ptr()) };
        if i < -count || i >= count {
            return Err(GgmlError::InvalidDimensions);
        }
        // SAFETY: index is checked against node count, including negative indices,
        // and the graph and every dependency share the retained arena.
        let t = unsafe { llama_cpp_sys::ggml_graph_node(self.0.as_ptr(), i) };
        Ok(GgmlTensor(
            NonNull::new(t).ok_or(GgmlError::NullHandle)?,
            self.1,
        ))
    }
}

// ── Backend ─────────────────────────────────────────────────

/// A safe wrapper around `ggml_backend`.
#[derive(Debug)]
pub struct GgmlBackend {
    backend: llama_cpp_sys::ggml_backend_t,
}

impl GgmlBackend {
    /// Create a CPU backend.
    ///
    /// # Errors
    ///
    /// Returns [`GgmlError::NullHandle`] if CPU initialization fails.
    pub fn cpu() -> Result<Self, GgmlError> {
        // SAFETY: native CPU initialization accepts no borrowed pointers and
        // returns a uniquely owned backend, released by Drop after allocators.
        let backend = unsafe { llama_cpp_sys::ggml_backend_cpu_init() };
        NonNull::new(backend).ok_or(GgmlError::NullHandle)?;
        Ok(Self { backend })
    }

    /// Set the number of threads for the CPU backend.
    pub fn set_n_threads(&self, n_threads: i32) {
        // SAFETY: only CPU initialization constructs this backend type, and the
        // setter updates native thread configuration without retaining pointers.
        unsafe { llama_cpp_sys::ggml_backend_cpu_set_n_threads(self.backend, n_threads) }
    }

    /// Allocate all tensors in a context on this backend.
    ///
    /// Returns the raw buffer handle which must be kept alive.
    ///
    /// # Safety
    /// Retain the returned buffer and this backend through every operation that
    /// uses any tensor address from `ctx`, including subsequent view creation.
    /// Free it exactly once, after the final such operation.
    #[must_use]
    pub unsafe fn alloc_ctx_tensors(
        &self,
        ctx: &GgmlContext,
    ) -> *mut llama_cpp_sys::ggml_backend_buffer {
        // SAFETY: forwarded external-storage lifetime contract; backend and
        // arena handles themselves are valid throughout this call.
        unsafe { llama_cpp_sys::ggml_backend_alloc_ctx_tensors(ctx.as_ptr(), self.backend) }
    }

    /// Compute a graph, reporting a non-success native status.
    ///
    /// # Safety
    /// Every input and intermediate must have initialized, sufficiently large
    /// storage for the selected backend. Retain every allocator/backend buffer
    /// through this call and all future metadata operations using its addresses.
    /// No other graph may mutate or reallocate the same tensor storage during
    /// execution. These external buffer lifetimes are not tracked by this API.
    ///
    /// # Errors
    /// Returns [`GgmlError::Compute`] for a non-success native status.
    pub unsafe fn graph_compute(&self, graph: &mut GgmlGraph<'_>) -> Result<(), GgmlError> {
        // SAFETY: the caller guarantees all tensor buffers remain live and
        // initialized, with no competing execution or allocation.
        let status =
            unsafe { llama_cpp_sys::ggml_backend_graph_compute(self.backend, graph.as_ptr()) };
        if status != llama_cpp_sys::GGML_STATUS_SUCCESS {
            return Err(GgmlError::Compute(status));
        }
        Ok(())
    }

    /// Get the default buffer type for this backend.
    #[must_use]
    pub fn default_buffer_type(&self) -> llama_cpp_sys::ggml_backend_buffer_type_t {
        // SAFETY: backend owns this borrowed buffer type. Only unsafe raw users
        // can retain/use it after the backend is destroyed.
        unsafe { llama_cpp_sys::ggml_backend_get_default_buffer_type(self.backend) }
    }

    /// Get the raw backend pointer.
    #[must_use]
    pub fn as_ptr(&self) -> llama_cpp_sys::ggml_backend_t {
        self.backend
    }
}

impl Drop for GgmlBackend {
    fn drop(&mut self) {
        // SAFETY: this wrapper uniquely owns the backend. Allocators borrow it, so
        // no safe allocator handle can survive this destructor.
        unsafe { llama_cpp_sys::ggml_backend_free(self.backend) }
    }
}

// ── Graph allocator ─────────────────────────────────────────

/// A safe wrapper around `ggml_gallocr`.
#[derive(Debug)]
pub struct GgmlAllocr<'backend> {
    alloc: llama_cpp_sys::ggml_gallocr_t,
    /// Allocator buffers are created by, and cannot outlive, this backend.
    _backend: &'backend GgmlBackend,
}

impl<'backend> GgmlAllocr<'backend> {
    /// Create a new graph allocator for the given backend.
    ///
    /// # Errors
    ///
    /// Returns [`GgmlError::NullHandle`] if allocator initialization fails.
    pub fn new(backend: &'backend GgmlBackend) -> Result<Self, GgmlError> {
        // SAFETY: backend is borrowed for this allocator lifetime, so its
        // default buffer type cannot be invalidated before allocator Drop.
        let alloc = unsafe { llama_cpp_sys::ggml_gallocr_new(backend.default_buffer_type()) };
        NonNull::new(alloc).ok_or(GgmlError::NullHandle)?;
        Ok(Self {
            alloc,
            _backend: backend,
        })
    }

    /// Allocate all tensors in a graph.
    ///
    /// # Safety
    /// Retain this allocator through every use of the tensor addresses it
    /// installs, including metadata/view operations. Reallocation may invalidate
    /// addresses from any previously allocated graph. No competing access to
    /// affected tensors is allowed during allocation.
    #[must_use]
    pub unsafe fn alloc_graph(&mut self, graph: &mut GgmlGraph<'_>) -> bool {
        // SAFETY: allocator/graph handles are live; the caller guarantees no
        // competing tensor access and retains all installed storage addresses.
        unsafe { llama_cpp_sys::ggml_gallocr_alloc_graph(self.alloc, graph.as_ptr()) }
    }
}

impl Drop for GgmlAllocr<'_> {
    fn drop(&mut self) {
        // SAFETY: the unique allocator is freed exactly once before its borrowed
        // backend expires; callers own outstanding tensor-data address contracts.
        unsafe { llama_cpp_sys::ggml_gallocr_free(self.alloc) }
    }
}

/// Validate native type-table indices and byte-size arithmetic before FFI.
fn tensor_bytes(typ: ggml_type, dimensions: &[i64]) -> Result<usize, GgmlError> {
    if typ >= llama_cpp_sys::GGML_TYPE_COUNT || dimensions.is_empty() || dimensions.len() > 4 {
        return Err(GgmlError::InvalidDimensions);
    }
    // SAFETY: typ is within the native type table. Removed types have zero
    // block/type size and are rejected below, before any tensor is allocated.
    let block = unsafe { llama_cpp_sys::ggml_blck_size(typ) };
    let block = usize::try_from(block).map_err(|_| GgmlError::InvalidDimensions)?;
    // SAFETY: checked native type-table index as above.
    let size = unsafe { llama_cpp_sys::ggml_type_size(typ) };
    if block == 0 || size == 0 {
        return Err(GgmlError::InvalidDimensions);
    }
    let mut elements = 1_usize;
    for &dimension in dimensions {
        let length = usize::try_from(dimension).map_err(|_| GgmlError::InvalidDimensions)?;
        if length == 0 {
            return Err(GgmlError::InvalidDimensions);
        }
        elements = elements
            .checked_mul(length)
            .ok_or(GgmlError::InvalidDimensions)?;
    }
    let first = usize::try_from(dimensions[0]).map_err(|_| GgmlError::InvalidDimensions)?;
    if first % block != 0 || i64::try_from(elements).is_err() {
        return Err(GgmlError::InvalidDimensions);
    }
    let bytes = (elements / block)
        .checked_mul(size)
        .ok_or(GgmlError::InvalidDimensions)?;
    if bytes > isize::MAX as usize {
        return Err(GgmlError::InvalidDimensions);
    }
    Ok(bytes)
}

// ── Utility functions ───────────────────────────────────────

/// Set tensor data from a byte slice.
///
/// # Safety
///
/// The tensor must have live backend storage covering `data.len()` bytes.
/// No competing reads, writes or execution may access that storage.
pub unsafe fn tensor_set(tensor: &GgmlTensor<'_>, data: &[u8]) {
    // SAFETY: the caller guarantees a live allocation covering the requested
    // bytes; the data slice is borrowed synchronously and is not retained.
    unsafe {
        llama_cpp_sys::ggml_backend_tensor_set(
            tensor.0.as_ptr(),
            data.as_ptr().cast(),
            0,
            data.len(),
        );
    };
}

/// Get tensor data into a byte slice.
///
/// # Safety
///
/// The tensor must have initialized live storage covering `data.len()` bytes.
/// No competing writes may access that storage.
pub unsafe fn tensor_get(tensor: &GgmlTensor<'_>, data: &mut [u8]) {
    // SAFETY: caller guarantees the live source allocation and no competing
    // writes. The destination slice covers exactly the requested byte count.
    unsafe {
        llama_cpp_sys::ggml_backend_tensor_get(
            tensor.0.as_ptr(),
            data.as_mut_ptr().cast(),
            0,
            data.len(),
        );
    };
}

/// Free a backend buffer.
///
/// # Safety
///
/// The buffer must be valid and not already freed.
pub unsafe fn buffer_free(buffer: *mut llama_cpp_sys::ggml_backend_buffer) {
    // SAFETY: caller guarantees unique ownership and no remaining address use.
    unsafe { llama_cpp_sys::ggml_backend_buffer_free(buffer) };
}

/// Get the overhead in bytes for tensor metadata.
#[must_use]
pub fn tensor_overhead() -> usize {
    // SAFETY: this returns a native layout constant and takes no pointers.
    unsafe { llama_cpp_sys::ggml_tensor_overhead() }
}

/// Get the overhead in bytes for a computation graph.
#[must_use]
pub fn graph_overhead() -> usize {
    // SAFETY: this returns the default graph layout size, with no borrowed data.
    unsafe { llama_cpp_sys::ggml_graph_overhead() }
}

/// Check if a type is quantized.
///
/// # Errors
/// Returns [`GgmlError::InvalidDimensions`] for an invalid native type index.
pub fn is_quantized(typ: ggml_type) -> Result<bool, GgmlError> {
    if typ >= llama_cpp_sys::GGML_TYPE_COUNT {
        return Err(GgmlError::InvalidDimensions);
    }
    // SAFETY: typ was range-checked before indexing the native type table.
    Ok(unsafe { llama_cpp_sys::ggml_is_quantized(typ) })
}

/// Get the name of a ggml type.
///
/// # Errors
/// Returns [`GgmlError`] for an invalid/removed type or invalid native UTF-8.
pub fn type_name(typ: ggml_type) -> Result<&'static str, GgmlError> {
    if typ >= llama_cpp_sys::GGML_TYPE_COUNT {
        return Err(GgmlError::InvalidDimensions);
    }
    // SAFETY: typ indexes the checked native table. Names are static strings;
    // a removed type can have a null entry, which is rejected before reading.
    let pointer = unsafe { llama_cpp_sys::ggml_type_name(typ) };
    if pointer.is_null() {
        return Err(GgmlError::InvalidDimensions);
    }
    // SAFETY: the table entry is a static NUL-terminated string.
    Ok(unsafe { CStr::from_ptr(pointer) }.to_str()?)
}
