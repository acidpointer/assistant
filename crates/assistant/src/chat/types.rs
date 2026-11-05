//! Owned, model-independent chat data. System instructions and supporting data
//! are distinct from messages. Tool calls are normalized only after full parsing;
//! partial output is never executable. These types cross worker boundaries.
use crate::engine::{Cancellation, FinishReason, GenerationFailure, Thinking, Usage};
use serde_json::{Value, json};
use std::time::{Duration, Instant};

/// Result used by configuration, middleware, and orchestration.
pub type ChatResult<T> = Result<T, ChatError>;
/// A failure outside, or reported by, native model evaluation.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ChatError {
    /// Required durable memory could not complete its contract.
    #[cfg(target_os = "linux")]
    #[error(transparent)]
    Memory(#[from] crate::memory::MemoryError),
    /// Invalid registration or configuration.
    #[error("configuration: {0}")]
    Configuration(String),
    /// Malformed assistant/tool protocol.
    #[error("protocol: {0}")]
    Protocol(String),
    /// A configured bound was reached.
    #[error("limit exceeded: {0}")]
    Limit(&'static str),
    /// The caller interrupted the run.
    #[error("run cancelled")]
    Cancelled,
    /// The run deadline expired.
    #[error("run deadline exceeded")]
    Deadline,
    /// Runtime stopped or a worker disconnected.
    #[error("runtime is closed")]
    Closed,
    /// A named extension failed.
    #[error("extension {name}: {message}")]
    Extension {
        /// Registered extension identity.
        name: String,
        /// Failure diagnostic from the extension.
        message: String,
    },
    /// Startup failed; every attempted rollback failure is retained.
    #[error("startup failed: {source}; rollback failures: {cleanup:?}")]
    Startup {
        /// Original startup failure.
        #[source]
        source: Box<ChatError>,
        /// Every failure encountered while rolling back started resources.
        cleanup: Vec<String>,
    },
    /// All cleanup steps were attempted; these failures need host attention.
    #[error("shutdown failures: {0:?}")]
    Shutdown(Vec<String>),
    /// Native configuration or preparation failed.
    #[error(transparent)]
    Engine(#[from] crate::engine::EngineError),
    /// Native execution failed with partial raw progress.
    #[error(transparent)]
    Generation(Box<GenerationFailure>),
    /// Structured data could not be encoded or decoded.
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

impl From<GenerationFailure> for ChatError {
    fn from(error: GenerationFailure) -> Self {
        Self::Generation(Box::new(error))
    }
}

/// A model-requested invocation. The host assigns missing IDs and validates all
/// arguments against a registered handler before execution.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolCall {
    /// Correlation ID shared with the result message.
    pub id: String,
    /// Registered, namespaced tool name.
    pub name: String,
    /// Parsed JSON object, never shell text or executable code.
    pub arguments: Value,
}
/// Normalized assistant output from one model turn.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AssistantMessage {
    /// User-facing text.
    pub content: String,
    /// Separately parsed reasoning, when the template provides it.
    pub reasoning: Option<String>,
    /// Complete tool requests from this turn.
    pub tool_calls: Vec<ToolCall>,
}
impl AssistantMessage {
    /// A text-only assistant message, useful for scripted backends and tests.
    #[must_use]
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            content: text.into(),
            ..Self::default()
        }
    }
    pub(crate) fn parse(value: &Value, prefix: &str) -> ChatResult<Self> {
        fn optional_text(value: Option<&Value>) -> ChatResult<Option<String>> {
            match value {
                None | Some(Value::Null) => Ok(None),
                Some(Value::String(text)) => Ok(Some(text.clone())),
                _ => Err(ChatError::Protocol("message text must be a string".into())),
            }
        }
        if value.get("role").and_then(Value::as_str) != Some("assistant") {
            return Err(ChatError::Protocol("expected an assistant message".into()));
        }
        let mut message = Self {
            content: optional_text(value.get("content"))?.unwrap_or_default(),
            reasoning: optional_text(value.get("reasoning_content"))?,
            tool_calls: Vec::new(),
        };
        if let Some(calls) = value.get("tool_calls").filter(|v| !v.is_null()) {
            let calls = calls
                .as_array()
                .ok_or_else(|| ChatError::Protocol("tool_calls must be an array".into()))?;
            if calls.len() > 16 {
                return Err(ChatError::Limit("tool calls per model turn"));
            }
            for (index, call) in calls.iter().enumerate() {
                if call
                    .get("type")
                    .and_then(Value::as_str)
                    .is_some_and(|kind| kind != "function")
                {
                    return Err(ChatError::Protocol("unsupported tool call type".into()));
                }
                let function = call
                    .get("function")
                    .ok_or_else(|| ChatError::Protocol("missing tool function".into()))?;
                let name = function
                    .get("name")
                    .and_then(Value::as_str)
                    .ok_or_else(|| ChatError::Protocol("missing tool name".into()))?
                    .to_owned();
                let arguments = match function.get("arguments") {
                    Some(Value::String(text)) => serde_json::from_str(text)?,
                    Some(value @ Value::Object(_)) => value.clone(),
                    _ => {
                        return Err(ChatError::Protocol(
                            "tool arguments must be an object".into(),
                        ));
                    }
                };
                if !arguments.is_object() {
                    return Err(ChatError::Protocol(
                        "tool arguments must decode to an object".into(),
                    ));
                }
                let id = match call.get("id") {
                    None | Some(Value::Null) => format!("{prefix}_{index}"),
                    Some(Value::String(id)) if id.is_empty() => format!("{prefix}_{index}"),
                    Some(Value::String(id)) => id.clone(),
                    _ => return Err(ChatError::Protocol("tool ID must be a string".into())),
                };
                if id.len() > 256 || name.len() > 64 {
                    return Err(ChatError::Limit("tool identifier bytes"));
                }
                message.tool_calls.push(ToolCall {
                    id,
                    name,
                    arguments,
                });
            }
        }
        Ok(message)
    }
}
/// A working conversation entry. Permanent memory is owned by a separate service.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum Message {
    /// Literal user input.
    User(String),
    /// A complete, normalized assistant turn.
    Assistant(AssistantMessage),
    /// Host-produced result associated with a tool call.
    Tool {
        /// ID of the assistant invocation being answered.
        call_id: String,
        /// Host-produced structured outcome.
        result: Value,
    },
}
impl Message {
    /// Construct literal user input.
    #[must_use]
    pub fn user(text: impl Into<String>) -> Self {
        Self::User(text.into())
    }
    pub(crate) fn value(&self) -> Value {
        match self {
            Self::User(text) => json!({"role":"user","content":text}),
            Self::Tool { call_id, result } => {
                json!({"role":"tool","tool_call_id":call_id,"content":result.to_string()})
            }
            Self::Assistant(message) => {
                let mut value = json!({"role":"assistant","content":message.content});
                if let Some(reasoning) = &message.reasoning {
                    value["reasoning_content"] = json!(reasoning);
                }
                if !message.tool_calls.is_empty() {
                    value["tool_calls"] = json!(message.tool_calls.iter().map(|call|
                        json!({"id":call.id,"type":"function","function":{"name":call.name,"arguments":call.arguments.to_string()}}))
                        .collect::<Vec<_>>());
                }
                value
            }
        }
    }
}
/// Per-request overrides. Context is supporting data, never system policy.
#[derive(Debug, Clone, Default)]
pub struct RequestOptions {
    /// Optional host-selected JSON Schema, enforced by native grammar.
    pub json_schema: Option<String>,
    /// Text chat by default; raw completion is an explicit diagnostic mode.
    pub mode: InputMode,
    /// Automatic calls, no calls, or a required call on the first turn only.
    pub tool_use: ToolUse,
    /// Overrides the model's output allowance for each model turn.
    pub max_new_tokens: Option<usize>,
    /// Overrides supported template thinking behavior.
    pub thinking: Option<Thinking>,
    /// Supporting data kept only for this run and its tool rounds.
    pub context: Vec<String>,
    /// Optional subset of installed tools; `None` uses the chat selection.
    pub tools: Option<Vec<String>>,
    /// Absolute deadline, further constrained by the chat's maximum run time.
    pub deadline: Option<Instant>,
    /// Interrupt this request, including pending tools and native work.
    pub cancellation: Cancellation,
}
/// Input interpretation for the native worker.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum InputMode {
    /// Render structured chat messages and tools.
    #[default]
    Chat,
    /// Evaluate literal completion text without chat formatting.
    Raw,
}
/// Tool selection mode. Requiring a call on every turn would prevent a final answer.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum ToolUse {
    /// Let the model choose selected tools.
    #[default]
    Auto,
    /// Do not advertise or execute tools.
    Disabled,
    /// Require a tool only on the first model evaluation.
    RequireFirst,
}
/// Hard limits enforced inside the runner after extension modifications.
#[derive(Debug, Clone)]
pub struct RunLimits {
    /// Maximum evaluations in one interaction, including tool continuations.
    pub model_turns: usize,
    /// Maximum tool requests, including rejected calls.
    pub tool_calls: usize,
    /// Total newly sampled tokens across the run.
    pub generated_tokens: usize,
    /// Maximum serialized request bytes before native rendering.
    pub input_bytes: usize,
    /// Maximum raw output bytes per model turn.
    pub output_bytes: usize,
    /// Maximum bytes returned from one tool.
    pub tool_result_bytes: usize,
    /// Maximum retained working messages; overflow is reported, not truncated.
    pub working_messages: usize,
    /// Wall-clock bound for the whole interaction.
    pub duration: Duration,
}
impl Default for RunLimits {
    fn default() -> Self {
        Self {
            model_turns: 8,
            tool_calls: 8,
            generated_tokens: 8192,
            input_bytes: 262_144,
            output_bytes: 131_072,
            tool_result_bytes: 16_384,
            working_messages: 128,
            duration: Duration::from_secs(600),
        }
    }
}
impl RunLimits {
    pub(crate) fn validate(&self) -> ChatResult<()> {
        if [
            self.model_turns,
            self.tool_calls,
            self.generated_tokens,
            self.input_bytes,
            self.output_bytes,
            self.tool_result_bytes,
            self.working_messages,
        ]
        .contains(&0)
            || self.duration.is_zero()
        {
            return Err(ChatError::Configuration(
                "run limits must be positive".into(),
            ));
        }
        Ok(())
    }
}
/// Outcome of a tool request, including failures returned to the model.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolReceipt {
    /// Validated or rejected request.
    pub call: ToolCall,
    /// Structured result or error; a cancelled invocation may have unknown effects.
    pub result: Value,
}
/// Terminal state shared by responses and lifecycle events.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum RunStatus {
    /// Final response obtained.
    Completed,
    /// Generation was deliberately truncated.
    Truncated,
    /// Caller cancelled work.
    Cancelled,
    /// Deadline elapsed.
    Deadline,
    /// An operation failed.
    Failed,
}
/// Final or partial result from a bounded interaction.
#[derive(Debug, Clone)]
pub struct ChatResponse {
    /// Why a token boundary stopped output, if known.
    pub limit_reason: Option<&'static str>,
    /// Bounded per-model-turn native performance records.
    pub performance: Vec<ModelPerformance>,
    /// Runtime-local interaction ID.
    pub run_id: u64,
    /// Last interpreted assistant response.
    pub message: AssistantMessage,
    /// Completed/rejected tool requests. Side effects must not be replayed implicitly.
    pub tools: Vec<ToolReceipt>,
    /// Totals over all model turns.
    pub usage: Usage,
    /// Last native termination condition, when there was an evaluation.
    pub finish_reason: Option<FinishReason>,
    /// Terminal interaction status.
    pub status: RunStatus,
    /// End-to-end wall-clock duration.
    pub elapsed: Duration,
    /// Noncritical terminal-observer failures, never silently swallowed.
    pub warnings: Vec<String>,
    /// Messages produced by this interaction, including source user input.
    pub messages: Vec<Message>,
}
impl ChatResponse {
    /// Parsed user-visible content, excluding reasoning and protocol markers.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.message.content
    }
}
/// Failed interaction with its partial response and actual tool receipts.
#[derive(Debug, thiserror::Error)]
#[error("{source}")]
pub struct RunFailure {
    /// Typed cause.
    #[source]
    pub source: ChatError,
    /// Progress safe to inspect without re-executing operations.
    pub partial: Box<ChatResponse>,
}
/// Semantic events. Tool arguments are emitted only after complete parsing.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum RunEvent {
    /// Throttled native accounting, including hidden reasoning/protocol tokens.
    ModelProgress {
        /// Interaction identity.
        run_id: u64,
        /// One-based model turn.
        turn: usize,
        /// Native counters.
        usage: Usage,
        /// Owner-thread measurements.
        metrics: crate::engine::GenerationMetrics,
    },
    /// A model backend has returned; transports may close its measurement window.
    ModelTurnFinished {
        /// Interaction identity.
        run_id: u64,
        /// One-based model turn.
        turn: usize,
    },
    /// A new model turn begins; transports must not merge tool continuations.
    ModelTurnStarted {
        /// Interaction identity.
        run_id: u64,
        /// One-based turn index within the interaction.
        turn: usize,
    },
    /// A request was admitted to the runner.
    Started {
        /// Interaction identity.
        run_id: u64,
    },
    /// New stable, user-visible text.
    TextDelta {
        /// Interaction identity.
        run_id: u64,
        /// Newly delivered text.
        text: String,
    },
    /// New separately parsed reasoning.
    ReasoningDelta {
        /// Interaction identity.
        run_id: u64,
        /// Newly delivered reasoning text.
        text: String,
    },
    /// A complete tool request is about to be dispatched.
    ToolStarted {
        /// Interaction identity.
        run_id: u64,
        /// Complete request before middleware dispatch.
        call: ToolCall,
    },
    /// A tool returned a result or a structured failure.
    ToolFinished {
        /// Interaction identity.
        run_id: u64,
        /// Actual recorded outcome.
        receipt: ToolReceipt,
    },
    /// Exactly one terminal event is attempted; disconnected consumers cannot receive it.
    Finished {
        /// Interaction identity.
        run_id: u64,
        /// Terminal outcome.
        status: RunStatus,
    },
}

/// Mutable model-turn draft. Core checks run after middleware modifications.
#[derive(Debug, Clone)]
pub struct ModelRequest {
    /// Trusted host instructions.
    pub system: String,
    /// Complete desired working messages.
    pub messages: Vec<Message>,
    /// Supporting data, encoded as literal content rather than host instructions.
    pub context: Vec<String>,
    /// Selected definitions; names are checked against registered capabilities.
    pub tools: Vec<crate::tools::ToolSpec>,
    /// Resolved model-turn settings and cancellation.
    pub options: RequestOptions,
}

/// Parsed output and accounting from one model evaluation.
#[derive(Debug, Clone)]
pub struct ModelTurn {
    /// Context reduction can lower the requested response allowance.
    pub limit_reason: Option<&'static str>,
    /// Native owner-thread timings, absent for synthetic middleware results.
    pub metrics: Option<crate::engine::GenerationMetrics>,
    /// Interpreted assistant output.
    pub message: AssistantMessage,
    /// Native accounting.
    pub usage: Usage,
    /// Why the native evaluation stopped.
    pub finish_reason: FinishReason,
}

/// One model turn's counters and timings, retained even after cancellation.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ModelPerformance {
    /// True only when the native owner returned terminal accounting; interruption
    /// can otherwise preserve only a measured prefix of the operation.
    pub measurement_complete: bool,
    /// One-based model turn.
    pub turn: usize,
    /// Sampled non-EOG tokens, including reasoning and protocol.
    pub generated_tokens: usize,
    /// Timings are unavailable for synthetic model middleware responses.
    pub timing: Option<crate::engine::GenerationMetrics>,
}
