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
    embedding::{E5_Q8_SHA256, Embedding},
};
use rusqlite::{OptionalExtension, TransactionBehavior, params};
use std::collections::BTreeMap;

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
        if end <= start || end - start > 2048 || start > expected_covered || end <= expected_covered
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
        let row: Option<(String, i64, String, bool, i64)> = self.connection.query_row("SELECT s.id,s.revision,s.text,s.preserved,coalesce(c.covered_bytes,0) FROM sources s LEFT JOIN embedding_coverage c ON c.principal=s.principal AND c.source_id=s.id AND c.source_revision=s.revision AND c.model=?2 WHERE s.principal=?1 AND s.suppressed=0 AND coalesce(c.complete,0)=0 ORDER BY s.rowid LIMIT 1",params![self.owner.principal().to_string(),E5_Q8_SHA256],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?))).optional()?;
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
        let old: Option<i64> = tx.query_row("SELECT covered_bytes FROM embedding_coverage WHERE principal=?1 AND source_id=?2 AND source_revision=?3 AND model=?4", params![principal,source.id.to_string(),source.revision.get(),E5_Q8_SHA256],|r|r.get(0)).optional()?;
        if old.unwrap_or(0) != i64::from(cursor) {
            return Err(MemoryError::Conflict);
        }
        if text.get(start as usize..end as usize).is_none() {
            return Err(MemoryError::InvalidInput("embedding UTF-8 span"));
        }
        let count: i64 = tx.query_row("SELECT count(*) FROM embedding_chunks WHERE principal=?1 AND source_id=?2 AND source_revision=?3 AND model=?4",params![principal,source.id.to_string(),source.revision.get(),E5_Q8_SHA256],|r|r.get(0))?;
        if count >= 1024 {
            return Err(MemoryError::Capacity);
        }
        let bytes: Vec<u8> = vector
            .values()
            .iter()
            .flat_map(|v| v.to_le_bytes())
            .collect();
        tx.execute("INSERT INTO embedding_chunks(principal,source_id,source_revision,model,byte_start,byte_end,vector) VALUES(?1,?2,?3,?4,?5,?6,?7)",params![principal,source.id.to_string(),source.revision.get(),E5_Q8_SHA256,start,end,bytes])?;
        tx.execute("INSERT INTO embedding_coverage(principal,source_id,source_revision,model,covered_bytes,complete) VALUES(?1,?2,?3,?4,?5,?6) ON CONFLICT(principal,source_id,source_revision,model) DO UPDATE SET covered_bytes=excluded.covered_bytes,complete=excluded.complete",params![principal,source.id.to_string(),source.revision.get(),E5_Q8_SHA256,end,end as usize == text.len()])?;
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
        let mut rows = statement.query(params![principal, E5_Q8_SHA256])?;
        let mut best = BTreeMap::<String, (f32, u32, u32)>::new();
        while let Some(row) = rows.next()? {
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
