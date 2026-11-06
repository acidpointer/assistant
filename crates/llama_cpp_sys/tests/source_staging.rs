//! Verify snapshot freshness and interrupted publication without a native build.
#[path = "../source_staging.rs"]
mod source_staging;

use std::{
    fs,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, UNIX_EPOCH},
};

struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "native-staging-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&root).expect("test directory");
        Self(root)
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).expect("remove test directory");
    }
}

#[test]
fn unchanged_files_keep_mtime_and_removed_patch_files_disappear() {
    let scratch = Scratch::new();
    let old = scratch.0.join("llama.cpp");
    let candidate = scratch.0.join("candidate");
    fs::create_dir_all(old.join("shaders")).expect("old source");
    fs::create_dir_all(candidate.join("shaders")).expect("candidate source");
    for root in [&old, &candidate] {
        fs::write(root.join("shaders/kernel.comp"), "unchanged").expect("shader fixture");
        fs::write(
            root.join("backend.cpp"),
            if root == &old { "old" } else { "new" },
        )
        .expect("C++ fixture");
    }
    fs::write(old.join("removed.cpp"), "removed codec").expect("old codec");
    let time = UNIX_EPOCH + Duration::from_secs(1_000_000);
    for name in ["shaders/kernel.comp", "backend.cpp"] {
        fs::File::options()
            .write(true)
            .open(old.join(name))
            .expect("open old fixture")
            .set_times(fs::FileTimes::new().set_modified(time))
            .expect("set old mtime");
    }
    fs::File::options()
        .write(true)
        .open(candidate.join("backend.cpp"))
        .expect("copied C++ fixture")
        .set_times(fs::FileTimes::new().set_modified(time))
        .expect("simulate a copy preserving old timestamps");
    source_staging::publish(&candidate, &old).expect("publish snapshot");
    assert_eq!(
        fs::metadata(old.join("shaders/kernel.comp"))
            .expect("shader metadata")
            .modified()
            .expect("shader mtime"),
        time
    );
    assert_ne!(
        fs::metadata(old.join("backend.cpp"))
            .expect("C++ metadata")
            .modified()
            .expect("C++ mtime"),
        time
    );
    assert_eq!(
        fs::read_to_string(old.join("backend.cpp")).expect("updated C++"),
        "new"
    );
    assert!(!old.join("removed.cpp").exists());
}

#[test]
fn interrupted_directory_replacement_is_recovered() {
    let scratch = Scratch::new();
    let backup = scratch.0.join(".llama.cpp.previous");
    let candidate = scratch.0.join("candidate");
    fs::create_dir(&backup).expect("interrupted backup");
    fs::write(backup.join("same.h"), "same").expect("backup file");
    let old_time = fs::metadata(backup.join("same.h"))
        .expect("old metadata")
        .modified()
        .expect("old time");
    fs::create_dir(&candidate).expect("new snapshot");
    fs::write(candidate.join("same.h"), "same").expect("new file");
    let destination = scratch.0.join("llama.cpp");
    source_staging::publish(&candidate, &destination).expect("recover and publish");
    assert_eq!(
        fs::metadata(destination.join("same.h"))
            .expect("new metadata")
            .modified()
            .expect("new time"),
        old_time
    );
    assert!(!backup.exists());
}

#[test]
fn incomplete_pristine_copy_is_not_accepted() {
    let scratch = Scratch::new();
    let source = scratch.0.join("source");
    fs::create_dir(&source).expect("source fixture");
    fs::write(source.join("CMakeLists.txt"), "stub").expect("partial fixture");
    assert!(source_staging::copy_tree(&source, &scratch.0.join("candidate")).is_err());
}
