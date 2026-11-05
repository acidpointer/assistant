//! Feature selection/cache-key regression tests without compiling a codec.
#[path = "../patch_staging.rs"]
mod patch_staging;
use patch_staging::{fingerprint, stage, CodecPatches};
use std::{
    fs,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};

#[derive(Debug)]
struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "assistant-patch-stage-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).expect("unique test scratch directory");
        Self(path)
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).expect("clean test scratch tree");
    }
}
#[test]
fn feature_selection_invalidates_cache_and_removes_stale_codec_sources() {
    let temp = Scratch::new();
    let source = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("patches");
    let staged = temp.0.join("active");
    stage(&source, &staged, CodecPatches::Disabled).expect("base patches");
    let baseline = fingerprint(&staged).expect("base fingerprint");
    assert!(staged
        .join("0011-vulkan-blocking-fence-wait.patch")
        .is_file());
    assert!(!staged.join("0008-pq3-cpu-format.patch").exists());
    assert!(!staged.join("0009-pq3-kv-graph-state.patch").exists());
    stage(&source, &staged, CodecPatches::Pq3Cpu).expect("codec patches");
    assert!(staged.join("0009-pq3-kv-graph-state.patch").is_file());
    assert!(!staged
        .join("0010-pq3-vulkan-cache-attention.patch")
        .exists());
    assert_ne!(baseline, fingerprint(&staged).expect("codec fingerprint"));
    let cpu = fingerprint(&staged).expect("CPU fingerprint");
    stage(&source, &staged, CodecPatches::Pq3Graph).expect("full codec sources");
    assert!(staged
        .join("0011-vulkan-blocking-fence-wait.patch")
        .is_file());
    assert!(staged
        .join("0010-pq3-vulkan-cache-attention.patch")
        .is_file());
    assert_ne!(
        cpu,
        fingerprint(&staged).expect("Vulkan source fingerprint")
    );
    stage(&source, &staged, CodecPatches::Disabled).expect("feature disabled again");
    assert!(!staged
        .join("0010-pq3-vulkan-cache-attention.patch")
        .exists());
    assert_eq!(
        baseline,
        fingerprint(&staged).expect("base fingerprint restored")
    );
    assert!(!staged.join("0008-pq3-cpu-format.patch").exists());
    assert!(!staged.join("0009-pq3-kv-graph-state.patch").exists());
}
#[test]
fn missing_required_patch_and_read_errors_fail_closed() {
    let temp = Scratch::new();
    let source = temp.0.join("missing");
    fs::create_dir(&source).expect("source directory");
    let error =
        stage(&source, &temp.0.join("active"), CodecPatches::Pq3Cpu).expect_err("missing patch");
    assert!(error.contains("required native patch is absent"));
    assert!(fingerprint(&temp.0.join("absent")).is_err());
}
