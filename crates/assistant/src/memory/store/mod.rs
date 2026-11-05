//! Bounded storage workers. One writer serializes complete commands; two readers
//! own independent query-only connections. Replies are owned snapshots. Losing
//! a reply does not cancel a queued mutation. Shutdown joins all three threads.

mod backup;
mod database;
mod embeddings;
mod events;
pub use embeddings::{EmbeddingTask, SemanticHit};
mod graph;
mod jobs;
mod migrations;
mod operations;
mod private_dir;
mod purge;
mod worker;

use super::{
    Claim, ClaimInput, Entity, EntityKind, GraphBundle, MemoryConfig, MemoryError, MemoryId,
    MemoryReceipt, MemoryResult, MemoryText, OperationKey, Revision, SearchBundle, Source,
    SuppressionReceipt,
};
use database::Database;
use std::{
    fmt,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    thread,
};
#[cfg(test)]
use tokio::sync::oneshot;
use worker::{Worker, WorkerRole};

/// Host-issued authority. It cannot be deserialized or constructed by a model.
#[derive(Clone)]
pub struct OwnerCapability {
    store: MemoryId,
    principal: MemoryId,
}
impl OwnerCapability {
    /// The owner's graph node; possession of this ID alone grants no access.
    #[must_use]
    pub fn principal(&self) -> MemoryId {
        self.principal
    }
}
impl fmt::Debug for OwnerCapability {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("OwnerCapability([redacted])")
    }
}

#[derive(Debug)]
struct Shared {
    writer: Worker,
    readers: Vec<Worker>,
    next: AtomicUsize,
    closed: AtomicBool,
    authority: OwnerCapability,
}

/// Scoped access to durable memory. Every operation rechecks host authority.
#[derive(Debug, Clone)]
pub struct MemoryHandle(Arc<Shared>);
impl MemoryHandle {
    #[cfg(test)]
    pub(crate) async fn test_writer_barrier(
        &self,
        started: oneshot::Sender<()>,
        release: std::sync::mpsc::Receiver<()>,
    ) -> MemoryResult<()> {
        self.0
            .writer
            .call(move |_| {
                let _started = started.send(());
                release
                    .recv_timeout(std::time::Duration::from_secs(5))
                    .map_err(|_| MemoryError::Deadline)
            })
            .await
    }
    #[cfg(test)]
    pub(crate) fn test_writer_pending(&self) -> usize {
        56 - self.0.writer.ordinary.available_permits()
    }
    #[cfg(test)]
    pub(crate) fn test_writer_critical_pending(&self) -> usize {
        7 - self.0.writer.critical.available_permits()
    }
    #[cfg(test)]
    pub(crate) fn test_settlement_pending(&self) -> bool {
        self.0.writer.test_settlement_pending()
    }
    #[cfg(test)]
    pub(crate) async fn test_limit_database_pages(&self) -> MemoryResult<()> {
        self.0
            .writer
            .critical(|db| {
                let pages: i64 = db
                    .connection
                    .pragma_query_value(None, "page_count", |row| row.get(0))?;
                db.connection.pragma_update(None, "max_page_count", pages)?;
                Ok(())
            })
            .await
    }
    /// Lease one pending extraction job with persistent fence and compute reservation.
    /// # Errors
    /// Rejects unauthorized access, invalid clock, exceeded allowances or storage failure.
    pub async fn claim_extraction_job(
        &self,
        owner: &OwnerCapability,
        utc_millis: i64,
    ) -> MemoryResult<Option<super::ExtractionLease>> {
        self.authorize(owner)?;
        if utc_millis < 0 {
            return Err(MemoryError::InvalidInput("job clock"));
        }
        self.0
            .writer
            .call(move |db| db.claim_extraction_job(utc_millis))
            .await
    }

    /// Apply a validated extraction and complete its attempt in one transaction.
    /// # Errors
    /// Rejects obsolete fences, changed sources, invalid evidence, or exceeded bounds.
    pub async fn finish_extraction_job(
        &self,
        owner: &OwnerCapability,
        lease: super::ExtractionLease,
        changes: super::ExtractionChanges,
        outcome: super::ExtractionOutcome,
        usage: super::MemoryJobUsage,
    ) -> MemoryResult<Vec<Claim>> {
        self.authorize(owner)?;
        if changes.entities.len() > 8 || changes.claims.len() > 4 {
            return Err(MemoryError::InvalidInput("extraction size"));
        }
        for entity in &changes.entities {
            if entity.label.as_str().len() > 512 {
                return Err(MemoryError::InvalidInput("entity label"));
            }
        }
        for claim in &changes.claims {
            graph::validate_bounds(claim)?;
        }
        self.0
            .writer
            .critical(move |db| db.finish_extraction_job(&lease, &changes, outcome, usage))
            .await
    }
    pub(super) fn authorize(&self, owner: &OwnerCapability) -> MemoryResult<()> {
        if self.0.closed.load(Ordering::Acquire) {
            return Err(MemoryError::Unavailable);
        }
        if owner.store != self.0.authority.store || owner.principal != self.0.authority.principal {
            return Err(MemoryError::Forbidden);
        }
        Ok(())
    }
    fn reader(&self) -> &Worker {
        &self.0.readers[self.0.next.fetch_add(1, Ordering::Relaxed) % self.0.readers.len()]
    }

    /// Commit literal user knowledge and an extraction job before acknowledging.
    /// # Errors
    /// Rejects foreign authority, key conflicts, oversized input or storage failure.
    pub async fn remember(
        &self,
        owner: &OwnerCapability,
        key: OperationKey,
        text: MemoryText,
    ) -> MemoryResult<MemoryReceipt<Source>> {
        self.authorize(owner)?;
        if text.as_str().len() > 16 * 1024 {
            return Err(MemoryError::InvalidInput("user source bytes"));
        }
        self.0.writer.call(move |db| db.remember(&key, &text)).await
    }

    /// Read an unsuppressed immutable source.
    /// # Errors
    /// Rejects unknown/suppressed IDs, foreign authority and unavailable storage.
    pub async fn source(&self, owner: &OwnerCapability, id: MemoryId) -> MemoryResult<Source> {
        self.authorize(owner)?;
        self.reader().call(move |db| db.source(id)).await
    }

    /// Search literal terms. FTS syntax in input is always treated as data.
    /// # Errors
    /// Rejects invalid limits, query bounds, foreign authority or storage failure.
    pub async fn search(
        &self,
        owner: &OwnerCapability,
        query: MemoryText,
        limit: usize,
        min_sequence: Option<Revision>,
    ) -> MemoryResult<SearchBundle> {
        self.authorize(owner)?;
        if !(1..=64).contains(&limit) || query.as_str().len() > 4096 {
            return Err(MemoryError::InvalidInput("search bounds"));
        }
        self.reader()
            .call(move |db| db.search(&query, limit, min_sequence))
            .await
    }

    /// Add an explicitly identified entity; matching names never merge records.
    /// # Errors
    /// Rejects oversized labels, conflicting keys and unauthorized/unavailable stores.
    pub async fn create_entity(
        &self,
        owner: &OwnerCapability,
        key: OperationKey,
        kind: EntityKind,
        label: MemoryText,
    ) -> MemoryResult<MemoryReceipt<Entity>> {
        self.authorize(owner)?;
        if label.as_str().len() > 512 {
            return Err(MemoryError::InvalidInput("entity label"));
        }
        self.0
            .writer
            .call(move |db| db.create_entity(&key, kind, label))
            .await
    }

    /// Commit a host-validated claim with exact evidence. This API does not prove entailment.
    /// # Errors
    /// Rejects invalid types, evidence, source revisions, authority or key reuse.
    pub async fn add_claim(
        &self,
        owner: &OwnerCapability,
        key: OperationKey,
        input: ClaimInput,
    ) -> MemoryResult<MemoryReceipt<Claim>> {
        self.authorize(owner)?;
        graph::validate_bounds(&input)?;
        self.0
            .writer
            .call(move |db| db.add_claim(&key, &input))
            .await
    }

    /// Correct a known assertion atomically, retaining its historical version.
    /// # Errors
    /// Rejects stale revisions, changed identity/predicate, invalid evidence and key conflicts.
    pub async fn correct_claim(
        &self,
        owner: &OwnerCapability,
        key: OperationKey,
        id: MemoryId,
        expected: Revision,
        input: ClaimInput,
    ) -> MemoryResult<MemoryReceipt<Claim>> {
        self.authorize(owner)?;
        graph::validate_bounds(&input)?;
        self.0
            .writer
            .call(move |db| db.correct_claim(&key, id, expected, &input))
            .await
    }

    /// Inspect qualified assertion history. Suppressed evidence is never returned.
    /// # Errors
    /// Rejects foreign authority, unknown assertions, histories over 256 versions
    /// (use [`Self::claim_history_page`]) or storage failure.
    pub async fn claim_history(
        &self,
        owner: &OwnerCapability,
        id: MemoryId,
    ) -> MemoryResult<Vec<Claim>> {
        self.authorize(owner)?;
        self.reader().call(move |db| db.claim_history(id)).await
    }

    /// Inspect a bounded page of eligible assertion versions, with explicit continuation.
    /// # Errors
    /// Rejects limits outside 1..=256, foreign authority or storage failure.
    pub async fn claim_history_page(
        &self,
        owner: &OwnerCapability,
        id: MemoryId,
        after: Option<Revision>,
        limit: usize,
    ) -> MemoryResult<super::ClaimHistoryPage> {
        self.authorize(owner)?;
        if !(1..=256).contains(&limit) {
            return Err(MemoryError::InvalidInput("history limit"));
        }
        self.reader()
            .call(move |db| db.claim_history_page(id, after, limit))
            .await
    }

    /// Follow current outgoing factual edges with cumulative node/edge/depth bounds.
    /// # Errors
    /// Rejects depths outside 1..=12, foreign authority and storage failure.
    pub async fn follow(
        &self,
        owner: &OwnerCapability,
        start: MemoryId,
        depth: usize,
    ) -> MemoryResult<GraphBundle> {
        self.authorize(owner)?;
        if !(1..=12).contains(&depth) {
            return Err(MemoryError::InvalidInput("graph depth"));
        }
        let at = i64::try_from(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_err(|_| MemoryError::Unavailable)?
                .as_millis(),
        )
        .map_err(|_| MemoryError::Unavailable)?;
        self.reader()
            .call(move |db| db.follow(start, depth, at))
            .await
    }

    /// Follow factual edges as of an explicit UTC millisecond instant.
    /// # Errors
    /// Rejects invalid depth, foreign authority or unavailable storage.
    pub async fn follow_at(
        &self,
        owner: &OwnerCapability,
        start: MemoryId,
        depth: usize,
        at: i64,
    ) -> MemoryResult<GraphBundle> {
        self.authorize(owner)?;
        if !(1..=12).contains(&depth) {
            return Err(MemoryError::InvalidInput("graph depth"));
        }
        self.reader()
            .call(move |db| db.follow(start, depth, at))
            .await
    }

    /// Suppress a source, journal the revocation and reject later graph use.
    ///
    /// This is a storage barrier. A product must additionally establish its output
    /// barrier before confirming full logical forgetting to a user.
    /// # Errors
    /// Rejects stale source revisions, conflicting keys or journal/storage failure.
    pub async fn suppress_source(
        &self,
        owner: &OwnerCapability,
        key: OperationKey,
        id: MemoryId,
        expected: Revision,
    ) -> MemoryResult<MemoryReceipt<SuppressionReceipt>> {
        self.authorize(owner)?;
        self.0
            .writer
            .critical(move |db| db.suppress_source(&key, id, expected))
            .await
    }

    /// Physically purge up to 256 affected records, preserving replay tombstones.
    /// # Errors
    /// Rejects missing suppression, foreign authority or storage failure.
    pub async fn purge_source_batch(
        &self,
        owner: &OwnerCapability,
        id: MemoryId,
    ) -> MemoryResult<bool> {
        self.authorize(owner)?;
        self.0
            .writer
            .call(move |db| db.purge_source_batch(id))
            .await
    }

    /// Purge one bounded batch from the next journaled suppressed source.
    ///
    /// Product suppression can include multiple sources from the same exchange.
    /// Repeat until true to drain their physical purge without enumerating prose.
    /// # Errors
    /// Rejects foreign authority, pending journal acknowledgment or storage failure.
    pub async fn purge_next_batch(&self, owner: &OwnerCapability) -> MemoryResult<bool> {
        self.authorize(owner)?;
        self.0.writer.call(|db| {
            use rusqlite::OptionalExtension;
            let id: Option<String> = db.connection.query_row("SELECT source_id FROM forget_operations WHERE purged=0 ORDER BY deletion_generation,source_id LIMIT 1", [], |r|r.get(0)).optional()?;
            if let Some(id) = id { db.purge_source_batch(MemoryId::parse(&id)?)?; }
            let pending: bool=db.connection.query_row("SELECT EXISTS(SELECT 1 FROM forget_operations WHERE purged=0)",[],|r|r.get(0))?;
            Ok(!pending)
        }).await
    }
}

/// Runtime ownership of workers and the exclusive data-directory lease.
pub struct MemoryStore {
    handle: MemoryHandle,
    threads: Vec<thread::JoinHandle<()>>,
    directory: Option<private_dir::PrivateDirectory>,
}
impl fmt::Debug for MemoryStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MemoryStore")
            .field("closed", &self.handle.0.closed.load(Ordering::Relaxed))
            .finish_non_exhaustive()
    }
}
impl MemoryStore {
    /// Open, migrate, recover and start owner-confined storage workers.
    /// # Errors
    /// Rejects insecure paths, competing daemons, unknown schemas or failed recovery.
    pub async fn open(config: MemoryConfig) -> MemoryResult<Self> {
        tokio::task::spawn_blocking(move || Self::open_blocking(&config))
            .await
            .map_err(|_| MemoryError::Unavailable)?
    }
    fn open_blocking(config: &MemoryConfig) -> MemoryResult<Self> {
        let directory = private_dir::PrivateDirectory::open(config)?;
        let mut writer = Database::open(&directory, config)?;
        writer.recover()?;
        let authority = writer.authority();
        let mut databases = Vec::new();
        for _ in 0..2 {
            databases.push(Database::reader(&directory, config, &authority)?);
        }
        let mut threads = Vec::new();
        let (writer, thread) =
            Worker::start(writer, "assistant-memory-writer", WorkerRole::Writer)?;
        threads.push(thread);
        let mut readers = Vec::new();
        for database in databases {
            let (reader, thread) =
                Worker::start(database, "assistant-memory-reader", WorkerRole::Reader)?;
            readers.push(reader);
            threads.push(thread);
        }
        let handle = MemoryHandle(Arc::new(Shared {
            writer,
            readers,
            next: AtomicUsize::new(0),
            closed: AtomicBool::new(false),
            authority,
        }));
        Ok(Self {
            handle,
            threads,
            directory: Some(directory),
        })
    }
    /// Restricted command sender. It carries no owner credential itself.
    #[must_use]
    pub fn handle(&self) -> MemoryHandle {
        self.handle.clone()
    }
    /// Issue the bootstrapping host's owner capability.
    #[must_use]
    pub fn owner(&self) -> OwnerCapability {
        self.handle.0.authority.clone()
    }
    /// Drain previously submitted commands, stop workers and release the lock.
    /// # Errors
    /// Reports worker termination or a failed join.
    pub async fn shutdown(mut self) -> MemoryResult<()> {
        self.handle.0.closed.store(true, Ordering::Release);
        let mut failed = false;
        for worker in std::iter::once(&self.handle.0.writer).chain(&self.handle.0.readers) {
            if worker.close().is_err() {
                failed = true;
            }
        }
        let threads = std::mem::take(&mut self.threads);
        tokio::task::spawn_blocking(move || -> MemoryResult<()> {
            for thread in threads {
                if thread.join().is_err() {
                    failed = true;
                }
            }
            if failed {
                Err(MemoryError::Unavailable)
            } else {
                Ok(())
            }
        })
        .await
        .map_err(|_| MemoryError::Unavailable)??;
        self.directory.take();
        Ok(())
    }
}
impl Drop for MemoryStore {
    fn drop(&mut self) {
        self.handle.0.closed.store(true, Ordering::Release);
        for worker in std::iter::once(&self.handle.0.writer).chain(&self.handle.0.readers) {
            worker.abort();
        }
        // Each database retains the directory lease. Detached worker cleanup cannot
        // release it early, until native storage ownership actually ends.
    }
}
