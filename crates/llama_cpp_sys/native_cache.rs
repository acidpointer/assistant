//! Reuse immutable static native libraries between equivalent Cargo outputs.
//! Initially restricted to default system-toolchain Linux GNU builds using
//! Ninja and distribution-managed dependencies. Other builds use CMake normally.
//! SHA-256 identifies sources/configuration and verifies every copied archive
//! and external header. Only the current OUT_DIR is written; other outputs are
//! read-only candidates. CMake state, bindgen and Rust-profile shims are not shared.

use ring::digest::{Context, SHA256};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, BTreeSet},
    env,
    ffi::{OsStr, OsString},
    fs,
    io::Read,
    path::{Component, Path, PathBuf},
    process::Command,
};

const SCHEMA: u64 = 1;

/// Private cache identity and publication location for the current Cargo build.
#[derive(Debug)]
pub struct Cache {
    key: String,
    output: PathBuf,
}

/// Hash bytes with a cryptographic content digest, never a modification-time key.
#[cfg(test)]
pub fn hash(bytes: &[u8]) -> String {
    ring::digest::digest(&SHA256, bytes)
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Hash a file without loading an archive into memory.
fn hash_file(path: &Path) -> Result<String, String> {
    let mut file = fs::File::open(path)
        .map_err(|e| format!("read native cache input {}: {e}", path.display()))?;
    let mut digest = Context::new(&SHA256);
    let mut buffer = [0_u8; 65536];
    loop {
        let count = file
            .read(&mut buffer)
            .map_err(|e| format!("hash native input: {e}"))?;
        if count == 0 {
            break;
        }
        digest.update(&buffer[..count]);
    }
    Ok(digest
        .finish()
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}

/// Enumerate files deterministically and reject links/special entries.
fn files(root: &Path) -> Result<Vec<PathBuf>, String> {
    let mut result = Vec::new();
    for entry in fs::read_dir(root)
        .map_err(|e| format!("read native cache directory {}: {e}", root.display()))?
    {
        let entry = entry.map_err(|e| format!("read native cache entry: {e}"))?;
        let kind = entry
            .file_type()
            .map_err(|e| format!("inspect native cache entry: {e}"))?;
        if kind.is_dir() {
            result.extend(files(&entry.path())?);
        } else if kind.is_file() {
            result.push(entry.path());
        } else {
            return Err(format!(
                "non-regular native cache input: {}",
                entry.path().display()
            ));
        }
    }
    result.sort();
    Ok(result)
}

/// Include field boundaries, so concatenation cannot alias distinct settings.
fn field(context: &mut Context, bytes: &[u8]) {
    context.update(&(bytes.len() as u64).to_le_bytes());
    context.update(bytes);
}

/// Key the complete compiled source directories, excluding unbuilt test/model data.
fn source_identity(source: &Path, digest: &mut Context) -> Result<(), String> {
    for name in [
        "CMakeLists.txt",
        "cmake",
        "include",
        "src",
        "ggml",
        "common",
        "vendor",
        "tools/mtmd",
    ] {
        let path = source.join(name);
        let paths = if path.is_dir() {
            files(&path)?
        } else {
            vec![path]
        };
        for file in paths {
            let relative = file
                .strip_prefix(source)
                .map_err(|e| format!("native input path: {e}"))?;
            field(digest, relative.as_os_str().as_encoded_bytes());
            field(digest, hash_file(&file)?.as_bytes());
        }
    }
    Ok(())
}

/// Scheduling affects build resource use, but not the generated native payload.
fn definitions_identity(digest: &mut Context, defines: &BTreeMap<OsString, OsString>) {
    for (key, value) in defines {
        if key == "GGML_VULKAN_SHADER_JOBS" || key == "GGML_VULKAN_SHADER_RULES" {
            continue;
        }
        field(digest, key.as_encoded_bytes());
        field(digest, value.as_encoded_bytes());
    }
}

/// Cache only audited system-toolchain configurations; preserve normal builds elsewhere.
pub fn prepare(
    source: &Path,
    output: &Path,
    profile: &str,
    defines: &BTreeMap<OsString, OsString>,
) -> Result<Option<Cache>, String> {
    println!("cargo:rerun-if-env-changed=LLAMA_NATIVE_CACHE");
    for name in [
        "CC",
        "CXX",
        "AR",
        "CFLAGS",
        "CXXFLAGS",
        "CPPFLAGS",
        "LDFLAGS",
        "CPATH",
        "C_INCLUDE_PATH",
        "CPLUS_INCLUDE_PATH",
        "LIBRARY_PATH",
        "VULKAN_SDK",
        "SDKROOT",
        "CMAKE",
        "CMAKE_TOOLCHAIN_FILE",
        "CMAKE_PREFIX_PATH",
        "CRATE_CC_NO_DEFAULTS",
        "LD_PRELOAD",
        "PKG_CONFIG",
        "PKG_CONFIG_PATH",
        "PKG_CONFIG_LIBDIR",
        "PKG_CONFIG_SYSROOT_DIR",
        "GGML_CPU_ARM_ARCH",
    ] {
        println!("cargo:rerun-if-env-changed={name}");
    }
    if env::var("LLAMA_NATIVE_CACHE").as_deref() == Ok("off") {
        return Ok(None);
    }
    let target = env::var("TARGET").map_err(|e| format!("native cache target: {e}"))?;
    let host = env::var("HOST").map_err(|e| format!("native cache host: {e}"))?;
    let is_on = |name: &str| {
        defines
            .get(OsStr::new(name))
            .is_some_and(|v| v == "ON" || v == "1")
    };
    if target != host
        || !target.ends_with("linux-gnu")
        || profile != "Release"
        || [
            "BUILD_SHARED_LIBS",
            "GGML_NATIVE",
            "GGML_CUDA",
            "GGML_HIP",
            "GGML_BLAS",
            "GGML_OPENCL",
            "GGML_WEBGPU",
            "GGML_BACKEND_DL",
            "LLAMA_BUILD_TOOLS",
            "LLAMA_MPI",
            "LLAMA_LLGUIDANCE",
            "LLAMA_BUILD_TESTS",
            "LLAMA_BUILD_EXAMPLES",
        ]
        .iter()
        .any(|name| is_on(name))
        || env::var("CARGO_CFG_TARGET_FEATURE")
            .is_ok_and(|v| v.split(',').any(|s| s == "crt-static"))
    {
        return Ok(None);
    }
    // These can alter implicit include/toolchain discovery beyond the default
    // system configuration. Until audited, retain ordinary CMake compilation.
    if defines.keys().any(|key| {
        matches!(
            key.to_str(),
            Some(
                "CMAKE_TOOLCHAIN_FILE"
                    | "CMAKE_SYSROOT"
                    | "CMAKE_C_COMPILER"
                    | "CMAKE_CXX_COMPILER"
            )
        )
    }) || env::vars_os().any(|(k, _)| {
        let k = k.to_string_lossy();
        [
            "CC", "CXX", "AR", "CFLAGS", "CXXFLAGS", "CPPFLAGS", "LDFLAGS",
        ]
        .iter()
        .any(|name| k == *name || k.starts_with(&format!("{name}_")))
            || matches!(
                k.as_ref(),
                "CPATH"
                    | "C_INCLUDE_PATH"
                    | "CPLUS_INCLUDE_PATH"
                    | "LIBRARY_PATH"
                    | "VULKAN_SDK"
                    | "SDKROOT"
                    | "CMAKE"
                    | "CRATE_CC_NO_DEFAULTS"
                    | "LD_PRELOAD"
                    | "CMAKE_TOOLCHAIN_FILE"
                    | "CMAKE_PREFIX_PATH"
            )
    }) {
        return Ok(None);
    }
    let mut digest = Context::new(&SHA256);
    field(&mut digest, b"llama-rs-native-bundle-v1");
    field(&mut digest, target.as_bytes());
    field(&mut digest, profile.as_bytes());
    definitions_identity(&mut digest, defines);
    for name in ["CMAKE_C_COMPILER_LAUNCHER", "CMAKE_CXX_COMPILER_LAUNCHER"] {
        if let Some(path) = defines.get(OsStr::new(name)) {
            let path = Path::new(path);
            if !path.is_file() {
                return Ok(None);
            }
            println!("cargo:rerun-if-changed={}", path.display());
            field(&mut digest, hash_file(path)?.as_bytes());
        }
    }
    source_identity(source, &mut digest)?;
    field(
        &mut digest,
        &fs::read(output.join(".llama-src-version"))
            .map_err(|e| format!("read native pin identity: {e}"))?,
    );
    for patch in files(&output.join("patches-active"))? {
        field(
            &mut digest,
            patch
                .file_name()
                .ok_or("native patch has no filename")?
                .as_encoded_bytes(),
        );
        field(&mut digest, hash_file(&patch)?.as_bytes());
    }
    let manifest =
        PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").ok_or("missing manifest directory")?);
    let Some(lock) = manifest
        .ancestors()
        .map(|root| root.join("Cargo.lock"))
        .find(|path| path.is_file())
    else {
        return Ok(None);
    };
    let lock_contents =
        fs::read_to_string(lock).map_err(|e| format!("read native dependency versions: {e}"))?;
    for package in lock_contents.split("[[package]]") {
        if package
            .lines()
            .any(|line| matches!(line, "name = \"cmake\"" | "name = \"cc\""))
        {
            field(&mut digest, package.as_bytes());
        }
    }
    // Version the audited implicit Linux CMake defaults, not Rust helper source
    // or tests. Every explicit native define and all compiled sources are keyed
    // above, while cc/cmake versions cover their implicit flag behavior. The
    // wrapper disables reuse for untracked flags, environments and generators.
    // Bump this recipe version when changing an implicit native build input.
    field(&mut digest, b"system-linux-static-release-cmake-recipe-v1");
    // Package database contents invalidate headers, libraries and compiler support
    // binaries when the system toolchain changes, even at unchanged paths.
    if Path::new("/var/lib/pacman/local").is_dir() {
        println!("cargo:rerun-if-changed=/var/lib/pacman/local");
        for file in files(Path::new("/var/lib/pacman/local"))? {
            field(&mut digest, hash_file(&file)?.as_bytes());
        }
    } else if Path::new("/var/lib/dpkg/status").is_file() {
        println!("cargo:rerun-if-changed=/var/lib/dpkg/status");
        field(
            &mut digest,
            hash_file(Path::new("/var/lib/dpkg/status"))?.as_bytes(),
        );
    } else {
        return Ok(None);
    }
    for name in ["cc", "c++", "ar", "cmake", "ninja"]
        .into_iter()
        .chain(is_on("GGML_VULKAN").then_some("glslc"))
    {
        let Some(tool) = crate::native_build::resolve_program(OsStr::new(name)) else {
            return Ok(None);
        };
        println!("cargo:rerun-if-changed={}", tool.display());
        field(&mut digest, tool.as_os_str().as_encoded_bytes());
        field(&mut digest, hash_file(&tool)?.as_bytes());
    }
    for (key, value) in env::vars_os().collect::<BTreeMap<_, _>>() {
        let name = key.to_string_lossy();
        if (name.starts_with("CMAKE_")
            && name != "CMAKE_BUILD_PARALLEL_LEVEL"
            && name != "CMAKE_VERBOSE")
            || name.starts_with("GGML_")
            || name.starts_with("PKG_CONFIG")
            || name == "CARGO_CFG_TARGET_FEATURE"
        {
            println!("cargo:rerun-if-env-changed={name}");
            field(&mut digest, key.as_encoded_bytes());
            field(&mut digest, value.as_encoded_bytes());
        }
    }
    let key = digest
        .finish()
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    Ok(Some(Cache {
        key,
        output: output.to_owned(),
    }))
}

/// Validate manifest paths before joining them to any local output directory.
fn relative_path(value: &Value) -> Result<PathBuf, String> {
    let raw = value.as_str().ok_or("invalid native bundle path")?;
    let path = PathBuf::from(raw);
    if path.components().next().is_none()
        || path
            .components()
            .any(|c| !matches!(c, Component::Normal(_)))
    {
        return Err("unsafe native bundle relative path".to_owned());
    }
    Ok(path)
}

impl Cache {
    /// Short identity for ordinary Cargo build output.
    pub fn short_key(&self) -> &str {
        &self.key[..12]
    }

    fn candidates(&self) -> Result<Vec<PathBuf>, String> {
        let mut candidates = vec![self.output.join("native-bundles").join(&self.key)];
        // OUT_DIR is <target>/<profile>/build/<crate-hash>/out. Searching the
        // common parent also covers custom profiles and explicit target triples.
        let root = self
            .output
            .ancestors()
            .nth(4)
            .ok_or("unexpected native OUT_DIR layout")?;
        for profile in fs::read_dir(root).map_err(|e| format!("scan native cache profiles: {e}"))? {
            let profile = profile.map_err(|e| format!("scan native cache profile: {e}"))?;
            let build = profile.path().join("build");
            if !build.is_dir() {
                continue;
            }
            for entry in
                fs::read_dir(build).map_err(|e| format!("scan native build outputs: {e}"))?
            {
                let entry = entry.map_err(|e| format!("scan native build output: {e}"))?;
                if entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with("llama_cpp_sys-")
                {
                    candidates.push(entry.path().join("out/native-bundles").join(&self.key));
                }
            }
        }
        candidates.sort();
        candidates.dedup();
        Ok(candidates)
    }

    /// Verify copied archives and external inputs before exposing a link root.
    pub fn restore(&self) -> Result<Option<PathBuf>, String> {
        for bundle in self.candidates()? {
            if !bundle.is_dir() {
                continue;
            }
            let manifest: Value = serde_json::from_slice(
                &fs::read(bundle.join("manifest.json"))
                    .map_err(|e| format!("read native bundle manifest: {e}"))?,
            )
            .map_err(|e| format!("parse native bundle manifest: {e}"))?;
            if manifest["schema"] != SCHEMA || manifest["key"] != self.key {
                continue;
            }
            let external = manifest["external"]
                .as_array()
                .ok_or("missing native external inputs")?;
            for input in external {
                let path = input["path"]
                    .as_str()
                    .ok_or("invalid external native input")?;
                println!("cargo:rerun-if-changed={path}");
                if input["sha256"] != hash_file(Path::new(path))? {
                    return Ok(None);
                }
            }
            let entries = manifest["files"]
                .as_array()
                .ok_or("missing native archives")?;
            if entries.is_empty() {
                return Err("empty native bundle".to_owned());
            }
            let destination = self.output.join("native-import");
            let temporary = self.output.join(".native-import.tmp");
            if temporary.exists() {
                fs::remove_dir_all(&temporary).map_err(|e| format!("clear native import: {e}"))?;
            }
            for entry in entries {
                let relative = relative_path(&entry["path"])?;
                let source = bundle.join(&relative);
                if !fs::symlink_metadata(&source)
                    .map_err(|e| format!("inspect cached archive: {e}"))?
                    .is_file()
                {
                    return Err("cached archive is not a regular file".to_owned());
                }
                let path = temporary.join(&relative);
                fs::create_dir_all(path.parent().ok_or("archive has no parent")?)
                    .map_err(|e| format!("create native import: {e}"))?;
                fs::copy(source, &path).map_err(|e| format!("copy cached archive: {e}"))?;
                if entry["sha256"] != hash_file(&path)? {
                    return Err("native archive checksum mismatch".to_owned());
                }
            }
            // Existing link glue reads this one metadata flag. This private
            // import root is never used as a CMake build/configuration tree.
            fs::create_dir_all(temporary.join("build"))
                .map_err(|e| format!("create import metadata: {e}"))?;
            let openmp = manifest["openmp"]
                .as_bool()
                .ok_or("missing native OpenMP metadata")?;
            fs::write(
                temporary.join("build/CMakeCache.txt"),
                format!(
                    "GGML_OPENMP_ENABLED:INTERNAL={}\n",
                    if openmp { "ON" } else { "OFF" }
                ),
            )
            .map_err(|e| format!("write import metadata: {e}"))?;
            if destination.exists() {
                fs::remove_dir_all(&destination)
                    .map_err(|e| format!("retire native import: {e}"))?;
            }
            fs::rename(temporary, &destination)
                .map_err(|e| format!("publish native import: {e}"))?;
            return Ok(Some(destination));
        }
        Ok(None)
    }

    /// Publish independent archives after a successful normal native build.
    pub fn publish(&self, root: &Path) -> Result<(), String> {
        let store = self.output.join("native-bundles");
        fs::create_dir_all(&store).map_err(|e| format!("create native bundle store: {e}"))?;
        let temporary = store.join(".publishing");
        if temporary.exists() {
            fs::remove_dir_all(&temporary)
                .map_err(|e| format!("clear native bundle candidate: {e}"))?;
        }
        fs::create_dir(&temporary).map_err(|e| format!("create native bundle candidate: {e}"))?;
        let mut archives = Vec::new();
        for name in ["lib", "lib64", "build/common"] {
            let directory = root.join(name);
            if directory.is_dir() {
                for file in files(&directory)? {
                    if file.extension().is_some_and(|e| e == "a") {
                        archives.push(file);
                    }
                }
            }
        }
        let mut entries = Vec::new();
        for archive in archives {
            let relative = archive
                .strip_prefix(root)
                .map_err(|e| format!("native archive prefix: {e}"))?;
            let target = temporary.join(relative);
            fs::create_dir_all(target.parent().ok_or("archive directory missing")?)
                .map_err(|e| format!("create bundle archive directory: {e}"))?;
            fs::copy(&archive, &target).map_err(|e| format!("copy bundle archive: {e}"))?;
            entries.push(json!({"path":relative,"sha256":hash_file(&target)?}));
        }
        if !entries.iter().any(|entry| {
            entry["path"]
                .as_str()
                .is_some_and(|p| p.ends_with("/libllama.a"))
        }) {
            return Err("native build produced no libllama.a for bundle".to_owned());
        }
        let output = Command::new("ninja")
            .args(["-C"])
            .arg(root.join("build"))
            .args(["-t", "deps"])
            .output()
            .map_err(|e| format!("read Ninja input dependencies: {e}"))?;
        if !output.status.success() {
            return Err("Ninja could not enumerate native dependencies".to_owned());
        }
        let mut external = BTreeSet::new();
        for line in String::from_utf8_lossy(&output.stdout).lines() {
            if let Some(name) = line.strip_prefix("    ") {
                let path = PathBuf::from(name);
                if path.is_absolute() && !path.starts_with(&self.output) && path.is_file() {
                    println!("cargo:rerun-if-changed={}", path.display());
                    external.insert(path);
                }
            }
        }
        let external = external
            .into_iter()
            .map(|path| Ok(json!({"path":path,"sha256":hash_file(&path)?})))
            .collect::<Result<Vec<Value>, String>>()?;
        let cache = fs::read_to_string(root.join("build/CMakeCache.txt"))
            .map_err(|e| format!("read native build metadata: {e}"))?;
        let manifest = json!({"schema":SCHEMA,"key":self.key,"files":entries,"external":external,"openmp":cache.lines().any(|s|s=="GGML_OPENMP_ENABLED:INTERNAL=ON")});
        fs::write(
            temporary.join("manifest.json"),
            serde_json::to_vec(&manifest).map_err(|e| format!("serialize native bundle: {e}"))?,
        )
        .map_err(|e| format!("write native bundle manifest: {e}"))?;
        let destination = store.join(&self.key);
        if destination.exists() {
            fs::remove_dir_all(&destination)
                .map_err(|e| format!("retire invalid native bundle: {e}"))?;
        }
        fs::rename(temporary, destination).map_err(|e| format!("publish native bundle: {e}"))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    struct Scratch(PathBuf);
    impl Scratch {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let root = env::temp_dir().join(format!(
                "native-bundle-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir_all(&root).expect("bundle fixture");
            Self(root)
        }
    }
    impl Drop for Scratch {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0).expect("remove bundle fixture");
        }
    }
    fn fixture(root: &Path) -> (Cache, PathBuf) {
        let key = hash(b"matching native configuration");
        let cache = Cache {
            key: key.clone(),
            output: root.join("target/release/build/llama_cpp_sys-new/out"),
        };
        let bundle = root
            .join("target/debug/build/llama_cpp_sys-old/out/native-bundles")
            .join(&key);
        fs::create_dir_all(bundle.join("lib")).expect("cached libraries");
        fs::create_dir_all(&cache.output).expect("new Cargo output");
        fs::write(bundle.join("lib/libllama.a"), b"archive fixture").expect("archive");
        let manifest = json!({"schema":SCHEMA,"key":key,"openmp":true,"external":[],"files":[{"path":"lib/libllama.a","sha256":hash(b"archive fixture")}]});
        fs::write(bundle.join("manifest.json"), manifest.to_string()).expect("bundle manifest");
        (cache, bundle)
    }
    #[test]
    fn matching_bundle_is_verified_and_copied_across_profiles() {
        let scratch = Scratch::new();
        let (cache, bundle) = fixture(&scratch.0);
        let imported = cache.restore().expect("verify bundle").expect("cache hit");
        assert_eq!(
            fs::read(imported.join("lib/libllama.a")).expect("imported archive"),
            b"archive fixture"
        );
        fs::write(imported.join("lib/libllama.a"), b"local mutation").expect("mutate local copy");
        assert_eq!(
            fs::read(bundle.join("lib/libllama.a")).expect("immutable cached archive"),
            b"archive fixture"
        );
        assert!(fs::read_to_string(imported.join("build/CMakeCache.txt"))
            .expect("OpenMP metadata")
            .contains("INTERNAL=ON"));
    }
    #[test]
    fn corrupt_archive_and_unsafe_paths_never_become_link_inputs() {
        let scratch = Scratch::new();
        let (cache, bundle) = fixture(&scratch.0);
        fs::write(bundle.join("lib/libllama.a"), b"corrupt").expect("corruption fixture");
        assert!(cache.restore().is_err());
        assert!(!cache.output.join("native-import").exists());
        assert!(relative_path(&json!("../libllama.a")).is_err());
        assert!(relative_path(&json!("/tmp/libllama.a")).is_err());
    }
    #[test]
    fn external_header_change_invalidates_cached_libraries() {
        let scratch = Scratch::new();
        let (cache, bundle) = fixture(&scratch.0);
        let header = scratch.0.join("system-header.h");
        fs::write(&header, b"old ABI").expect("header");
        let mut manifest: Value =
            serde_json::from_slice(&fs::read(bundle.join("manifest.json")).expect("read manifest"))
                .expect("parse manifest");
        manifest["external"] = json!([{"path":header,"sha256":hash(b"old ABI")}]);
        fs::write(bundle.join("manifest.json"), manifest.to_string()).expect("updated manifest");
        fs::write(header, b"new ABI").expect("header update");
        assert!(cache.restore().expect("header invalidation").is_none());
    }
    #[test]
    fn field_boundaries_distinguish_configuration_changes() {
        let mut a = Context::new(&SHA256);
        field(&mut a, b"ab");
        field(&mut a, b"c");
        let mut b = Context::new(&SHA256);
        field(&mut b, b"a");
        field(&mut b, b"bc");
        assert_ne!(a.finish().as_ref(), b.finish().as_ref());
    }

    #[test]
    fn native_flags_change_identity_while_job_counts_do_not() {
        let signature = |defines: &BTreeMap<OsString, OsString>| {
            let mut digest = Context::new(&SHA256);
            definitions_identity(&mut digest, defines);
            digest.finish().as_ref().to_vec()
        };
        let mut defines = BTreeMap::from([(OsString::from("GGML_RPC"), OsString::from("OFF"))]);
        let initial = signature(&defines);
        defines.insert(
            OsString::from("GGML_VULKAN_SHADER_JOBS"),
            OsString::from("1"),
        );
        assert_eq!(initial, signature(&defines));
        defines.insert(
            OsString::from("GGML_VULKAN_SHADER_JOBS"),
            OsString::from("4"),
        );
        assert_eq!(initial, signature(&defines));
        defines.insert(OsString::from("GGML_RPC"), OsString::from("ON"));
        assert_ne!(initial, signature(&defines));
    }
}
