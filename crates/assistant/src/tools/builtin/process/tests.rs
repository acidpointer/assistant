//! Public contracts: schema/validation boundaries, bounded payloads, cancellation,
//! and real procfs reads without an inference model or external command.

use super::{
    ProcessDetail, ProcessDetailsArgs, ProcessDetailsOutput, ProcessSummary, ProcessTopArgs,
    ProcessTopOutput,
};
#[cfg(target_os = "linux")]
use super::{ProcessDetails, ProcessError, ProcessTop};
#[cfg(target_os = "linux")]
use crate::{
    engine::Cancellation,
    tools::{Tool, ToolContext},
};

#[test]
fn strict_arguments_and_documented_defaults() {
    let args: ProcessTopArgs = serde_json::from_str("{}").expect("default top arguments");
    assert_eq!(args.limit, 10);
    assert_eq!(ProcessTopArgs::default().limit, args.limit);
    assert!(serde_json::from_str::<ProcessTopArgs>(r#"{"command":"ps"}"#).is_err());
    assert!(serde_json::from_str::<ProcessDetailsArgs>(r#"{"query":"htop","pid":2}"#).is_err());
    for limit in [0, 26, 255] {
        assert!(
            ProcessTopArgs {
                limit,
                ..ProcessTopArgs::default()
            }
            .validate()
            .is_err()
        );
    }
    for cpu in [-1.0, f64::NAN, f64::INFINITY] {
        assert!(
            ProcessTopArgs {
                min_cpu_percent: Some(cpu),
                ..ProcessTopArgs::default()
            }
            .validate()
            .is_err()
        );
    }
    assert!(
        ProcessTopArgs {
            min_cpu_percent: Some(200.0),
            ..ProcessTopArgs::default()
        }
        .validate()
        .is_ok()
    );
    for query in ["", "  ", "htop\n", &"x".repeat(129)] {
        assert!(super::validate_query(query).is_err());
    }
    let schema = schemars::schema_for!(ProcessTopArgs).to_value();
    assert_eq!(schema["additionalProperties"], false);
    assert_eq!(schema["properties"]["limit"]["maximum"], 25);
}

fn worst_case_summary() -> ProcessSummary {
    ProcessSummary {
        pid: u32::MAX,
        parent_pid: u32::MAX,
        name: "\\".repeat(128),
        state: 'R',
        start_time_ticks: u64::MAX,
        cpu_percent: Some(f64::MAX),
        cpu_interval_ms: Some(f64::MAX),
        rss_bytes: u64::MAX,
        virtual_bytes: u64::MAX,
    }
}

#[test]
fn maximum_results_fit_the_default_tool_budget_even_with_json_escaping() {
    let top = ProcessTopOutput {
        observed_processes: 16_384,
        unavailable_processes: 16_384,
        matched_processes: 16_384,
        truncated: true,
        processes: (0..25).map(|_| worst_case_summary()).collect(),
    };
    let details = ProcessDetailsOutput {
        query: "\\".repeat(128),
        observed_processes: 16_384,
        unavailable_processes: 16_384,
        matched_processes: 16_384,
        truncated: true,
        processes: (0..10)
            .map(|_| ProcessDetail {
                process: worst_case_summary(),
                uid: u32::MAX,
                executable: Some("\\".repeat(256)),
                executable_truncated: true,
                threads: u32::MAX,
                priority: i64::MIN,
                nice: i32::MIN,
                cpu_time_seconds: f64::MAX,
                elapsed_seconds: f64::MAX,
            })
            .collect(),
    };
    for bytes in [
        serde_json::to_vec(&serde_json::json!({"ok":true,"result":top}))
            .expect("top tool envelope"),
        serde_json::to_vec(&serde_json::json!({"ok":true,"result":details}))
            .expect("details tool envelope"),
    ] {
        assert!(
            bytes.len() < crate::chat::RunLimits::default().tool_result_bytes,
            "payload is {} bytes",
            bytes.len()
        );
    }
}

#[cfg(target_os = "linux")]
fn context(cancellation: Cancellation) -> ToolContext {
    ToolContext {
        invocation_id: "process-test".into(),
        cancellation,
    }
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn live_pid_lookup_and_name_filtered_top_observe_this_process() {
    let result = ProcessDetails
        .call(
            context(Cancellation::default()),
            ProcessDetailsArgs {
                query: std::process::id().to_string(),
                limit: 1,
            },
        )
        .await
        .expect("read own process");
    assert_eq!(result.processes.len(), 1);
    let own = &result.processes[0];
    assert_eq!(own.process.pid, std::process::id());
    assert!(own.process.rss_bytes > 0);
    assert!(own.process.virtual_bytes >= own.process.rss_bytes);
    assert!(own.executable.is_some());
    assert!(own.process.cpu_interval_ms.is_some_and(|ms| ms >= 250.0));
    let top = ProcessTop
        .call(
            context(Cancellation::default()),
            ProcessTopArgs {
                name: Some(own.process.name.clone()),
                ..ProcessTopArgs::default()
            },
        )
        .await
        .expect("list by own process name");
    assert!(
        top.processes
            .iter()
            .any(|process| process.pid == std::process::id())
    );
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn cancellation_interrupts_sampling_without_blocking_the_async_executor() {
    let cancellation = Cancellation::default();
    let stop = cancellation.clone();
    let (result, ()) = tokio::join!(
        ProcessTop.call(context(cancellation), ProcessTopArgs::default()),
        async move {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            stop.cancel();
        }
    );
    assert!(matches!(result, Err(ProcessError::Cancelled)));
}
