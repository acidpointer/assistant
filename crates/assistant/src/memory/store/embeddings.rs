//! Source-bound exact vector index. Chunk coverage advances with a checked
//! compare-and-swap in the same transaction as the vector. Suppressed sources
//! are excluded at read/write time; FK cleanup deletes derived rows on purge.
//! Results carry literal evidence, never an inferred factual conclusion.

use super::{
    MemoryHandle, OwnerCapability,
    database::{Database, WriteBudget, check_budget, revision},
};
use crate::memory::{
    MemoryError, MemoryResult, Source, SourceRef,
    embedding::{E5_INDEX_ID, Embedding},
};
use rusqlite::{OptionalExtension, TransactionBehavior, params};
use std::collections::BTreeMap;

/// Current derived generation and authoritative source coverage, without paths.
#[derive(Debug, Clone, serde::Serialize)]
pub struct IndexCoverage {
    /// Exact artifact and preprocessing/window policy identity.
    pub generation: &'static str,
    /// Unsuppressed sources eligible for indexing.
    pub eligible_sources: u64,
    /// Eligible sources with complete coverage in this generation.
    pub completed_sources: u64,
    /// Eligible sources waiting for retry or review after failure.
    pub failed_sources: u64,
    /// Eligible chunk vectors in this generation.
    pub chunks: u64,
}

/// One immutable source and its durably indexed byte coverage.
#[derive(Debug, Clone)]
pub struct EmbeddingTask {
    /// Source to index.
    pub source: Source,
    /// Next uncovered byte offset.
    pub covered_bytes: u32,
}
/// Evidence selected by cosine similarity, retaining exact supporting span.
#[derive(Debug, Clone)]
pub struct SemanticHit {
    /// Original literal source.
    pub source: Source,
    /// Normalized cosine similarity, not a probability.
    pub score: f32,
    /// Inclusive UTF-8 byte offset of the matching chunk.
    pub byte_start: u32,
    /// Exclusive UTF-8 byte offset of the matching chunk.
    pub byte_end: u32,
}

impl MemoryHandle {
    /// Inspect source eligibility and current-generation coverage in one snapshot.
    /// # Errors
    /// Rejects foreign authority, unavailable storage or corrupt counters.
    pub async fn index_coverage(&self, owner: &OwnerCapability) -> MemoryResult<IndexCoverage> {
        self.authorize(owner)?;
        self.reader().call(|db| {
            let (eligible,complete,failed,chunks):(i64,i64,i64,i64)=db.connection.query_row(
                "SELECT count(*),coalesce(sum(coalesce(c.complete,0)),0),coalesce(sum(EXISTS(SELECT 1 FROM embedding_failures f WHERE f.principal=s.principal AND f.source_id=s.id AND f.source_revision=s.revision AND f.model=?2)),0),coalesce(sum((SELECT count(*) FROM embedding_chunks v WHERE v.principal=s.principal AND v.source_id=s.id AND v.source_revision=s.revision AND v.model=?2)),0) FROM sources s LEFT JOIN embedding_coverage c ON c.principal=s.principal AND c.source_id=s.id AND c.source_revision=s.revision AND c.model=?2 WHERE s.principal=?1 AND s.suppressed=0",
                params![db.owner.principal().to_string(),E5_INDEX_ID], |r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?)))?;
            Ok(IndexCoverage {generation:E5_INDEX_ID,eligible_sources:u64::try_from(eligible).map_err(|_|MemoryError::CorruptStore)?,completed_sources:u64::try_from(complete).map_err(|_|MemoryError::CorruptStore)?,failed_sources:u64::try_from(failed).map_err(|_|MemoryError::CorruptStore)?,chunks:u64::try_from(chunks).map_err(|_|MemoryError::CorruptStore)?})
        }).await
    }
    /// Defer one failed source without blocking subsequent index work.
    /// # Errors
    /// Rejects foreign authority, obsolete sources, quota or writer failure.
    pub(crate) async fn embedding_failed(
        &self,
        owner: &OwnerCapability,
        source: SourceRef,
    ) -> MemoryResult<()> {
        self.authorize(owner)?;
        self.0.writer.call(move |db| {
            check_budget(&db.connection, db.directory.used_bytes()?, db.quota, WriteBudget::Ordinary)?;
            let now = crate::memory::session::utc_millis()?;
            let principal = db.owner.principal().to_string();
            let tx = db.connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let exists: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM sources WHERE principal=?1 AND id=?2 AND revision=?3 AND suppressed=0)", params![principal,source.id.to_string(),source.revision.get()], |r|r.get(0))?;
            if !exists { return Err(MemoryError::NotFound); }
            let attempts: i64 = tx.query_row("SELECT coalesce((SELECT attempts FROM embedding_failures WHERE principal=?1 AND source_id=?2 AND source_revision=?3 AND model=?4),0)", params![principal,source.id.to_string(),source.revision.get(),E5_INDEX_ID], |r|r.get(0))?;
            let next = (attempts + 1).min(3);
            let retry = match next { 1=>Some(now.saturating_add(5000)), 2=>Some(now.saturating_add(30000)), _=>None };
            tx.execute("INSERT INTO embedding_failures(principal,source_id,source_revision,model,attempts,retry_after) VALUES(?1,?2,?3,?4,?5,?6) ON CONFLICT(principal,source_id,source_revision,model) DO UPDATE SET attempts=excluded.attempts,retry_after=excluded.retry_after", params![principal,source.id.to_string(),source.revision.get(),E5_INDEX_ID,next,retry])?;
            tx.commit()?;
            Ok(())
        }).await
    }
    pub(crate) async fn embedding_failures(
        &self,
        owner: &OwnerCapability,
    ) -> MemoryResult<(u64, Option<std::time::Duration>)> {
        self.authorize(owner)?;
        self.reader().call(|db| {
            let (count, due): (i64,Option<i64>) = db.connection.query_row("SELECT count(*),min(f.retry_after) FROM embedding_failures f JOIN sources s ON s.principal=f.principal AND s.id=f.source_id AND s.revision=f.source_revision WHERE f.principal=?1 AND f.model=?2 AND s.suppressed=0",params![db.owner.principal().to_string(),E5_INDEX_ID], |r|Ok((r.get(0)?,r.get(1)?)))?;
            let now = crate::memory::session::utc_millis()?;
            Ok((u64::try_from(count).map_err(|_|MemoryError::CorruptStore)?,due.map(|due| u64::try_from(due.saturating_sub(now).max(0)).map(|millis| std::time::Duration::from_millis(millis).min(std::time::Duration::from_secs(60))).map_err(|_|MemoryError::CorruptStore)).transpose()?))
        }).await
    }
    /// Read one source still needing semantic coverage. No native work runs in `SQLite`.
    /// # Errors
    /// Rejects foreign authority and unavailable storage.
    pub async fn next_embedding(
        &self,
        owner: &OwnerCapability,
    ) -> MemoryResult<Option<EmbeddingTask>> {
        self.authorize(owner)?;
        self.reader().call(Database::next_embedding).await
    }
    /// Commit one bounded chunk and advance coverage only if the source/cursor
    /// remain eligible. A lost reply is reconciled by reading the next task.
    /// # Errors
    /// Rejects stale cursors, invalid UTF-8 spans, foreign authority or quota.
    pub async fn put_embedding(
        &self,
        owner: &OwnerCapability,
        source: SourceRef,
        expected_covered: u32,
        start: u32,
        end: u32,
        vector: Embedding,
    ) -> MemoryResult<()> {
        self.authorize(owner)?;
        if end <= start
            || end - start > 16_384
            || start > expected_covered
            || end <= expected_covered
        {
            return Err(MemoryError::InvalidInput("embedding span"));
        }
        self.0
            .writer
            .call(move |db| db.put_embedding(source, expected_covered, start, end, &vector))
            .await
    }
    /// Exact cosine scan across eligible chunks, deduplicated by source. It is
    /// bounded by the reader's query deadline, with at most 64 returned sources.
    /// # Errors
    /// Rejects invalid limits, foreign authority, timeout or storage corruption.
    pub async fn semantic_search(
        &self,
        owner: &OwnerCapability,
        query: Embedding,
        limit: usize,
    ) -> MemoryResult<Vec<SemanticHit>> {
        self.authorize(owner)?;
        if !(1..=64).contains(&limit) {
            return Err(MemoryError::InvalidInput("semantic search limit"));
        }
        self.reader()
            .call(move |db| db.semantic_search(&query, limit))
            .await
    }
}
impl Database {
    fn next_embedding(&mut self) -> MemoryResult<Option<EmbeddingTask>> {
        let now = crate::memory::session::utc_millis()?;
        let row: Option<(String, i64, String, bool, i64)> = self.connection.query_row("SELECT s.id,s.revision,s.text,s.preserved,coalesce(c.covered_bytes,0) FROM sources s LEFT JOIN embedding_coverage c ON c.principal=s.principal AND c.source_id=s.id AND c.source_revision=s.revision AND c.model=?2 WHERE s.principal=?1 AND s.suppressed=0 AND coalesce(c.complete,0)=0 AND NOT EXISTS(SELECT 1 FROM embedding_failures f WHERE f.principal=s.principal AND f.source_id=s.id AND f.source_revision=s.revision AND f.model=?2 AND (f.retry_after IS NULL OR f.retry_after>?3)) ORDER BY s.rowid LIMIT 1",params![self.owner.principal().to_string(),E5_INDEX_ID,now],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?))).optional()?;
        row.map(|(id, rev, text, preserved, cursor)| {
            Ok(EmbeddingTask {
                source: Source {
                    id: crate::memory::MemoryId::parse(&id)?,
                    revision: revision(rev)?,
                    text: crate::memory::MemoryText::new(text)
                        .map_err(|_| MemoryError::CorruptStore)?,
                    preserved,
                },
                covered_bytes: u32::try_from(cursor).map_err(|_| MemoryError::CorruptStore)?,
            })
        })
        .transpose()
    }
    fn put_embedding(
        &mut self,
        source: SourceRef,
        cursor: u32,
        start: u32,
        end: u32,
        vector: &Embedding,
    ) -> MemoryResult<()> {
        check_budget(
            &self.connection,
            self.directory.used_bytes()?,
            self.quota,
            WriteBudget::Ordinary,
        )?;
        let principal = self.owner.principal().to_string();
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let text: String = tx.query_row("SELECT text FROM sources WHERE principal=?1 AND id=?2 AND revision=?3 AND suppressed=0",params![principal,source.id.to_string(),source.revision.get()],|r|r.get(0))?;
        let old: Option<i64> = tx.query_row("SELECT covered_bytes FROM embedding_coverage WHERE principal=?1 AND source_id=?2 AND source_revision=?3 AND model=?4", params![principal,source.id.to_string(),source.revision.get(),E5_INDEX_ID],|r|r.get(0)).optional()?;
        if old.unwrap_or(0) != i64::from(cursor) {
            return Err(MemoryError::Conflict);
        }
        if text.get(start as usize..end as usize).is_none() {
            return Err(MemoryError::InvalidInput("embedding UTF-8 span"));
        }
        let count: i64 = tx.query_row("SELECT count(*) FROM embedding_chunks WHERE principal=?1 AND source_id=?2 AND source_revision=?3 AND model=?4",params![principal,source.id.to_string(),source.revision.get(),E5_INDEX_ID],|r|r.get(0))?;
        if count >= 1024 {
            return Err(MemoryError::Capacity);
        }
        let bytes: Vec<u8> = vector
            .values()
            .iter()
            .flat_map(|v| v.to_le_bytes())
            .collect();
        tx.execute("INSERT INTO embedding_chunks(principal,source_id,source_revision,model,byte_start,byte_end,vector) VALUES(?1,?2,?3,?4,?5,?6,?7)",params![principal,source.id.to_string(),source.revision.get(),E5_INDEX_ID,start,end,bytes])?;
        tx.execute("INSERT INTO embedding_coverage(principal,source_id,source_revision,model,covered_bytes,complete) VALUES(?1,?2,?3,?4,?5,?6) ON CONFLICT(principal,source_id,source_revision,model) DO UPDATE SET covered_bytes=excluded.covered_bytes,complete=excluded.complete",params![principal,source.id.to_string(),source.revision.get(),E5_INDEX_ID,end,end as usize == text.len()])?;
        tx.execute("DELETE FROM embedding_failures WHERE principal=?1 AND source_id=?2 AND source_revision=?3 AND model=?4",params![principal,source.id.to_string(),source.revision.get(),E5_INDEX_ID])?;
        tx.commit()?;
        Ok(())
    }
    fn semantic_search(
        &mut self,
        query: &Embedding,
        limit: usize,
    ) -> MemoryResult<Vec<SemanticHit>> {
        let principal = self.owner.principal().to_string();
        let tx = self.connection.transaction()?;
        let mut statement = tx.prepare("SELECT v.source_id,v.byte_start,v.byte_end,v.vector FROM embedding_chunks v JOIN sources s ON s.principal=v.principal AND s.id=v.source_id AND s.revision=v.source_revision WHERE v.principal=?1 AND v.model=?2 AND s.suppressed=0")?;
        let mut rows = statement.query(params![principal, E5_INDEX_ID])?;
        let mut best = BTreeMap::<String, (f32, u32, u32)>::new();
        let started = std::time::Instant::now();
        let mut scanned: usize = 0;
        while let Some(row) = rows.next()? {
            scanned += 1;
            // SQLite cannot bound Rust scoring work. Never return a partial
            // ranking as an exact top-k when the host allowance is exhausted.
            if scanned > 1_000_000 || started.elapsed() >= std::time::Duration::from_millis(250) {
                return Err(MemoryError::Deadline);
            }
            let bytes: Vec<u8> = row.get(3)?;
            if bytes.len() != 1536 {
                return Err(MemoryError::CorruptStore);
            }
            let mut score = 0.0;
            for (chunk, q) in bytes.chunks_exact(4).zip(query.values()) {
                let array: [u8; 4] = chunk.try_into().map_err(|_| MemoryError::CorruptStore)?;
                let value = f32::from_le_bytes(array);
                if !value.is_finite() {
                    return Err(MemoryError::CorruptStore);
                }
                score += value * q;
            }
            if !score.is_finite() {
                return Err(MemoryError::CorruptStore);
            }
            let id: String = row.get(0)?;
            let start: u32 = row.get(1)?;
            let end: u32 = row.get(2)?;
            if let Some(old) = best.get_mut(&id) {
                if score > old.0 {
                    *old = (score, start, end);
                }
            } else if best.len() < limit {
                best.insert(id, (score, start, end));
            } else if let Some((worst, lowest)) = best
                .iter()
                .min_by(|a, b| a.1.0.total_cmp(&b.1.0))
                .map(|(id, entry)| (id.clone(), entry.0))
                && score > lowest
            {
                best.remove(&worst);
                best.insert(id, (score, start, end));
            }
        }
        drop(rows);
        drop(statement);
        let mut results = Vec::with_capacity(best.len());
        for (id, (score, start, end)) in best {
            let (rev,text,preserved): (i64,String,bool) = tx.query_row("SELECT revision,text,preserved FROM sources WHERE principal=?1 AND id=?2 AND suppressed=0",params![principal,id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?;
            if text.get(start as usize..end as usize).is_none() {
                return Err(MemoryError::CorruptStore);
            }
            results.push(SemanticHit {
                source: Source {
                    id: crate::memory::MemoryId::parse(&id)?,
                    revision: revision(rev)?,
                    text: crate::memory::MemoryText::new(text)
                        .map_err(|_| MemoryError::CorruptStore)?,
                    preserved,
                },
                score,
                byte_start: start,
                byte_end: end,
            });
        }
        results.sort_by(|a, b| {
            b.score
                .total_cmp(&a.score)
                .then(a.source.id.cmp(&b.source.id))
        });
        tx.commit()?;
        Ok(results)
    }
}
