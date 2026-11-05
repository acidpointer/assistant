//! Bounded orchestration, independent of native handles. Every real model/tool
//! call crosses a terminal guard after middleware. Errors keep actual receipts;
//! incomplete or truncated tool protocol is never dispatched.
use super::{
    AssistantMessage, ChatError, ChatResponse, ChatResult, EventSink, InputMode, Message,
    ModelBackend, ModelRequest, ModelTurn, RequestOptions, RunEvent, RunLimits, RunStatus,
    ToolCall, ToolReceipt, TurnContext,
};
use crate::{
    engine::{Cancellation, FinishReason, Usage},
    pipeline::{NextModel, NextTool, Pipeline, Services},
    tools::{ToolContext, failure},
};
use std::{collections::BTreeSet, sync::Arc, time::Instant};

/// Typed per-interaction state for hooks. Limits, cancellation, and execution
/// handles are private so modifying a draft cannot bypass the core boundaries.
#[derive(Debug)]
pub struct RunContext {
    #[cfg(target_os = "linux")]
    pub(crate) memory: Option<Arc<crate::memory::session::MemoryOperation>>,
    /// Runtime-local interaction ID.
    pub(crate) run_id: u64,
    /// Trusted host instructions for this run.
    pub system: String,
    /// Working messages, initially ending in the newly accepted user input.
    pub messages: Vec<Message>,
    /// Supporting knowledge kept only for this run.
    pub context: Vec<String>,
    /// Selected registered tool names. Hooks may select a different subset.
    pub selected_tools: Vec<String>,
    /// Model-turn preferences; execution limits remain separately enforced.
    pub options: RequestOptions,
    pub(crate) accepted: bool,
    base_messages: usize,
    chat_id: u64,
    lease: Arc<()>,
    backend: Arc<dyn ModelBackend>,
    pipeline: Arc<Pipeline>,
    events: EventSink,
    limits: RunLimits,
    cancellation: Cancellation,
    deadline: Instant,
    started: Instant,
    model_calls: usize,
    tool_calls: usize,
    usage: Usage,
    performance: Vec<super::ModelPerformance>,
    last_message: AssistantMessage,
    last_finish: Option<FinishReason>,
    limit_reason: Option<&'static str>,
    receipts: Vec<ToolReceipt>,
    evaluated_tools: Vec<String>,
}
impl RunContext {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        run_id: u64,
        chat_id: u64,
        lease: Arc<()>,
        system: String,
        mut messages: Vec<Message>,
        input: String,
        options: RequestOptions,
        events: EventSink,
        limits: RunLimits,
        backend: Arc<dyn ModelBackend>,
        pipeline: Arc<Pipeline>,
        started: Instant,
        deadline: Instant,
    ) -> Self {
        let base_messages = messages.len();
        messages.push(Message::User(input));
        let selected_tools = if options.tool_use == super::ToolUse::Disabled {
            Vec::new()
        } else {
            options.tools.clone().unwrap_or_else(|| {
                pipeline
                    .tools
                    .definitions()
                    .into_iter()
                    .map(|spec| spec.name)
                    .collect()
            })
        };
        Self {
            #[cfg(target_os = "linux")]
            memory: None,
            run_id,
            chat_id,
            lease,
            system,
            messages,
            context: options.context.clone(),
            selected_tools,
            cancellation: options.cancellation.clone(),
            options,
            events,
            limits,
            backend,
            pipeline,
            started,
            deadline,
            model_calls: 0,
            tool_calls: 0,
            usage: Usage::default(),
            performance: Vec::new(),
            last_message: AssistantMessage::default(),
            last_finish: None,
            limit_reason: None,
            receipts: Vec::new(),
            evaluated_tools: Vec::new(),
            accepted: false,
            base_messages,
        }
    }
    /// Runtime-local interaction identity; extensions cannot rewrite it.
    #[must_use]
    pub fn run_id(&self) -> u64 {
        self.run_id
    }
    /// Access shared, typed service handles.
    #[must_use]
    pub fn services(&self) -> &Services {
        &self.pipeline.services
    }
    /// Read hard limits without gaining mutation access to them.
    #[must_use]
    pub fn limits(&self) -> &RunLimits {
        &self.limits
    }
    /// Borrow cooperative run cancellation.
    #[must_use]
    pub fn cancellation(&self) -> &Cancellation {
        &self.cancellation
    }
    /// Publish a semantic event; delivery errors are propagated.
    /// # Errors
    /// Returns observer/consumer failure.
    pub async fn emit(&self, event: RunEvent) -> ChatResult<()> {
        self.events.emit(event).await
    }
    pub(crate) fn validate_input(&mut self) -> ChatResult<()> {
        self.check_running()?;
        if self.messages.len() > self.limits.working_messages {
            return Err(ChatError::Limit(
                "working messages; explicitly clear or replace completed history",
            ));
        }
        check_bytes(
            &self.system,
            &self.messages,
            &self.context,
            self.limits.input_bytes,
        )?;
        self.pipeline.tools.select(&self.selected_tools)?;
        if self.options.tool_use == super::ToolUse::RequireFirst && self.selected_tools.is_empty() {
            return Err(ChatError::Configuration(
                "required tool choice needs a selected tool".into(),
            ));
        }
        self.accepted = true;
        Ok(())
    }
    fn check_running(&self) -> ChatResult<()> {
        if self.cancellation.is_cancelled() {
            return Err(ChatError::Cancelled);
        }
        if Instant::now() >= self.deadline {
            return Err(ChatError::Deadline);
        }
        Ok(())
    }
    pub(crate) async fn evaluate(&mut self, mut request: ModelRequest) -> ChatResult<ModelTurn> {
        self.check_running()?;
        if self.model_calls >= self.limits.model_turns {
            return Err(ChatError::Limit("model turns"));
        }
        if request.messages.len() > self.limits.working_messages {
            return Err(ChatError::Limit("working messages"));
        }
        let names: Vec<_> = request.tools.iter().map(|tool| tool.name.clone()).collect();
        // Canonical definitions match the exact handlers that will deserialize.
        request.tools = self.pipeline.tools.select(&names)?;
        check_bytes(
            &request.system,
            &request.messages,
            &request.context,
            self.limits.input_bytes,
        )?;
        if serde_json::to_vec(
            &request
                .tools
                .iter()
                .map(crate::tools::ToolSpec::value)
                .collect::<Vec<_>>(),
        )?
        .len()
            > self.limits.input_bytes
        {
            return Err(ChatError::Limit("tool schema bytes"));
        }
        let max_tokens = request
            .options
            .max_new_tokens
            .unwrap_or(self.backend.default_max_tokens());
        let remaining = self
            .limits
            .generated_tokens
            .saturating_sub(self.usage.generated_tokens);
        if max_tokens == 0 || remaining == 0 {
            return Err(ChatError::Limit("remaining generated-token allowance"));
        }
        request.options.max_new_tokens = Some(max_tokens.min(remaining));
        request
            .options
            .thinking
            .get_or_insert(self.backend.default_thinking());
        request.options.cancellation = self.cancellation.clone();
        request.options.deadline = Some(self.deadline);
        if request.options.mode == InputMode::Raw
            && (!request.tools.is_empty()
                || !request.context.is_empty()
                || request.messages.len() != 1)
        {
            return Err(ChatError::Configuration(
                "raw completion requires one user message and no tools/context".into(),
            ));
        }
        self.evaluated_tools = names;
        self.model_calls += 1;
        self.events.reset().await;
        let turn = self.turn_context();
        let result = self
            .backend
            .evaluate(request, turn, self.events.clone())
            .await;
        // Record accounting before publishing a fallible terminal observation.
        match result {
            Ok(result) => {
                self.record_model_turn(&result, remaining, max_tokens)?;
                self.events.usage(Usage::default()).await;
                self.last_message = result.message.clone();
                self.last_finish = Some(result.finish_reason);
                self.emit(RunEvent::ModelTurnFinished {
                    run_id: self.run_id,
                    turn: self.model_calls,
                })
                .await?;
                Ok(result)
            }
            Err(error) => {
                if let Some(partial) = error.partial {
                    let (message, usage) = *partial;
                    self.performance.push(super::ModelPerformance {
                        measurement_complete: false,
                        turn: self.model_calls,
                        generated_tokens: usage.generated_tokens,
                        timing: error.metrics.map(|metrics| *metrics),
                    });
                    self.add_usage(&usage)?;
                    self.events.usage(Usage::default()).await;
                    self.last_message = message;
                }
                Err(error.source)
            }
        }
    }
    /// Capture the immutable execution boundary after admission has been checked.
    fn turn_context(&self) -> TurnContext {
        TurnContext {
            chat_id: self.chat_id,
            run_id: self.run_id,
            turn: self.model_calls,
            lease: self.lease.clone(),
            limits: self.limits.clone(),
            cancellation: self.cancellation.clone(),
            deadline: self.deadline,
        }
    }
    /// Retain terminal owner-thread measurements before any fallible observer.
    fn record_model_turn(
        &mut self,
        result: &ModelTurn,
        remaining: usize,
        requested: usize,
    ) -> ChatResult<()> {
        if result.finish_reason == FinishReason::TokenLimit {
            self.limit_reason = result.limit_reason.or(Some(if remaining <= requested {
                "run_tokens"
            } else {
                "output_tokens"
            }));
        }
        self.performance.push(super::ModelPerformance {
            measurement_complete: result.metrics.is_some(),
            turn: self.model_calls,
            generated_tokens: result.usage.generated_tokens,
            timing: result.metrics.clone(),
        });
        self.add_usage(&result.usage)
    }
    fn add_usage(&mut self, usage: &Usage) -> ChatResult<()> {
        self.usage.input_tokens = self
            .usage
            .input_tokens
            .checked_add(usage.input_tokens)
            .ok_or(ChatError::Limit("usage overflow"))?;
        self.usage.cached_tokens = self
            .usage
            .cached_tokens
            .checked_add(usage.cached_tokens)
            .ok_or(ChatError::Limit("usage overflow"))?;
        self.usage.prefill_tokens = self
            .usage
            .prefill_tokens
            .checked_add(usage.prefill_tokens)
            .ok_or(ChatError::Limit("usage overflow"))?;
        self.usage.generated_tokens = self
            .usage
            .generated_tokens
            .checked_add(usage.generated_tokens)
            .ok_or(ChatError::Limit("usage overflow"))?;
        if self.usage.generated_tokens > self.limits.generated_tokens {
            return Err(ChatError::Limit("generated tokens"));
        }
        Ok(())
    }
    pub(crate) async fn execute_tool(
        &mut self,
        call: ToolCall,
        allowed: &[String],
        expected_id: &str,
    ) -> ChatResult<ToolReceipt> {
        if call.id != expected_id {
            return Err(ChatError::Protocol(
                "tool middleware changed correlation ID".into(),
            ));
        }
        self.check_running()?;
        if serde_json::to_vec(&call.arguments)?.len() > self.limits.input_bytes {
            return Err(ChatError::Limit("tool argument bytes"));
        }
        let context = ToolContext {
            invocation_id: call.id.clone(),
            cancellation: self.cancellation.clone(),
        };
        let tools = self.pipeline.tools.clone();
        #[cfg(target_os = "linux")]
        if let Some(memory) = &self.memory {
            return memory
                .execute_tool(tools, call, allowed.to_vec(), context, self.deadline)
                .await;
        }
        let result = tokio::select! {
            () = self.cancellation.cancelled() => ToolReceipt { call: call.clone(), result: failure("interrupted", "no confirmed result; external effects may have occurred") },
            () = tokio::time::sleep_until(self.deadline.into()) => ToolReceipt { call: call.clone(), result: failure("deadline", "no confirmed result; external effects may have occurred") },
            result = tools.execute(call.clone(), allowed, context) => result,
        };
        Ok(result)
    }
    pub(crate) async fn response(&self, status: RunStatus) -> ChatResponse {
        let progress = self.events.progress.lock().await;
        let mut message = self.last_message.clone();
        if progress.message.content.len() > message.content.len()
            || progress.message.reasoning.as_ref().map_or(0, String::len)
                > message.reasoning.as_ref().map_or(0, String::len)
        {
            message = progress.message.clone();
        }
        let mut usage = self.usage.clone();
        usage.input_tokens += progress.usage.input_tokens;
        usage.cached_tokens += progress.usage.cached_tokens;
        usage.prefill_tokens += progress.usage.prefill_tokens;
        usage.generated_tokens += progress.usage.generated_tokens;
        let mut performance = self.performance.clone();
        if !performance.iter().any(|p| p.turn == self.model_calls)
            && let Some(timing) = progress.metrics.clone()
        {
            performance.push(super::ModelPerformance {
                turn: self.model_calls,
                generated_tokens: progress.usage.generated_tokens,
                timing: Some(timing),
                measurement_complete: false,
            });
        }
        ChatResponse {
            performance,
            limit_reason: self.limit_reason,
            run_id: self.run_id,
            message,
            tools: self.receipts.clone(),
            usage,
            finish_reason: self.last_finish,
            status,
            elapsed: self.started.elapsed(),
            warnings: Vec::new(),
            messages: self.messages[self.base_messages.min(self.messages.len())..].to_vec(),
        }
    }
    pub(crate) fn receipts(&self) -> &[ToolReceipt] {
        &self.receipts
    }
    pub(crate) fn run_messages(&self) -> &[Message] {
        &self.messages[self.base_messages.min(self.messages.len())..]
    }
    pub(crate) fn close_pending_tools(&mut self) {
        let mut pending = std::collections::BTreeMap::new();
        for message in &self.messages {
            match message {
                Message::Assistant(message) => {
                    for call in &message.tool_calls {
                        pending.insert(call.id.clone(), call.clone());
                    }
                }
                Message::Tool { call_id, .. } => {
                    pending.remove(call_id);
                }
                Message::User(_) => {}
            }
        }
        for call in pending.into_values() {
            let receipt = ToolReceipt {
                call: call.clone(),
                result: failure(
                    "interrupted",
                    "no confirmed result; do not assume execution completed",
                ),
            };
            self.messages.push(Message::Tool {
                call_id: call.id,
                result: receipt.result.clone(),
            });
            self.receipts.push(receipt);
        }
    }
}

// Discard only complete older exchanges. The last user input and its tool
// continuation are indivisible, so an oversized current turn fails explicitly.
pub(crate) fn drop_oldest_exchange(messages: &mut Vec<Message>) -> bool {
    let Some(end) = messages
        .iter()
        .enumerate()
        .filter(|(_, m)| matches!(m, Message::User(_)))
        .nth(1)
        .map(|(i, _)| i)
    else {
        return false;
    };
    messages.drain(..end);
    true
}
pub(super) fn trim_history(
    messages: &mut Vec<Message>,
    system: &str,
    input: &str,
    context: &[String],
    limits: &RunLimits,
) {
    messages.push(Message::User(input.into()));
    while messages.len() > limits.working_messages
        || check_bytes(system, messages, context, limits.input_bytes).is_err()
    {
        if !drop_oldest_exchange(messages) {
            break;
        }
    }
    messages.pop();
}

fn check_bytes(
    system: &str,
    messages: &[Message],
    context: &[String],
    limit: usize,
) -> ChatResult<()> {
    fn take(remaining: &mut usize, bytes: usize) -> ChatResult<()> {
        *remaining = remaining
            .checked_sub(bytes)
            .ok_or(ChatError::Limit("input bytes"))?;
        Ok(())
    }
    // Count structured payloads without allocating another serialized transcript.
    struct Counter(usize);
    impl std::io::Write for Counter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0 = self
                .0
                .checked_sub(bytes.len())
                .ok_or_else(|| std::io::Error::other("byte limit"))?;
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    fn value(remaining: &mut usize, value: &serde_json::Value) -> ChatResult<()> {
        let mut counter = Counter(*remaining);
        serde_json::to_writer(&mut counter, value).map_err(|_| ChatError::Limit("input bytes"))?;
        *remaining = counter.0;
        Ok(())
    }
    let mut remaining = limit;
    take(&mut remaining, system.len())?;
    for text in context {
        take(&mut remaining, text.len())?;
    }
    for message in messages {
        take(&mut remaining, 64)?; // bounded protocol overhead for each entry
        match message {
            Message::User(text) => take(&mut remaining, text.len())?,
            Message::Assistant(message) => {
                take(&mut remaining, message.content.len())?;
                take(
                    &mut remaining,
                    message.reasoning.as_ref().map_or(0, String::len),
                )?;
                for call in &message.tool_calls {
                    take(&mut remaining, call.name.len())?;
                    take(&mut remaining, call.id.len())?;
                    value(&mut remaining, &call.arguments)?;
                }
            }
            Message::Tool { call_id, result } => {
                take(&mut remaining, call_id.len())?;
                value(&mut remaining, result)?;
            }
        }
    }
    Ok(())
}

async fn publish_message(context: &RunContext, message: &AssistantMessage) -> ChatResult<()> {
    let reasoning = message.reasoning.as_deref().unwrap_or_default();
    if message.content.len().saturating_add(reasoning.len()) > context.limits.output_bytes {
        return Err(ChatError::Limit("output bytes"));
    }
    // Text already observed by a client cannot be retracted by an after hook.
    let visible = context.events.progress.lock().await.message.content.clone();
    if !message.content.starts_with(&visible) {
        return Err(ChatError::Protocol(
            "extension changed already streamed text".into(),
        ));
    }
    if message.content.len() > visible.len() {
        context
            .emit(RunEvent::TextDelta {
                run_id: context.run_id,
                text: message.content[visible.len()..].into(),
            })
            .await?;
    }
    let delivered = context
        .events
        .progress
        .lock()
        .await
        .message
        .reasoning
        .clone()
        .unwrap_or_default();
    if !reasoning.starts_with(&delivered) {
        return Err(ChatError::Protocol(
            "extension changed streamed reasoning".into(),
        ));
    }
    if reasoning.len() > delivered.len() {
        context
            .emit(RunEvent::ReasoningDelta {
                run_id: context.run_id,
                text: reasoning[delivered.len()..].into(),
            })
            .await?;
    }
    Ok(())
}

/// Core loop used by the terminal run continuation. Callers normally use `Chat`.
/// # Errors
/// Returns bounded-run, model, protocol, or event-delivery failures.
pub async fn run_loop(context: &mut RunContext, pipeline: &Pipeline) -> ChatResult<ChatResponse> {
    pipeline.before_run(context).await?;
    let mut used_ids: BTreeSet<String> = context
        .messages
        .iter()
        .filter_map(|message| match message {
            Message::Assistant(message) => {
                Some(message.tool_calls.iter().map(|call| call.id.clone()))
            }
            _ => None,
        })
        .flatten()
        .collect();
    for turn_index in 0..context.limits.model_turns {
        context.check_running()?;
        context
            .emit(RunEvent::ModelTurnStarted {
                run_id: context.run_id,
                turn: turn_index + 1,
            })
            .await?;
        let mut request = ModelRequest {
            system: context.system.clone(),
            messages: context.messages.clone(),
            context: context.context.clone(),
            tools: pipeline.tools.select(&context.selected_tools)?,
            options: context.options.clone(),
        };
        pipeline.before_model(context, &mut request).await?;
        context.evaluated_tools = request.tools.iter().map(|tool| tool.name.clone()).collect();
        let mut turn = NextModel {
            layers: &pipeline.model_layers,
        }
        .run(context, request)
        .await?;
        let allowed = context.evaluated_tools.clone();
        pipeline.after_model(context, &mut turn.message).await?;
        context.last_message = turn.message.clone();
        context.last_finish = Some(turn.finish_reason);
        publish_message(context, &turn.message).await?;
        if turn.finish_reason == FinishReason::Cancelled {
            return Err(ChatError::Cancelled);
        }
        if turn.finish_reason == FinishReason::Deadline {
            return Err(ChatError::Deadline);
        }
        if turn.finish_reason == FinishReason::TokenLimit && !turn.message.tool_calls.is_empty() {
            return Err(ChatError::Limit("truncated tool request was not executed"));
        }
        if turn_index == 0
            && context.options.tool_use == super::ToolUse::RequireFirst
            && turn.message.tool_calls.is_empty()
        {
            return Err(ChatError::Protocol(
                "first model turn did not request a required tool".into(),
            ));
        }
        if turn.message.tool_calls.is_empty() {
            let status = if turn.finish_reason == FinishReason::TokenLimit {
                RunStatus::Truncated
            } else {
                RunStatus::Completed
            };
            let mut response = context.response(status).await;
            response.message = turn.message;
            return Ok(response);
        }
        dispatch_tools(context, pipeline, turn.message, &allowed, &mut used_ids).await?;
    }
    Err(ChatError::Limit("model turns"))
}

async fn dispatch_tools(
    context: &mut RunContext,
    pipeline: &Pipeline,
    message: AssistantMessage,
    allowed: &[String],
    used_ids: &mut BTreeSet<String>,
) -> ChatResult<()> {
    if context
        .tool_calls
        .checked_add(message.tool_calls.len())
        .is_none_or(|n| n > context.limits.tool_calls)
    {
        return Err(ChatError::Limit("tool calls"));
    }
    for call in &message.tool_calls {
        if call.id.is_empty() || !used_ids.insert(call.id.clone()) {
            return Err(ChatError::Protocol(
                "duplicate or empty tool call ID".into(),
            ));
        }
    }
    let calls = message.tool_calls.clone();
    context.messages.push(Message::Assistant(message));
    for call in calls {
        context.check_running()?;
        context.tool_calls += 1;
        context
            .emit(RunEvent::ToolStarted {
                run_id: context.run_id,
                call: call.clone(),
            })
            .await?;
        let expected = call.clone();
        let mut receipt = NextTool {
            layers: &pipeline.tool_layers,
            allowed,
            expected_id: &expected.id,
        }
        .run(context, call)
        .await?;
        if receipt.call.id != expected.id {
            return Err(ChatError::Protocol(
                "tool middleware changed correlation ID".into(),
            ));
        }
        if serde_json::to_vec(&receipt.result)?.len() > context.limits.tool_result_bytes {
            receipt.result = failure(
                "result_too_large",
                "result exceeded configured byte limit; use pagination",
            );
        }
        context.messages.push(Message::Tool {
            call_id: receipt.call.id.clone(),
            result: receipt.result.clone(),
        });
        context.receipts.push(receipt.clone());
        context
            .emit(RunEvent::ToolFinished {
                run_id: context.run_id,
                receipt,
            })
            .await?;
    }
    Ok(())
}
