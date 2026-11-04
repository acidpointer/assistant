//! Per-buffer-type memory usage reported by llama.cpp.
//!
//! [`MemoryBreakdownEntry`] values are produced by
//! [`crate::context::LlamaContext::memory_breakdown`] and classify bytes into
//! model weights, KV / recurrent cache, and temporary compute buffers for each
//! ggml backend buffer type (e.g. `CUDA0`, `Metal`, `Host`).
//!
//! # Examples
//!
//! ```no_run
//! use llama_cpp::prelude::*;
//!
//! fn main() {
//!     let backend = LlamaBackend::init().unwrap();
//!     let model = LlamaModel::load_from_file(&backend, "model.gguf", &LlamaModelParams::default()).unwrap();
//!     let ctx = model.new_context(&backend, LlamaContextParams::default()).unwrap();
//!     for entry in ctx.memory_breakdown() {
//!         println!("{}: {} bytes total", entry.buft_name, entry.total());
//!     }
//! }
//! ```

use crate::{shim, ShimError};
use std::ffi::CStr;

/// Memory attributed to a single backend buffer type (e.g. CUDA0, Host).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryBreakdownEntry {
    /// Human-readable buffer-type name from ggml.
    pub buft_name: String,
    /// Bytes used by model weights on this buffer type.
    pub model: usize,
    /// Bytes used by the KV / recurrent context cache.
    pub context: usize,
    /// Bytes used by temporary compute buffers.
    pub compute: usize,
}

impl MemoryBreakdownEntry {
    /// Checked sum for admission/accounting. Includes shared model weights.
    #[must_use]
    pub fn checked_total(&self) -> Option<usize> {
        self.model
            .checked_add(self.context)?
            .checked_add(self.compute)
    }

    /// Per-context accounted buffers, excluding shared model weights.
    #[must_use]
    pub fn checked_context_compute(&self) -> Option<usize> {
        self.context.checked_add(self.compute)
    }
    /// Sum of model, context, and compute bytes.
    #[must_use]
    pub fn total(&self) -> usize {
        self.model + self.context + self.compute
    }
}

/// Decode a complete fixed-width ABI name without silently truncating it.
pub(crate) fn decode_name(name: &[std::ffi::c_char]) -> Result<String, ShimError> {
    let end = name
        .iter()
        .position(|b| *b == 0)
        .ok_or(ShimError::CorruptResult)?;
    String::from_utf8(name[..end].iter().map(|b| b.to_ne_bytes()[0]).collect())
        .map_err(ShimError::from)
}

/// Read a stable, bounded scalar snapshot with the shared query/fill protocol.
pub(crate) fn read_records<T: Copy>(
    empty: T,
    mut call: impl FnMut(*mut T, usize, *mut usize) -> i32,
) -> Result<Vec<T>, ShimError> {
    let mut needed = 0;
    let status = call(std::ptr::null_mut(), 0, &raw mut needed);
    if status != llama_cpp_sys::LLAMA_SHIM_BUFFER_TOO_SMALL {
        shim::check_status(status)?;
    }
    if needed > 4096 {
        return Err(ShimError::CorruptResult);
    }
    if needed == 0 {
        if status == llama_cpp_sys::LLAMA_SHIM_BUFFER_TOO_SMALL {
            return Err(ShimError::CorruptResult);
        }
        return Ok(Vec::new());
    }
    let mut records = Vec::new();
    records
        .try_reserve_exact(needed)
        .map_err(|e| ShimError::Failed(e.to_string()))?;
    records.resize(needed, empty);
    let capacity = records.len();
    let status = call(records.as_mut_ptr(), capacity, &raw mut needed);
    if status == llama_cpp_sys::LLAMA_SHIM_BUFFER_TOO_SMALL || needed != capacity {
        return Err(ShimError::CorruptResult);
    }
    shim::check_status(status)?;
    records.truncate(needed);
    Ok(records)
}

/// Collect complete accounting or return the native/ABI error.
pub(crate) fn try_collect(
    ctx: *const llama_cpp_sys::llama_context,
) -> Result<Vec<MemoryBreakdownEntry>, ShimError> {
    let empty = llama_cpp_sys::llama_memory_breakdown_entry {
        buft_name: [0; 128],
        model: 0,
        context: 0,
        compute: 0,
    };
    // SAFETY: the caller retains a thread-confined live context. The helper
    // supplies writable arrays and required-length storage of the ABI types.
    let records = read_records(empty, |out, cap, required| unsafe {
        llama_cpp_sys::llama_memory_breakdown_collect_v2(ctx, out, cap, required)
    })?;
    records
        .iter()
        .map(|entry| {
            Ok(MemoryBreakdownEntry {
                buft_name: decode_name(&entry.buft_name)?,
                model: entry.model,
                context: entry.context,
                compute: entry.compute,
            })
        })
        .collect()
}

fn raw_entry_to_rust(entry: &llama_cpp_sys::llama_memory_breakdown_entry) -> MemoryBreakdownEntry {
    let bytes: &[u8] = unsafe {
        std::slice::from_raw_parts(entry.buft_name.as_ptr().cast(), entry.buft_name.len())
    };
    let name = CStr::from_bytes_until_nul(bytes)
        .map(|c| c.to_string_lossy().into_owned())
        .unwrap_or_default();
    MemoryBreakdownEntry {
        buft_name: name,
        model: entry.model,
        context: entry.context,
        compute: entry.compute,
    }
}

/// Collect memory breakdown entries for a live context.
///
/// Wraps the `ext_shim` helper around `llama_get_memory_breakdown`. Grows the
/// output buffer until every entry fits. Returns an empty vector when the
/// context pointer is invalid or no buffers are registered yet.
///
/// Prefer [`crate::context::LlamaContext::memory_breakdown`] in application code.
#[must_use]
pub(crate) fn collect_memory_breakdown(
    ctx: *const llama_cpp_sys::llama_context,
) -> Vec<MemoryBreakdownEntry> {
    if ctx.is_null() {
        return Vec::new();
    }

    let mut capacity = 16usize;
    loop {
        let mut raw = vec![
            llama_cpp_sys::llama_memory_breakdown_entry {
                buft_name: [0; 128],
                model: 0,
                context: 0,
                compute: 0,
            };
            capacity
        ];

        let n = unsafe {
            llama_cpp_sys::llama_memory_breakdown_collect(ctx, raw.as_mut_ptr(), capacity)
        };

        if n < capacity {
            return raw
                .into_iter()
                .take(n)
                .map(|e| raw_entry_to_rust(&e))
                .collect();
        }

        capacity = capacity.saturating_mul(2);
        if capacity > 4096 {
            return raw.into_iter().map(|e| raw_entry_to_rust(&e)).collect();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn incomplete_or_unbounded_reports_fail_instead_of_truncating() {
        let mut calls = 0;
        let result = read_records(0_u32, |_, _, needed| {
            calls += 1;
            // SAFETY: read_records supplies a valid required-length pointer.
            unsafe {
                *needed = if calls == 1 { 1 } else { 2 };
            }
            llama_cpp_sys::LLAMA_SHIM_BUFFER_TOO_SMALL
        });
        assert!(matches!(result, Err(ShimError::CorruptResult)));
        let result = read_records(0_u32, |_, _, needed| {
            // SAFETY: read_records supplies a valid required-length pointer.
            unsafe {
                *needed = 4097;
            }
            llama_cpp_sys::LLAMA_SHIM_BUFFER_TOO_SMALL
        });
        assert!(matches!(result, Err(ShimError::CorruptResult)));
        assert!(decode_name(&[1, 2]).is_err());
        assert!(decode_name(&[std::ffi::c_char::from_ne_bytes([0xff]), 0]).is_err());
        let result = read_records(0_u32, |_, _, _| llama_cpp_sys::LLAMA_SHIM_INVALID_ARG);
        assert!(matches!(result, Err(ShimError::InvalidArg)));
        let result = read_records(0_u32, |_, _, _| llama_cpp_sys::LLAMA_SHIM_BUFFER_TOO_SMALL);
        assert!(matches!(result, Err(ShimError::CorruptResult)));
    }

    #[test]
    fn stable_snapshot_is_complete_and_shrinking_counts_are_rejected() {
        let mut calls = 0;
        let result = read_records(0_u32, |out, capacity, required| {
            calls += 1;
            // SAFETY: the helper supplies valid scalar count storage, and on
            // fill a writable array of exactly the reported capacity.
            unsafe {
                *required = 2;
                if capacity == 0 {
                    return llama_cpp_sys::LLAMA_SHIM_BUFFER_TOO_SMALL;
                }
                assert_eq!(capacity, 2);
                out.write(17);
                out.add(1).write(23);
            }
            llama_cpp_sys::LLAMA_SHIM_OK
        })
        .expect("complete query/fill snapshot");
        assert_eq!(calls, 2);
        assert_eq!(result, [17, 23]);
        let mut calls = 0;
        let result = read_records(0_u32, |_, _, required| {
            calls += 1;
            // SAFETY: writable required-count storage belongs to the helper.
            unsafe {
                *required = if calls == 1 { 2 } else { 1 };
            }
            llama_cpp_sys::LLAMA_SHIM_OK
        });
        assert!(matches!(result, Err(ShimError::CorruptResult)));
    }

    #[test]
    fn accounting_does_not_wrap() {
        let entry = MemoryBreakdownEntry {
            buft_name: String::new(),
            model: 1,
            context: usize::MAX,
            compute: 1,
        };
        assert_eq!(entry.checked_total(), None);
        assert_eq!(entry.checked_context_compute(), None);
    }
}
