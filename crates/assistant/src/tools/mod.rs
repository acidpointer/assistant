//! Typed host capabilities. Definitions are derived from argument types; actual
//! dispatch validates selection and deserializes again. Tools are trusted Rust
//! implementations, never model-generated executables. Async tools must cooperate
//! with cancellation; a timeout cannot undo an external side effect.
use crate::{
    chat::{ChatError, ChatResult, ToolCall, ToolReceipt},
    engine::Cancellation,
    pipeline::BoxFuture,
};
use schemars::JsonSchema;
use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use std::{collections::BTreeMap, error::Error, fmt::Debug, future::Future, sync::Arc};

pub mod builtin;

/// Invocation metadata supplied by the host, not by model arguments.
#[derive(Debug, Clone)]
pub struct ToolContext {
    /// Correlation ID for audit and application-specific idempotency.
    pub invocation_id: String,
    /// Shared run cancellation.
    pub cancellation: Cancellation,
}
/// A typed asynchronous host function. Argument structs should reject unknown
/// fields and validate domain-specific constraints before performing side effects.
pub trait Tool: Debug + Send + Sync + 'static {
    /// Stable unique name, optionally namespaced with dots.
    const NAME: &'static str;
    /// Arguments accepted by the handler and described to the model.
    type Args: DeserializeOwned + JsonSchema + Send;
    /// Structured successful result.
    type Output: Serialize + Send;
    /// Domain failure presented to the model as a failed tool result.
    type Error: Error + Send + Sync + 'static;
    /// Explain when the model should use this capability.
    fn description(&self) -> &str;
    /// Host-declared effect and durable capture. Conservative defaults retain no
    /// arguments/results and assume interruption may leave an external effect.
    #[cfg(target_os = "linux")]
    fn memory_policy(&self) -> (crate::memory::ToolEffect, ToolRetention) {
        (
            crate::memory::ToolEffect::External,
            ToolRetention::Ephemeral,
        )
    }

    /// Execute validated Rust arguments. Never interpret model text as code.
    fn call(
        &self,
        context: ToolContext,
        args: Self::Args,
    ) -> impl Future<Output = Result<Self::Output, Self::Error>> + Send;
}
/// Durable capture policy selected by the trusted registered handler.
#[cfg(target_os = "linux")]
#[derive(Debug, Clone, Copy)]
pub enum ToolRetention {
    /// Keep execution identity/outcome only.
    Ephemeral,
    /// Keep bounded arguments/results as attributed evidence.
    Retained,
}
/// Serializable schema exposed to the chat template.
#[derive(Debug, Clone)]
pub struct ToolSpec {
    /// Registered tool name.
    pub name: String,
    /// Human-authored description.
    pub description: String,
    /// JSON Schema derived from the accepted argument type.
    pub parameters: Value,
}
impl ToolSpec {
    pub(crate) fn value(&self) -> Value {
        json!({"type":"function","function":{"name":self.name,"description":self.description,"parameters":self.parameters}})
    }
}
trait ErasedTool: Debug + Send + Sync {
    fn validate(&self, args: &Value) -> ChatResult<()>;
    fn execute(&self, context: ToolContext, args: Value) -> BoxFuture<'_, Value>;
}
#[derive(Debug)]
struct Adapter<T>(T);
impl<T: Tool> ErasedTool for Adapter<T> {
    fn validate(&self, args: &Value) -> ChatResult<()> {
        serde_json::from_value::<T::Args>(args.clone())
            .map(|_| ())
            .map_err(|_| ChatError::Protocol("invalid tool arguments".into()))
    }

    fn execute(&self, context: ToolContext, args: Value) -> BoxFuture<'_, Value> {
        Box::pin(async move {
            let args = match serde_json::from_value::<T::Args>(args) {
                Ok(args) => args,
                Err(error) => return failure("invalid_arguments", error.to_string()),
            };
            match self.0.call(context, args).await {
                Ok(output) => match serde_json::to_value(output) {
                    Ok(value) => json!({"ok":true,"result":value}),
                    Err(error) => failure("invalid_result", error.to_string()),
                },
                Err(error) => failure("tool_failed", error.to_string()),
            }
        })
    }
}
pub(crate) fn failure(kind: &str, message: impl Into<String>) -> Value {
    json!({"ok":false,"error":{"kind":kind,"message":message.into()}})
}
/// Cheaply cloneable registry. Clones share handlers; selected schemas remain
/// per request. Mutation happens only during configuration.
#[derive(Debug, Clone, Default)]
pub struct ToolRegistry {
    tools: BTreeMap<String, Registered>,
}
#[derive(Debug, Clone)]
struct Registered {
    #[cfg(target_os = "linux")]
    policy: (crate::memory::ToolEffect, ToolRetention),
    spec: ToolSpec,
    handler: Arc<dyn ErasedTool>,
}
impl ToolRegistry {
    /// Construct an empty capability registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
    /// Register a typed handler and its argument schema.
    /// # Errors
    /// Rejects duplicate/invalid names, excessive schema size, or registry size.
    pub fn register<T: Tool>(&mut self, tool: T) -> ChatResult<()> {
        let name = T::NAME;
        if name.is_empty()
            || name.len() > 64
            || !name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"_.-".contains(&b))
        {
            return Err(ChatError::Configuration("invalid tool name".into()));
        }
        if self.tools.contains_key(name) {
            return Err(ChatError::Configuration(format!("duplicate tool: {name}")));
        }
        if self.tools.len() >= 1024 {
            return Err(ChatError::Limit("registered tools"));
        }
        let adapter = Adapter(tool);
        let spec = ToolSpec {
            name: T::NAME.into(),
            description: adapter.0.description().into(),
            parameters: schemars::schema_for!(T::Args).to_value(),
        };
        if serde_json::to_vec(&spec.value())?.len() > 32_768 {
            return Err(ChatError::Limit("tool schema bytes"));
        }
        self.tools.insert(
            name.into(),
            Registered {
                spec,
                #[cfg(target_os = "linux")]
                policy: adapter.0.memory_policy(),
                handler: Arc::new(adapter),
            },
        );
        Ok(())
    }
    /// List definitions without exposing handler internals.
    #[must_use]
    pub fn definitions(&self) -> Vec<ToolSpec> {
        self.tools.values().map(|tool| tool.spec.clone()).collect()
    }
    /// Select registered definitions by name. An empty selection disables tools.
    /// # Errors
    /// Unknown names are configuration errors, not silently ignored.
    pub fn select(&self, names: &[String]) -> ChatResult<Vec<ToolSpec>> {
        if names.len() > 1024 {
            return Err(ChatError::Limit("selected tools"));
        }
        let unique: std::collections::BTreeSet<_> = names.iter().collect();
        if unique.len() != names.len() {
            return Err(ChatError::Configuration("duplicate selected tool".into()));
        }
        names
            .iter()
            .map(|name| {
                self.tools
                    .get(name)
                    .map(|tool| tool.spec.clone())
                    .ok_or_else(|| {
                        ChatError::Configuration(format!("unknown selected tool: {name}"))
                    })
            })
            .collect()
    }
    #[cfg(target_os = "linux")]
    pub(crate) fn durable_dispatch(
        &self,
        call: &ToolCall,
        allowed: &[String],
    ) -> ChatResult<(crate::memory::ToolEffect, ToolRetention)> {
        if !allowed.contains(&call.name) {
            return Err(ChatError::Protocol("tool is not selected".into()));
        }
        let tool = self
            .tools
            .get(&call.name)
            .ok_or_else(|| ChatError::Protocol("unknown tool".into()))?;
        tool.handler.validate(&call.arguments)?;
        Ok(tool.policy)
    }
    pub(crate) async fn execute(
        &self,
        call: ToolCall,
        allowed: &[String],
        context: ToolContext,
    ) -> ToolReceipt {
        let result = if context.cancellation.is_cancelled() {
            failure("cancelled", "execution did not start")
        } else if !allowed.contains(&call.name) {
            failure("not_allowed", "tool is not selected for this model turn")
        } else if let Some(tool) = self.tools.get(&call.name) {
            tool.handler.execute(context, call.arguments.clone()).await
        } else {
            failure("unknown_tool", "no registered handler")
        };
        ToolReceipt { call, result }
    }
}

#[cfg(test)]
mod tests;
