//! Session/state I/O over a live borrowed context on its owner thread. Owned
//! paths and token buffers outlive each synchronous native call. PQ3 persistence
//! fails before allocation/I/O until a versioned format exists. Legacy raw-copy
//! callers retain the documented capacity obligations; bounded reads use slices.

use crate::context::LlamaContext;
use crate::token::LlamaToken;
use std::ffi::{CString, NulError};
use std::path::{Path, PathBuf};

/// Failed to save a Session file
#[derive(Debug, Eq, PartialEq, thiserror::Error)]
pub enum SaveSessionError {
    /// Experimental PQ3 cache persistence has no versioned state format yet.
    #[cfg(feature = "turboquant")]
    #[error("PQ3 persistence is unsupported; reset and replay instead")]
    Pq3Unsupported,
    /// llama.cpp failed to save the session file
    #[error("Failed to save session file")]
    FailedToSave,

    /// null byte in string
    #[error("null byte in string {0}")]
    NullError(#[from] NulError),

    /// failed to convert path to str
    #[error("failed to convert path {0} to str")]
    PathToStrError(PathBuf),
}

/// Failed to load a Session file
#[derive(Debug, Eq, PartialEq, thiserror::Error)]
pub enum LoadSessionError {
    /// Experimental PQ3 cache persistence has no versioned state format yet.
    #[cfg(feature = "turboquant")]
    #[error("PQ3 persistence is unsupported; reset and replay instead")]
    Pq3Unsupported,
    /// llama.cpp failed to load the session file
    #[error("Failed to load session file")]
    FailedToLoad,

    /// null byte in string
    #[error("null byte in string {0}")]
    NullError(#[from] NulError),

    /// failed to convert path to str
    #[error("failed to convert path {0} to str")]
    PathToStrError(PathBuf),

    /// Insufficient max length
    #[error("max_length is not large enough to hold {n_out} (was {max_tokens})")]
    InsufficientMaxLength {
        /// The length of the session file
        n_out: usize,
        /// The maximum length
        max_tokens: usize,
    },
}

impl LlamaContext<'_> {
    /// Save the current session to a file.
    ///
    /// # Parameters
    ///
    /// * `path_session` - The file to save to.
    /// * `tokens` - The tokens to associate the session with. This should be a prefix of a sequence of tokens that the context has processed, so that the relevant KV caches are already filled.
    ///
    /// # Errors
    ///
    /// Fails if the path is not a valid utf8, is not a valid c string, or llama.cpp fails to save the session file.
    pub fn save_session_file(
        &self,
        path_session: impl AsRef<Path>,
        tokens: &[LlamaToken],
    ) -> Result<(), SaveSessionError> {
        #[cfg(feature = "turboquant")]
        if self.uses_pq3_cache() {
            return Err(SaveSessionError::Pq3Unsupported);
        }
        let path = path_session.as_ref();
        let path = path
            .to_str()
            .ok_or_else(|| SaveSessionError::PathToStrError(path.to_path_buf()))?;

        let cstr = CString::new(path)?;

        if unsafe {
            llama_cpp_sys::llama_save_session_file(
                self.context.as_ptr(),
                cstr.as_ptr(),
                tokens.as_ptr().cast::<llama_cpp_sys::llama_token>(),
                tokens.len(),
            )
        } {
            Ok(())
        } else {
            Err(SaveSessionError::FailedToSave)
        }
    }
    /// Load a session file into the current context.
    ///
    /// You still need to pass the returned tokens to the context for inference to work. What this function buys you is that the KV caches are already filled with the relevant data.
    ///
    /// # Parameters
    ///
    /// * `path_session` - The file to load from. It must be a session file from a compatible context, otherwise the function will error.
    /// * `max_tokens` - The maximum token length of the loaded session. If the session was saved with a longer length, the function will error.
    ///
    /// # Errors
    ///
    /// Fails if the path is not a valid utf8, is not a valid c string, or llama.cpp fails to load the session file. (e.g. the file does not exist, is not a session file, etc.)
    pub fn load_session_file(
        &mut self,
        path_session: impl AsRef<Path>,
        max_tokens: usize,
    ) -> Result<Vec<LlamaToken>, LoadSessionError> {
        #[cfg(feature = "turboquant")]
        if self.uses_pq3_cache() {
            return Err(LoadSessionError::Pq3Unsupported);
        }
        let path = path_session.as_ref();
        let path = path
            .to_str()
            .ok_or(LoadSessionError::PathToStrError(path.to_path_buf()))?;

        let cstr = CString::new(path)?;
        let mut tokens: Vec<LlamaToken> = Vec::with_capacity(max_tokens);
        let mut n_out = 0;

        // SAFETY: cast is valid as LlamaToken is repr(transparent)
        let tokens_out = tokens.as_mut_ptr().cast::<llama_cpp_sys::llama_token>();

        let load_session_success = unsafe {
            llama_cpp_sys::llama_load_session_file(
                self.context.as_ptr(),
                cstr.as_ptr(),
                tokens_out,
                max_tokens,
                &raw mut n_out,
            )
        };
        if load_session_success {
            if n_out > max_tokens {
                return Err(LoadSessionError::InsufficientMaxLength { n_out, max_tokens });
            }
            // SAFETY: we checked that n_out <= max_tokens and llama.cpp promises that n_out tokens will be written
            unsafe {
                tokens.set_len(n_out);
            }
            Ok(tokens)
        } else {
            Err(LoadSessionError::FailedToLoad)
        }
    }

    /// Returns the maximum size in bytes of the state (rng, logits, embedding
    /// and `kv_cache`) - will often be smaller after compacting tokens
    #[must_use]
    pub fn get_state_size(&self) -> usize {
        #[cfg(feature = "turboquant")]
        if self.uses_pq3_cache() {
            return 0;
        }
        // SAFETY: the context remains live throughout the synchronous size query.
        unsafe { llama_cpp_sys::llama_get_state_size(self.context.as_ptr()) }
    }

    /// Copies the state to the specified destination address.
    ///
    /// Returns the number of bytes copied
    ///
    /// # Safety
    ///
    /// Destination needs to have allocated enough memory.
    pub unsafe fn copy_state_data(&self, dest: *mut u8) -> usize {
        #[cfg(feature = "turboquant")]
        if self.uses_pq3_cache() {
            return 0;
        }
        // SAFETY: the caller guarantees destination capacity and exclusive access
        // for the complete current native state, as required by this unsafe API.
        unsafe { llama_cpp_sys::llama_copy_state_data(self.context.as_ptr(), dest) }
    }

    /// Set the state reading from the specified address
    /// Returns the number of bytes read
    ///
    /// # Safety
    ///
    /// The bytes must be a complete native state from a compatible context.
    /// Experimental PQ3 persistence returns zero without reading them.
    pub unsafe fn set_state_data(&mut self, src: &[u8]) -> usize {
        #[cfg(feature = "turboquant")]
        if self.uses_pq3_cache() {
            return 0;
        }
        // SAFETY: the input slice bounds every native read and the context is
        // exclusively borrowed. Keep the legacy unsafe signature for compatibility.
        unsafe {
            llama_cpp_sys::llama_state_set_data(self.context.as_ptr(), src.as_ptr(), src.len())
        }
    }
}
