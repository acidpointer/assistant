//! Pure build configuration and artifact-integrity tests, without compiling C++.
// The included build-script helpers also contain entry points exercised by
// ordinary Cargo builds, rather than this isolated unit harness.
#![allow(dead_code)]
#[path = "../native_build.rs"]
mod native_build;
#[path = "../native_cache.rs"]
mod native_cache;

/// Exercise the optional shim wrapper with a delegating cache fixture, including
/// spaces and single quotes in paths. Environment changes stay in a child process.
#[cfg(all(unix, target_os = "linux"))]
#[test]
fn shim_launcher_quotes_paths_and_forwards_arguments() {
    use std::{fs, os::unix::fs::PermissionsExt, path::PathBuf, process::Command};
    if let Some(root) = std::env::var_os("SHIM_CACHE_TEST_ROOT") {
        let root = PathBuf::from(root);
        let host = std::env::var("HOST").expect("child host triple");
        let mut build = cc::Build::new();
        build
            .cpp(true)
            .target(&host)
            .host(&host)
            .opt_level(0)
            .debug(false)
            .cargo_metadata(false)
            .out_dir(&root)
            .file(root.join("input with 'quotes.cpp"));
        native_build::cache_shims(&mut build, &host, &root).expect("configure shim launcher");
        build
            .try_compile("cache_smoke")
            .expect("compile through shim launcher");
        return;
    }
    struct Scratch(PathBuf);
    impl Drop for Scratch {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0).expect("remove shim fixture");
        }
    }
    let root = std::env::temp_dir().join(format!("shim cache 'fixture-{}", std::process::id()));
    fs::create_dir(&root).expect("create shim fixture");
    let _scratch = Scratch(root.clone());
    let launcher = root.join("cache helper 'quoted");
    fs::write(
        &launcher,
        b"#!/bin/sh\nprintf 'invoked\\n' >> \"$SHIM_CACHE_LOG\"\nexec \"$@\"\n",
    )
    .expect("write cache fixture");
    fs::set_permissions(&launcher, fs::Permissions::from_mode(0o700)).expect("cache permissions");
    fs::write(
        root.join("input with 'quotes.cpp"),
        b"extern \"C\" int cache_smoke() { return 42; }\n",
    )
    .expect("write C++ fixture");
    let rustc = Command::new("rustc")
        .arg("-vV")
        .output()
        .expect("query host triple");
    let version = String::from_utf8(rustc.stdout).expect("rustc host output");
    let host = version
        .lines()
        .find_map(|line| line.strip_prefix("host: "))
        .expect("host triple");
    let mut child = Command::new(std::env::current_exe().expect("test binary"));
    child
        .args([
            "--exact",
            "shim_launcher_quotes_paths_and_forwards_arguments",
        ])
        .env("SHIM_CACHE_TEST_ROOT", &root)
        .env("HOST", host)
        .env("LLAMA_COMPILER_CACHE", &launcher)
        .env("SHIM_CACHE_LOG", root.join("invocations.txt"));
    for (name, _) in std::env::vars_os() {
        let text = name.to_string_lossy();
        if text == "CC"
            || text == "CXX"
            || text == "RUSTC_WRAPPER"
            || text.starts_with("CC_")
            || text.starts_with("CXX_")
        {
            child.env_remove(name);
        }
    }
    let output = child.output().expect("run shim compiler child");
    assert!(
        output.status.success(),
        "shim child: {} {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(root.join("libcache_smoke.a").is_file());
    assert!(fs::read_to_string(root.join("invocations.txt"))
        .expect("cache invocation log")
        .contains("invoked"));
}
