//! Transport-neutral process telemetry. A sampler is owned by one blocking task;
//! no native inference handles cross this boundary. Linux RSS/PSS use rollup page
//! accounting; model-scoped GPU residency uses optional NVML or DRM counters.
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
    /// Process residency summed only across the primary model's allocated GPUs.
    pub vram_bytes: Option<u64>,
    /// Authoritative model devices and their readings. None means unknown
    /// placement; an empty list establishes CPU-only model/context allocation.
    pub vram_devices: Option<Vec<VramDevice>>,
    /// Aggregate accounting scope, absent when any participant is unmeasurable.
    pub vram_source: Option<&'static str>,
    /// Process RAM source, absent when unavailable.
    pub ram_source: Option<&'static str>,
}
/// Owned identity of a device with actual non-host model/context buffers.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct ModelGpuDevice {
    pub name: String,
    pub pci_bus_id: Option<String>,
}
/// Driver accounting for one GPU participating in the primary model.
#[derive(Debug, Clone, Serialize)]
pub struct VramDevice {
    /// Native hardware description, not an enumeration index.
    pub name: String,
    /// Canonical PCI identity used to match the provider; None is unattributable.
    pub pci_bus_id: Option<String>,
    /// Process residency on this device; absent when accounting is unavailable.
    pub bytes: Option<u64>,
    /// Driver source for this specific observation.
    pub source: Option<&'static str>,
}
/// Normalize driver/backend PCI spellings before sysfs, DRM and NVML matching.
pub(crate) fn pci_id(value: &str) -> Option<String> {
    let parts = value.split(':').collect::<Vec<_>>();
    if parts.len() != 3 {
        return None;
    }
    let domain = u32::from_str_radix(parts[0], 16).ok()?;
    let bus = u8::from_str_radix(parts[1], 16).ok()?;
    let (device, function) = parts[2].split_once('.')?;
    let device = u8::from_str_radix(device, 16).ok()?;
    let function = u8::from_str_radix(function, 16).ok()?;
    (device <= 31 && function <= 7)
        .then(|| format!("{domain:04x}:{bus:02x}:{device:02x}.{function:x}"))
}
pub(crate) type ModelGpuScope = Option<Vec<ModelGpuDevice>>;

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
/// A missing participating device invalidates the total; it is never a subtotal.
fn total_vram(devices: Option<&[VramDevice]>) -> Option<u64> {
    devices?
        .iter()
        .try_fold(0_u64, |total, device| total.checked_add(device.bytes?))
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
    gpu: linux::GpuProviders,
}
impl Sampler {
    pub(crate) fn sample(&mut self, devices: Option<&[ModelGpuDevice]>) -> ResourceSample {
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
            sample.vram_devices = devices.map(|devices| self.gpu.sample(devices));
        }
        #[cfg(not(target_os = "linux"))]
        {
            sample.vram_devices = devices.map(|devices| {
                devices
                    .iter()
                    .map(|device| VramDevice {
                        name: device.name.clone(),
                        pci_bus_id: device.pci_bus_id.clone(),
                        bytes: None,
                        source: None,
                    })
                    .collect()
            });
        }
        sample.vram_bytes = total_vram(sample.vram_devices.as_deref());
        if sample.vram_bytes.is_some() {
            sample.vram_source = Some("model_devices");
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
mod linux;

#[cfg(test)]
mod tests {
    //! Scope semantics do not depend on a live GPU or GPU utilization.
    use super::*;
    #[test]
    fn mixed_gpu_totals_never_hide_a_missing_participating_device() {
        let mut devices = vec![
            VramDevice {
                name: "AMD".into(),
                pci_bus_id: Some("0000:28:00.0".into()),
                bytes: Some(4096),
                source: Some("test"),
            },
            VramDevice {
                name: "NVIDIA".into(),
                pci_bus_id: Some("0000:25:00.0".into()),
                bytes: None,
                source: None,
            },
        ];
        assert_eq!(total_vram(Some(&devices)), None);
        devices[1].bytes = Some(8192);
        assert_eq!(total_vram(Some(&devices)), Some(12288));
        assert_eq!(
            total_vram(Some(&devices[..1])),
            Some(4096),
            "only the model's selected device contributes"
        );
    }
    #[test]
    fn pci_identity_is_canonical_and_rejects_paths() {
        assert_eq!(pci_id("00000000:25:00.0").as_deref(), Some("0000:25:00.0"));
        assert_eq!(pci_id("0000:2A:1F.7").as_deref(), Some("0000:2a:1f.7"));
        for id in ["../../device", "0:25:20.0", "0:25:00.8", "Metal"] {
            assert!(pci_id(id).is_none());
        }
    }
    #[test]
    fn cpu_only_scope_excludes_all_registered_gpus_and_unknown_scope_stays_unknown() {
        let mut sampler = Sampler::default();
        let cpu = sampler.sample(Some(&[]));
        assert_eq!(cpu.vram_bytes, Some(0));
        assert!(cpu.vram_devices.expect("known CPU scope").is_empty());
        let unknown = sampler.sample(None);
        assert!(unknown.vram_bytes.is_none());
        assert!(unknown.vram_devices.is_none());
    }
}
