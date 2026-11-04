//! Transport-neutral process telemetry. A sampler is owned by one blocking task;
//! no native inference handles cross this boundary. Linux RSS/PSS use rollup page
//! accounting; GPU memory is driver-reported per PID through optional NVML.
//! Unsupported or inaccessible counters are `None`, never invented zeroes.
//! Snapshots are observations, not allocation traces or exclusive message costs.

use serde::Serialize;
use std::{
    sync::OnceLock,
    time::{Instant, SystemTime, UNIX_EPOCH},
};

/// A single process observation, with explicit measurement sources.
#[derive(Debug, Clone, Default, Serialize)]
pub struct ResourceSample {
    /// UTC sample completion time in milliseconds.
    pub sampled_at_unix_ms: Option<u64>,
    /// Monotonic sampler age in nanoseconds; use for intervals, never UTC.
    pub monotonic_ns: u64,
    /// Process CPU nanoseconds (all threads) since launch.
    pub process_cpu_ns: Option<u64>,
    /// CPU utilization over the previous sampling interval; 100% is one core.
    pub cpu_percent: Option<f64>,
    /// Resident process pages; includes shared mappings.
    pub ram_rss_bytes: Option<u64>,
    /// Proportional resident process pages.
    pub ram_pss_bytes: Option<u64>,
    /// Dedicated device memory charged to this PID, summed across devices.
    pub vram_bytes: Option<u64>,
    /// Driver/accounting provider, absent when unsupported.
    pub vram_source: Option<&'static str>,
    /// Process RAM source, absent when unavailable.
    pub ram_source: Option<&'static str>,
}
/// Bounded aggregate for a run or model-turn window; includes other daemon work.
#[derive(Debug, Clone, Default, Serialize)]
pub struct ResourceWindow {
    /// Exact process CPU/window-start counters at the presentation phase boundary.
    pub started: Option<ResourceBoundary>,
    /// Latest counters within the phase, closed at its terminal event.
    pub ended: Option<ResourceBoundary>,
    /// Most recent observation before the window.
    pub before: Option<ResourceSample>,
    /// Last observation during or at the end of the window.
    pub after: Option<ResourceSample>,
    /// Highest RSS actually observed, not a continuous peak.
    pub max_observed_rss_bytes: Option<u64>,
    /// Highest dedicated VRAM actually observed, not exclusive allocation cost.
    pub max_observed_vram_bytes: Option<u64>,
}
/// Cheap boundary counters; sampled without reading process mappings or drivers.
#[derive(Debug, Clone, Serialize)]
pub struct ResourceBoundary {
    /// Monotonic telemetry age; its origin is the first observation.
    pub monotonic_ns: u64,
    /// All-thread process CPU time.
    pub process_cpu_ns: Option<u64>,
}
pub(crate) fn boundary() -> ResourceBoundary {
    ResourceBoundary {
        monotonic_ns: monotonic_ns(),
        process_cpu_ns: {
            #[cfg(target_os = "linux")]
            {
                linux::cpu_time()
            }
            #[cfg(not(target_os = "linux"))]
            {
                None
            }
        },
    }
}
fn monotonic_ns() -> u64 {
    static ORIGIN: OnceLock<Instant> = OnceLock::new();
    u64::try_from(ORIGIN.get_or_init(Instant::now).elapsed().as_nanos()).unwrap_or(u64::MAX)
}
impl ResourceWindow {
    pub(crate) fn observe(&mut self, sample: &ResourceSample) {
        if self.before.is_none() {
            self.before = Some(sample.clone());
        }
        self.after = Some(sample.clone());
        self.ended = Some(ResourceBoundary {
            monotonic_ns: sample.monotonic_ns,
            process_cpu_ns: sample.process_cpu_ns,
        });
        self.max_observed_rss_bytes = maximum(self.max_observed_rss_bytes, sample.ram_rss_bytes);
        self.max_observed_vram_bytes = maximum(self.max_observed_vram_bytes, sample.vram_bytes);
    }
}
fn maximum(a: Option<u64>, b: Option<u64>) -> Option<u64> {
    match (a, b) {
        (Some(a), Some(b)) => Some(a.max(b)),
        (a, b) => a.or(b),
    }
}
/// One reusable provider. Call `sample` on a blocking thread, never an async worker.
#[derive(Debug, Default)]
pub(crate) struct Sampler {
    previous: Option<(u64, u64)>,
    #[cfg(target_os = "linux")]
    gpu: Option<linux::Nvml>,
    initialized: bool,
}
impl Sampler {
    pub(crate) fn sample(&mut self) -> ResourceSample {
        if !self.initialized {
            #[cfg(target_os = "linux")]
            {
                self.gpu = linux::Nvml::open();
            }
            self.initialized = true;
        }
        let mut sample = ResourceSample {
            monotonic_ns: monotonic_ns(),
            sampled_at_unix_ms: None,
            ..ResourceSample::default()
        };
        #[cfg(target_os = "linux")]
        {
            if let Ok(rollup) = std::fs::read_to_string("/proc/self/smaps_rollup") {
                sample.ram_rss_bytes = linux::kilobytes(&rollup, "Rss:");
                sample.ram_pss_bytes = linux::kilobytes(&rollup, "Pss:");
                if sample.ram_rss_bytes.is_some() {
                    sample.ram_source = Some("linux_smaps_rollup");
                }
            }
            sample.vram_bytes = self.gpu.as_ref().and_then(linux::Nvml::memory);
            if sample.vram_bytes.is_some() {
                sample.vram_source = Some("nvml_process_v2");
            }
        }
        // Pair CPU and monotonic counters after provider reads; variable driver
        // latency must not skew the utilization denominator or phase windows.
        let point = boundary();
        sample.monotonic_ns = point.monotonic_ns;
        sample.process_cpu_ns = point.process_cpu_ns;
        sample.sampled_at_unix_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .ok()
            .and_then(|duration| u64::try_from(duration.as_millis()).ok());
        if let Some(cpu) = sample.process_cpu_ns {
            if let Some((previous_at, previous_cpu)) = self.previous {
                let wall = std::time::Duration::from_nanos(
                    sample.monotonic_ns.saturating_sub(previous_at),
                )
                .as_secs_f64();
                let used =
                    std::time::Duration::from_nanos(cpu.saturating_sub(previous_cpu)).as_secs_f64();
                if wall > 0.0 {
                    sample.cpu_percent = Some(used / wall * 100.0);
                }
            }
            self.previous = Some((sample.monotonic_ns, cpu));
        }
        sample
    }
}

#[cfg(target_os = "linux")]
mod linux {
    //! Process-local Linux accounting and optional NVML C ABI. The sampler alone
    //! owns the library; pointers never escape a synchronous provider query.
    use libloading::Library;
    use std::{ffi::c_void, mem::MaybeUninit};
    pub(super) fn kilobytes(text: &str, key: &str) -> Option<u64> {
        text.lines().find_map(|line| {
            let mut parts = line.split_whitespace();
            if parts.next()? != key {
                return None;
            }
            let value = parts.next()?.parse::<u64>().ok()?;
            (parts.next()? == "kB").then_some(value)?.checked_mul(1024)
        })
    }
    pub(super) fn cpu_time() -> Option<u64> {
        // CLOCK_PROCESS_CPUTIME_ID sums all process threads with nanosecond units.
        let mut time = MaybeUninit::<libc::timespec>::uninit();
        // SAFETY: the pointer addresses writable storage of the required ABI type.
        if unsafe { libc::clock_gettime(libc::CLOCK_PROCESS_CPUTIME_ID, time.as_mut_ptr()) } != 0 {
            return None;
        }
        // SAFETY: a successful clock_gettime initializes both fields.
        let time = unsafe { time.assume_init() };
        u64::try_from(time.tv_sec)
            .ok()?
            .checked_mul(1_000_000_000)?
            .checked_add(u64::try_from(time.tv_nsec).ok()?)
    }
    /// NVML v2's stable process ABI. MIG IDs participate only in driver reporting;
    /// PID/device memory is deduplicated across compute and graphics queues.
    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct Process {
        pid: u32,
        memory: u64,
        gpu_instance: u32,
        compute_instance: u32,
    }
    type Status = u32;
    type Device = *mut c_void;
    type Processes = unsafe extern "C" fn(Device, *mut u32, *mut Process) -> Status;
    pub(super) struct Nvml {
        library: Library,
    }
    impl std::fmt::Debug for Nvml {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("Nvml")
        }
    }
    impl Nvml {
        pub(super) fn open() -> Option<Self> {
            // SAFETY: loads the system NVIDIA management library, never model data.
            // All symbols use NVIDIA's documented C ABI and remain library-bound.
            let library = unsafe { Library::new("libnvidia-ml.so.1") }.ok()?;
            // SAFETY: the versioned symbol has the NVML initialization signature.
            let status = unsafe {
                library
                    .get::<unsafe extern "C" fn() -> Status>(b"nvmlInit_v2\0")
                    .ok()?()
            };
            if status == 0 {
                Some(Self { library })
            } else {
                None
            }
        }
        pub(super) fn memory(&self) -> Option<u64> {
            // SAFETY: all pointers below point to live stack storage. Handles come
            // from NVML and are used only while initialized and library is alive.
            // Fixed process arrays bound work and fail closed on insufficient size.
            unsafe {
                let count_fn = self
                    .library
                    .get::<unsafe extern "C" fn(*mut u32) -> Status>(b"nvmlDeviceGetCount_v2\0")
                    .ok()?;
                let handle_fn = self
                    .library
                    .get::<unsafe extern "C" fn(u32, *mut Device) -> Status>(
                        b"nvmlDeviceGetHandleByIndex_v2\0",
                    )
                    .ok()?;
                let mut count = 0;
                if count_fn(&raw mut count) != 0 || count > 32 {
                    return None;
                }
                let mut total = 0_u64;
                for index in 0..count {
                    let mut device = std::ptr::null_mut();
                    if handle_fn(index, &raw mut device) != 0 {
                        return None;
                    }
                    let mut device_bytes = None;
                    for name in [
                        b"nvmlDeviceGetComputeRunningProcesses_v2\0".as_slice(),
                        b"nvmlDeviceGetGraphicsRunningProcesses_v2\0".as_slice(),
                    ] {
                        let processes = self.library.get::<Processes>(name).ok()?;
                        let mut entries = [Process::default(); 128];
                        let mut length = 128;
                        let status = processes(device, &raw mut length, entries.as_mut_ptr());
                        if status == 3 {
                            continue;
                        } // This queue class is unsupported.
                        if status != 0 || length > 128 {
                            return None;
                        }
                        let mut bytes = 0_u64;
                        for process in &entries[..length as usize] {
                            if process.pid == std::process::id() {
                                if process.memory == u64::MAX {
                                    return None;
                                }
                                bytes = bytes.checked_add(process.memory)?;
                            }
                        }
                        device_bytes = Some(device_bytes.map_or(bytes, |n: u64| n.max(bytes)));
                    }
                    total = total.checked_add(device_bytes?)?;
                }
                (count > 0).then_some(total)
            }
        }
    }
    impl Drop for Nvml {
        fn drop(&mut self) {
            // SAFETY: balances this instance's successful nvmlInit_v2. No symbols
            // or device handles outlive the library; provider owns no other tasks.
            unsafe {
                if let Ok(shutdown) = self
                    .library
                    .get::<unsafe extern "C" fn() -> Status>(b"nvmlShutdown\0")
                {
                    let _ = shutdown();
                }
            }
        }
    }
    #[cfg(test)]
    mod tests {
        //! Deterministic source parsing; no GPU is required.
        #[test]
        fn rollup_units_and_missing_fields_are_explicit() {
            assert_eq!(
                super::kilobytes("Rss: 12 kB\nPss: 5 kB\n", "Rss:"),
                Some(12288)
            );
            assert_eq!(super::kilobytes("Rss: 12 MB", "Rss:"), None);
            assert_eq!(super::kilobytes("", "Pss:"), None);
        }
    }
}
