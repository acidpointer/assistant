//! Exercise the build-time patch verifier against overlapping changes.
#[path = "../patch_verification.rs"]
mod patch_verification;

#[path = "../patch_staging.rs"]
mod patch_staging;

/// Compare both engines on the real maintained series, including PQ3's new file
/// and overlaps with the mandatory context-safety patches. Only touched files
/// are copied; the pinned submodule remains read-only.
#[test]
fn real_patch_series_matches_cli_at_each_increment() {
    use patch_apply::Patch;
    use std::{collections::BTreeSet, fs, path::PathBuf, process::Command};

    struct Fixture(PathBuf);
    impl Drop for Fixture {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0).expect("remove real-series fixture");
        }
    }
    let crate_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let source = crate_root.join("llama.cpp");
    let root = std::env::temp_dir().join(format!("llama-real-patches-{}", std::process::id()));
    let _fixture = Fixture(root.clone());
    for (name, selection) in [
        ("default", patch_staging::CodecPatches::Disabled),
        ("pq3", patch_staging::CodecPatches::Pq3Reference),
    ] {
        let selected = root.join(name).join("selected");
        let prefix = root.join(name).join("prefix");
        let destination = root.join(name).join("destination");
        fs::create_dir_all(&prefix).expect("prefix directory");
        fs::create_dir_all(&destination).expect("destination directory");
        patch_staging::stage(&crate_root.join("patches"), &selected, selection)
            .expect("stage real patches");
        assert!(!patch_staging::fingerprint(&selected)
            .expect("selected fingerprint")
            .is_empty());
        let mut paths: Vec<_> = fs::read_dir(&selected)
            .expect("selected patches")
            .map(|entry| entry.expect("patch entry").path())
            .collect();
        paths.sort();
        let mut touched = BTreeSet::new();
        for path in &paths {
            let text = fs::read_to_string(path).expect("patch contents");
            for patch in Patch::from_multiple(&text).expect("parse real patch") {
                if patch.old.path.as_ref() == "/dev/null" {
                    continue;
                }
                let (_, relative) = patch
                    .old
                    .path
                    .split_once('/')
                    .expect("relative source path");
                if touched.insert(relative.to_owned()) {
                    let target = destination.join(relative);
                    fs::create_dir_all(target.parent().expect("target parent"))
                        .expect("target directories");
                    fs::copy(source.join(relative), &target).expect("copy pristine touched file");
                }
            }
        }
        for path in paths {
            let output = Command::new("patch")
                .args(["-p1", "--batch", "--forward", "--fuzz=0", "-i"])
                .arg(&path)
                .current_dir(&destination)
                .output()
                .expect("CLI patch engine must be installed for native build verification");
            assert!(
                output.status.success(),
                "{}: {} {}",
                path.display(),
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            fs::copy(
                &path,
                prefix.join(path.file_name().expect("patch filename")),
            )
            .expect("extend verified patch prefix");
            assert!(
                patch_verification::patches_match_new_side(&prefix, &source, &destination)
                    .expect("verify CLI output using the Rust engine"),
                "{}",
                path.display()
            );
        }
    }
}
