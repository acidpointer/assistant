//! Bounded procfs observations, never shell output. Each stat read owns its file;
//! CPU comparisons require equal PID/start-time identities. Detail reads anchor a
//! proc directory descriptor so PID reuse cannot redirect later reads. Procfs
//! work runs only on blocking workers, with cooperative cancellation per record.

use super::{
    ProcessDetail, ProcessDetailsArgs, ProcessDetailsOutput, ProcessError, ProcessSort,
    ProcessSummary, ProcessTopArgs, ProcessTopOutput, SortOrder,
};
use crate::engine::Cancellation;
use std::{
    cmp::Ordering,
    collections::BTreeMap,
    fs::{self, File},
    io::{self, Read},
    os::fd::AsRawFd,
    path::Path,
    time::{Duration, Instant},
};

const MAX_PROCESSES: usize = 16_384;
const SAMPLE_WINDOW: Duration = Duration::from_millis(250);

/// Kernel counters stay integral until the final presentation conversion.
#[derive(Debug)]
struct Record {
    pid: u32,
    parent_pid: u32,
    name: String,
    state: char,
    cpu_ticks: u64,
    start_time: u64,
    rss: u64,
    virtual_bytes: u64,
    threads: u32,
    priority: i64,
    nice: i32,
    observed_at: Instant,
}

#[derive(Debug)]
struct Snapshot {
    records: BTreeMap<u32, Record>,
    unavailable: usize,
    ticks_per_second: u64,
    page_size: u64,
    uptime: f64,
}

fn io_error(operation: &'static str, source: io::Error) -> ProcessError {
    ProcessError::Io { operation, source }
}
fn check_cancel(cancellation: &Cancellation) -> Result<(), ProcessError> {
    if cancellation.is_cancelled() {
        Err(ProcessError::Cancelled)
    } else {
        Ok(())
    }
}
fn read_bounded(path: &Path, maximum: usize) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    File::open(path)?
        .take((maximum + 1) as u64)
        .read_to_end(&mut bytes)?;
    if bytes.len() > maximum {
        return Err(invalid_stat());
    }
    Ok(bytes)
}
fn invalid_stat() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "invalid or oversized procfs record",
    )
}

/// Control bytes stay data, not terminal escapes or injected output structure.
fn bounded_text(text: &str, maximum: usize) -> (String, bool) {
    let mut output = String::new();
    for character in text.chars() {
        let character = if character.is_control() {
            '�'
        } else {
            character
        };
        if output.len() + character.len_utf8() > maximum {
            return (output, true);
        }
        output.push(character);
    }
    (output, false)
}

/// comm is parenthesized and may itself contain spaces, ')' and newlines. Numeric
/// fields start after the final ')', not after whitespace token number two.
fn parse_stat(text: &str, pid: u32, page_size: u64) -> io::Result<Record> {
    let open = text.find('(').ok_or_else(invalid_stat)?;
    let close = text.rfind(')').ok_or_else(invalid_stat)?;
    if close <= open || text[..open].trim().parse::<u32>().ok() != Some(pid) {
        return Err(invalid_stat());
    }
    let fields = text[close + 1..].split_whitespace().collect::<Vec<_>>();
    let number = |index: usize| -> io::Result<u64> {
        fields
            .get(index)
            .and_then(|value| value.parse().ok())
            .ok_or_else(invalid_stat)
    };
    let signed = |index: usize| -> io::Result<i64> {
        fields
            .get(index)
            .and_then(|value| value.parse().ok())
            .ok_or_else(invalid_stat)
    };
    let state = fields.first().ok_or_else(invalid_stat)?;
    if state.len() != 1 || !state.as_bytes()[0].is_ascii_alphabetic() {
        return Err(invalid_stat());
    }
    Ok(Record {
        pid,
        parent_pid: u32::try_from(number(1)?).map_err(|_| invalid_stat())?,
        name: bounded_text(&text[open + 1..close], 128).0,
        state: char::from(state.as_bytes()[0]),
        cpu_ticks: number(11)?
            .checked_add(number(12)?)
            .ok_or_else(invalid_stat)?,
        priority: signed(15)?,
        nice: i32::try_from(signed(16)?).map_err(|_| invalid_stat())?,
        threads: u32::try_from(number(17)?).map_err(|_| invalid_stat())?,
        start_time: number(19)?,
        virtual_bytes: number(20)?,
        rss: number(21)?
            .checked_mul(page_size)
            .ok_or_else(invalid_stat)?,
        observed_at: Instant::now(),
    })
}

fn read_record(path: &Path, pid: u32, page_size: u64) -> io::Result<Record> {
    parse_stat(
        &String::from_utf8_lossy(&read_bounded(path, 8192)?),
        pid,
        page_size,
    )
}

fn read_snapshot(cancellation: &Cancellation, pid: Option<u32>) -> Result<Snapshot, ProcessError> {
    check_cancel(cancellation)?;
    // SAFETY: sysconf accepts these constant selectors and takes no pointers.
    let ticks = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
    // SAFETY: same scalar-only sysconf contract as above.
    let page_size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    let positive = |value| {
        u64::try_from(value)
            .ok()
            .filter(|&value| value > 0)
            .ok_or_else(|| {
                io_error(
                    "read kernel clock/page units",
                    io::Error::other("invalid sysconf result"),
                )
            })
    };
    let mut snapshot = Snapshot {
        records: BTreeMap::new(),
        unavailable: 0,
        ticks_per_second: positive(ticks)?,
        page_size: positive(page_size)?,
        uptime: 0.0,
    };
    let mut observe = |pid: u32| match read_record(
        &Path::new("/proc").join(pid.to_string()).join("stat"),
        pid,
        snapshot.page_size,
    ) {
        Ok(record) => {
            snapshot.records.insert(pid, record);
        }
        Err(_) => snapshot.unavailable += 1,
    };
    if let Some(pid) = pid {
        observe(pid);
    } else {
        let entries = fs::read_dir("/proc").map_err(|error| io_error("list procfs", error))?;
        let mut processes = 0;
        for (index, entry) in entries.enumerate() {
            check_cancel(cancellation)?;
            if index >= MAX_PROCESSES + 1024 {
                return Err(ProcessError::ScanLimit);
            }
            let entry = entry.map_err(|error| io_error("list procfs", error))?;
            let Some(pid) = entry
                .file_name()
                .to_str()
                .and_then(|name| name.parse::<u32>().ok())
            else {
                continue;
            };
            if processes >= MAX_PROCESSES {
                return Err(ProcessError::ScanLimit);
            }
            processes += 1;
            observe(pid);
        }
    }
    let uptime = read_bounded(Path::new("/proc/uptime"), 256)
        .map_err(|error| io_error("read system uptime", error))?;
    snapshot.uptime = String::from_utf8_lossy(&uptime)
        .split_whitespace()
        .next()
        .and_then(|value| value.parse::<f64>().ok())
        .filter(|value| value.is_finite() && *value >= 0.0)
        .ok_or_else(|| io_error("decode system uptime", invalid_stat()))?;
    Ok(snapshot)
}

async fn snapshot(cancellation: &Cancellation, pid: Option<u32>) -> Result<Snapshot, ProcessError> {
    let worker_token = cancellation.clone();
    tokio::select! {
        () = cancellation.cancelled() => Err(ProcessError::Cancelled),
        result = tokio::task::spawn_blocking(move || read_snapshot(&worker_token, pid)) => result?,
    }
}
async fn sample(
    cancellation: &Cancellation,
    pid: Option<u32>,
) -> Result<(Snapshot, Snapshot), ProcessError> {
    let before = snapshot(cancellation, pid).await?;
    tokio::select! {
        () = cancellation.cancelled() => return Err(ProcessError::Cancelled),
        () = tokio::time::sleep(SAMPLE_WINDOW) => {}
    }
    let after = snapshot(cancellation, pid).await?;
    Ok((before, after))
}

/// CPU percentages are necessarily approximate: procfs quantizes counters to
/// ticks. Interval readings subtract integral samples before converting; lifetime
/// counters are converted only for presentation.
#[allow(clippy::cast_precision_loss)]
fn seconds(ticks: u64, ticks_per_second: u64) -> f64 {
    ticks as f64 / ticks_per_second as f64
}
fn summary(record: &Record, before: &Snapshot) -> ProcessSummary {
    let reading = before
        .records
        .get(&record.pid)
        .filter(|old| old.start_time == record.start_time)
        .and_then(|old| {
            let ticks = record.cpu_ticks.checked_sub(old.cpu_ticks)?;
            let wall = record
                .observed_at
                .checked_duration_since(old.observed_at)?
                .as_secs_f64();
            (wall > 0.0).then(|| {
                (
                    seconds(ticks, before.ticks_per_second) / wall * 100.0,
                    wall * 1000.0,
                )
            })
        });
    ProcessSummary {
        pid: record.pid,
        parent_pid: record.parent_pid,
        name: record.name.clone(),
        state: record.state,
        start_time_ticks: record.start_time,
        cpu_percent: reading.map(|value| value.0),
        cpu_interval_ms: reading.map(|value| value.1),
        rss_bytes: record.rss,
        virtual_bytes: record.virtual_bytes,
    }
}

fn matches(process: &ProcessSummary, args: &ProcessTopArgs) -> bool {
    args.name.as_ref().is_none_or(|name| {
        process
            .name
            .to_lowercase()
            .contains(&name.trim().to_lowercase())
    }) && args
        .min_cpu_percent
        .is_none_or(|min| process.cpu_percent.is_some_and(|cpu| cpu >= min))
        && args
            .min_rss_bytes
            .is_none_or(|min| process.rss_bytes >= min)
        && args
            .min_virtual_bytes
            .is_none_or(|min| process.virtual_bytes >= min)
}
fn compare(a: &ProcessSummary, b: &ProcessSummary, key: ProcessSort, order: SortOrder) -> Ordering {
    let descending = matches!(order, SortOrder::Desc);
    if matches!(key, ProcessSort::Cpu) {
        match (a.cpu_percent, b.cpu_percent) {
            (None, Some(_)) => return Ordering::Greater,
            (Some(_), None) => return Ordering::Less,
            _ => {}
        }
    }
    let value = match key {
        ProcessSort::Cpu => a
            .cpu_percent
            .partial_cmp(&b.cpu_percent)
            .unwrap_or(Ordering::Equal),
        ProcessSort::Rss => a.rss_bytes.cmp(&b.rss_bytes),
        ProcessSort::Virt => a.virtual_bytes.cmp(&b.virtual_bytes),
        ProcessSort::Name => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
    };
    (if descending { value.reverse() } else { value }).then_with(|| a.pid.cmp(&b.pid))
}

pub(super) async fn top(
    cancellation: Cancellation,
    args: ProcessTopArgs,
) -> Result<ProcessTopOutput, ProcessError> {
    let (before, after) = sample(&cancellation, None).await?;
    tokio::task::spawn_blocking(move || {
        let mut processes = after
            .records
            .values()
            .map(|record| summary(record, &before))
            .filter(|process| matches(process, &args))
            .collect::<Vec<_>>();
        processes.sort_unstable_by(|a, b| compare(a, b, args.sort_by, args.order));
        let matched_processes = processes.len();
        processes.truncate(usize::from(args.limit));
        check_cancel(&cancellation)?;
        Ok(ProcessTopOutput {
            observed_processes: after.records.len(),
            unavailable_processes: after.unavailable,
            matched_processes,
            truncated: processes.len() < matched_processes,
            processes,
        })
    })
    .await?
}

fn relevance(name: &str, query: &str) -> Option<u8> {
    let name = name.to_lowercase();
    if name == query {
        Some(0)
    } else if name.starts_with(query) {
        Some(1)
    } else if name.contains(query) {
        Some(2)
    } else {
        None
    }
}

/// Proc-directory ownership can change to root for a nondumpable process; use
/// the status record's effective UID rather than mistaking that for its owner.
fn effective_uid(status: &str) -> io::Result<u32> {
    status
        .lines()
        .find_map(|line| line.strip_prefix("Uid:"))
        .and_then(|values| values.split_whitespace().nth(1))
        .and_then(|value| value.parse().ok())
        .ok_or_else(invalid_stat)
}

/// Keep the directory fd alive for all child reads. An exited /proc/PID directory
/// cannot resolve to a replacement process even if the numeric PID is reused.
fn detail(record: &Record, before: &Snapshot, after: &Snapshot) -> io::Result<ProcessDetail> {
    let directory = File::open(Path::new("/proc").join(record.pid.to_string()))?;
    let anchored = Path::new("/proc/self/fd").join(directory.as_raw_fd().to_string());
    let current = read_record(&anchored.join("stat"), record.pid, after.page_size)?;
    if current.start_time != record.start_time {
        return Err(invalid_stat());
    }
    let status = read_bounded(&anchored.join("status"), 8192)?;
    let uid = effective_uid(&String::from_utf8_lossy(&status))?;
    let executable = match fs::read_link(anchored.join("exe")) {
        Ok(path) => Some(bounded_text(&path.to_string_lossy(), 256)),
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::PermissionDenied
            ) =>
        {
            None
        }
        Err(error) => return Err(error),
    };
    Ok(ProcessDetail {
        process: summary(record, before),
        uid,
        executable: executable.as_ref().map(|value| value.0.clone()),
        executable_truncated: executable.is_some_and(|value| value.1),
        threads: record.threads,
        priority: record.priority,
        nice: record.nice,
        cpu_time_seconds: seconds(record.cpu_ticks, after.ticks_per_second),
        elapsed_seconds: (after.uptime - seconds(record.start_time, after.ticks_per_second))
            .max(0.0),
    })
}
pub(super) async fn details(
    cancellation: Cancellation,
    args: ProcessDetailsArgs,
) -> Result<ProcessDetailsOutput, ProcessError> {
    let query = args.query.trim().to_owned();
    let pid = if query.bytes().all(|byte| byte.is_ascii_digit()) {
        Some(
            query
                .parse::<u32>()
                .ok()
                .filter(|&pid| pid > 0)
                .ok_or(ProcessError::InvalidArguments("PID must be a positive u32"))?,
        )
    } else {
        None
    };
    let (before, after) = sample(&cancellation, pid).await?;
    let token = cancellation.clone();
    tokio::task::spawn_blocking(move || {
        let lowered = query.to_lowercase();
        let mut matches = after
            .records
            .values()
            .filter_map(|record| {
                let rank = if pid == Some(record.pid) {
                    Some(0)
                } else {
                    relevance(&record.name, &lowered)
                }?;
                Some((rank, record.pid, record))
            })
            .collect::<Vec<_>>();
        matches.sort_unstable_by_key(|(rank, pid, _)| (*rank, *pid));
        let matched_processes = matches.len();
        let mut processes = Vec::new();
        let mut unavailable_processes = after.unavailable;
        for (_, _, record) in matches.iter().take(usize::from(args.limit)) {
            check_cancel(&token)?;
            match detail(record, &before, &after) {
                Ok(process) => processes.push(process),
                Err(_) => unavailable_processes += 1,
            }
        }
        Ok(ProcessDetailsOutput {
            query,
            observed_processes: after.records.len(),
            unavailable_processes,
            matched_processes,
            truncated: matched_processes > usize::from(args.limit),
            processes,
        })
    })
    .await?
}

#[cfg(test)]
mod tests;
