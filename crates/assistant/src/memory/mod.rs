//! Durable, owner-scoped experiences and a temporal knowledge graph.
//!
//! A dedicated `SQLite` writer owns mutations; bounded reader workers return owned
//! evidence. Success means a FULL/WAL transaction committed. Native inference is
//! independent of this store. Capabilities are issued by the local host, never
//! deserialized from model output. Closing the store joins its worker threads.

pub mod embedding;
mod error;
mod operation_types;
pub mod service;
pub(crate) mod session;
mod store;
mod types;

pub use error::{MemoryError, MemoryResult};
pub use operation_types::{
    ClaimHistoryPage, ClientRequestKey, CompletionKind, CompletionLimit, ConsumerRequirement,
    EventPage, FormationState, MemoryChange, OperationAdmission, OperationDetails,
    OperationFinalization, OperationLimits, OperationRecord, OperationState, SourceRecord,
    SourceRef, SourceRole, StorageHealth, StorageStatus, TerminalOutcome, ToolCapture, ToolEffect,
    ToolIntent, ToolInvocation, ToolOutcome,
};
pub use store::{EmbeddingTask, MemoryHandle, MemoryStore, OwnerCapability, SemanticHit};
pub use types::{
    Claim, ClaimEvidence, ClaimInput, ClaimModality, ClaimObject, Entity, EntityKind,
    ExtractionChanges, ExtractionLease, ExtractionOutcome, GraphBundle, MemoryConfig, MemoryId,
    MemoryJobUsage, MemoryReceipt, MemoryText, OperationKey, Predicate, Revision, SearchBundle,
    Source, SuppressionReceipt,
};

#[cfg(test)]
mod tests;
