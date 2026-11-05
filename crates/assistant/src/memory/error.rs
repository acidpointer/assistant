//! Sanitized memory failures. Internal causes are retained without exposing
//! source text, database paths, SQL or credentials through Display or Debug.
//! Errors cross the worker boundary as owned values; no retry is implicit.

use std::fmt;

/// Result of a durable memory operation.
pub type MemoryResult<T> = Result<T, MemoryError>;

/// A rejected or failed memory operation. A failed reply never implies rollback.
#[derive(thiserror::Error)]
#[non_exhaustive]
pub enum MemoryError {
    /// Invalid typed input or an exceeded payload bound.
    #[error("invalid memory input: {0}")]
    InvalidInput(&'static str),
    /// The capability belongs to another store or owner.
    #[error("memory access forbidden")]
    Forbidden,
    /// A referenced record is absent.
    #[error("memory record not found")]
    NotFound,
    /// A forgotten request key must not execute again.
    #[error("memory record was forgotten")]
    Gone,
    /// A key was reused or a required revision changed.
    #[error("memory revision or operation key conflict")]
    Conflict,
    /// Another daemon owns this data directory.
    #[error("memory store is already open")]
    Busy,
    /// A bounded command or storage allowance is exhausted.
    #[error("memory capacity exceeded")]
    Capacity,
    /// SQLite/filesystem could not allocate storage, unlike a soft admission quota.
    #[error("memory storage is full")]
    StorageFull,
    /// The store is closed or its worker is unavailable.
    #[error("memory store unavailable")]
    Unavailable,
    /// A command may have committed; reconcile using its original key.
    #[error("memory outcome indeterminate; reconcile the original operation key")]
    Indeterminate,
    /// An interruptible query exhausted its work-time allowance.
    #[error("memory query deadline exceeded")]
    Deadline,
    /// Unknown or modified migration history.
    #[error("unsupported memory schema or migration")]
    UnsupportedVersion,
    /// Persisted invariants failed validation.
    #[error("memory store integrity failure")]
    CorruptStore,
    /// Private filesystem or database details are available through the cause.
    #[error("memory storage operation failed")]
    Storage(#[source] Box<dyn std::error::Error + Send + Sync>),
}

impl fmt::Debug for MemoryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

impl From<rusqlite::Error> for MemoryError {
    fn from(error: rusqlite::Error) -> Self {
        match &error {
            rusqlite::Error::QueryReturnedNoRows => Self::NotFound,
            rusqlite::Error::SqliteFailure(e, _) => match e.code {
                rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked => {
                    Self::Busy
                }
                rusqlite::ErrorCode::DiskFull => Self::StorageFull,
                // Constraint rejection is a failed proposal, not an I/O failure
                // that should latch otherwise healthy writer admission closed.
                rusqlite::ErrorCode::ConstraintViolation => Self::Conflict,
                rusqlite::ErrorCode::OperationInterrupted => Self::Deadline,
                rusqlite::ErrorCode::DatabaseCorrupt | rusqlite::ErrorCode::NotADatabase => {
                    Self::CorruptStore
                }
                _ => Self::Storage(Box::new(error)),
            },
            _ => Self::Storage(Box::new(error)),
        }
    }
}

impl From<std::io::Error> for MemoryError {
    fn from(error: std::io::Error) -> Self {
        if matches!(error.raw_os_error(), Some(libc::ENOSPC | libc::EDQUOT)) {
            Self::StorageFull
        } else {
            Self::Storage(Box::new(error))
        }
    }
}

impl From<serde_json::Error> for MemoryError {
    fn from(error: serde_json::Error) -> Self {
        Self::Storage(Box::new(error))
    }
}
