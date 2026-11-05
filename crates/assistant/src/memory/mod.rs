//! Durable, owner-scoped experiences and a temporal knowledge graph.
//!
//! A dedicated `SQLite` writer owns mutations; bounded reader workers return owned
//! evidence. Success means a FULL/WAL transaction committed. Native inference is
//! independent of this store. Capabilities are issued by the local host, never
//! deserialized from model output. Closing the store joins its worker threads.

mod error;
mod operation_types;
mod store;
mod types;

pub use error::{MemoryError, MemoryResult};
pub use operation_types::{
    ClaimHistoryPage, ClientRequestKey, ConsumerRequirement, EventPage, FormationState,
    MemoryChange, OperationAdmission, OperationFinalization, OperationLimits, OperationRecord,
    OperationState, SourceRecord, SourceRef, SourceRole, StorageHealth, StorageStatus,
    TerminalOutcome, ToolCapture, ToolEffect, ToolIntent, ToolInvocation, ToolOutcome,
};
pub use store::{MemoryHandle, MemoryStore, OwnerCapability};
pub use types::{
    Claim, ClaimEvidence, ClaimInput, ClaimModality, ClaimObject, Entity, EntityKind,
    ExtractionChanges, ExtractionLease, ExtractionOutcome, GraphBundle, MemoryConfig, MemoryId,
    MemoryJobUsage, MemoryReceipt, MemoryText, OperationKey, Predicate, Revision, SearchBundle,
    Source, SuppressionReceipt,
};

#[cfg(test)]
mod tests;
