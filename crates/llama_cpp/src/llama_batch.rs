//! Owned token batches for synchronous llama.cpp evaluation.
//!
//! Rust owns every array and sequence-ID row; their allocations remain stable
//! when the batch moves. Safe additions validate all capacities before changing
//! the batch. Native decode borrows the arrays only for the duration of a call.
//! Batches are thread-confined and report invalid sizes or allocation failures
//! through typed errors. The raw [`LlamaBatch::get_one`] helper owns no storage.

use crate::token::LlamaToken;
use llama_cpp_sys::{llama_batch, llama_pos};

/// An owned batch with stable storage for tokens and sequence memberships.
#[derive(Debug)]
#[allow(clippy::struct_field_names)]
pub struct LlamaBatch {
    /// Capacity of every token-indexed array.
    allocated: usize,
    /// Capacity of each sequence-ID row.
    n_seq_max: usize,
    /// Output positions initialized by the next successful decode.
    pub(crate) initialized_logits: Vec<i32>,
    /// Native view into `storage`; no native code owns these arrays.
    pub(crate) llama_batch: llama_batch,
    /// Owns every allocation referenced by `llama_batch`.
    storage: BatchStorage,
}

/// Stable backing arrays, allocated before the native view is constructed.
#[derive(Debug)]
struct BatchStorage {
    tokens: Vec<i32>,
    positions: Vec<i32>,
    sequence_counts: Vec<i32>,
    sequence_ids: Vec<i32>,
    sequence_rows: Vec<*mut i32>,
    logits: Vec<i8>,
}

/// Failure to validate or allocate batch storage.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum BatchInitError {
    /// A length does not fit the native signed token count or address space.
    #[error("batch dimensions exceed the native or address-space limit")]
    SizeOverflow,
    /// Every token needs capacity for at least one sequence membership.
    #[error("sequence capacity must be positive, got {0}")]
    InvalidSequenceCapacity(i32),
    /// Storage could not be reserved without exceeding available memory.
    #[error("failed to allocate batch storage: {0}")]
    Allocation(#[from] std::collections::TryReserveError),
}

/// Failure to add tokens; every error leaves the batch unchanged.
#[derive(thiserror::Error, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum BatchAddError {
    /// The token capacity would be exceeded.
    #[error("insufficient token capacity ({0})")]
    InsufficientSpace(usize),
    /// A token has more memberships than its allocated sequence-ID row.
    #[error("{requested} sequence IDs exceed the per-token capacity {capacity}")]
    TooManySequenceIds {
        /// Number of requested memberships.
        requested: usize,
        /// Allocated memberships per token.
        capacity: usize,
    },
    /// Sequence IDs must be nonnegative; their upper bound is context-specific.
    #[error("sequence IDs must be nonnegative, got {0}")]
    InvalidSequenceId(i32),
    /// The native token count or position cannot be represented.
    #[error("batch token count or position exceeds the native limit")]
    SizeOverflow,
}

/// Reserve before initializing so allocation failure is returned to the caller.
fn array<T: Clone>(length: usize, value: T) -> Result<Vec<T>, BatchInitError> {
    let mut values = Vec::new();
    values.try_reserve_exact(length)?;
    values.resize(length, value);
    Ok(values)
}

impl LlamaBatch {
    /// Create a token batch with `n_seq_max` memberships per token.
    ///
    /// # Errors
    ///
    /// Returns [`BatchInitError`] for nonpositive sequence capacity, dimensions
    /// outside native/address-space limits, or a failed allocation. A zero-token
    /// batch is valid and rejects additions.
    pub fn new(n_tokens: usize, n_seq_max: i32) -> Result<Self, BatchInitError> {
        i32::try_from(n_tokens).map_err(|_| BatchInitError::SizeOverflow)?;
        if n_seq_max <= 0 {
            return Err(BatchInitError::InvalidSequenceCapacity(n_seq_max));
        }
        let n_seq_max = usize::try_from(n_seq_max).map_err(|_| BatchInitError::SizeOverflow)?;
        let memberships = n_tokens
            .checked_mul(n_seq_max)
            .ok_or(BatchInitError::SizeOverflow)?;
        let rows = n_tokens
            .checked_add(1)
            .ok_or(BatchInitError::SizeOverflow)?;
        let mut storage = BatchStorage {
            tokens: array(n_tokens, 0)?,
            positions: array(n_tokens, 0)?,
            sequence_counts: array(n_tokens, 0)?,
            sequence_ids: array(memberships, 0)?,
            sequence_rows: array(rows, std::ptr::null_mut())?,
            logits: array(n_tokens, 0)?,
        };
        for (row, pointer) in storage.sequence_rows[..n_tokens].iter_mut().enumerate() {
            // SAFETY: checked multiplication and the allocation above provide
            // `memberships` elements. Each row begins strictly within it.
            *pointer = unsafe { storage.sequence_ids.as_mut_ptr().add(row * n_seq_max) };
        }
        let native = llama_batch {
            n_tokens: 0,
            token: storage.tokens.as_mut_ptr(),
            embd: std::ptr::null_mut(),
            pos: storage.positions.as_mut_ptr(),
            n_seq_id: storage.sequence_counts.as_mut_ptr(),
            seq_id: storage.sequence_rows.as_mut_ptr(),
            logits: storage.logits.as_mut_ptr(),
        };
        // Reserve logits bookkeeping too, so add() cannot allocate mid-update.
        let mut initialized_logits = Vec::new();
        initialized_logits.try_reserve_exact(n_tokens)?;
        Ok(Self {
            allocated: n_tokens,
            n_seq_max,
            initialized_logits,
            llama_batch: native,
            storage,
        })
    }

    /// Reset token and output counts, retaining all allocated storage.
    pub fn clear(&mut self) {
        self.llama_batch.n_tokens = 0;
        self.initialized_logits.clear();
    }

    /// Add a token at `pos` with the supplied sequence memberships.
    ///
    /// Empty memberships follow llama.cpp's native behavior. Sequence IDs need
    /// not be smaller than the row capacity: the context checks their range.
    ///
    /// # Errors
    ///
    /// Returns [`BatchAddError`] before writing if token or membership capacity
    /// is exceeded, a sequence ID is negative, or a count cannot be represented.
    pub fn add(
        &mut self,
        LlamaToken(id): LlamaToken,
        pos: llama_pos,
        seq_ids: &[i32],
        logits: bool,
    ) -> Result<(), BatchAddError> {
        let offset =
            usize::try_from(self.llama_batch.n_tokens).map_err(|_| BatchAddError::SizeOverflow)?;
        if offset >= self.allocated {
            return Err(BatchAddError::InsufficientSpace(self.allocated));
        }
        self.validate_sequence_ids(seq_ids)?;
        let count = i32::try_from(seq_ids.len()).map_err(|_| BatchAddError::SizeOverflow)?;
        self.storage.tokens[offset] = id;
        self.storage.positions[offset] = pos;
        self.storage.sequence_counts[offset] = count;
        let start = offset * self.n_seq_max;
        self.storage.sequence_ids[start..start + seq_ids.len()].copy_from_slice(seq_ids);
        self.storage.logits[offset] = i8::from(logits);
        if logits {
            self.initialized_logits.push(self.llama_batch.n_tokens);
        }
        self.llama_batch.n_tokens += 1;
        Ok(())
    }

    /// Validate memberships without mutating any batch state.
    fn validate_sequence_ids(&self, seq_ids: &[i32]) -> Result<(), BatchAddError> {
        if seq_ids.len() > self.n_seq_max {
            return Err(BatchAddError::TooManySequenceIds {
                requested: seq_ids.len(),
                capacity: self.n_seq_max,
            });
        }
        if let Some(&id) = seq_ids.iter().find(|&&id| id < 0) {
            return Err(BatchAddError::InvalidSequenceId(id));
        }
        Ok(())
    }

    /// Add a sequence at positions starting at zero, enabling the last logits.
    ///
    /// # Errors
    ///
    /// Returns [`BatchAddError`] without changing the batch if the complete
    /// sequence will not fit or the sequence ID is invalid.
    pub fn add_sequence(
        &mut self,
        tokens: &[LlamaToken],
        seq_id: i32,
        logits_all: bool,
    ) -> Result<(), BatchAddError> {
        let used =
            usize::try_from(self.llama_batch.n_tokens).map_err(|_| BatchAddError::SizeOverflow)?;
        if tokens.len() > self.allocated.saturating_sub(used) {
            return Err(BatchAddError::InsufficientSpace(self.allocated));
        }
        self.validate_sequence_ids(&[seq_id])?;
        for (index, &token) in tokens.iter().enumerate() {
            let position = i32::try_from(index).map_err(|_| BatchAddError::SizeOverflow)?;
            self.add(
                token,
                position,
                &[seq_id],
                logits_all || index + 1 == tokens.len(),
            )?;
        }
        Ok(())
    }

    /// Number of initialized tokens currently in the batch.
    #[must_use]
    pub fn n_tokens(&self) -> i32 {
        self.llama_batch.n_tokens
    }

    /// Make a raw, non-owning batch over a token slice.
    ///
    /// All tokens belong to sequence zero; only the last produces logits. The
    /// raw pointers must not be used after `tokens` is dropped or reallocated.
    /// Never pass this view to `llama_batch_free`.
    ///
    /// # Errors
    ///
    /// Returns [`BatchInitError::SizeOverflow`] if the slice exceeds `i32::MAX`.
    pub fn get_one(tokens: &mut [LlamaToken]) -> Result<llama_batch, BatchInitError> {
        let length = i32::try_from(tokens.len()).map_err(|_| BatchInitError::SizeOverflow)?;
        // SAFETY: LlamaToken is repr(transparent) over the native token. The
        // returned raw view neither dereferences nor frees its borrowed pointer.
        Ok(unsafe { llama_cpp_sys::llama_batch_get_one(tokens.as_mut_ptr().cast(), length) })
    }
}

#[cfg(test)]
mod tests {
    use super::{BatchAddError, BatchInitError, LlamaBatch};
    use crate::token::LlamaToken;

    #[test]
    fn oversized_memberships_leave_batch_unchanged() {
        let mut batch = LlamaBatch::new(2, 1).expect("valid batch");
        batch
            .add(LlamaToken(7), 0, &[5], true)
            .expect("one membership");
        assert_eq!(
            batch.add(LlamaToken(8), 1, &[0, 1], true),
            Err(BatchAddError::TooManySequenceIds {
                requested: 2,
                capacity: 1
            })
        );
        assert_eq!(batch.n_tokens(), 1);
        assert_eq!(batch.storage.tokens, [7, 0]);
        assert_eq!(batch.storage.sequence_ids, [5, 0]);
        assert_eq!(batch.initialized_logits, [0]);
        batch
            .add(LlamaToken(8), 1, &[0], false)
            .expect("rejected add consumed no capacity");
    }

    #[test]
    fn dimensions_and_sequence_ids_are_checked() {
        assert!(matches!(
            LlamaBatch::new(1, 0),
            Err(BatchInitError::InvalidSequenceCapacity(0))
        ));
        assert!(matches!(
            LlamaBatch::new(1, -1),
            Err(BatchInitError::InvalidSequenceCapacity(-1))
        ));
        assert!(matches!(
            LlamaBatch::new(usize::MAX, 1),
            Err(BatchInitError::SizeOverflow)
        ));
        let mut empty = LlamaBatch::new(0, 1).expect("zero token capacity");
        assert_eq!(
            empty.add(LlamaToken(0), 0, &[0], true),
            Err(BatchAddError::InsufficientSpace(0))
        );
        let mut batch = LlamaBatch::new(2, 2).expect("valid batch");
        assert_eq!(
            batch.add_sequence(&[LlamaToken(1)], -1, true),
            Err(BatchAddError::InvalidSequenceId(-1))
        );
        assert_eq!(batch.n_tokens(), 0);
        batch
            .add(LlamaToken(1), 0, &[0, 1], true)
            .expect("exact sequence capacity");
        batch.clear();
        batch
            .add_sequence(&[LlamaToken(2), LlamaToken(3)], 0, false)
            .expect("reuse all capacity");
        assert_eq!(batch.initialized_logits, [1]);
    }
}
