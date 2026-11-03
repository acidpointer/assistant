//! Build-time plugins and request-time composition. Named hooks are sorted once
//! into a deterministic plan. Native and tool guards are terminal operations,
//! inside middleware, so modifications are checked immediately before execution.
//! Shared services are immutable handles; request state lives in `RunContext`.
use crate::{
    chat::{
        AssistantMessage, ChatError, ChatResponse, ChatResult, ModelRequest, ModelTurn, RunContext,
        ToolCall, ToolReceipt,
    },
    tools::{Tool, ToolRegistry},
};
use std::{
    any::{Any, TypeId, type_name},
    collections::{BTreeMap, BTreeSet, HashMap},
    fmt,
    future::Future,
    pin::Pin,
    sync::Arc,
};

/// An asynchronous extension operation borrowing its invocation state.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;
/// A trusted package of capabilities installed before startup.
pub trait Plugin: fmt::Debug + Send + Sync {
    /// Stable registration identity; duplicate instances fail at build time.
    fn name(&self) -> &str {
        type_name::<Self>()
    }
    /// Install tools, services, hooks, and middleware without starting workers.
    /// # Errors
    /// Return an actionable configuration failure.
    fn configure(&self, registrar: &mut PluginRegistrar) -> ChatResult<()>;
}
/// Lifecycle for a long-lived service. Implementations own their background
/// tasks and must stop them during shutdown. Startup is separate from registration.
pub trait Service: fmt::Debug + Send + Sync {
    /// Start owned resources. A failing service must clean up its partial start.
    fn start(&self) -> BoxFuture<'_, ChatResult<()>> {
        Box::pin(async { Ok(()) })
    }
    /// Flush/stop owned work; called in reverse successful-start order.
    fn shutdown(&self) -> BoxFuture<'_, ChatResult<()>> {
        Box::pin(async { Ok(()) })
    }
}
/// Shared service handles. The map is frozen before runs begin.
#[derive(Clone, Default)]
pub struct Services {
    values: HashMap<TypeId, Arc<dyn Any + Send + Sync>>,
}
impl fmt::Debug for Services {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Services")
            .field("count", &self.values.len())
            .finish()
    }
}
impl Services {
    /// Borrow a registered typed handle without holding a global mutable lock.
    /// # Errors
    /// Reports a missing dependency by its type name.
    pub fn get<T: Send + Sync + 'static>(&self) -> ChatResult<Arc<T>> {
        self.values
            .get(&TypeId::of::<T>())
            .cloned()
            .and_then(|v| v.downcast().ok())
            .ok_or_else(|| {
                ChatError::Configuration(format!("missing service: {}", type_name::<T>()))
            })
    }
}

/// Stages with fixed global ordering and configurable ordering inside each stage.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    /// Once before the interaction loop.
    BeforeRun,
    /// Before every model turn.
    BeforeModel,
    /// After a parsed model turn.
    AfterModel,
    /// One terminal notification, including errors and cancellation.
    RunFinished,
}
type RunHook =
    Arc<dyn for<'a> Fn(&'a mut RunContext) -> BoxFuture<'a, ChatResult<()>> + Send + Sync>;
type ModelHook = Arc<
    dyn for<'a> Fn(&'a mut RunContext, &'a mut ModelRequest) -> BoxFuture<'a, ChatResult<()>>
        + Send
        + Sync,
>;
type AfterHook = Arc<
    dyn for<'a> Fn(&'a mut RunContext, &'a mut AssistantMessage) -> BoxFuture<'a, ChatResult<()>>
        + Send
        + Sync,
>;
type FinishHook = Arc<
    dyn for<'a> Fn(&'a ChatResponse, &'a Services) -> BoxFuture<'a, ChatResult<()>> + Send + Sync,
>;
#[derive(Clone)]
enum Handler {
    Run(RunHook),
    Model(ModelHook),
    After(AfterHook),
    Finished(FinishHook),
}
impl fmt::Debug for Handler {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Hook")
    }
}
/// Ordering declaration returned by hook registration.
#[derive(Debug, Clone)]
pub struct HookRegistration {
    id: String,
    stage: Stage,
    after: Vec<String>,
    before: Vec<String>,
    handler: Handler,
}
impl HookRegistration {
    /// Run after a hook in the same stage; missing names are build errors.
    pub fn after(&mut self, id: impl Into<String>) -> &mut Self {
        self.after.push(id.into());
        self
    }
    /// Run before a hook in the same stage; cycles are build errors.
    pub fn before(&mut self, id: impl Into<String>) -> &mut Self {
        self.before.push(id.into());
        self
    }
}

/// Middleware around a complete user interaction.
pub trait RunLayer: fmt::Debug + Send + Sync {
    /// Call the owned continuation at most once, or return an explicit response.
    fn call<'a>(
        &'a self,
        context: &'a mut RunContext,
        next: NextRun<'a>,
    ) -> BoxFuture<'a, ChatResult<ChatResponse>>;
}
/// Middleware around one native model turn.
pub trait ModelLayer: fmt::Debug + Send + Sync {
    /// Changes are validated by the terminal model boundary.
    fn call<'a>(
        &'a self,
        context: &'a mut RunContext,
        request: ModelRequest,
        next: NextModel<'a>,
    ) -> BoxFuture<'a, ChatResult<ModelTurn>>;
}
/// Middleware around one host tool invocation.
pub trait ToolLayer: fmt::Debug + Send + Sync {
    /// Changes are checked against the immutable selected-tool set at dispatch.
    fn call<'a>(
        &'a self,
        context: &'a mut RunContext,
        call: ToolCall,
        next: NextTool<'a>,
    ) -> BoxFuture<'a, ChatResult<ToolReceipt>>;
}
/// One-shot continuation for run middleware; consuming it prevents double calls.
pub struct NextRun<'a> {
    pub(crate) layers: &'a [Arc<dyn RunLayer>],
    pub(crate) pipeline: &'a Pipeline,
}
impl fmt::Debug for NextRun<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NextRun")
            .field("remaining", &self.layers.len())
            .finish()
    }
}
impl<'a> NextRun<'a> {
    /// Continue through remaining layers and the core bounded runner.
    pub fn run<'b>(self, context: &'b mut RunContext) -> BoxFuture<'b, ChatResult<ChatResponse>>
    where
        'a: 'b,
    {
        Box::pin(async move {
            if let Some((layer, rest)) = self.layers.split_first() {
                layer
                    .call(
                        context,
                        NextRun {
                            layers: rest,
                            pipeline: self.pipeline,
                        },
                    )
                    .await
            } else {
                crate::chat::run_loop(context, self.pipeline).await
            }
        })
    }
}
/// One-shot continuation for model middleware.
pub struct NextModel<'a> {
    pub(crate) layers: &'a [Arc<dyn ModelLayer>],
}
impl fmt::Debug for NextModel<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NextModel")
            .field("remaining", &self.layers.len())
            .finish()
    }
}
impl<'a> NextModel<'a> {
    /// Continue; the final boundary enforces limits before worker submission.
    pub fn run<'b>(
        self,
        context: &'b mut RunContext,
        request: ModelRequest,
    ) -> BoxFuture<'b, ChatResult<ModelTurn>>
    where
        'a: 'b,
    {
        Box::pin(async move {
            if let Some((layer, rest)) = self.layers.split_first() {
                layer
                    .call(context, request, NextModel { layers: rest })
                    .await
            } else {
                context.evaluate(request).await
            }
        })
    }
}
/// One-shot continuation for tool middleware.
pub struct NextTool<'a> {
    pub(crate) layers: &'a [Arc<dyn ToolLayer>],
    pub(crate) allowed: &'a [String],
    pub(crate) expected_id: &'a str,
}
impl fmt::Debug for NextTool<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NextTool")
            .field("remaining", &self.layers.len())
            .finish()
    }
}
impl<'a> NextTool<'a> {
    /// Continue through immutable selection checks and registered dispatch.
    pub fn run<'b>(
        self,
        context: &'b mut RunContext,
        call: ToolCall,
    ) -> BoxFuture<'b, ChatResult<ToolReceipt>>
    where
        'a: 'b,
    {
        Box::pin(async move {
            if let Some((layer, rest)) = self.layers.split_first() {
                layer
                    .call(
                        context,
                        call,
                        NextTool {
                            layers: rest,
                            allowed: self.allowed,
                            expected_id: self.expected_id,
                        },
                    )
                    .await
            } else {
                context
                    .execute_tool(call, self.allowed, self.expected_id)
                    .await
            }
        })
    }
}

/// Builder-only registration. Runtime state never lives inside this registrar.
#[derive(Debug, Default)]
pub struct PluginRegistrar {
    pub(crate) tools: ToolRegistry,
    services: Services,
    pub(crate) lifecycle: Vec<Arc<dyn Service>>,
    requirements: Vec<(TypeId, &'static str)>,
    plugins: BTreeSet<String>,
    hooks: Vec<HookRegistration>,
    run_layers: Vec<Arc<dyn RunLayer>>,
    model_layers: Vec<Arc<dyn ModelLayer>>,
    tool_layers: Vec<Arc<dyn ToolLayer>>,
}
impl PluginRegistrar {
    /// Install one plugin, including nested plugin dependencies.
    /// # Errors
    /// Rejects duplicates and propagates registration failures.
    pub fn plugin<P: Plugin>(&mut self, plugin: P) -> ChatResult<()> {
        if !self.plugins.insert(plugin.name().into()) {
            return Err(ChatError::Configuration(format!(
                "duplicate plugin: {}",
                plugin.name()
            )));
        }
        plugin.configure(self)
    }
    /// Register a typed capability.
    /// # Errors
    /// Rejects duplicate/invalid tool definitions.
    pub fn add_tool<T: Tool>(&mut self, tool: T) -> ChatResult<()> {
        self.tools.register(tool)
    }
    /// Supply a shared service handle. No I/O starts here.
    /// # Errors
    /// Rejects duplicate providers of the same concrete type.
    pub fn provide<T: Send + Sync + 'static>(&mut self, value: Arc<T>) -> ChatResult<()> {
        if self.services.values.contains_key(&TypeId::of::<T>()) {
            return Err(ChatError::Configuration(format!(
                "duplicate service: {}",
                type_name::<T>()
            )));
        }
        self.services.values.insert(TypeId::of::<T>(), value);
        Ok(())
    }
    /// Register ordered runtime startup/shutdown in addition to a typed handle.
    pub fn lifecycle<S: Service + 'static>(&mut self, service: Arc<S>) {
        self.lifecycle.push(service);
    }
    /// Declare a dependency resolved after all plugins have configured.
    pub fn require<T: Send + Sync + 'static>(&mut self) {
        self.requirements
            .push((TypeId::of::<T>(), type_name::<T>()));
    }
    /// Register an async hook before the interaction loop.
    pub fn before_run<F>(&mut self, id: impl Into<String>, hook: F) -> &mut HookRegistration
    where
        F: for<'a> Fn(&'a mut RunContext) -> BoxFuture<'a, ChatResult<()>> + Send + Sync + 'static,
    {
        self.add_hook(id.into(), Stage::BeforeRun, Handler::Run(Arc::new(hook)))
    }
    /// Register an async model-input transformation.
    pub fn before_model<F>(&mut self, id: impl Into<String>, hook: F) -> &mut HookRegistration
    where
        F: for<'a> Fn(&'a mut RunContext, &'a mut ModelRequest) -> BoxFuture<'a, ChatResult<()>>
            + Send
            + Sync
            + 'static,
    {
        self.add_hook(
            id.into(),
            Stage::BeforeModel,
            Handler::Model(Arc::new(hook)),
        )
    }
    /// Register an async transformation of fully parsed model output.
    pub fn after_model<F>(&mut self, id: impl Into<String>, hook: F) -> &mut HookRegistration
    where
        F: for<'a> Fn(
                &'a mut RunContext,
                &'a mut AssistantMessage,
            ) -> BoxFuture<'a, ChatResult<()>>
            + Send
            + Sync
            + 'static,
    {
        self.add_hook(id.into(), Stage::AfterModel, Handler::After(Arc::new(hook)))
    }
    /// Register a terminal observer. Its errors become response warnings, since
    /// completed side effects cannot be replayed to retry an observer.
    pub fn on_run_finished<F>(&mut self, id: impl Into<String>, hook: F) -> &mut HookRegistration
    where
        F: for<'a> Fn(&'a ChatResponse, &'a Services) -> BoxFuture<'a, ChatResult<()>>
            + Send
            + Sync
            + 'static,
    {
        self.add_hook(
            id.into(),
            Stage::RunFinished,
            Handler::Finished(Arc::new(hook)),
        )
    }
    fn add_hook(&mut self, id: String, stage: Stage, handler: Handler) -> &mut HookRegistration {
        self.hooks.push(HookRegistration {
            id,
            stage,
            after: Vec::new(),
            before: Vec::new(),
            handler,
        });
        let index = self.hooks.len() - 1;
        &mut self.hooks[index]
    }
    /// Wrap complete interactions in registration order.
    pub fn layer_run<L: RunLayer + 'static>(&mut self, layer: L) {
        self.run_layers.push(Arc::new(layer));
    }
    /// Wrap model evaluations in registration order.
    pub fn layer_model<L: ModelLayer + 'static>(&mut self, layer: L) {
        self.model_layers.push(Arc::new(layer));
    }
    /// Wrap tool invocations in registration order.
    pub fn layer_tool<L: ToolLayer + 'static>(&mut self, layer: L) {
        self.tool_layers.push(Arc::new(layer));
    }
    pub(crate) fn build(self) -> ChatResult<Pipeline> {
        for (id, name) in &self.requirements {
            if !self.services.values.contains_key(id) {
                return Err(ChatError::Configuration(format!(
                    "missing required service: {name}"
                )));
            }
        }
        let hooks = order_hooks(&self.hooks)?;
        Ok(Pipeline {
            tools: self.tools,
            services: Arc::new(self.services),
            lifecycle: self.lifecycle,
            hooks,
            run_layers: self.run_layers,
            model_layers: self.model_layers,
            tool_layers: self.tool_layers,
        })
    }
}

/// Frozen execution plan. It contains no native context or mutable global state.
#[derive(Debug)]
pub struct Pipeline {
    pub(crate) tools: ToolRegistry,
    pub(crate) services: Arc<Services>,
    pub(crate) lifecycle: Vec<Arc<dyn Service>>,
    hooks: Vec<HookRegistration>,
    pub(crate) run_layers: Vec<Arc<dyn RunLayer>>,
    pub(crate) model_layers: Vec<Arc<dyn ModelLayer>>,
    pub(crate) tool_layers: Vec<Arc<dyn ToolLayer>>,
}
impl Pipeline {
    /// Ordered extension names for diagnostics, without exposing request data.
    #[must_use]
    pub fn stages(&self) -> Vec<(Stage, &str)> {
        self.hooks
            .iter()
            .map(|hook| (hook.stage, hook.id.as_str()))
            .collect()
    }
    pub(crate) async fn before_run(&self, context: &mut RunContext) -> ChatResult<()> {
        for hook in &self.hooks {
            if let Handler::Run(call) = &hook.handler {
                call(context).await.map_err(|e| hook_error(hook, &e))?;
            }
        }
        Ok(())
    }
    pub(crate) async fn before_model(
        &self,
        context: &mut RunContext,
        request: &mut ModelRequest,
    ) -> ChatResult<()> {
        for hook in &self.hooks {
            if let Handler::Model(call) = &hook.handler {
                call(context, request)
                    .await
                    .map_err(|e| hook_error(hook, &e))?;
            }
        }
        Ok(())
    }
    pub(crate) async fn after_model(
        &self,
        context: &mut RunContext,
        message: &mut AssistantMessage,
    ) -> ChatResult<()> {
        for hook in &self.hooks {
            if let Handler::After(call) = &hook.handler {
                call(context, message)
                    .await
                    .map_err(|e| hook_error(hook, &e))?;
            }
        }
        Ok(())
    }
    pub(crate) async fn finished(&self, response: &mut ChatResponse) {
        for hook in &self.hooks {
            if let Handler::Finished(call) = &hook.handler {
                match tokio::time::timeout(
                    std::time::Duration::from_secs(1),
                    call(response, &self.services),
                )
                .await
                {
                    Ok(Ok(())) => {}
                    Ok(Err(error)) => response.warnings.push(format!("{}: {error}", hook.id)),
                    Err(_) => response.warnings.push(format!(
                        "{}: terminal observer exceeded one second",
                        hook.id
                    )),
                }
            }
        }
    }
}
fn hook_error(hook: &HookRegistration, error: &ChatError) -> ChatError {
    ChatError::Extension {
        name: hook.id.clone(),
        message: error.to_string(),
    }
}
fn order_hooks(hooks: &[HookRegistration]) -> ChatResult<Vec<HookRegistration>> {
    let mut names = BTreeMap::new();
    for (index, hook) in hooks.iter().enumerate() {
        if hook.id.is_empty() || names.insert(hook.id.clone(), index).is_some() {
            return Err(ChatError::Configuration(format!(
                "duplicate/empty hook: {}",
                hook.id
            )));
        }
    }
    let mut edges = vec![BTreeSet::new(); hooks.len()];
    let mut indegree = vec![0usize; hooks.len()];
    for (index, hook) in hooks.iter().enumerate() {
        for (name, before) in hook
            .after
            .iter()
            .map(|id| (id, false))
            .chain(hook.before.iter().map(|id| (id, true)))
        {
            let target = *names
                .get(name)
                .ok_or_else(|| ChatError::Configuration(format!("missing hook: {name}")))?;
            if hooks[target].stage != hook.stage {
                return Err(ChatError::Configuration(
                    "ordering constraints must stay within one stage".into(),
                ));
            }
            let (from, to) = if before {
                (index, target)
            } else {
                (target, index)
            };
            if edges[from].insert(to) {
                indegree[to] += 1;
            }
        }
    }
    let mut result = Vec::with_capacity(hooks.len());
    let mut done = vec![false; hooks.len()];
    for stage in [
        Stage::BeforeRun,
        Stage::BeforeModel,
        Stage::AfterModel,
        Stage::RunFinished,
    ] {
        loop {
            let ready = hooks
                .iter()
                .enumerate()
                .find(|(i, h)| h.stage == stage && !done[*i] && indegree[*i] == 0)
                .map(|(i, _)| i);
            let Some(index) = ready else { break };
            done[index] = true;
            result.push(hooks[index].clone());
            for &next in &edges[index] {
                indegree[next] -= 1;
            }
        }
    }
    if result.len() != hooks.len() {
        return Err(ChatError::Configuration("hook dependency cycle".into()));
    }
    Ok(result)
}

#[cfg(test)]
mod tests;
