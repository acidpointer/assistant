//! Private local filesystem ownership. The lease is reference-counted by every
//! database worker so dropping an async owner cannot unlock a still-open store.
//! Same-UID hostile processes and root are outside the isolation contract.

use super::super::{MemoryConfig, MemoryError, MemoryResult};
use std::{
    fmt,
    fs::{self, File, OpenOptions},
    os::unix::{
        fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt},
        io::AsRawFd,
    },
    path::{Component, Path, PathBuf},
    sync::Arc,
};

struct Lease {
    path: PathBuf,
    _lock: File,
}
impl fmt::Debug for Lease {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("PrivateDirectory([redacted])")
    }
}

#[derive(Debug, Clone)]
pub(super) struct PrivateDirectory(Arc<Lease>);
impl PrivateDirectory {
    pub(super) fn open(config: &MemoryConfig) -> MemoryResult<Self> {
        let path = &config.directory;
        if path.as_os_str().is_empty() {
            return Err(MemoryError::InvalidInput("data directory"));
        }
        if !path.exists() {
            let mut builder = fs::DirBuilder::new();
            builder.recursive(true).mode(0o700).create(path)?;
        }
        let metadata = fs::symlink_metadata(path)?;
        // SAFETY: getuid has no pointer arguments and no ownership side effects.
        let uid = unsafe { libc::getuid() };
        if !metadata.is_dir() || metadata.uid() != uid || metadata.mode() & 0o077 != 0 {
            return Err(MemoryError::Forbidden);
        }
        // NOFOLLOW on individual files does not prevent replacing an ancestor
        // directory. Only the owner and the filesystem-root administrator may
        // own ancestors, and writable shared ancestors must have sticky semantics.
        // Filesystem-root UID also works under user-namespace UID mappings.
        let administrator = fs::metadata("/")?.uid();
        let absolute = if path.is_absolute() {
            path.clone()
        } else {
            std::env::current_dir()?.join(path)
        };
        if absolute
            .components()
            .any(|part| matches!(part, Component::ParentDir))
        {
            return Err(MemoryError::Forbidden);
        }
        for ancestor in absolute.ancestors() {
            let meta = fs::symlink_metadata(ancestor)?;
            if !meta.is_dir()
                || ![uid, administrator].contains(&meta.uid())
                || (meta.mode() & 0o022 != 0 && meta.mode() & 0o1000 == 0)
            {
                return Err(MemoryError::Forbidden);
            }
        }
        let lock = private_file(&path.join("store.lock"))?;
        // SAFETY: the file owns a live descriptor for the duration of this lease.
        let result = unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if result != 0 {
            let e = std::io::Error::last_os_error();
            return if e.kind() == std::io::ErrorKind::WouldBlock {
                Err(MemoryError::Busy)
            } else {
                Err(e.into())
            };
        }
        for name in [
            "memory.sqlite3",
            "memory.sqlite3-wal",
            "memory.sqlite3-shm",
            "deletions.jsonl",
        ] {
            let p = path.join(name);
            if p.exists() || fs::symlink_metadata(&p).is_ok() {
                let _validated = private_file(&p)?;
            }
        }
        let _database = private_file(&path.join("memory.sqlite3"))?;
        let _journal = private_file(&path.join("deletions.jsonl"))?;
        // These entries are created by the host before SQLite opens them. Persist
        // the directory entries as part of the initial durability boundary.
        let canonical = fs::canonicalize(path)?;
        for ancestor in canonical.ancestors() {
            File::open(ancestor)?.sync_all()?;
        }
        Ok(Self(Arc::new(Lease {
            path: canonical,
            _lock: lock,
        })))
    }
    pub(super) fn database(&self) -> PathBuf {
        self.0.path.join("memory.sqlite3")
    }
    pub(super) fn journal(&self) -> PathBuf {
        self.0.path.join("deletions.jsonl")
    }
    pub(super) fn used_bytes(&self) -> MemoryResult<u64> {
        let mut total = 0_u64;
        for entry in fs::read_dir(&self.0.path)? {
            let entry = entry?;
            let meta = entry.metadata()?;
            if meta.is_file() {
                total = total.checked_add(meta.len()).ok_or(MemoryError::Capacity)?;
            }
        }
        Ok(total)
    }
}

pub(super) fn private_file(path: &Path) -> MemoryResult<File> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)?;
    let meta = file.metadata()?;
    // SAFETY: getuid takes no pointers and only reads process credentials.
    let uid = unsafe { libc::getuid() };
    if !meta.is_file()
        || meta.uid() != uid
        || meta.permissions().mode() & 0o077 != 0
        || meta.nlink() != 1
    {
        return Err(MemoryError::Forbidden);
    }
    Ok(file)
}
