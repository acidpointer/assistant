//! Select and fingerprint build-time patches before copying upstream sources.
//! The selection is explicit and ordered; missing files and I/O failures are
//! errors. No native state or process environment is mutated by this module.
use std::{fs, path::Path};

/// Experimental native source selection, independent of backend selection.
#[derive(Debug, Clone, Copy)]
pub enum CodecPatches {
    /// Existing mandatory safety patches only.
    Disabled,
    /// Add the PQ3 CPU reference; does not enable inference support.
    Pq3Reference,
}

const REQUIRED: &[&str] = &[
    "0003-exact-speculative-state.patch",
    "0004-exact-decode-lifecycle-hooks.patch",
    "0005-fail-closed-eagle3-process.patch",
    "0006-runtime-state-safety.patch",
    "0007-vulkan-resource-lifecycle.patch",
];

/// Stage exactly the selected patches; no unlisted patch can become active.
/// # Errors
/// Reports missing required patches and failed directory/copy operations.
pub fn stage(source: &Path, destination: &Path, codec: CodecPatches) -> Result<(), String> {
    if destination.exists() {
        fs::remove_dir_all(destination).map_err(|e| format!("clear staged patches: {e}"))?;
    }
    fs::create_dir_all(destination).map_err(|e| format!("create staged patches: {e}"))?;
    let experimental = match codec {
        CodecPatches::Disabled => None,
        CodecPatches::Pq3Reference => Some("0008-pq3-cpu-format.patch"),
    };
    for name in REQUIRED.iter().copied().chain(experimental) {
        let input = source.join(name);
        if !input.is_file() {
            return Err(format!(
                "required native patch is absent: {}",
                input.display()
            ));
        }
        fs::copy(&input, destination.join(name))
            .map_err(|e| format!("stage native patch {name}: {e}"))?;
    }
    Ok(())
}

/// Fingerprint selected names and bytes, including boundaries between files.
/// This is a deterministic cache key, not a cryptographic integrity claim.
/// # Errors
/// Reports directory/entry/file reads; never substitutes an empty fingerprint.
pub fn fingerprint(directory: &Path) -> Result<String, String> {
    let mut paths = fs::read_dir(directory)
        .map_err(|e| format!("read staged patches: {e}"))?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("read staged patch entry: {e}"))?;
    paths.retain(|p| p.extension().is_some_and(|e| e == "patch"));
    paths.sort();
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for path in paths {
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or("invalid patch filename")?;
        let bytes = fs::read(&path).map_err(|e| format!("read patch {name}: {e}"))?;
        for byte in name.bytes().chain([0]).chain(bytes).chain([0]) {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x100_0000_01b3);
        }
    }
    Ok(format!("{hash:016x}"))
}
