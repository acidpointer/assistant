//! Transport-independent presentation contracts. IDs are instance-scoped strings;
//! retained messages are an ephemeral view, not the model's working history.
//! Only safe error codes cross this boundary. All payloads are owned and bounded
//! by the application coordinator before publication.

use serde::Serialize;

/// Stable public failure categories, independent of native diagnostics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ApplicationError {
    /// Input or identifiers failed validation.
    #[error("invalid input")]
    InvalidInput,
    /// Another interaction owns this conversation.
    #[error("chat is busy")]
    Busy,
    /// A request key was reused with different input.
    #[error("request key conflicts with an earlier submission")]
    Conflict,
    /// A bounded queue, subscription or retention allowance is exhausted.
    #[error("application capacity exceeded")]
    Capacity,
    /// The requested resource is unknown or no longer retained.
    #[error("resource is not retained")]
    NotFound,
    /// Admission has closed.
    #[error("assistant is shutting down")]
    Closed,
}

/// A new text interaction. The host supplies a transport scope separately.
#[derive(Debug, Clone)]
pub struct Submission {
    /// Client namespace, bounded and interpreted only as an opaque identifier.
    pub client_id: String,
    /// Stable identity reused only for the same submission within this instance.
    pub request_id: String,
    /// Literal user input, never trusted instructions.
    pub text: String,
}

/// Display and run outcomes; stopping is a request, not a confirmed outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Status {
    /// Admitted, before the core starts evaluation.
    Accepted,
    /// Evaluation or tool work is active.
    Running,
    /// Cooperative cancellation was requested.
    Stopping,
    /// Completed normally.
    Completed,
    /// Output allowance exhausted.
    Truncated,
    /// Cancellation confirmed.
    Cancelled,
    /// Deadline expired.
    Deadline,
    /// The interaction failed.
    Failed,
}
impl Status {
    /// Whether no further execution belongs to this run.
    #[must_use]
    pub fn is_terminal(self) -> bool {
        !matches!(self, Self::Accepted | Self::Running | Self::Stopping)
    }
}

/// One visible message. Tool arguments and hidden reasoning are excluded.
#[derive(Debug, Clone, Serialize)]
pub struct DisplayMessage {
    /// Instance-scoped message identity.
    pub id: String,
    /// Interaction owning this entry.
    pub run_id: String,
    /// Either `user` or `assistant`.
    pub role: &'static str,
    /// User-visible text.
    pub text: String,
    /// Whether the associated interaction is complete or interrupted.
    pub status: Status,
}

/// Retained interaction metadata; it never retains complete raw model responses.
#[derive(Debug, Clone, Serialize)]
pub struct RunView {
    /// Per-model-turn counters/timings and observed process resource windows.
    pub performance: Vec<PerformanceView>,
    /// Whole-interaction observed resources, including tool and scheduler time.
    pub resources: crate::telemetry::ResourceWindow,
    /// Explicit termination boundary: `output_tokens`, `context_capacity`, `run_tokens`.
    pub limit_reason: Option<&'static str>,
    /// Instance-scoped interaction identity.
    pub id: String,
    /// Conversation identity.
    pub chat_id: String,
    /// Correlates optimistic browser input with the accepted operation.
    pub client_id: String,
    /// Original caller submission key.
    pub client_request_id: String,
    /// Current or terminal outcome.
    pub status: Status,
    /// Stable, sanitized terminal error code.
    pub error: Option<&'static str>,
    /// Aggregate prompt token count.
    pub input_tokens: usize,
    /// Aggregate generated token count.
    pub generated_tokens: usize,
    /// Completion-observer warnings, without exposing private diagnostics.
    pub warning_count: usize,
    /// Interrupted tools may have effects that cannot be confirmed or rolled back.
    pub unknown_tool_outcome: bool,
}

/// Bounded visible state, captured atomically with subscription registration.
#[derive(Debug, Clone, Serialize)]
pub struct Snapshot {
    /// Latest process observation; absent before the first demand-driven sample.
    pub resources: Option<crate::telemetry::ResourceSample>,
    /// Weighted lifetime native throughput counters (never mean of rates).
    pub inference: InferenceTotals,
    /// Changes on every process runtime start.
    pub instance_id: String,
    /// Primary conversation identity, also changes across instances.
    pub chat_id: String,
    /// Current event cursor, represented as a string to avoid JSON precision loss.
    pub revision: String,
    /// Retained display entries, independent of native context trimming.
    pub messages: Vec<DisplayMessage>,
    /// The current interaction, if any.
    pub active_run: Option<RunView>,
    /// Retained outcomes allow recovery from a lost submission acknowledgment.
    pub recent_runs: Vec<RunView>,
    /// Older visible exchanges have been evicted.
    pub truncated: bool,
}

/// A typed state change. Consumers may always recover through a fresh snapshot.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[non_exhaustive]
pub enum Update {
    /// Latest process counters plus weighted lifetime inference totals.
    Telemetry {
        /// Current process observation.
        sample: crate::telemetry::ResourceSample,
        /// Sum of measured tokens and active decode times.
        inference: InferenceTotals,
    },
    /// Admission, execution, cancellation request or terminal metadata.
    Run {
        /// New run state.
        run: Box<RunView>,
    },
    /// Insert or replace one complete display entry.
    Message {
        /// Authoritative entry.
        message: DisplayMessage,
    },
    /// Append text to an already announced message.
    Delta {
        /// Target entry.
        message_id: String,
        /// New text only.
        text: String,
    },
    /// Host activity label, without arguments or results.
    Activity {
        /// Owning interaction.
        run_id: String,
        /// Safe display label.
        label: &'static str,
    },
    /// Retention removed complete old exchanges.
    Evicted {
        /// IDs no longer present in snapshots.
        message_ids: Vec<String>,
    },
}

/// Ordered update shared across subscribers without copying full transcripts.
#[derive(Debug, Clone, Serialize)]
pub struct ApplicationEvent {
    /// Instance-qualified event cursor.
    pub id: String,
    /// Typed payload.
    pub update: Update,
}

/// One visible model turn's measured progress. Resource figures describe the
/// daemon during its window, including resident weights and unrelated work.
#[derive(Debug, Clone, Serialize)]
pub struct PerformanceView {
    /// False when only the last delivered native progress report is available.
    pub measurement_complete: bool,
    /// One-based model turn; matches the assistant message ID suffix.
    pub turn: usize,
    /// Native sampled non-EOG tokens, including hidden reasoning and protocol.
    pub generated_tokens: usize,
    /// Owner-thread active timings. Synthetic backends may not provide these.
    pub timing: Option<crate::engine::GenerationMetrics>,
    /// Observed process resources during this turn.
    pub resources: crate::telemetry::ResourceWindow,
}
/// Exact sums for computing weighted native throughput across completed turns.
#[derive(Debug, Clone, Default, Serialize)]
pub struct InferenceTotals {
    /// Only tokens with measured timing participate in this sum.
    pub generated_tokens: u64,
    /// Sum of active generation times in nanoseconds.
    pub generation_ns: u64,
}
