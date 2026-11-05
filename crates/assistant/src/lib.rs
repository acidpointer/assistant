//! Local assistant foundation: native inference, typed tools, and an event-driven runtime.
//!
//! Native contexts stay on their owning worker. Public runtime handles exchange
//! owned messages; they never expose FFI pointers. Durable memory and transports
//! have explicit ownership; required memory participates in product admission
//! and settlement outside optional middleware.

pub mod application;
pub mod chat;
pub mod engine;
#[cfg(target_os = "linux")]
pub mod memory;
pub mod pipeline;
pub mod runtime;
pub mod telemetry;
pub mod tools;

pub use chat::{Chat, ChatResponse, Message, RequestOptions, RunEvent};
pub use pipeline::{Plugin, PluginRegistrar};
pub use runtime::{AssistantRuntime, RuntimeBuilder};
