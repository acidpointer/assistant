//! Bounded, reusable native inference state for one causal text sequence.
//!
//! A prepared input is a complete desired working prompt, not an append command.
//! Cache reuse requires an exact evaluated-token prefix. Native failures poison
//! the cache; the next operation rebuilds it. This module owns no chat history.
//! Contexts and generation cursors remain on their creating worker thread.
use super::{
    EngineError, EngineResult, LLMModel,
    helpers::{find_stop, holdback_len},
};
use llama_cpp::prelude::{
    AddBos, ChatApplyParams, ChatParams, ChatTemplates, DetokenizeError, LlamaBatch, LlamaContext,
    LlamaSampler, LlamaToken, Special, StreamDetokenizer,
};
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

#[derive(Debug, Default)]
struct CancelState {
    flag: AtomicBool,
    notify: tokio::sync::Notify,
}

/// Thread-safe cooperative cancellation, including asynchronous waiters.
#[derive(Debug, Clone, Default)]
pub struct Cancellation(Arc<CancelState>);
impl Cancellation {
    /// Signal all copies. Native execution observes this between bounded steps.
    pub fn cancel(&self) {
        self.0.flag.store(true, Ordering::Release);
        self.0.notify.notify_waiters();
    }
    /// Whether cancellation has been requested.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.0.flag.load(Ordering::Acquire)
    }
    /// Wait without polling. Registration precedes the flag check to avoid a
    /// lost notification when cancellation races with first polling.
    pub async fn cancelled(&self) {
        let notified = self.0.notify.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        if !self.is_cancelled() {
            notified.await;
        }
    }
}

/// Feedback from an output consumer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamCmd {
    /// Continue generation.
    Continue,
    /// Cancel generation after the delivered chunk.
    Stop,
}
/// Raw protocol output consumer. User-visible streaming belongs to chat parsing.
pub trait TokenSink {
    /// Accept complete UTF-8 text, or fail delivery.
    /// # Errors
    /// Return a delivery error when the recipient can no longer accept output.
    fn emit(&mut self, text: &str) -> EngineResult<StreamCmd>;
}
impl<F: FnMut(&str) -> EngineResult<StreamCmd>> TokenSink for F {
    fn emit(&mut self, text: &str) -> EngineResult<StreamCmd> {
        self(text)
    }
}

/// Why generation terminated. Errors carry partial progress separately.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum FinishReason {
    /// Model emitted its end token, which is not decoded or counted as output.
    EndOfGeneration,
    /// The generated-token allowance was consumed.
    TokenLimit,
    /// A configured stop sequence matched; it is excluded from text.
    Stop,
    /// Cooperative cancellation or a consumer stop request.
    Cancelled,
    /// The operation deadline expired between native calls.
    Deadline,
}
/// Counters for a single model evaluation, including hidden protocol output.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Usage {
    /// Total tokens in the supplied working prompt.
    pub input_tokens: usize,
    /// Input tokens whose evaluated state was reused.
    pub cached_tokens: usize,
    /// Input tokens evaluated during this operation.
    pub prefill_tokens: usize,
    /// Sampled non-EOG tokens, including stop-sequence tokens.
    pub generated_tokens: usize,
}
/// Monotonic measurements made on the native owner thread. Active times sum only
/// calls to `advance`; scheduler and consumer waits are excluded. GPU work uses
/// the existing synchronous decode boundary, with no per-token extra sync.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct GenerationMetrics {
    /// Active prompt evaluation time, in nanoseconds.
    pub prefill_ns: u64,
    /// Active sampling, decoding and detokenization time, in nanoseconds.
    pub generation_ns: u64,
    /// Wall time from native cursor creation to this measurement.
    pub elapsed_ns: u64,
    /// Wall time to the first sampled non-EOG token; not first visible text.
    pub first_token_ns: Option<u64>,
}
/// Successful or deliberately truncated native output.
#[derive(Debug, Clone)]
pub struct Generation {
    /// Raw text with configured stop sequences excluded.
    pub text: String,
    /// Token accounting for this evaluation.
    pub usage: Usage,
    /// Explicit termination condition.
    pub finish_reason: FinishReason,
    /// Time spent preparing the prompt on the native context.
    pub prefill_time: Duration,
    /// Time spent sampling and decoding output.
    pub generation_time: Duration,
    /// Detailed owner-thread measurements.
    pub metrics: GenerationMetrics,
}
/// Valid output already obtained before a failure.
#[derive(Debug, Clone, Default)]
pub struct GenerationProgress {
    /// Measurements at the last completed native step.
    pub metrics: GenerationMetrics,
    /// Completed UTF-8 output, excluding matched stop sequences.
    pub text: String,
    /// Accounting up to the failure boundary.
    pub usage: Usage,
}
/// A native operation failed; callers can retain already-produced output.
#[derive(Debug, thiserror::Error)]
#[error("generation failed: {source}")]
pub struct GenerationFailure {
    /// Original typed failure.
    #[source]
    pub source: EngineError,
    /// Progress that must not be confused with a completed assistant message.
    pub partial: Box<GenerationProgress>,
}
impl From<EngineError> for GenerationFailure {
    fn from(source: EngineError) -> Self {
        Self {
            source,
            partial: Box::default(),
        }
    }
}

/// Per-operation settings. Model defaults are used when an override is absent.
#[derive(Debug, Clone)]
pub struct GenerationOptions {
    /// New-token allowance; must be positive and fit with the full prompt.
    pub max_new_tokens: Option<usize>,
    /// Additional literal stop sequences. Empty strings are ignored.
    pub stop_sequences: Vec<String>,
    /// Upper bound for generated text bytes, including protocol text.
    pub max_output_bytes: usize,
    /// Deadline checked before and between native calls.
    pub deadline: Option<Instant>,
    /// Shared cancellation signal.
    pub cancellation: Cancellation,
}
impl Default for GenerationOptions {
    fn default() -> Self {
        Self {
            max_new_tokens: None,
            stop_sequences: Vec::new(),
            max_output_bytes: 262_144,
            deadline: None,
            cancellation: Cancellation::default(),
        }
    }
}

/// Whether protocol parsing must consume a complete model response.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParseMode {
    /// Final output; malformed or incomplete protocol is an error.
    Complete,
    /// Streaming prefix; incomplete trailing protocol is tolerated.
    Partial,
}

/// Coherent, model-bound input. Most callers use the high-level chat API.
#[derive(Debug, Clone)]
pub struct PreparedInput<'model> {
    model: &'model LLMModel<'model>,
    tokens: Vec<LlamaToken>,
    chat: Option<ChatParams>,
}
impl PreparedInput<'_> {
    /// Number of rendered input tokens, including template control tokens.
    #[must_use]
    pub fn token_count(&self) -> usize {
        self.tokens.len()
    }
    /// Parse raw output with the exact parser used to prepare this input.
    /// # Errors
    /// Returns native parsing errors or malformed normalized JSON.
    pub fn parse(&self, text: &str, mode: ParseMode) -> EngineResult<serde_json::Value> {
        match &self.chat {
            Some(chat) => Ok(serde_json::from_str(
                &chat.parse(text, mode == ParseMode::Partial)?,
            )?),
            None => Ok(serde_json::json!({"role":"assistant","content":text})),
        }
    }
}

#[derive(Debug, Default)]
struct Cache {
    tokens: Vec<LlamaToken>,
    valid: bool,
    logits: bool,
}

/// Worker-confined state borrowing one loaded model. It never owns permanent memory.
#[derive(Debug)]
pub struct LLMContext<'model> {
    native: LlamaContext<'model>,
    model: &'model LLMModel<'model>,
    batch: LlamaBatch,
    templates: Option<ChatTemplates>,
    cache: Cache,
    epoch: Option<Arc<()>>,
}
impl<'model> LLMContext<'model> {
    /// Evaluate a prepared prompt without sampling. Exact-prefix reuse follows
    /// the same rules as generation. Completes pending work before returning,
    /// making this an explicit prefill timing/inspection boundary.
    /// # Errors
    /// Requires one free output position, as generation does. Returns validation,
    /// decode or synchronization errors. Native failures
    /// invalidate reuse so the next operation rebuilds the prompt.
    pub fn prefill(&mut self, input: &PreparedInput<'model>) -> EngineResult<Usage> {
        let mut state = self.begin(
            input,
            GenerationOptions {
                max_new_tokens: Some(1),
                ..GenerationOptions::default()
            },
        )?;
        while state.cursor < input.token_count() {
            self.advance(&mut state)?;
        }
        self.synchronize()?;
        Ok(state.usage)
    }
    /// Wait for pending native work before ending a measurement interval.
    /// # Errors
    /// Returns synchronization failure and invalidates cache reuse.
    pub fn synchronize(&mut self) -> EngineResult<()> {
        if let Err(error) = self.native.try_synchronize() {
            self.cache.valid = false;
            self.cache.logits = false;
            return Err(error.into());
        }
        Ok(())
    }
    /// Owned cache/allocation snapshot, safe to send off this worker thread.
    /// # Errors
    /// Returns native inspection, corrupt-result or checked-accounting failures.
    pub fn cache_diagnostics(&self) -> EngineResult<super::CacheDiagnostics> {
        super::CacheDiagnostics::collect(&self.native, self.model)
    }
    pub(super) fn new(
        native: LlamaContext<'model>,
        model: &'model LLMModel<'model>,
    ) -> EngineResult<Self> {
        if !native.causal_attn() {
            return Err(EngineError::Configuration(
                "this generation path requires causal attention",
            ));
        }
        let batch = LlamaBatch::new(native.n_batch() as usize, 1)?;
        Ok(Self {
            native,
            model,
            batch,
            templates: None,
            cache: Cache::default(),
            epoch: None,
        })
    }
    /// Actual token capacity for the single native sequence.
    #[must_use]
    pub fn capacity(&self) -> usize {
        self.native.n_ctx_seq() as usize
    }
    /// Number of evaluated tokens represented by valid cache bookkeeping.
    #[must_use]
    pub fn cached_tokens(&self) -> usize {
        if self.cache.valid {
            self.cache.tokens.len()
        } else {
            0
        }
    }
    /// Discard native inference state while retaining loaded weights.
    pub fn reset(&mut self) {
        self.epoch = None;
        self.native.clear_kv_cache();
        self.cache.tokens.clear();
        self.cache.valid = true;
        self.cache.logits = false;
    }
    /// Prepare a raw completion prompt without evaluating it.
    /// # Errors
    /// Returns byte-limit, tokenization, or input-capacity failures.
    pub fn prepare_raw(&self, text: &str) -> EngineResult<PreparedInput<'model>> {
        if text.len() > 1_048_576 {
            return Err(EngineError::Limit("input bytes"));
        }
        let tokens = self.model.model_ref().str_to_token(text, AddBos::Always)?;
        self.prepare_tokens(tokens, None)
    }
    /// Prepare chat rendering, tokens, grammar, stops, and parser together.
    /// # Errors
    /// Returns template, tokenization, or input-capacity failures.
    pub fn prepare_chat(
        &mut self,
        request: &ChatApplyParams,
    ) -> EngineResult<PreparedInput<'model>> {
        if self.templates.is_none() {
            self.templates = Some(ChatTemplates::from_model(self.model.model_ref(), None)?);
        }
        let templates = self
            .templates
            .as_ref()
            .ok_or(EngineError::State("missing templates"))?;
        let chat = templates.apply(request)?;
        if chat.prompt.len() > 1_048_576 {
            return Err(EngineError::Limit("rendered input bytes"));
        }
        let tokens = chat.tokenize_prompt(self.model.model_ref(), AddBos::Always)?;
        self.prepare_tokens(tokens, Some(chat))
    }
    fn prepare_tokens(
        &self,
        tokens: Vec<LlamaToken>,
        chat: Option<ChatParams>,
    ) -> EngineResult<PreparedInput<'model>> {
        if tokens.is_empty() {
            return Err(EngineError::Configuration("empty tokenized input"));
        }
        if tokens.len() > self.capacity() {
            return Err(EngineError::Budget {
                input: tokens.len(),
                output: 0,
                capacity: self.capacity(),
            });
        }
        Ok(PreparedInput {
            model: self.model,
            tokens,
            chat,
        })
    }
    /// Execute a complete prepared input, reusing only a verified cache prefix.
    /// # Errors
    /// Failures preserve partial progress; uncertain native state is invalidated.
    pub fn generate<S: TokenSink>(
        &mut self,
        input: &PreparedInput<'model>,
        options: GenerationOptions,
        sink: &mut S,
    ) -> Result<Generation, GenerationFailure> {
        let mut state = self.begin(input, options)?;
        while !state.is_finished() {
            let chunk = self
                .advance(&mut state)
                .map_err(|source| state.failure(source))?;
            if let Some(chunk) = chunk {
                match sink.emit(&chunk) {
                    Ok(StreamCmd::Continue) => {}
                    Ok(StreamCmd::Stop) => state.options.cancellation.cancel(),
                    Err(source) => return Err(state.failure(source)),
                }
            }
        }
        state.result()
    }
    pub(crate) fn begin(
        &mut self,
        input: &PreparedInput<'model>,
        options: GenerationOptions,
    ) -> EngineResult<GenerationState<'model>> {
        if !std::ptr::eq(self.model, input.model) {
            return Err(EngineError::ModelMismatch);
        }
        let max_tokens = options
            .max_new_tokens
            .unwrap_or(self.model.config().max_new_tokens);
        if max_tokens == 0 {
            return Err(EngineError::Configuration("zero output allowance"));
        }
        if input
            .tokens
            .len()
            .checked_add(max_tokens)
            .is_none_or(|n| n > self.capacity())
        {
            return Err(EngineError::Budget {
                input: input.tokens.len(),
                output: max_tokens,
                capacity: self.capacity(),
            });
        }
        let mut stops: Vec<String> = input
            .chat
            .as_ref()
            .map(|chat| serde_json::from_str(&chat.additional_stops_json))
            .transpose()?
            .unwrap_or_default();
        stops.extend(options.stop_sequences.iter().cloned());
        if stops.len() > 64 || stops.iter().map(String::len).sum::<usize>() > 4096 {
            return Err(EngineError::Limit("stop sequences"));
        }
        let mut samplers = Vec::new();
        if let Some(chat) = &input.chat
            && let Some(grammar) = chat.try_grammar_sampler(self.model.model_ref())?
        {
            samplers.push(grammar);
        }
        samplers.push(LlamaSampler::try_greedy()?);
        let sampler = LlamaSampler::try_chain(samplers)?;
        let special = if input.chat.is_some() {
            Special::Tokenize
        } else {
            Special::Plaintext
        };
        let interrupted = options.cancellation.is_cancelled()
            || options.deadline.is_some_and(|t| Instant::now() >= t);
        let reusable = self.cache.valid
            && input.tokens.starts_with(&self.cache.tokens)
            && (input.tokens.len() != self.cache.tokens.len() || self.cache.logits);
        // Pre-cancelled requests must leave the previous valid state untouched.
        if !interrupted && !reusable {
            self.reset();
        }
        let cached = if reusable { self.cache.tokens.len() } else { 0 };
        let epoch = Arc::new(());
        self.epoch = Some(epoch.clone());
        Ok(GenerationState {
            epoch,
            input: input.clone(),
            options,
            sampler,
            detokenizer: Some(StreamDetokenizer::new(self.model.model_ref(), special)),
            buffer: StopBuffer::new(stops),
            cursor: cached,
            max_tokens,
            usage: Usage {
                input_tokens: input.tokens.len(),
                cached_tokens: cached,
                ..Usage::default()
            },
            started: Instant::now(),
            metrics: GenerationMetrics::default(),
            reason: None,
        })
    }
    pub(crate) fn advance(
        &mut self,
        state: &mut GenerationState<'model>,
    ) -> EngineResult<Option<String>> {
        if !self
            .epoch
            .as_ref()
            .is_some_and(|epoch| Arc::ptr_eq(epoch, &state.epoch))
        {
            return Err(EngineError::State("stale generation cursor"));
        }
        let started = Instant::now();
        let prefill = state.cursor < state.input.tokens.len();
        let tokens = state.usage.generated_tokens;
        let result = self.advance_inner(state);
        let elapsed = nanos(started.elapsed());
        if prefill {
            state.metrics.prefill_ns += elapsed;
        } else {
            state.metrics.generation_ns += elapsed;
        }
        state.metrics.elapsed_ns = nanos(state.started.elapsed());
        if state.usage.generated_tokens > tokens && state.metrics.first_token_ns.is_none() {
            state.metrics.first_token_ns = Some(state.metrics.elapsed_ns);
        }
        if result.is_err() {
            self.cache.valid = false;
            self.cache.logits = false;
        }
        result
    }
    fn advance_inner(
        &mut self,
        state: &mut GenerationState<'model>,
    ) -> EngineResult<Option<String>> {
        if state.is_finished() {
            return Ok(None);
        }
        if state.options.cancellation.is_cancelled() {
            return state.finish(FinishReason::Cancelled);
        }
        if state
            .options
            .deadline
            .is_some_and(|deadline| Instant::now() >= deadline)
        {
            return state.finish(FinishReason::Deadline);
        }
        if state.cursor < state.input.tokens.len() {
            let end = (state.cursor + self.native.n_batch() as usize).min(state.input.tokens.len());
            self.batch.clear();
            for index in state.cursor..end {
                self.batch.add(
                    state.input.tokens[index],
                    native_position(index)?,
                    &[0],
                    index + 1 == state.input.tokens.len(),
                )?;
            }
            self.native.decode(&mut self.batch)?;
            self.cache
                .tokens
                .extend_from_slice(&state.input.tokens[state.cursor..end]);
            self.cache.valid = true;
            self.cache.logits = end == state.input.tokens.len();
            state.usage.prefill_tokens += end - state.cursor;
            state.cursor = end;
            return Ok(None);
        }
        let token = state.sampler.try_sample(&self.native, -1)?;
        if self.model.model_ref().is_eog_token(token) {
            return state.finish(FinishReason::EndOfGeneration);
        }
        state.usage.generated_tokens += 1;
        let detok = state
            .detokenizer
            .as_mut()
            .ok_or(EngineError::State("missing detokenizer"))?;
        let piece = detok.push(token)?;
        if state
            .buffer
            .text
            .len()
            .checked_add(piece.len())
            .is_none_or(|n| n > state.options.max_output_bytes)
        {
            return Err(EngineError::Limit("output bytes"));
        }
        let (mut ready, stopped) = state.buffer.push(&piece);
        if stopped {
            state.reason = Some(FinishReason::Stop);
            return Ok(nonempty(ready));
        }
        self.batch.clear();
        self.batch
            .add(token, native_position(self.cache.tokens.len())?, &[0], true)?;
        self.native.decode(&mut self.batch)?;
        self.cache.tokens.push(token);
        self.cache.logits = true;
        if state.usage.generated_tokens == state.max_tokens
            && let Some(tail) = state.finish(FinishReason::TokenLimit)?
        {
            ready.push_str(&tail);
        }
        Ok(nonempty(ready))
    }
}

fn native_position(index: usize) -> EngineResult<i32> {
    i32::try_from(index).map_err(|_| EngineError::Configuration("native token position overflow"))
}
fn nonempty(text: String) -> Option<String> {
    if text.is_empty() { None } else { Some(text) }
}

/// Owned cursor; it borrows weights, never the context being stepped.
#[derive(Debug)]
pub(crate) struct GenerationState<'model> {
    epoch: Arc<()>,
    input: PreparedInput<'model>,
    options: GenerationOptions,
    sampler: LlamaSampler,
    detokenizer: Option<StreamDetokenizer<'model>>,
    buffer: StopBuffer,
    cursor: usize,
    max_tokens: usize,
    usage: Usage,
    started: Instant,
    metrics: GenerationMetrics,
    reason: Option<FinishReason>,
}
impl GenerationState<'_> {
    pub(crate) fn is_finished(&self) -> bool {
        self.reason.is_some()
    }
    pub(crate) fn progress(&self) -> GenerationProgress {
        GenerationProgress {
            text: self.buffer.text.clone(),
            metrics: self.metrics.clone(),
            usage: self.usage.clone(),
        }
    }
    pub(crate) fn failure(&self, source: EngineError) -> GenerationFailure {
        GenerationFailure {
            source,
            partial: Box::new(self.progress()),
        }
    }
    pub(crate) fn result(&self) -> Result<Generation, GenerationFailure> {
        let reason = self
            .reason
            .ok_or_else(|| self.failure(EngineError::State("generation is not finished")))?;
        Ok(Generation {
            text: self.buffer.text.clone(),
            metrics: self.metrics.clone(),
            usage: self.usage.clone(),
            finish_reason: reason,
            prefill_time: Duration::from_nanos(self.metrics.prefill_ns),
            generation_time: Duration::from_nanos(self.metrics.generation_ns),
        })
    }
    fn finish(&mut self, reason: FinishReason) -> EngineResult<Option<String>> {
        let detok = self
            .detokenizer
            .take()
            .ok_or(EngineError::State("already finalized"))?;
        let tail = match detok.finish() {
            Ok(tail) => tail,
            // Preserve the existing truncation policy for deliberate cutoffs.
            Err(DetokenizeError::IncompleteUtf8(bytes))
                if reason != FinishReason::EndOfGeneration =>
            {
                String::from_utf8_lossy(&bytes).into_owned()
            }
            Err(error) => return Err(error.into()),
        };
        if self
            .buffer
            .text
            .len()
            .checked_add(tail.len())
            .is_none_or(|n| n > self.options.max_output_bytes)
        {
            return Err(EngineError::Limit("output bytes"));
        }
        let (mut ready, stopped) = self.buffer.push(&tail);
        self.reason = Some(if stopped { FinishReason::Stop } else { reason });
        if !stopped {
            ready.push_str(&self.buffer.flush());
        }
        Ok(nonempty(ready))
    }
}

#[derive(Debug)]
struct StopBuffer {
    text: String,
    pending: String,
    stops: Vec<String>,
    max_len: usize,
}
impl StopBuffer {
    fn new(stops: Vec<String>) -> Self {
        let max_len = stops.iter().map(String::len).max().unwrap_or_default();
        Self {
            text: String::new(),
            pending: String::new(),
            stops,
            max_len,
        }
    }
    fn push(&mut self, piece: &str) -> (String, bool) {
        self.text.push_str(piece);
        self.pending.push_str(piece);
        if let Some(offset) = find_stop(&self.pending, &self.stops) {
            self.text
                .truncate(self.text.len() - self.pending.len() + offset);
            let ready = self.pending[..offset].to_owned();
            self.pending.clear();
            return (ready, true);
        }
        let end = self.pending.len() - holdback_len(&self.pending, &self.stops, self.max_len);
        let ready = self.pending[..end].to_owned();
        self.pending.replace_range(..end, "");
        (ready, false)
    }
    fn flush(&mut self) -> String {
        std::mem::take(&mut self.pending)
    }
}

#[cfg(test)]
mod cache_tests;
#[cfg(test)]
mod tests;

fn nanos(duration: Duration) -> u64 {
    u64::try_from(duration.as_nanos()).unwrap_or(u64::MAX)
}
