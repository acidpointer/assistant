//! Runtime-ready extension lifecycle. Factories prepare resources before model
//! loading; activation receives a bounded application handle. The runtime owns
//! running services outside the request pipeline, so transports cannot create a
//! pipeline/chat ownership cycle. All operations run on Tokio and report typed
//! errors; implementations must clean up partial preparation or activation.

use crate::{
    application::AssistantHandle,
    chat::ChatResult,
    engine::Cancellation,
    pipeline::{BoxFuture, Services},
};
use std::{fmt, sync::Arc};

/// Dependencies available only after the assistant is ready.
#[derive(Debug, Clone)]
pub struct RuntimeContext {
    /// Shared admission, conversation state, cancellation and subscriptions.
    pub assistant: AssistantHandle,
    /// Frozen plugin-provided dependencies.
    pub services: Arc<Services>,
    /// Read-only notification that runtime cleanup has been requested.
    pub shutdown: ShutdownSignal,
}

/// Read-only stop notification; runtime services cannot request global shutdown
/// through this handle. Each service still owns and joins its background tasks.
#[derive(Debug, Clone)]
pub struct ShutdownSignal(pub(crate) Cancellation);
impl ShutdownSignal {
    /// Whether runtime cleanup has been requested.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.0.is_cancelled()
    }
    /// Wait without polling for runtime cleanup to begin.
    pub async fn cancelled(&self) {
        self.0.cancelled().await;
    }
}

/// A registered factory. Registration itself must not acquire resources.
pub trait RuntimeServiceFactory: fmt::Debug + Send + Sync {
    /// Stable identity, unique among runtime services.
    fn name(&self) -> &str;
    /// Reserve resources without accepting application work.
    /// # Errors
    /// Must release partial resources before returning an error or being cancelled.
    fn prepare(&self) -> BoxFuture<'_, ChatResult<Box<dyn PreparedService>>>;
}

/// Resources reserved for activation. Dropping this handle releases them.
pub trait PreparedService: fmt::Debug + Send {
    /// Activate after models and application state exist.
    /// # Errors
    /// Must stop any partially started tasks before returning an error.
    fn activate(
        self: Box<Self>,
        context: RuntimeContext,
    ) -> BoxFuture<'static, ChatResult<Box<dyn RunningService>>>;
}

/// Owned background execution. Drop must request stop without blocking.
pub trait RunningService: fmt::Debug + Send + Sync {
    /// Stop accepting new application work; final result delivery may continue.
    /// # Errors
    /// Reports failure to quiesce; runtime still attempts remaining cleanup.
    fn quiesce(&self) -> BoxFuture<'_, ChatResult<()>>;
    /// Wait for unexpected termination. Normal operation leaves this pending.
    /// # Errors
    /// Returns the terminal failure, or success for an unexpected clean exit.
    fn terminated(&self) -> BoxFuture<'_, ChatResult<()>>;
    /// Close streams and join all owned background tasks.
    /// # Errors
    /// Reports task or resource cleanup failures.
    fn shutdown(&mut self) -> BoxFuture<'_, ChatResult<()>>;
}
