//! Experimental, model-free PQ3 CPU oracle. Blocks own their bytes; operations
//! are stateless/thread-safe and contain native exceptions through the shim.
//! Inputs must already be in the chosen graph transform's coordinate system.
//! This module does not enable PQ3 caches, GPU execution or portable state I/O.
use crate::{shim, ShimError};

/// Values in one PQ3 reference block; 64-value heads are deliberately unsupported.
pub const BLOCK_VALUES: usize = 128;
/// Packed indices (48 bytes) followed by one native GGML half norm (2 bytes).
pub const BLOCK_BYTES: usize = 50;

/// Validated immutable PQ3 reference storage. This is process-native block data,
/// not a versioned cache file. Do not persist it as an inference checkpoint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pq3Block {
    bytes: [u8; BLOCK_BYTES],
}

fn check_abi() -> Result<(), ShimError> {
    // SAFETY: the feature-selected native symbol is a pure layout/version query.
    // Referencing it also prevents linking this API against a feature-off library.
    let version = unsafe { llama_cpp_sys::ggml_pq3_reference_abi_v1() };
    if version != 1 {
        return Err(ShimError::Failed(format!(
            "unsupported PQ3 reference ABI {version}"
        )));
    }
    // SAFETY: pure query in the CPU archive; a separate symbol prevents mixing
    // a new base library with an older CPU type-traits table.
    let cpu_version = unsafe { llama_cpp_sys::ggml_pq3_cpu_reference_abi_v1() };
    if cpu_version != 1 {
        return Err(ShimError::Failed(format!(
            "unsupported PQ3 CPU ABI {cpu_version}"
        )));
    }
    Ok(())
}

impl Pq3Block {
    /// Quantize one already-transformed block with the frozen L2/codebook policy.
    /// # Errors
    /// Rejects non-finite input, L2 norms above 65504, ABI mismatches and native failures.
    pub fn quantize(values: &[f32; BLOCK_VALUES]) -> Result<Self, ShimError> {
        check_abi()?;
        let mut bytes = [0; BLOCK_BYTES];
        // SAFETY: fixed arrays provide the exact readable/writable extents; the
        // shim validates numeric inputs and copies through aligned native storage.
        shim::check_status(unsafe {
            llama_cpp_sys::llama_pq3_encode_block_guarded(
                values.as_ptr(),
                values.len(),
                bytes.as_mut_ptr(),
                bytes.len(),
            )
        })?;
        Ok(Self { bytes })
    }
    /// Validate a complete raw block before retaining it.
    /// # Errors
    /// Rejects non-finite/negative norms, noncanonical zero blocks and native/ABI errors.
    pub fn from_bytes(bytes: [u8; BLOCK_BYTES]) -> Result<Self, ShimError> {
        let block = Self { bytes };
        block.dequantize()?;
        Ok(block)
    }
    /// Reconstruct coordinates without applying inverse graph rotation.
    /// # Errors
    /// Returns native validation or ABI failures.
    pub fn dequantize(&self) -> Result<[f32; BLOCK_VALUES], ShimError> {
        check_abi()?;
        let mut values = [0.0; BLOCK_VALUES];
        // SAFETY: immutable block bytes and writable values have fixed ABI sizes.
        // Native reads use memcpy, so the byte array need not be half-aligned.
        shim::check_status(unsafe {
            llama_cpp_sys::llama_pq3_decode_block_guarded(
                self.bytes.as_ptr(),
                self.bytes.len(),
                values.as_mut_ptr(),
                values.len(),
            )
        })?;
        Ok(values)
    }
    /// CPU reference score against a query quantized to four Q8_0 blocks.
    /// This includes Q8 query error; it is not an exact F32-query dot product.
    /// # Errors
    /// Rejects invalid/oversized query norms, native failures and ABI mismatches.
    pub fn dot_q8(&self, query: &[f32; BLOCK_VALUES]) -> Result<f32, ShimError> {
        check_abi()?;
        let mut score = 0.0;
        // SAFETY: fixed input arrays and one writable scalar meet the guarded ABI.
        shim::check_status(unsafe {
            llama_cpp_sys::llama_pq3_dot_q8_guarded(
                self.bytes.as_ptr(),
                self.bytes.len(),
                query.as_ptr(),
                query.len(),
                &raw mut score,
            )
        })?;
        Ok(score)
    }
    /// Borrow immutable native bytes for numerical fixtures; not a cache-file format.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8; BLOCK_BYTES] {
        &self.bytes
    }
}
