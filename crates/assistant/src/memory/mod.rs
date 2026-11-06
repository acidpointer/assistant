//! Durable, owner-scoped experiences and a temporal knowledge graph.
//!
//! A dedicated `SQLite` writer owns mutations; bounded reader workers return owned
//! evidence. Success means a FULL/WAL transaction committed. Native inference is
//! independent of this store. Capabilities are issued by the local host, never
//! deserialized from model output. Closing the store joins its worker threads.

mod assistant;
pub use assistant::{
    AssistantNotice, AssistantProfile, AssistantSettings, MemoryLink, SourceGraph,
};
pub mod embedding;
mod error;
#[cfg(feature = "experimental-extraction")]
pub mod extraction;
#[cfg(feature = "experimental-extraction")]
pub mod formation;
mod operation_types;
pub mod service;
pub(crate) mod session;
mod store;
pub mod tools;
mod types;

pub use error::{MemoryError, MemoryResult};
pub use operation_types::{
    ClaimHistoryPage, ClientRequestKey, CompletionKind, CompletionLimit, ConsumerRequirement,
    EventPage, FormationState, HistoryPage, MemoryChange, OperationAdmission, OperationDetails,
    OperationExchange, OperationFinalization, OperationLimits, OperationRecord, OperationState,
    SourceAttribution, SourceRecord, SourceRef, SourceRole, StorageHealth, StorageStatus,
    TerminalOutcome, ToolCapture, ToolEffect, ToolIntent, ToolInvocation, ToolOutcome,
};
pub use store::{
    EmbeddingTask, IndexCoverage, MemoryHandle, MemoryStore, OwnerCapability, SemanticHit,
};
pub use types::{
    Claim, ClaimEvidence, ClaimInput, ClaimModality, ClaimObject, Entity, EntityKind,
    ExtractionAccounting, ExtractionBudget, ExtractionChanges, ExtractionLease, ExtractionOutcome,
    GraphBundle, GraphPath, GraphStep, MemoryConfig, MemoryId, MemoryJobUsage, MemoryReceipt,
    MemoryText, OperationKey, Predicate, Revision, SearchBundle, Source, SuppressionReceipt,
};

#[cfg(test)]
mod tests;
