//! Opt-in capabilities used by the daemon and inference smoke tests. External
//! commands are fixed by host code; process tools read bounded OS metadata.
//! The model supplies validated arguments, never executable code or shell text.
use super::{Tool, ToolContext};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};
use std::io;

pub mod process;
pub use process::{ProcessDetails, ProcessTop};

/// Empty arguments for the current-time capability.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TimeArgs {}
/// Read local date/time using the host's fixed `date` executable.
#[derive(Debug)]
pub struct CurrentTime;
impl Tool for CurrentTime {
    #[cfg(target_os = "linux")]
    fn memory_policy(&self) -> (crate::memory::ToolEffect, super::ToolRetention) {
        (
            crate::memory::ToolEffect::ReadOnly,
            super::ToolRetention::Retained,
        )
    }
    const NAME: &'static str = "current_time";
    type Args = TimeArgs;
    type Output = Value;
    type Error = io::Error;
    fn description(&self) -> &'static str {
        "Get actual current local date, time and timezone. Always use this for current time/date questions. Never guess."
    }
    async fn call(&self, context: ToolContext, _args: TimeArgs) -> io::Result<Value> {
        if context.cancellation.is_cancelled() {
            return Err(io::Error::other("cancelled"));
        }
        let output = tokio::process::Command::new("date")
            .kill_on_drop(true)
            .output()
            .await?;
        if !output.status.success() {
            return Err(io::Error::other("date command failed"));
        }
        let text = String::from_utf8(output.stdout).map_err(io::Error::other)?;
        Ok(json!({"local_time":text.trim_end()}))
    }
}
/// Supported checked integer arithmetic.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Operation {
    /// Sum.
    Add,
    /// Difference.
    Subtract,
    /// Product.
    Multiply,
    /// Integer quotient.
    Divide,
}
/// Strict calculator arguments. Arithmetic errors are checked by the handler.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CalculateArgs {
    /// Arithmetic operation to perform.
    pub operation: Operation,
    /// First integer operand.
    pub a: i64,
    /// Second integer operand.
    pub b: i64,
}
/// Checked integer calculator; no expression parser or executable input.
#[derive(Debug)]
pub struct Calculator;
impl Tool for Calculator {
    #[cfg(target_os = "linux")]
    fn memory_policy(&self) -> (crate::memory::ToolEffect, super::ToolRetention) {
        (
            crate::memory::ToolEffect::ReadOnly,
            super::ToolRetention::Retained,
        )
    }
    const NAME: &'static str = "calculate";
    type Args = CalculateArgs;
    type Output = Value;
    type Error = io::Error;
    fn description(&self) -> &'static str {
        "Calculate exact integer arithmetic. Use for any arithmetic question, including multiplication. Choose the operation and provide a and b."
    }
    async fn call(&self, _context: ToolContext, args: CalculateArgs) -> io::Result<Value> {
        let value = match args.operation {
            Operation::Add => args.a.checked_add(args.b),
            Operation::Subtract => args.a.checked_sub(args.b),
            Operation::Multiply => args.a.checked_mul(args.b),
            Operation::Divide => args.a.checked_div(args.b),
        }
        .ok_or_else(|| io::Error::other("integer overflow or division by zero"))?;
        Ok(json!({"value":value}))
    }
}
