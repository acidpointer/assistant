//! Universal source navigation and assistant settings. Topics and personalities
//! are freeform text. The host supplies authority, attribution and scheduling;
//! model-authored links are navigation aids, never factual entailment.

use super::{MemoryId, MemoryText, Revision, SourceRecord, SourceRef};
use serde::{Deserialize, Serialize};

/// One directed, labeled connection between saved experiences.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryLink {
    /// Original source of the connection.
    pub from: SourceRef,
    /// Connected source; traversal may follow either direction.
    pub to: SourceRef,
    /// Freeform relationship, without a fixed ontology.
    pub relation: MemoryText,
}
/// Bounded neighborhood with host-attributed source records.
#[derive(Debug, Clone, Serialize)]
pub struct SourceGraph {
    /// Saved experiences and notes reached from the supplied entry point.
    pub sources: Vec<SourceRecord>,
    /// Original link direction is retained.
    pub links: Vec<MemoryLink>,
    /// Some neighbors exceeded the node/edge/byte allowance.
    pub truncated: bool,
    /// Continue the same entry point's adjacency page, when available.
    pub next_cursor: Option<String>,
}
/// Editable instructions and optional minimum interval between initiative checks.
/// Validation at the storage boundary also protects non-HTTP clients.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AssistantSettings {
    /// Freeform personality and behavior; Debug redacts private instructions.
    pub instructions: MemoryText,
    /// `None` disables initiative; accepted intervals are 60..=86400 seconds.
    pub initiative_interval_seconds: Option<u32>,
}
/// Versioned, durable assistant settings for optimistic editing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssistantProfile {
    /// Compare-and-swap version; stale editors cannot overwrite new instructions.
    pub revision: Revision,
    /// Instructions shared by foreground dialogue and initiative.
    pub settings: AssistantSettings,
    /// Next eligible decision time in UTC milliseconds.
    pub next_check_at: Option<i64>,
    /// Sanitized latest loop outcome, with no personal text.
    pub last_check: Option<String>,
}
/// A durable message initiated by the assistant, independently of any client.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssistantNotice {
    /// Source identity; doubles as a stable message identity across restart.
    pub id: MemoryId,
    /// Literal assistant-authored message.
    pub text: MemoryText,
    /// Host delivery time, UTC milliseconds.
    pub created_at: i64,
    /// Owner has dismissed this item; dismissal does not erase memory.
    pub dismissed: bool,
}
