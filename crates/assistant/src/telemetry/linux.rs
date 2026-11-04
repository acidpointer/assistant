//! Linux process accounting and device-scoped GPU providers. Native placement
//! supplies validated PCI identities; counters never select a device by vendor
//! index or instantaneous utilization. Missing/ambiguous totals fail closed.

use super::{ModelGpuDevice, VramDevice};
use libloading::Library;
use std::{
    collections::BTreeMap,
    ffi::{CString, c_void},
    mem::MaybeUninit,
};

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
/// Optional driver sessions are initialized only for participating NVIDIA PCI IDs.
#[derive(Debug, Default)]
pub(super) struct GpuProviders {
    nvml: Option<Nvml>,
    nvml_attempted: bool,
}
impl GpuProviders {
    pub(super) fn sample(&mut self, devices: &[ModelGpuDevice]) -> Vec<VramDevice> {
        let drm = std::cell::OnceCell::new();
        devices
            .iter()
            .map(|device| {
                let mut sample = VramDevice {
                    name: device.name.clone(),
                    pci_bus_id: device.pci_bus_id.clone(),
                    bytes: None,
                    source: None,
                };
                let Some(pci) = &device.pci_bus_id else {
                    return sample;
                };
                let nvidia = std::fs::read_to_string(format!("/sys/bus/pci/devices/{pci}/vendor"))
                    .is_ok_and(|vendor| vendor.trim() == "0x10de");
                if nvidia {
                    if !self.nvml_attempted {
                        self.nvml = Nvml::open();
                        self.nvml_attempted = true;
                    }
                    sample.bytes = self.nvml.as_ref().and_then(|nvml| nvml.memory(pci));
                    if sample.bytes.is_some() {
                        sample.source = Some("nvml_process_v2");
                    }
                }
                if sample.bytes.is_none() {
                    sample.bytes = drm
                        .get_or_init(read_drm)
                        .as_ref()
                        .and_then(|readings| readings.get(pci))
                        .copied()
                        .flatten();
                    if sample.bytes.is_some() {
                        sample.source = Some("linux_drm_resident_vram");
                    }
                }
                sample
            })
            .collect()
    }
}

/// Duplicate fds share a DRM client ID; sum distinct clients only when shared
/// buffers cannot be counted twice. Never substitute total allocation for residency.
#[derive(Debug, Clone)]
struct DrmClient {
    resident: Option<u64>,
    shared: Option<u64>,
}
fn value<'a>(text: &'a str, key: &str) -> Option<&'a str> {
    text.lines().find_map(|line| {
        let (name, value) = line.split_once(':')?;
        (name == key).then(|| value.trim())
    })
}
fn bytes(value: &str) -> Option<u64> {
    let mut fields = value.split_whitespace();
    let count = fields.next()?.parse::<u64>().ok()?;
    let unit = match fields.next() {
        None => 1,
        Some("KiB") => 1024,
        Some("MiB") => 1024 * 1024,
        _ => return None,
    };
    if fields.next().is_some() {
        return None;
    }
    count.checked_mul(unit)
}
fn drm_client(text: &str) -> Option<(String, Option<u64>, DrmClient)> {
    let pci = super::pci_id(value(text, "drm-pdev")?)?;
    let id = value(text, "drm-client-id").and_then(|id| id.parse().ok());
    // Kernel documentation defines the old amdgpu memory key as a resident alias:
    // https://docs.kernel.org/gpu/drm-usage-stats.html#memory
    let resident = value(text, "drm-resident-vram")
        .and_then(bytes)
        .or_else(|| {
            (value(text, "drm-driver") == Some("amdgpu"))
                .then(|| value(text, "drm-memory-vram").and_then(bytes))
                .flatten()
        });
    Some((
        pci,
        id,
        DrmClient {
            resident,
            shared: value(text, "drm-shared-vram").and_then(bytes),
        },
    ))
}
fn aggregate_clients(clients: &BTreeMap<u64, DrmClient>) -> Option<u64> {
    let total = clients
        .values()
        .try_fold(0_u64, |total, client| total.checked_add(client.resident?))?;
    let resident_clients = clients
        .values()
        .filter(|client| client.resident.is_some_and(|bytes| bytes > 0))
        .collect::<Vec<_>>();
    if resident_clients.len() > 1
        && resident_clients
            .iter()
            .any(|client| client.shared != Some(0))
    {
        return None;
    }
    Some(total)
}

fn read_drm() -> Option<BTreeMap<String, Option<u64>>> {
    use std::io::Read;
    let mut devices: BTreeMap<String, BTreeMap<u64, DrmClient>> = BTreeMap::new();
    let mut unidentified = std::collections::BTreeSet::new();
    for (index, entry) in std::fs::read_dir("/proc/self/fdinfo").ok()?.enumerate() {
        if index >= 4096 {
            return None;
        }
        let path = entry.ok()?.path();
        let file = match std::fs::File::open(path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(_) => return None,
        };
        let mut text = String::new();
        file.take(65537).read_to_string(&mut text).ok()?;
        if text.len() > 65536 {
            return None;
        }
        if let Some((pci, id, client)) = drm_client(&text) {
            if let Some(id) = id {
                devices.entry(pci).or_default().insert(id, client);
            } else {
                unidentified.insert(pci);
            }
        }
    }
    let mut readings = devices
        .into_iter()
        .map(|(pci, clients)| (pci, aggregate_clients(&clients)))
        .collect::<BTreeMap<_, _>>();
    for pci in unidentified {
        readings.insert(pci, None);
    }
    Some(readings)
}

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
#[derive(Debug)]
struct Nvml {
    library: Library,
}
impl Nvml {
    fn open() -> Option<Self> {
        // SAFETY: load the installed management library with its documented C ABI.
        // No symbol or device pointer outlives this owned library handle.
        let library = unsafe { Library::new("libnvidia-ml.so.1") }.ok()?;
        // SAFETY: the versioned symbol has NVML's initialization signature.
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
    fn memory(&self, pci: &str) -> Option<u64> {
        let pci = CString::new(pci).ok()?;
        // SAFETY: the PCI string and output storage live throughout each call.
        // Device handles are obtained from NVML while initialized. Fixed arrays
        // bound output; unsuccessful or incomplete queries never become totals.
        unsafe {
            let get = self
                .library
                .get::<unsafe extern "C" fn(*const std::ffi::c_char, *mut Device) -> Status>(
                    b"nvmlDeviceGetHandleByPciBusId_v2\0",
                )
                .ok()?;
            let mut device = std::ptr::null_mut();
            if get(pci.as_ptr(), &raw mut device) != 0 {
                return None;
            }
            let mut memory = None;
            for name in [
                b"nvmlDeviceGetComputeRunningProcesses_v2\0".as_slice(),
                b"nvmlDeviceGetGraphicsRunningProcesses_v2\0".as_slice(),
            ] {
                let query = self.library.get::<Processes>(name).ok()?;
                let mut entries = [Process::default(); 128];
                let mut length = 128;
                let status = query(device, &raw mut length, entries.as_mut_ptr());
                if status == 3 {
                    continue;
                }
                if status != 0 || length > 128 {
                    return None;
                }
                let mut bytes = 0_u64;
                for process in &entries[..usize::try_from(length).ok()?] {
                    if process.pid == std::process::id() {
                        if process.memory == u64::MAX {
                            return None;
                        }
                        bytes = bytes.checked_add(process.memory)?;
                    }
                }
                // A PID can appear in both compute and graphics accounting.
                memory = Some(memory.map_or(bytes, |previous: u64| previous.max(bytes)));
            }
            memory
        }
    }
}
impl Drop for Nvml {
    fn drop(&mut self) {
        // SAFETY: balances this provider's successful init. No device handles or
        // borrowed symbols survive this call or library destruction.
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
    //! Driver parsing fixtures require no GPU and distinguish allocation/residency.
    use super::*;
    #[test]
    fn resident_alias_units_and_client_deduplication() {
        let fixture = "drm-driver: amdgpu\ndrm-pdev: 0000:28:00.0\ndrm-client-id: 7\ndrm-total-vram: 999 MiB\ndrm-memory-vram: 4 KiB\n";
        let (pci, id, client) = drm_client(fixture).expect("AMD resident alias");
        assert_eq!(pci, "0000:28:00.0");
        assert_eq!(client.resident, Some(4096));
        let mut clients = BTreeMap::new();
        clients.insert(id.expect("client identity"), client.clone());
        clients.insert(7, client);
        assert_eq!(
            aggregate_clients(&clients),
            Some(4096),
            "duplicate fds are one client"
        );
        clients.insert(
            8,
            DrmClient {
                resident: Some(1024),
                shared: Some(0),
            },
        );
        assert_eq!(
            aggregate_clients(&clients),
            None,
            "unknown sharing cannot be added precisely"
        );
        clients.get_mut(&7).expect("client").shared = Some(0);
        assert_eq!(aggregate_clients(&clients), Some(5120));
        assert!(
            drm_client(&fixture.replace("drm-memory-vram: 4 KiB", ""))
                .expect("identity")
                .2
                .resident
                .is_none()
        );
    }
    #[test]
    fn explicit_residency_precedes_legacy_alias() {
        let text = "drm-driver: amdgpu\ndrm-pdev: 0000:28:00.0\ndrm-client-id: 1\ndrm-resident-vram: 2 MiB\ndrm-memory-vram: 4 MiB\n";
        assert_eq!(
            drm_client(text).expect("client").2.resident,
            Some(2 * 1024 * 1024)
        );
        assert_eq!(bytes("1 GiB"), None);
        assert_eq!(kilobytes("Rss: 12 kB\n", "Rss:"), Some(12288));
    }
}
