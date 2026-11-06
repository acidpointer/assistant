//! Restricted structured inference over resident weights. Internal jobs bypass
//! product submission, working history, tools and optional application hooks.
//! One bounded job owns a separate context lease; the model owner gives it lower
//! priority than foreground work. Cancellation returns measured partial usage.

use super::RuntimeControl;
use crate::{
    chat::{
        EventSink, ExecutionClass, Message, ModelBackend, ModelFailure, ModelRequest,
        RequestOptions, RunLimits, ToolUse, TurnContext,
    },
    engine::{Cancellation, FinishReason, GenerationMetrics, Thinking, Usage},
};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};

/// A bounded host-authored structured task, independent of memory's vocabulary.
#[derive(Debug, Clone)]
pub struct InternalTask {
    /// Host policy or owner personality, never an untrusted quotation.
    pub instructions: crate::memory::MemoryText,
    /// Untrusted observations, encoded by the caller with attribution.
    pub data: crate::memory::MemoryText,
    /// Native JSON grammar; callers still validate the completed proposal.
    pub schema: crate::memory::MemoryText,
}
/// Restricted host capability for bounded structured jobs over resident weights.
#[derive(Debug, Clone)]
pub struct InternalInference {
    backend: Arc<dyn ModelBackend>,
    control: Arc<RuntimeControl>,
    slot: Arc<tokio::sync::Semaphore>,
}
impl InternalInference {
    pub(crate) fn new(backend: Arc<dyn ModelBackend>, control: Arc<RuntimeControl>) -> Self {
        Self {
            backend,
            control,
            slot: Arc::new(tokio::sync::Semaphore::new(1)),
        }
    }
    /// Evaluate one source task without tools, product submission or chat history.
    /// The caller must own durable job fencing/accounting before dispatch.
    /// # Errors
    /// Reports capacity, cancellation, native failure or the full rendered input
    /// exceeding 1,024 tokens. No failed/truncated output is a validated proposal.
    #[cfg(feature = "experimental-extraction")]
    pub async fn extract(
        &self,
        task: crate::memory::extraction::ExtractionTask,
        cancellation: Cancellation,
    ) -> Result<InternalOutput, InternalFailure> {
        self.extract_bounded(task, cancellation, InternalLimits::default())
            .await
    }
    /// Evaluate under a caller's remaining root allowance and absolute deadline.
    /// # Errors
    /// Rejects limits above 1,024 input/512 output tokens, expired deadlines,
    /// unavailable capacity and native failures with measured partial usage.
    #[cfg(feature = "experimental-extraction")]
    pub async fn extract_bounded(
        &self,
        task: crate::memory::extraction::ExtractionTask,
        cancellation: Cancellation,
        limits: InternalLimits,
    ) -> Result<InternalOutput, InternalFailure> {
        if limits.input_tokens > 1024 {
            return Err(InternalFailure::from(ModelFailure {
                source: crate::chat::ChatError::Limit("extraction input tokens"),
                partial: None,
                metrics: None,
            }));
        }
        let instructions = crate::memory::MemoryText::new(
            "Extract explicit propositions from the offered data. Follow the requested JSON schema.",
        );
        let task = instructions.and_then(|instructions| {
            Ok(InternalTask {
                instructions,
                data: crate::memory::MemoryText::new(task.prompt)?,
                schema: crate::memory::MemoryText::new(task.schema)?,
            })
        });
        let task = task.map_err(|error| {
            InternalFailure::from(ModelFailure {
                source: crate::chat::ChatError::Memory(error),
                partial: None,
                metrics: None,
            })
        })?;
        self.run(task, cancellation, limits).await
    }
    /// Run a host-authored JSON task using the existing resident model.
    /// # Errors
    /// Reports unavailable capacity, bounded-input/deadline failure or native errors.
    pub async fn run(
        &self,
        task: InternalTask,
        cancellation: Cancellation,
        limits: InternalLimits,
    ) -> Result<InternalOutput, InternalFailure> {
        self.run_inner(task, cancellation, limits)
            .await
            .map_err(InternalFailure::from)
    }
    async fn run_inner(
        &self,
        task: InternalTask,
        cancellation: Cancellation,
        limits: InternalLimits,
    ) -> Result<InternalOutput, ModelFailure> {
        if task
            .data
            .as_str()
            .len()
            .saturating_add(task.instructions.as_str().len())
            > 16_384
            || task.schema.as_str().len() > 16_384
            || self.control.is_closed()
            || !(1..=4096).contains(&limits.input_tokens)
            || !(1..=512).contains(&limits.output_tokens)
            || limits.deadline <= Instant::now()
        {
            return Err(ModelFailure {
                metrics: None,
                source: crate::chat::ChatError::Limit("internal task bounds or closed runtime"),
                partial: None,
            });
        }
        let _permit = self
            .slot
            .clone()
            .try_acquire_owned()
            .map_err(|_| ModelFailure {
                metrics: None,
                source: crate::chat::ChatError::Limit("internal inference capacity"),
                partial: None,
            })?;
        let id = self.control.next_id();
        let turn = TurnContext {
            recalled: None,
            execution: ExecutionClass::Background,
            input_token_limit: Some(limits.input_tokens),
            chat_id: id,
            run_id: id,
            turn: 1,
            lease: Arc::new(()),
            limits: RunLimits {
                model_turns: 1,
                tool_calls: 0,
                generated_tokens: limits.output_tokens,
                input_bytes: 16384,
                output_bytes: 16384,
                duration: Duration::from_secs(120),
                ..RunLimits::default()
            },
            cancellation,
            deadline: limits
                .deadline
                .min(Instant::now() + Duration::from_secs(120)),
        };
        let request = ModelRequest {
            system: task.instructions.as_str().to_owned(),
            messages: vec![Message::User(task.data.as_str().to_owned())],
            context: Vec::new(),
            tools: Vec::new(),
            options: RequestOptions {
                json_schema: Some(task.schema.as_str().to_owned()),
                max_new_tokens: Some(limits.output_tokens),
                thinking: Some(Thinking::Disabled),
                tool_use: ToolUse::Disabled,
                ..RequestOptions::default()
            },
        };
        let result = self
            .backend
            .evaluate(request, turn, EventSink::default())
            .await?;
        let json = crate::memory::MemoryText::new(result.message.content).map_err(|error| {
            ModelFailure {
                metrics: result.metrics.clone().map(Box::new),
                source: crate::chat::ChatError::Memory(error),
                partial: Some(Box::new((
                    crate::chat::AssistantMessage::default(),
                    result.usage.clone(),
                ))),
            }
        })?;
        Ok(InternalOutput {
            json,
            usage: result.usage,
            finish_reason: result.finish_reason,
            metrics: result.metrics,
        })
    }
}

/// One pass's hard native allowance; all passes share the caller's root deadline.
#[derive(Debug, Clone, Copy)]
pub struct InternalLimits {
    /// Maximum full rendered input (up to 4,096), checked before prefill.
    pub input_tokens: usize,
    /// Maximum sampled output, including unsuccessful JSON.
    pub output_tokens: usize,
    /// Absolute root-job deadline, never extended by a new pass.
    pub deadline: Instant,
}
impl Default for InternalLimits {
    fn default() -> Self {
        Self {
            input_tokens: 1024,
            output_tokens: 512,
            deadline: Instant::now() + Duration::from_secs(120),
        }
    }
}

/// Owned output from one internal evaluation, before semantic validation.
#[derive(Debug)]
pub struct InternalOutput {
    /// Untrusted JSON text; Debug is redacted through MemoryText.
    pub json: crate::memory::MemoryText,
    /// Actual owner-thread token usage.
    pub usage: Usage,
    /// Native stop boundary; token-limited output cannot be applied.
    pub finish_reason: FinishReason,
    /// Native measurements where available.
    pub metrics: Option<GenerationMetrics>,
}
/// An internal failure with measured partial usage for durable accounting.
#[derive(thiserror::Error)]
#[error("internal inference failed")]
pub struct InternalFailure {
    /// Host diagnostic cause; adapters must map it to sanitized public codes.
    #[source]
    pub source: crate::chat::ChatError,
    /// Observed partial usage; a lost native boundary still needs conservative reservation.
    pub usage: Usage,
    /// Last observed native timings, incomplete if evaluation did not finish.
    pub metrics: Option<GenerationMetrics>,
    /// Observed untrusted partial JSON, or its explicit capture validation error.
    /// Never promote partial output or expose it in an unauthenticated diagnostic.
    pub partial_json: crate::memory::MemoryResult<crate::memory::MemoryText>,
}
impl std::fmt::Debug for InternalFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InternalFailure")
            .field("usage", &self.usage)
            .finish_non_exhaustive()
    }
}
impl From<ModelFailure> for InternalFailure {
    fn from(failure: ModelFailure) -> Self {
        let (partial_json, usage) = match failure.partial {
            Some(partial) => (crate::memory::MemoryText::new(partial.0.content), partial.1),
            None => (
                Err(crate::memory::MemoryError::InvalidInput(
                    "no observed partial output",
                )),
                Usage::default(),
            ),
        };
        Self {
            source: failure.source,
            usage,
            metrics: failure.metrics.map(|metrics| *metrics),
            partial_json,
        }
    }
}
