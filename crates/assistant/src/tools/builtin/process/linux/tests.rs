//! Procfs parsing and measurement fixtures cover unusual names, byte units,
//! PID reuse, multicore CPU, missing baselines, and deterministic query results.

use super::{Record, Snapshot, compare, effective_uid, matches, parse_stat, relevance, summary};
use crate::tools::builtin::process::{ProcessSort, ProcessTopArgs, SortOrder};
use std::{
    cmp::Ordering,
    collections::BTreeMap,
    time::{Duration, Instant},
};

fn record(name: &str) -> Record {
    let mut fields = vec!["0"; 22];
    fields[0] = "R";
    fields[1] = "7";
    fields[11] = "100";
    fields[12] = "20";
    fields[15] = "20";
    fields[16] = "-5";
    fields[17] = "3";
    fields[19] = "500";
    fields[20] = "900000";
    fields[21] = "10";
    parse_stat(&format!("42 ({name}) {}", fields.join(" ")), 42, 4096).expect("valid proc stat")
}
fn baseline(record: Record) -> Snapshot {
    Snapshot {
        records: BTreeMap::from([(42, record)]),
        unavailable: 0,
        ticks_per_second: 100,
        page_size: 4096,
        uptime: 100.0,
    }
}

#[test]
fn stat_fields_survive_parentheses_spaces_and_control_characters_in_names() {
    let record = record("a ) b\nc");
    assert_eq!(record.name, "a ) b�c");
    assert_eq!(record.parent_pid, 7);
    assert_eq!(record.cpu_ticks, 120);
    assert_eq!(record.start_time, 500);
    assert_eq!(record.rss, 40960);
    assert_eq!(record.virtual_bytes, 900_000);
    assert_eq!(record.threads, 3);
    assert_eq!(record.nice, -5);
    assert_eq!(
        effective_uid("Name:\thtop\nUid:\t1000\t1001\t1002\t1003\n").expect("effective UID"),
        1001
    );
    assert!(effective_uid("Name:\thtop\n").is_err());
    assert!(parse_stat("42 (broken) R 0", 42, 4096).is_err());
    assert!(parse_stat("41 (wrong PID) R 0", 42, 4096).is_err());
    assert!(
        parse_stat(
            "42 (negative RSS) R 7 0 0 0 0 0 0 0 0 0 100 20 0 0 20 -5 3 0 500 900000 -1",
            42,
            4096
        )
        .is_err()
    );
}

#[test]
fn cpu_is_interval_based_multicore_and_rejects_pid_reuse_or_counter_regression() {
    let at = Instant::now();
    let mut old = record("htop");
    old.observed_at = at;
    let before = baseline(old);
    let mut next = record("htop");
    next.cpu_ticks += 50;
    next.observed_at = at + Duration::from_millis(250);
    let measured = summary(&next, &before);
    assert!((measured.cpu_percent.expect("comparable counters") - 200.0).abs() < 1e-9);
    next.start_time += 1;
    assert!(summary(&next, &before).cpu_percent.is_none());
    next.start_time -= 1;
    next.cpu_ticks = 119;
    assert!(summary(&next, &before).cpu_percent.is_none());
}

#[test]
fn filtering_and_sorting_keep_unknown_cpu_last_and_rank_names_without_shell_parsing() {
    let before = baseline(record("HTop"));
    let mut first = summary(&record("HTop"), &before);
    first.cpu_percent = Some(150.0);
    let mut second = summary(&record("other"), &before);
    second.pid += 1;
    second.cpu_percent = None;
    for order in [SortOrder::Asc, SortOrder::Desc] {
        assert_eq!(
            compare(&first, &second, ProcessSort::Cpu, order),
            Ordering::Less
        );
    }
    let args = ProcessTopArgs {
        name: Some("htop".into()),
        min_cpu_percent: Some(100.0),
        min_rss_bytes: Some(40960),
        min_virtual_bytes: Some(900_000),
        ..ProcessTopArgs::default()
    };
    assert!(matches(&first, &args));
    first.cpu_percent = None;
    assert!(!matches(&first, &args));
    assert_eq!(relevance("HTop", "htop"), Some(0));
    assert_eq!(relevance("htop-worker", "htop"), Some(1));
    assert_eq!(relevance("my-htop", "htop"), Some(2));
    assert_eq!(relevance("htop", "htop; date"), None);
}
