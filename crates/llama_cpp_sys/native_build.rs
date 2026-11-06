//! Track effective CMake settings and reuse immutable local native bundles.
//! Every mutable CMake tree and published bundle stays in its writer's OUT_DIR.
//! Shims and bindgen remain per Cargo configuration. Unsupported cache targets
//! retain the normal CMake path; missing compiler caches never change flags.

use std::{
    collections::BTreeMap,
    env,
    ffi::{OsStr, OsString},
    path::{Path, PathBuf},
};

/// CMake builder with explicit definitions available for native cache identity.
pub struct Config {
    inner: cmake::Config,
    source: PathBuf,
    defines: BTreeMap<OsString, OsString>,
    output: Option<PathBuf>,
    profile: String,
    cache_eligible: bool,
}

impl std::fmt::Debug for Config {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Config")
            .field("source", &self.source)
            .field("profile", &self.profile)
            .field("output", &self.output)
            .finish_non_exhaustive()
    }
}

impl Config {
    /// Create a native build from a verified patched source tree.
    pub fn new(source: &Path) -> Self {
        Self {
            inner: cmake::Config::new(source),
            source: source.to_owned(),
            defines: BTreeMap::new(),
            output: None,
            profile: String::new(),
            cache_eligible: true,
        }
    }
    /// Apply and track a CMake definition; last value wins in both representations.
    pub fn define(&mut self, key: impl AsRef<OsStr>, value: impl AsRef<OsStr>) -> &mut Self {
        self.defines
            .insert(key.as_ref().to_owned(), value.as_ref().to_owned());
        self.inner.define(key, value);
        self
    }
    /// Supply a default without replacing an explicitly forwarded option.
    pub fn define_default(&mut self, key: &str, value: &str) -> &mut Self {
        if !self.defines.contains_key(OsStr::new(key)) {
            self.define(key, value);
        }
        self
    }
    /// Select the mutable per-OUT_DIR build tree.
    pub fn out_dir(&mut self, value: &Path) -> &mut Self {
        self.output = Some(value.to_owned());
        self.inner.out_dir(value);
        self
    }
    /// Select native optimization independently of the Rust profile.
    pub fn profile(&mut self, value: &str) -> &mut Self {
        self.profile = value.to_owned();
        self.inner.profile(value);
        self
    }
    /// Select the generator.
    pub fn generator(&mut self, value: &str) -> &mut Self {
        self.cache_eligible &= value == "Ninja";
        self.inner.generator(value);
        self
    }
    /// Pass a native build scheduling argument.
    pub fn build_arg(&mut self, value: impl AsRef<OsStr>) -> &mut Self {
        self.cache_eligible &= value
            .as_ref()
            .to_str()
            .and_then(|value| value.strip_prefix("-j"))
            .is_some_and(|jobs| jobs.parse::<usize>().is_ok_and(|n| n > 0));
        self.inner.build_arg(value);
        self
    }
    /// Control CMake verbosity.
    pub fn very_verbose(&mut self, value: bool) -> &mut Self {
        self.inner.very_verbose(value);
        self
    }
    /// Honor configuration changes on every build-script rerun.
    pub fn always_configure(&mut self, value: bool) -> &mut Self {
        self.inner.always_configure(value);
        self
    }
    /// Set Windows CRT behavior. Such builds currently bypass bundle reuse.
    pub fn static_crt(&mut self, value: bool) -> &mut Self {
        self.cache_eligible = false;
        self.inner.static_crt(value);
        self
    }
    /// Add platform-specific C flags; these targets bypass bundle reuse.
    pub fn cflag(&mut self, value: impl AsRef<OsStr>) -> &mut Self {
        self.cache_eligible = false;
        self.inner.cflag(value);
        self
    }
    /// Add platform-specific C++ flags; these targets bypass bundle reuse.
    pub fn cxxflag(&mut self, value: impl AsRef<OsStr>) -> &mut Self {
        self.cache_eligible = false;
        self.inner.cxxflag(value);
        self
    }
    /// Set platform-specific environment; these targets bypass bundle reuse.
    pub fn env(&mut self, key: impl AsRef<OsStr>, value: impl AsRef<OsStr>) -> &mut Self {
        self.cache_eligible = false;
        self.inner.env(key, value);
        self
    }
    /// Build or import matching native libraries, returning the active link root.
    pub fn build(&mut self) -> PathBuf {
        self.try_build().unwrap_or_else(|error| {
            eprintln!("llama_cpp_sys native build: {error}");
            std::process::exit(1)
        })
    }
    fn try_build(&mut self) -> Result<PathBuf, String> {
        let context = match &self.output {
            Some(output) if self.cache_eligible => {
                crate::native_cache::prepare(&self.source, output, &self.profile, &self.defines)?
            }
            _ => None,
        };
        if let Some(context) = &context {
            match context.restore() {
                Ok(Some(root)) => {
                    println!(
                        "cargo:warning=Reused verified native libraries ({})",
                        context.short_key()
                    );
                    return Ok(root);
                }
                Ok(None) => {}
                Err(error) => println!("cargo:warning=Native cache rejected; rebuilding: {error}"),
            }
        }
        let root = self.inner.build();
        if let Some(context) = context {
            context.publish(&root)?;
        }
        Ok(root)
    }
}

/// Resolve a compiler cache explicitly or from PATH, without installing tools.
pub fn compiler_launcher() -> Result<Option<PathBuf>, String> {
    for name in [
        "LLAMA_COMPILER_CACHE",
        "LLAMA_NO_SCCACHE",
        "SCCACHE_PATH",
        "PATH",
    ] {
        println!("cargo:rerun-if-env-changed={name}");
    }
    if let Some(value) = env::var_os("LLAMA_COMPILER_CACHE") {
        if value == "off" {
            return Ok(None);
        }
        return resolve_program(&value)
            .map(Some)
            .ok_or_else(|| "LLAMA_COMPILER_CACHE is not an executable file on PATH".to_owned());
    }
    if env::var("LLAMA_NO_SCCACHE").as_deref() == Ok("1") {
        return Ok(None);
    }
    if let Some(value) = env::var_os("SCCACHE_PATH") {
        return resolve_program(&value)
            .map(Some)
            .ok_or_else(|| "SCCACHE_PATH is not an executable file".to_owned());
    }
    Ok(["sccache", "ccache"]
        .into_iter()
        .find_map(|name| resolve_program(OsStr::new(name))))
}

/// Find a program, allowing an absolute/relative explicit path.
pub fn resolve_program(name: &OsStr) -> Option<PathBuf> {
    let path = Path::new(name);
    if path.components().count() > 1 && path.is_file() {
        return path.canonicalize().ok();
    }
    env::var_os("PATH")
        .and_then(|paths| {
            env::split_paths(&paths)
                .map(|dir| dir.join(name))
                .find(|p| p.is_file())
        })
        .and_then(|p| p.canonicalize().ok())
}

/// Apply an optional compiler cache to native Unix shims without mutating the
/// build process environment or overriding user-supplied compiler wrappers.
pub fn cache_shims(build: &mut cc::Build, target: &str, output: &Path) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::{
            fs,
            os::unix::{ffi::OsStrExt, fs::PermissionsExt},
        };
        if env::var("HOST").as_deref() != Ok(target)
            || env::vars_os().any(|(name, _)| {
                let name = name.to_string_lossy();
                name == "CC"
                    || name == "CXX"
                    || name == "RUSTC_WRAPPER"
                    || name.starts_with("CC_")
                    || name.starts_with("CXX_")
            })
        {
            return Ok(());
        }
        let Some(launcher) = compiler_launcher()? else {
            return Ok(());
        };
        let compiler = build.get_compiler();
        if !compiler.cc_env().is_empty() {
            return Ok(());
        }
        let quote = |path: &Path| {
            let mut value = vec![b'\''];
            for byte in path.as_os_str().as_bytes() {
                if *byte == b'\'' {
                    value.extend_from_slice(b"'\"'\"'");
                } else {
                    value.push(*byte);
                }
            }
            value.push(b'\'');
            value
        };
        let mut script = b"#!/bin/sh\nexec ".to_vec();
        script.extend(quote(&launcher));
        script.push(b' ');
        script.extend(quote(compiler.path()));
        script.extend_from_slice(b" \"$@\"\n");
        let path = output.join("shim-compiler-cache.sh");
        if !path.is_file()
            || fs::read(&path).map_err(|e| format!("read shim cache wrapper: {e}"))? != script
        {
            fs::write(&path, script).map_err(|e| format!("write shim cache wrapper: {e}"))?;
        }
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700))
            .map_err(|e| format!("set shim wrapper permissions: {e}"))?;
        build.compiler(path);
    }
    #[cfg(not(unix))]
    let _ = (build, target, output);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn library_defaults_preserve_explicit_tool_requests() {
        let mut config = Config::new(Path::new("."));
        config.define("LLAMA_BUILD_TOOLS", "ON");
        config.define_default("LLAMA_BUILD_TOOLS", "OFF");
        assert_eq!(
            config.defines.get(OsStr::new("LLAMA_BUILD_TOOLS")),
            Some(&OsString::from("ON"))
        );
    }
}
