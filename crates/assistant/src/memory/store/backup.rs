//! Consistent online backups and quarantined restore candidates. The `SQLite`
//! backup API copies database pages; the writer seals the external deletion
//! journal afterward. Restore never replaces or activates a live store.

use super::super::{MemoryConfig, MemoryError, MemoryId, MemoryResult, Revision};
use super::{
    MemoryHandle, MemoryStore, OwnerCapability,
    database::{Database, decode, revision},
    private_dir::{PrivateDirectory, private_file},
};
use rusqlite::{
    Connection, OpenFlags,
    backup::{Backup, StepResult},
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{Read, Seek, SeekFrom, Write},
    os::unix::fs::DirBuilderExt,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    version: u32,
    store: MemoryId,
    owner: MemoryId,
    commit_sequence: i64,
    deletion_generation: i64,
    database_sha256: String,
    journal_sha256: String,
}

impl MemoryHandle {
    /// Create a private, checksummed backup in a new directory without overwriting.
    /// # Errors
    /// Rejects existing destinations, authorization, timeouts or filesystem errors.
    pub async fn backup(
        &self,
        owner: &OwnerCapability,
        destination: PathBuf,
    ) -> MemoryResult<Option<Revision>> {
        self.authorize(owner)?;
        let directory = tokio::task::spawn_blocking(move || fresh_directory(destination))
            .await
            .map_err(|_| MemoryError::Unavailable)??;
        let target = directory.clone();
        let (sequence, generation, checksum) = self
            .reader()
            .call(move |db| db.copy_backup(&target))
            .await?;
        self.0
            .writer
            .call(move |db| db.seal_backup(&directory, sequence, generation, checksum))
            .await?;
        if sequence == 0 {
            Ok(None)
        } else {
            Ok(Some(revision(sequence)?))
        }
    }

    /// Open a restore candidate reconciled with this store's latest deletion journal.
    ///
    /// The original store remains untouched. The product host must quiesce personal
    /// traffic and establish its output barrier before activating the candidate.
    /// This storage primitive never swaps the application's active owner or sessions.
    /// # Errors
    /// Rejects altered/foreign backups, existing targets, changed deletion state or I/O.
    pub async fn restore_candidate(
        &self,
        owner: &OwnerCapability,
        backup: PathBuf,
        destination: PathBuf,
    ) -> MemoryResult<MemoryStore> {
        self.authorize(owner)?;
        let (journal, generation, quota, job_limit) = self
            .0
            .writer
            .call(|db| {
                db.flush_journal()?;
                let generation: i64 =
                    db.connection
                        .query_row("SELECT deletion_generation FROM meta", [], |r| r.get(0))?;
                let mut file = private_file(&db.directory.journal())?;
                if file.metadata()?.len() > 64 * 1024 * 1024 {
                    return Err(MemoryError::Capacity);
                }
                let mut journal = Vec::new();
                file.read_to_end(&mut journal)?;
                Ok((journal, generation, db.quota, db.pending_job_limit))
            })
            .await?;
        let authority = owner.clone();
        let config = tokio::task::spawn_blocking(move || {
            let backup = PrivateDirectory::open(&MemoryConfig::new(backup))?;
            let mut file = private_file(&backup_path(&backup, "backup.json"))?;
            if file.metadata()?.len() > 4096 {
                return Err(MemoryError::CorruptStore);
            }
            let mut text = String::new();
            file.read_to_string(&mut text)?;
            let manifest: Manifest = decode(&text)?;
            if manifest.version != 1
                || manifest.store != authority.store
                || manifest.owner != authority.principal
            {
                return Err(MemoryError::Forbidden);
            }
            if hash_file(&backup.database())? != manifest.database_sha256
                || hash_file(&backup.journal())? != manifest.journal_sha256
            {
                return Err(MemoryError::CorruptStore);
            }
            let target = fresh_directory(destination.clone())?;
            fs::copy(backup.database(), target.database())?;
            private_file(&target.database())?.sync_all()?;
            let mut target_journal = private_file(&target.journal())?;
            target_journal.set_len(0)?;
            target_journal.write_all(&journal)?;
            target_journal.sync_all()?;
            drop(target);
            MemoryConfig::new(destination)
                .with_quota(quota)?
                .with_pending_job_limit(job_limit)
        })
        .await
        .map_err(|_| MemoryError::Unavailable)??;
        let candidate = MemoryStore::open(config).await?;
        let current: i64 = self
            .reader()
            .call(|db| {
                Ok(db
                    .connection
                    .query_row("SELECT deletion_generation FROM meta", [], |r| r.get(0))?)
            })
            .await?;
        if current != generation {
            candidate.shutdown().await?;
            return Err(MemoryError::Conflict);
        }
        Ok(candidate)
    }
}

impl Database {
    fn copy_backup(&mut self, target: &PrivateDirectory) -> MemoryResult<(i64, i64, String)> {
        let mut destination = Connection::open_with_flags(
            target.database(),
            OpenFlags::SQLITE_OPEN_READ_WRITE
                | OpenFlags::SQLITE_OPEN_NO_MUTEX
                | OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )?;
        let started = Instant::now();
        {
            let backup = Backup::new(&self.connection, &mut destination)?;
            loop {
                if started.elapsed() > Duration::from_secs(30) {
                    return Err(MemoryError::Deadline);
                }
                match backup.step(64)? {
                    StepResult::Done => break,
                    StepResult::More => {}
                    StepResult::Busy | StepResult::Locked => {
                        std::thread::sleep(Duration::from_millis(1));
                    }
                    _ => return Err(MemoryError::Unavailable),
                }
            }
        }
        let snapshot =
            destination.query_row("SELECT commit_seq,deletion_generation FROM meta", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })?;
        destination.close().map_err(|(_, e)| MemoryError::from(e))?;
        private_file(&target.database())?.sync_all()?;
        Ok((snapshot.0, snapshot.1, hash_file(&target.database())?))
    }
    fn seal_backup(
        &mut self,
        target: &PrivateDirectory,
        sequence: i64,
        generation: i64,
        checksum: String,
    ) -> MemoryResult<()> {
        self.flush_journal()?;
        let mut input = private_file(&self.directory.journal())?;
        if input.metadata()?.len() > 64 * 1024 * 1024 {
            return Err(MemoryError::Capacity);
        }
        let mut output = private_file(&target.journal())?;
        output.set_len(0)?;
        output.seek(SeekFrom::Start(0))?;
        std::io::copy(&mut input, &mut output)?;
        output.sync_all()?;
        let manifest = Manifest {
            version: 1,
            store: self.owner.store,
            owner: self.owner.principal,
            commit_sequence: sequence,
            deletion_generation: generation,
            database_sha256: checksum,
            journal_sha256: hash_file(&target.journal())?,
        };
        let mut file = private_file(&backup_path(target, "backup.json"))?;
        file.set_len(0)?;
        file.write_all(&serde_json::to_vec(&manifest)?)?;
        file.sync_all()?;
        fs::File::open(
            target
                .database()
                .parent()
                .ok_or(MemoryError::InvalidInput("backup directory"))?,
        )?
        .sync_all()?;
        Ok(())
    }
}

fn backup_path(directory: &PrivateDirectory, name: &str) -> PathBuf {
    directory.database().with_file_name(name)
}
fn fresh_directory(path: PathBuf) -> MemoryResult<PrivateDirectory> {
    if path.as_os_str().len() > 4096 {
        return Err(MemoryError::InvalidInput("backup path"));
    }
    fs::DirBuilder::new().mode(0o700).create(&path)?;
    PrivateDirectory::open(&MemoryConfig::new(path))
}
fn hash_file(path: &Path) -> MemoryResult<String> {
    let mut file = private_file(path)?;
    let mut digest = Sha256::new();
    let mut buffer = vec![0_u8; 65536];
    loop {
        let size = file.read(&mut buffer)?;
        if size == 0 {
            break;
        }
        digest.update(&buffer[..size]);
    }
    Ok(format!("{:x}", digest.finalize()))
}
