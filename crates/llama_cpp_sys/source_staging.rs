//! Publish complete patched snapshots without invalidating unchanged files.
//! The build script is the sole writer of its OUT_DIR. Candidates are independent
//! copies: patching cannot mutate the pin or an active snapshot through hardlinks.
//! Publication preserves byte-identical files' mtimes and uses directory renames;
//! an interrupted replacement is repaired before the next CMake invocation.

use std::{fs, path::Path, time::SystemTime};

/// Update timestamps without making Unix files writable to other users.
fn set_modified(path: &Path, time: SystemTime) -> Result<(), String> {
    let times = fs::FileTimes::new().set_modified(time);
    #[cfg(unix)]
    {
        fs::File::open(path)
            .and_then(|file| file.set_times(times))
            .map_err(|e| format!("set native source mtime: {e}"))
    }
    #[cfg(not(unix))]
    {
        let permissions = fs::metadata(path)
            .map_err(|e| format!("read native source permissions: {e}"))?
            .permissions();
        let readonly = permissions.readonly();
        if readonly {
            let mut writable = permissions.clone();
            writable.set_readonly(false);
            fs::set_permissions(path, writable)
                .map_err(|e| format!("prepare source attributes: {e}"))?;
        }
        let result = fs::File::options()
            .write(true)
            .open(path)
            .and_then(|file| file.set_times(times));
        if readonly {
            fs::set_permissions(path, permissions)
                .map_err(|e| format!("restore source attributes: {e}"))?;
        }
        result.map_err(|e| format!("set native source mtime: {e}"))
    }
}

/// Copy pristine inputs into a fresh candidate. Errors leave no accepted snapshot.
pub fn copy_tree(source: &Path, destination: &Path) -> Result<(), String> {
    if destination.exists() {
        fs::remove_dir_all(destination).map_err(|e| format!("clear source candidate: {e}"))?;
    }
    copy_contents(source, destination)?;
    for required in [
        "CMakeLists.txt",
        "ggml/src/ggml-version.h.in",
        "src/llama-version.h.in",
    ] {
        if !destination.join(required).is_file() {
            return Err(format!("native source candidate is missing {required}"));
        }
    }
    Ok(())
}

/// Recursively copy regular files/directories; refuse symlinks and special files.
fn copy_contents(source: &Path, destination: &Path) -> Result<(), String> {
    fs::create_dir_all(destination).map_err(|e| format!("create source candidate: {e}"))?;
    for entry in fs::read_dir(source).map_err(|e| format!("read native source: {e}"))? {
        let entry = entry.map_err(|e| format!("read native source entry: {e}"))?;
        // Git metadata is not a CMake input and may reference directories outside
        // the pin. The source identity is obtained separately by build.rs.
        if entry.file_name() == ".git" {
            continue;
        }
        let target = destination.join(entry.file_name());
        let kind = entry
            .file_type()
            .map_err(|e| format!("inspect native source entry: {e}"))?;
        if kind.is_dir() {
            copy_contents(&entry.path(), &target)?;
        } else if kind.is_file() {
            fs::copy(entry.path(), target).map_err(|e| format!("copy native source: {e}"))?;
        } else {
            return Err(format!(
                "unsupported native source entry: {}",
                entry.path().display()
            ));
        }
    }
    Ok(())
}

/// Retain previous mtimes only when the verified candidate's bytes match.
fn preserve_times(candidate: &Path, previous: &Path) -> Result<(), String> {
    for entry in fs::read_dir(candidate).map_err(|e| format!("read candidate timestamps: {e}"))? {
        let entry = entry.map_err(|e| format!("read candidate timestamp entry: {e}"))?;
        let old = previous.join(entry.file_name());
        let kind = entry
            .file_type()
            .map_err(|e| format!("inspect candidate timestamp entry: {e}"))?;
        if kind.is_dir() {
            preserve_times(&entry.path(), &old)?;
        } else if kind.is_file() && old.is_file() {
            let metadata =
                fs::symlink_metadata(&old).map_err(|e| format!("inspect previous source: {e}"))?;
            if metadata.is_file()
                && metadata.len()
                    == entry
                        .metadata()
                        .map_err(|e| format!("inspect candidate: {e}"))?
                        .len()
                && fs::read(&old).map_err(|e| format!("read previous source: {e}"))?
                    == fs::read(entry.path()).map_err(|e| format!("read candidate source: {e}"))?
            {
                set_modified(
                    &entry.path(),
                    metadata
                        .modified()
                        .map_err(|e| format!("read source mtime: {e}"))?,
                )?;
            } else {
                // Some copy implementations preserve old source timestamps.
                // Changed bytes must become new dependencies even on those hosts.
                set_modified(&entry.path(), SystemTime::now())?;
            }
        } else if kind.is_file() {
            set_modified(&entry.path(), SystemTime::now())?;
        }
    }
    Ok(())
}

/// Publish a verified candidate; the caller commits its sentinel only on success.
pub fn publish(candidate: &Path, destination: &Path) -> Result<(), String> {
    let parent = destination
        .parent()
        .ok_or("native snapshot has no parent")?;
    let backup = parent.join(".llama.cpp.previous");
    if backup.exists() {
        if !destination.exists() {
            fs::rename(&backup, destination)
                .map_err(|e| format!("restore interrupted snapshot: {e}"))?;
        } else {
            fs::remove_dir_all(&backup).map_err(|e| format!("remove retired snapshot: {e}"))?;
        }
    }
    // A missing snapshot can still have an old CMake object tree beside it.
    // Treat every file as changed in that case, including on copying hosts
    // that retain source timestamps.
    preserve_times(candidate, destination)?;
    if destination.exists() {
        fs::rename(destination, &backup).map_err(|e| format!("retire native snapshot: {e}"))?;
    }
    fs::rename(candidate, destination).map_err(|e| format!("publish native snapshot: {e}"))?;
    if backup.exists() {
        fs::remove_dir_all(backup).map_err(|e| format!("remove previous native snapshot: {e}"))?;
    }
    Ok(())
}
