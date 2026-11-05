//! Read-only process capabilities. The model resolves user intent into typed
//! filters or a name/PID query; host code never interprets that text as a command.
//! Linux observations run on blocking workers, with cancellation between bounded
//! reads. CPU uses two samples; RSS is the kernel's inexpensive estimate. Results
//! fit the default tool payload budget. Other platforms report unsupported.

use crate::tools::{Tool, ToolContext};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[cfg(target_os = "linux")]
mod linux;
#[cfg(test)]
mod tests;

/// Field used to rank a process listing; missing CPU readings always sort last.
#[derive(Debug, Clone, Copy, Default, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ProcessSort {
    /// Current CPU percentage, with 100% representing one CPU core.
    #[default]
    Cpu,
    /// Resident memory estimate in bytes.
    Rss,
    /// Virtual address space in bytes, not physical memory consumption.
    Virt,
    /// Case-insensitive process name.
    Name,
}

/// Direction of the selected sort key; PID breaks ties in ascending order.
#[derive(Debug, Clone, Copy, Default, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum SortOrder {
    /// Largest first (Z to A for names).
    #[default]
    Desc,
    /// Smallest first (A to Z for names).
    Asc,
}

/// Filters apply together, before the bounded result limit.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ProcessTopArgs {
    /// Sort by cpu (default), rss, virt, or name.
    #[serde(default)]
    pub sort_by: ProcessSort,
    /// Descending by default; use asc for alphabetical name ordering.
    #[serde(default)]
    pub order: SortOrder,
    /// Optional case-insensitive literal substring of the process name.
    pub name: Option<String>,
    /// Inclusive minimum CPU percentage; 100% is one core, not the entire machine.
    #[schemars(range(min = 0))]
    pub min_cpu_percent: Option<f64>,
    /// Inclusive minimum resident memory estimate, in bytes.
    pub min_rss_bytes: Option<u64>,
    /// Inclusive minimum virtual address space, in bytes.
    pub min_virtual_bytes: Option<u64>,
    /// Number of results, from 1 to 25. Defaults to 10.
    #[serde(default = "top_limit")]
    #[schemars(range(min = 1, max = 25))]
    pub limit: u8,
}

const fn top_limit() -> u8 {
    10
}
const fn details_limit() -> u8 {
    5
}

impl Default for ProcessTopArgs {
    fn default() -> Self {
        Self {
            sort_by: ProcessSort::default(),
            order: SortOrder::default(),
            name: None,
            min_cpu_percent: None,
            min_rss_bytes: None,
            min_virtual_bytes: None,
            limit: top_limit(),
        }
    }
}

impl ProcessTopArgs {
    fn validate(&self) -> Result<(), ProcessError> {
        validate_limit(self.limit, 25)?;
        if let Some(name) = &self.name {
            validate_query(name)?;
        }
        if self
            .min_cpu_percent
            .is_some_and(|value| !value.is_finite() || value < 0.0)
        {
            return Err(ProcessError::InvalidArguments(
                "min_cpu_percent must be finite and nonnegative",
            ));
        }
        Ok(())
    }
}

/// A process name query supplied by the model after interpreting the user's
/// request (for example, query "htop" for "tell me about the htop process").
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ProcessDetailsArgs {
    /// Process name or decimal PID. Name matching ignores case and ranks exact
    /// matches, prefixes, then substrings. Use a short name, not a whole question.
    #[schemars(length(min = 1, max = 128))]
    pub query: String,
    /// Number of matching processes, from 1 to 10. Defaults to 5.
    #[serde(default = "details_limit")]
    #[schemars(range(min = 1, max = 10))]
    pub limit: u8,
}

fn validate_limit(limit: u8, maximum: u8) -> Result<(), ProcessError> {
    if limit == 0 || limit > maximum {
        return Err(ProcessError::InvalidArguments(
            "limit is outside the tool's range",
        ));
    }
    Ok(())
}
fn validate_query(query: &str) -> Result<(), ProcessError> {
    if query.trim().is_empty() || query.len() > 128 || query.chars().any(char::is_control) {
        return Err(ProcessError::InvalidArguments(
            "name/query must contain 1..128 UTF-8 bytes without control characters",
        ));
    }
    Ok(())
}

/// Failures are returned as tool errors; unavailable process records are counted
/// in a successful report rather than fabricated or mistaken for idle processes.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ProcessError {
    /// Rejected before any process inspection.
    #[error("invalid process arguments: {0}")]
    InvalidArguments(&'static str),
    /// A cooperative stop was requested.
    #[error("process inspection cancelled")]
    Cancelled,
    /// This initial provider is Linux-only.
    #[error("process inspection requires Linux procfs")]
    Unsupported,
    /// A bounded scan cannot represent this process namespace safely.
    #[error("process scan exceeded its bounded capacity")]
    ScanLimit,
    /// Failure to read the process namespace or its measurement configuration.
    #[error("cannot {operation}: {source}")]
    Io {
        /// Host operation, without command arguments or environment data.
        operation: &'static str,
        /// Underlying operating-system failure.
        source: std::io::Error,
    },
    /// The blocking observation worker failed.
    #[error("process observation worker failed: {0}")]
    Worker(#[from] tokio::task::JoinError),
}

/// A process observed in the daemon's PID namespace with its access permissions.
#[derive(Debug, Serialize)]
pub struct ProcessSummary {
    /// Numeric PID; combine with `start_time_ticks` to distinguish PID reuse.
    pub pid: u32,
    /// Parent PID at observation time.
    pub parent_pid: u32,
    /// Kernel process name; control characters are replaced and text is bounded.
    pub name: String,
    /// Kernel state letter, such as R (running), S (sleeping), or Z (zombie).
    pub state: char,
    /// Start time since boot in kernel clock ticks.
    pub start_time_ticks: u64,
    /// CPU percentage across the actual sampling interval; 100% is one core.
    /// None means the process had no comparable baseline, never an assumed zero.
    pub cpu_percent: Option<f64>,
    /// Actual per-process interval in milliseconds, paired with `cpu_percent`.
    pub cpu_interval_ms: Option<f64>,
    /// Kernel RSS estimate in bytes; includes shared resident pages.
    pub rss_bytes: u64,
    /// Virtual address space in bytes; not allocated physical RAM.
    pub virtual_bytes: u64,
}

/// Bounded listing. Counts describe the second scan, not every host process.
#[derive(Debug, Serialize)]
pub struct ProcessTopOutput {
    /// Readable process records in the second scan.
    pub observed_processes: usize,
    /// Records that disappeared, were inaccessible, or could not be decoded.
    pub unavailable_processes: usize,
    /// Number of readable processes satisfying every filter before limiting.
    pub matched_processes: usize,
    /// True when additional matches were omitted by the result limit.
    pub truncated: bool,
    /// Ordered rows, bounded by the requested limit.
    pub processes: Vec<ProcessSummary>,
}

/// Extra live metadata. Command arguments, environment, and open files are not read.
#[derive(Debug, Serialize)]
pub struct ProcessDetail {
    /// Identity, current CPU and memory observation.
    #[serde(flatten)]
    pub process: ProcessSummary,
    /// Effective UID reported by the process's status record.
    pub uid: u32,
    /// Executable path, absent for kernel threads or inaccessible/exited processes.
    pub executable: Option<String>,
    /// Whether the executable path was shortened to its output bound.
    pub executable_truncated: bool,
    /// Number of threads at observation time.
    pub threads: u32,
    /// Raw kernel scheduling priority.
    pub priority: i64,
    /// Scheduling nice value.
    pub nice: i32,
    /// Total process user+system CPU seconds, excluding waited-for children.
    pub cpu_time_seconds: f64,
    /// Age since process creation, including system suspend.
    pub elapsed_seconds: f64,
}

/// Name/PID lookup can return multiple instances; no arbitrary instance is hidden.
#[derive(Debug, Serialize)]
pub struct ProcessDetailsOutput {
    /// Validated, trimmed lookup text.
    pub query: String,
    /// Readable process records in the second scan.
    pub observed_processes: usize,
    /// Inaccessible, vanished or changed records, including during detail reads.
    pub unavailable_processes: usize,
    /// Name/PID matches before limiting and final identity verification.
    pub matched_processes: usize,
    /// True when additional matches were omitted by the result limit.
    pub truncated: bool,
    /// Most relevant matches; empty means none were observable.
    pub processes: Vec<ProcessDetail>,
}

/// Rank and filter live processes without executing an OS command.
#[derive(Debug)]
pub struct ProcessTop;
impl Tool for ProcessTop {
    #[cfg(target_os = "linux")]
    fn memory_policy(&self) -> (crate::memory::ToolEffect, crate::tools::ToolRetention) {
        (
            crate::memory::ToolEffect::ReadOnly,
            crate::tools::ToolRetention::Ephemeral,
        )
    }
    const NAME: &'static str = "process_top";
    type Args = ProcessTopArgs;
    type Output = ProcessTopOutput;
    type Error = ProcessError;
    fn description(&self) -> &'static str {
        "List live Linux processes. Sort by cpu, rss (resident RAM bytes), virt (virtual address-space bytes), or name; optionally filter by name and minimum CPU/RSS/virtual memory. CPU is sampled over at least 250 ms; 100% is one core. RSS is a kernel estimate. Defaults: highest CPU first, 10 rows; maximum 25. Read-only."
    }
    async fn call(
        &self,
        context: ToolContext,
        args: Self::Args,
    ) -> Result<Self::Output, Self::Error> {
        args.validate()?;
        #[cfg(target_os = "linux")]
        {
            linux::top(context.cancellation, args).await
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = (context, args);
            Err(ProcessError::Unsupported)
        }
    }
}

/// Resolve a process name or PID to observed process metadata.
#[derive(Debug)]
pub struct ProcessDetails;
impl Tool for ProcessDetails {
    #[cfg(target_os = "linux")]
    fn memory_policy(&self) -> (crate::memory::ToolEffect, crate::tools::ToolRetention) {
        (
            crate::memory::ToolEffect::ReadOnly,
            crate::tools::ToolRetention::Ephemeral,
        )
    }
    const NAME: &'static str = "process_details";
    type Args = ProcessDetailsArgs;
    type Output = ProcessDetailsOutput;
    type Error = ProcessError;
    fn description(&self) -> &'static str {
        "Look up live Linux process details by name or PID. For 'tell me about the htop process', pass query='htop'. Name matches ignore case, ranked exact then prefix then substring; multiple instances may match. Includes executable, owner UID, parent, threads, priority/nice, CPU, RSS and virtual memory. CPU uses at least 250 ms; RSS is a kernel estimate. No matches means no observable matching process, not that a program is uninstalled. Read-only."
    }
    async fn call(
        &self,
        context: ToolContext,
        args: Self::Args,
    ) -> Result<Self::Output, Self::Error> {
        validate_query(&args.query)?;
        validate_limit(args.limit, 10)?;
        #[cfg(target_os = "linux")]
        {
            linux::details(context.cancellation, args).await
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = (context, args);
            Err(ProcessError::Unsupported)
        }
    }
}
