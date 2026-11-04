//! Persistent, event-driven owner of services and dedicated inference workers.
//!
//! Configuration does no native I/O. `start` starts services then workers, rolling
//! back on failure. Chat handles exchange owned requests through bounded queues.
//! Explicit shutdown cancels admitted runs, joins workers, then stops services.
//! Dropping a running runtime requests cancellation but cannot await service flush.
mod worker;
use crate::{
    chat::{Chat, ChatError, ChatResult, ModelBackend, RunLimits},
    engine::{Cancellation, LLMConfig, ModelEngine},
    pipeline::{ModelLayer, Pipeline, Plugin, PluginRegistrar, RunLayer, ToolLayer},
    tools::Tool,
};
use std::{
    collections::BTreeMap,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::sync::{Mutex, Notify, OwnedSemaphorePermit, Semaphore};
use worker::WorkerHandle;

/// Owned reports for contexts sharing one worker's resident model weights.
#[derive(Debug, Clone)]
pub struct ModelCacheDiagnostics {
    /// Per-context snapshots; their model columns refer to shared allocations.
    pub contexts: Vec<crate::engine::CacheDiagnostics>,
    /// Aggregate native buffer accounting, counting model weights once.
    /// Empty before the first context is allocated; this is not a model-only census.
    pub memory: Vec<llama_cpp::context::memory_breakdown::MemoryBreakdownEntry>,
}
impl ModelCacheDiagnostics {
    /// Prompt/path-free diagnostic JSON.
    #[must_use]
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({"contexts":self.contexts.iter().map(crate::engine::CacheDiagnostics::to_json).collect::<Vec<_>>(),"aggregate_memory":self.memory.iter().map(|m|serde_json::json!({"buffer":m.buft_name,"model_bytes":m.model,"context_bytes":m.context,"compute_bytes":m.compute})).collect::<Vec<_>>()})
    }
}

#[derive(Debug)]
struct Registration {
    cancellation: Cancellation,
    done: AtomicBool,
}
#[derive(Debug)]
pub(crate) struct RuntimeControl {
    closed: AtomicBool,
    pub(crate) shutdown: Cancellation,
    sequence: AtomicU64,
    admission: Arc<Semaphore>,
    active: Mutex<Vec<Arc<Registration>>>,
    idle: Notify,
}
impl RuntimeControl {
    pub(crate) fn new(capacity: usize) -> Arc<Self> {
        Arc::new(Self {
            closed: AtomicBool::new(false),
            shutdown: Cancellation::default(),
            sequence: AtomicU64::new(1),
            admission: Arc::new(Semaphore::new(capacity)),
            active: Mutex::new(Vec::new()),
            idle: Notify::new(),
        })
    }
    pub(crate) fn next_id(&self) -> u64 {
        self.sequence.fetch_add(1, Ordering::Relaxed)
    }
    pub(crate) fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Acquire)
    }
    pub(crate) fn check_open(&self) -> ChatResult<()> {
        if self.is_closed() {
            Err(ChatError::Closed)
        } else {
            Ok(())
        }
    }
    pub(crate) async fn admit(
        self: &Arc<Self>,
        cancellation: Cancellation,
    ) -> ChatResult<RunGuard> {
        self.check_open()?;
        let permit = self
            .admission
            .clone()
            .try_acquire_owned()
            .map_err(|error| match error {
                tokio::sync::TryAcquireError::Closed => ChatError::Closed,
                tokio::sync::TryAcquireError::NoPermits => {
                    ChatError::Limit("queued and active runs")
                }
            })?;
        let mut active = self.active.lock().await;
        self.check_open()?;
        active.retain(|entry| !entry.done.load(Ordering::Acquire));
        let entry = Arc::new(Registration {
            cancellation,
            done: AtomicBool::new(false),
        });
        active.push(entry.clone());
        Ok(RunGuard {
            entry,
            control: self.clone(),
            _permit: permit,
            completed: false,
        })
    }
    fn close(&self) {
        self.closed.store(true, Ordering::Release);
        self.admission.close();
        self.shutdown.cancel();
    }
    async fn cancel_and_wait(&self, grace: Duration) -> ChatResult<()> {
        self.close();
        {
            let active = self.active.lock().await;
            for entry in active.iter() {
                if !entry.done.load(Ordering::Acquire) {
                    entry.cancellation.cancel();
                }
            }
        }
        let wait = async {
            loop {
                if self
                    .active
                    .lock()
                    .await
                    .iter()
                    .all(|entry| entry.done.load(Ordering::Acquire))
                {
                    break;
                }
                self.idle.notified().await;
            }
        };
        tokio::time::timeout(grace, wait)
            .await
            .map_err(|_| ChatError::Deadline)
    }
}
#[derive(Debug)]
pub(crate) struct RunGuard {
    entry: Arc<Registration>,
    control: Arc<RuntimeControl>,
    _permit: OwnedSemaphorePermit,
    completed: bool,
}
impl RunGuard {
    pub(crate) fn complete(&mut self) {
        self.completed = true;
    }
}
impl Drop for RunGuard {
    fn drop(&mut self) {
        if !self.completed {
            self.entry.cancellation.cancel();
        }
        self.entry.done.store(true, Ordering::Release);
        self.control.idle.notify_one();
    }
}

/// Builder for the persistent assistant. All registration failures surface at
/// `build`; service/native initialization happens only when `start` is awaited.
#[derive(Debug)]
pub struct RuntimeBuilder {
    engine: &'static ModelEngine,
    models: Vec<(String, LLMConfig)>,
    registrar: PluginRegistrar,
    error: Option<ChatError>,
    system: String,
    limits: RunLimits,
    queue_capacity: usize,
    event_capacity: usize,
    max_contexts: usize,
    shutdown_grace: Duration,
}
impl RuntimeBuilder {
    /// Register resident weights. The first model is the primary chat model.
    #[must_use]
    pub fn model(mut self, name: impl Into<String>, config: LLMConfig) -> Self {
        self.models.push((name.into(), config));
        self
    }
    /// Set default trusted instructions for newly created chats.
    #[must_use]
    pub fn system(mut self, text: impl Into<String>) -> Self {
        self.system = text.into();
        self
    }
    /// Install one package of capabilities without starting its services.
    #[must_use]
    pub fn plugin<P: Plugin>(mut self, plugin: P) -> Self {
        if self.error.is_none() {
            self.error = self.registrar.plugin(plugin).err();
        }
        self
    }
    /// Convenience registration for a single typed tool.
    #[must_use]
    pub fn tool<T: Tool>(mut self, tool: T) -> Self {
        if self.error.is_none() {
            self.error = self.registrar.add_tool(tool).err();
        }
        self
    }
    /// Wrap every admitted interaction.
    #[must_use]
    pub fn layer_run<L: RunLayer + 'static>(mut self, layer: L) -> Self {
        self.registrar.layer_run(layer);
        self
    }
    /// Wrap individual model turns.
    #[must_use]
    pub fn layer_model<L: ModelLayer + 'static>(mut self, layer: L) -> Self {
        self.registrar.layer_model(layer);
        self
    }
    /// Wrap individual tool invocations.
    #[must_use]
    pub fn layer_tool<L: ToolLayer + 'static>(mut self, layer: L) -> Self {
        self.registrar.layer_tool(layer);
        self
    }
    /// Set immutable run bounds used after middleware modifications.
    #[must_use]
    pub fn limits(mut self, limits: RunLimits) -> Self {
        self.limits = limits;
        self
    }
    /// Bound admitted work, including requests waiting on a busy chat.
    #[must_use]
    pub fn queue_capacity(mut self, capacity: usize) -> Self {
        self.queue_capacity = capacity;
        self
    }
    /// Bound semantic/native events waiting for a consumer.
    #[must_use]
    pub fn event_capacity(mut self, capacity: usize) -> Self {
        self.event_capacity = capacity;
        self
    }
    /// Bound resident native contexts independently for each loaded model.
    #[must_use]
    pub fn max_contexts(mut self, count: usize) -> Self {
        self.max_contexts = count;
        self
    }
    /// Maximum cooperative run/service shutdown wait.
    #[must_use]
    pub fn shutdown_grace(mut self, grace: Duration) -> Self {
        self.shutdown_grace = grace;
        self
    }
    /// Validate and freeze configuration without starting workers.
    /// # Errors
    /// Rejects invalid models, bounds, plugin dependencies, and stage ordering.
    pub fn build(self) -> ChatResult<AssistantRuntime> {
        if let Some(error) = self.error {
            return Err(error);
        }
        self.limits.validate()?;
        if self.models.is_empty() {
            return Err(ChatError::Configuration(
                "at least one model is required".into(),
            ));
        }
        if self.queue_capacity == 0
            || self.event_capacity == 0
            || self.max_contexts == 0
            || self.shutdown_grace.is_zero()
        {
            return Err(ChatError::Configuration(
                "runtime capacities and grace must be positive".into(),
            ));
        }
        if self.queue_capacity > 65_536
            || self.event_capacity > 65_536
            || self.max_contexts > 1024
            || self.models.len() > 16
        {
            return Err(ChatError::Limit("runtime configuration capacity"));
        }
        if self.system.len() > self.limits.input_bytes {
            return Err(ChatError::Limit("system bytes"));
        }
        let primary = self.models[0].0.clone();
        let mut models = BTreeMap::new();
        for (name, config) in self.models {
            if name.is_empty()
                || name.len() > 64
                || !name
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"_.-".contains(&b))
                || models.contains_key(&name)
            {
                return Err(ChatError::Configuration(
                    "duplicate/invalid model name".into(),
                ));
            }
            config.validate()?;
            models.insert(name, config);
        }
        Ok(AssistantRuntime {
            engine: self.engine,
            models,
            primary,
            pipeline: Arc::new(self.registrar.build()?),
            system: self.system,
            limits: self.limits,
            queue_capacity: self.queue_capacity,
            event_capacity: self.event_capacity,
            max_contexts: self.max_contexts,
            shutdown_grace: self.shutdown_grace,
        })
    }
}

/// Validated, unstarted runtime. Dropping it has no worker/native side effects.
#[derive(Debug)]
pub struct AssistantRuntime {
    engine: &'static ModelEngine,
    models: BTreeMap<String, LLMConfig>,
    primary: String,
    pipeline: Arc<Pipeline>,
    system: String,
    limits: RunLimits,
    queue_capacity: usize,
    event_capacity: usize,
    max_contexts: usize,
    shutdown_grace: Duration,
}
impl AssistantRuntime {
    /// Begin configuration using the explicitly initialized singleton.
    #[must_use]
    pub fn builder(engine: &'static ModelEngine) -> RuntimeBuilder {
        RuntimeBuilder {
            engine,
            models: Vec::new(),
            registrar: PluginRegistrar::default(),
            error: None,
            system: "You are a concise, helpful assistant.".into(),
            limits: RunLimits::default(),
            queue_capacity: 16,
            event_capacity: 32,
            max_contexts: 4,
            shutdown_grace: Duration::from_secs(10),
        }
    }
    /// Inspect named extension order before starting native work.
    #[must_use]
    pub fn pipeline(&self) -> &Pipeline {
        &self.pipeline
    }
    /// Start services and resident model workers. Requires a Tokio runtime.
    /// # Errors
    /// Returns startup failures after stopping previously started components.
    pub async fn start(self) -> ChatResult<RunningAssistant> {
        tokio::runtime::Handle::try_current()
            .map_err(|_| ChatError::Configuration("start requires a Tokio runtime".into()))?;
        let control = RuntimeControl::new(self.queue_capacity);
        let mut started = 0;
        for service in &self.pipeline.lifecycle {
            let result = tokio::time::timeout(self.shutdown_grace, service.start())
                .await
                .unwrap_or(Err(ChatError::Deadline));
            if let Err(source) = result {
                let cleanup = stop_services(&self.pipeline, started, self.shutdown_grace).await;
                return Err(ChatError::Startup {
                    source: Box::new(source),
                    cleanup,
                });
            }
            started += 1;
        }
        let mut workers = BTreeMap::new();
        let mut threads = Vec::new();
        for (name, config) in self.models {
            match WorkerHandle::spawn(
                self.engine,
                config,
                &name,
                self.queue_capacity,
                self.event_capacity,
                self.max_contexts,
                control.clone(),
            )
            .await
            {
                Ok((worker, thread)) => {
                    workers.insert(name, Arc::new(worker));
                    threads.push(thread);
                }
                Err(error) => {
                    control.close();
                    for worker in workers.values() {
                        worker.stop();
                    }
                    let mut cleanup = join_workers(&mut threads).await;
                    cleanup
                        .extend(stop_services(&self.pipeline, started, self.shutdown_grace).await);
                    return Err(ChatError::Startup {
                        source: Box::new(error),
                        cleanup,
                    });
                }
            }
        }
        let backend: Arc<dyn ModelBackend> = workers
            .get(&self.primary)
            .cloned()
            .ok_or(ChatError::Closed)?;
        let chat = Chat::new(
            control.next_id(),
            self.system.clone(),
            backend,
            self.pipeline.clone(),
            self.limits.clone(),
            control.clone(),
        );
        Ok(RunningAssistant {
            workers,
            threads,
            chat,
            control,
            pipeline: self.pipeline,
            system: self.system,
            limits: self.limits,
            shutdown_grace: self.shutdown_grace,
        })
    }
}

/// Persistent runtime. A completed response does not stop its services or models.
/// Explicit [`Self::shutdown`] is required to await service flushing and joins.
#[derive(Debug)]
pub struct RunningAssistant {
    workers: BTreeMap<String, Arc<WorkerHandle>>,
    threads: Vec<std::thread::JoinHandle<()>>,
    chat: Chat,
    control: Arc<RuntimeControl>,
    pipeline: Arc<Pipeline>,
    system: String,
    limits: RunLimits,
    shutdown_grace: Duration,
}
impl RunningAssistant {
    /// Inspect resident contexts on their owning workers. No context is created
    /// by this call. The aggregate counts shared weights once per worker.
    /// # Errors
    /// Returns closed-worker, inspection or accounting errors.
    pub async fn cache_diagnostics(&self) -> ChatResult<BTreeMap<String, ModelCacheDiagnostics>> {
        self.control.check_open()?;
        let mut reports = BTreeMap::new();
        for (name, worker) in &self.workers {
            let contexts = worker.inspect().await?;
            let memory = crate::engine::CacheDiagnostics::aggregate_memory(&contexts)?;
            reports.insert(name.clone(), ModelCacheDiagnostics { contexts, memory });
        }
        Ok(reports)
    }
    /// Clone the primary conversation handle. All clones share working state.
    #[must_use]
    pub fn chat(&self) -> Chat {
        self.chat.clone()
    }
    /// Create independent bounded working state on a resident model.
    /// # Errors
    /// Rejects unknown models and closed runtimes. Native context limits are
    /// checked when the first request allocates its context.
    pub fn create_chat(&self, model: &str) -> ChatResult<Chat> {
        self.control.check_open()?;
        let backend: Arc<dyn ModelBackend> = self
            .workers
            .get(model)
            .cloned()
            .ok_or_else(|| ChatError::Configuration(format!("unknown model: {model}")))?;
        Ok(Chat::new(
            self.control.next_id(),
            self.system.clone(),
            backend,
            self.pipeline.clone(),
            self.limits.clone(),
            self.control.clone(),
        ))
    }
    /// Ordered extension diagnostics, with no prompt contents.
    #[must_use]
    pub fn pipeline(&self) -> &Pipeline {
        &self.pipeline
    }
    /// Cancel admitted work, join native owners, then flush services in reverse order.
    /// # Errors
    /// Reports grace expiration, worker panic, or service failure after cleanup.
    pub async fn shutdown(mut self) -> ChatResult<()> {
        let mut failures = Vec::new();
        if let Err(error) = self.control.cancel_and_wait(self.shutdown_grace).await {
            failures.push(error.to_string());
        }
        for worker in self.workers.values() {
            worker.stop();
        }
        failures.extend(join_workers(&mut self.threads).await);
        failures.extend(
            stop_services(
                &self.pipeline,
                self.pipeline.lifecycle.len(),
                self.shutdown_grace,
            )
            .await,
        );
        if failures.is_empty() {
            Ok(())
        } else {
            Err(ChatError::Shutdown(failures))
        }
    }
}
impl Drop for RunningAssistant {
    fn drop(&mut self) {
        self.control.close();
        for worker in self.workers.values() {
            worker.stop();
        }
    }
}

async fn join_workers(threads: &mut Vec<std::thread::JoinHandle<()>>) -> Vec<String> {
    let mut failures = Vec::new();
    for thread in threads.drain(..) {
        match tokio::task::spawn_blocking(move || thread.join()).await {
            Ok(Ok(())) => {}
            _ => failures.push("native worker panicked".into()),
        }
    }
    failures
}
async fn stop_services(pipeline: &Pipeline, started: usize, grace: Duration) -> Vec<String> {
    let mut failures = Vec::new();
    for service in pipeline.lifecycle[..started].iter().rev() {
        match tokio::time::timeout(grace, service.shutdown()).await {
            Ok(Ok(())) => {}
            Ok(Err(error)) => failures.push(error.to_string()),
            Err(_) => failures.push("service shutdown deadline exceeded".into()),
        }
    }
    failures
}

#[cfg(test)]
mod tests;
