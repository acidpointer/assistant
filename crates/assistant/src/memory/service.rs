//! Composite memory ownership: durable store plus resident embedding worker and
//! an event-driven indexer. No native polling or extraction runs on idle ticks.
//! The host explicitly issues owner access; model output cannot create it.
//! Shutdown joins indexing/native work before closing storage.

use super::{
    MemoryConfig, MemoryError, MemoryHandle, MemoryId, MemoryReceipt, MemoryResult, MemoryStore,
    MemoryText, OperationKey, OwnerCapability, Source, SourceRef,
    embedding::{EmbeddingConfig, EmbeddingHandle, EmbeddingPurpose, EmbeddingWorker},
};
use crate::{
    chat::{ChatError, ChatResult},
    engine::{Cancellation, ModelEngine},
};
use std::sync::Arc;
use tokio::{
    sync::{Notify, watch},
    task::JoinHandle,
};

/// A historical quotation with host attribution and literal UTF-8 source span.
#[derive(Debug, Clone, serde::Serialize)]
pub struct RecalledSource {
    /// Authoritative literal record, not a model-created assertion.
    pub record: super::SourceRecord,
    /// Matching span start, or zero for a lexical match.
    pub byte_start: u32,
    /// Matching span end.
    pub byte_end: u32,
}
/// Owned retrieval with independent channel availability.
#[derive(Debug, Clone, serde::Serialize)]
pub struct RecallBundle {
    /// Whether lexical terms or the encoder query needed a bounded prefix.
    pub query_truncated: bool,
    /// Reciprocal-rank-fused and deduplicated quotations.
    pub sources: Vec<RecalledSource>,
    /// Whether the semantic channel completed successfully.
    pub semantic_available: bool,
    /// Whether lexical candidates reached their result allowance.
    pub lexical_truncated: bool,
}
/// Host-owned quotation payload packed by the native owner, outside optional hooks.
#[derive(Clone)]
pub(crate) struct RecallContext {
    quotes: Vec<serde_json::Value>,
    semantic_available: bool,
    lexical_truncated: bool,
    query_truncated: bool,
}
impl std::fmt::Debug for RecallContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RecallContext")
            .field("quotes", &self.quotes.len())
            .finish_non_exhaustive()
    }
}
impl RecallContext {
    pub(crate) fn len(&self) -> usize {
        self.quotes.len()
    }
    pub(crate) fn render(&self, count: usize) -> String {
        serde_json::json!({"historical_quotations":self.quotes.iter().take(count).collect::<Vec<_>>(),"semantic_available":self.semantic_available,"lexical_truncated":self.lexical_truncated,"query_truncated":self.query_truncated,"evidence_truncated":count<self.quotes.len()}).to_string()
    }
}
/// Sanitized readiness without personal payloads or paths.
#[derive(Debug, Clone, serde::Serialize)]
pub struct MemoryHealth {
    /// Eligible source coverage in the current derived generation.
    pub index: super::IndexCoverage,
    /// Required storage admission state.
    pub storage: super::StorageStatus,
    /// Bytes in store files, including WAL and journal.
    pub used_bytes: u64,
    /// Configured storage quota.
    pub quota_bytes: u64,
    /// Durable pending formation work.
    pub pending_jobs: u64,
    /// Formation sources deferred for capacity.
    pub deferred_sources: u64,
    /// Latest sanitized index failure.
    pub indexing_fault: Option<&'static str>,
    /// Semantic formation qualification state.
    pub extraction: &'static str,
}

/// Complete startup selection; weights are always local, never downloaded.
#[derive(Debug, Clone)]
pub struct MemoryRuntimeConfig {
    /// Durable private storage directory and quota.
    pub storage: MemoryConfig,
    /// The selected E5 Q8 artifact.
    pub embedding: EmbeddingConfig,
}
/// Host-issued access to one owner's memory. Clone retains the same authority;
/// neither this capability nor its embedded store authority is deserializable.
#[derive(Debug, Clone)]
pub struct MemoryAccess {
    pub(crate) store: MemoryHandle,
    pub(crate) owner: OwnerCapability,
    embedding: EmbeddingHandle,
    changed: Arc<Notify>,
    updates: watch::Sender<u64>,
    health: watch::Receiver<Option<&'static str>>,
}
impl MemoryAccess {
    /// Retrieve bounded historical evidence without hiding channel failure.
    /// # Errors
    /// Rejects invalid bounds, required storage failure or inconsistent sources.
    pub async fn recall(&self, text: MemoryText, limit: usize) -> MemoryResult<RecallBundle> {
        if !(1..=32).contains(&limit) || text.as_str().len() > 4096 {
            return Err(MemoryError::InvalidInput("recall bounds"));
        }
        // Compile bounded literal terms before crossing the storage boundary.
        // Long questions and punctuation-only input are valid product messages.
        let mut terms = Vec::new();
        let mut query_truncated = false;
        for term in text
            .as_str()
            .split(|c: char| !c.is_alphanumeric() && c != '_')
            .filter(|term| !term.is_empty())
        {
            if terms.contains(&term) {
                continue;
            }
            if terms.len() == 16 {
                query_truncated = true;
                break;
            }
            terms.push(term);
        }
        let (lexical_sources, lexical_truncated) = if terms.is_empty() {
            self.store.storage_health(&self.owner).await?;
            (Vec::new(), false)
        } else {
            let lexical = self
                .store
                .search(&self.owner, MemoryText::new(terms.join(" "))?, 32, None)
                .await?;
            (lexical.sources, lexical.truncated)
        };
        let semantic = async {
            let end = self.embedding.query_window(text.clone()).await? as usize;
            query_truncated |= end < text.as_str().len();
            self.search(
                MemoryText::new(text.as_str().get(..end).ok_or(MemoryError::CorruptStore)?)?,
                32,
            )
            .await
        }
        .await;
        let semantic_available = semantic.is_ok();
        let mut candidates = std::collections::BTreeMap::new();
        for (rank, source) in lexical_sources.into_iter().enumerate() {
            let end =
                u32::try_from(source.text.as_str().len()).map_err(|_| MemoryError::Capacity)?;
            let rank = u32::try_from(rank).map_err(|_| MemoryError::Capacity)?;
            candidates.insert(source.id, (1.0 / (61.0 + f64::from(rank)), 0, end));
        }
        match semantic {
            Ok(hits) => {
                for (rank, hit) in hits.into_iter().enumerate() {
                    let rank = u32::try_from(rank).map_err(|_| MemoryError::Capacity)?;
                    let entry = candidates.entry(hit.source.id).or_insert((
                        0.0,
                        hit.byte_start,
                        hit.byte_end,
                    ));
                    entry.0 += 1.0 / (61.0 + f64::from(rank));
                    entry.1 = hit.byte_start;
                    entry.2 = hit.byte_end;
                }
            }
            Err(
                MemoryError::Unavailable
                | MemoryError::Deadline
                | MemoryError::Capacity
                | MemoryError::InvalidInput(_),
            ) => {}
            Err(error) => return Err(error),
        }
        let mut candidates = candidates.into_iter().collect::<Vec<_>>();
        candidates.sort_by(|(a, x), (b, y)| y.0.total_cmp(&x.0).then(a.cmp(b)));
        let mut sources = Vec::new();
        for (id, (_, byte_start, byte_end)) in candidates.into_iter().take(limit) {
            sources.push(RecalledSource {
                record: self.source(id).await?,
                byte_start,
                byte_end,
            });
        }
        Ok(RecallBundle {
            query_truncated,
            sources,
            semantic_available,
            lexical_truncated,
        })
    }
    /// Inspect storage readiness separately from model qualification.
    /// # Errors
    /// Reports unavailable required storage.
    pub async fn health(&self) -> MemoryResult<MemoryHealth> {
        let storage = self.store.storage_health(&self.owner).await?;
        Ok(MemoryHealth {
            index: self.store.index_coverage(&self.owner).await?,
            storage: storage.status,
            used_bytes: storage.used_bytes,
            quota_bytes: storage.quota_bytes,
            pending_jobs: storage.pending_jobs,
            deferred_sources: storage.deferred_sources,
            indexing_fault: self.indexing_fault(),
            extraction: "source_graph",
        })
    }
    #[cfg(test)]
    pub(crate) fn for_test(store: &MemoryStore) -> Self {
        let (_, health) = watch::channel(None);
        Self {
            store: store.handle(),
            owner: store.owner(),
            embedding: EmbeddingHandle::for_test(),
            changed: Arc::new(Notify::new()),
            updates: watch::channel(0).0,
            health,
        }
    }
    /// Commit literal input independently of successful embedding/structuring.
    /// # Errors
    /// Rejects invalid input, key conflicts, quota or storage failure.
    pub async fn remember(
        &self,
        key: OperationKey,
        text: MemoryText,
    ) -> MemoryResult<MemoryReceipt<Source>> {
        let receipt = self.store.remember(&self.owner, key, text).await?;
        self.wake();
        Ok(receipt)
    }
    /// Return the latest indexer fault without personal text or local paths.
    #[must_use]
    pub fn indexing_fault(&self) -> Option<&'static str> {
        *self.health.borrow()
    }
    /// Query semantic evidence. Similarity ranks sources; it does not prove facts.
    /// # Errors
    /// Rejects unavailable native/storage workers, input bounds or query timeout.
    pub async fn search(
        &self,
        text: MemoryText,
        limit: usize,
    ) -> MemoryResult<Vec<super::SemanticHit>> {
        let query = self.embedding.embed(EmbeddingPurpose::Query, text).await?;
        self.store.semantic_search(&self.owner, query, limit).await
    }
    /// Inspect literal evidence and host attribution.
    /// # Errors
    /// Rejects absent/suppressed source identities or closed storage.
    pub async fn source(&self, id: MemoryId) -> MemoryResult<super::SourceRecord> {
        self.store.source_record(&self.owner, id).await
    }
    pub(crate) fn wake(&self) {
        self.changed.notify_one();
        self.updates
            .send_modify(|epoch| *epoch = epoch.wrapping_add(1));
    }
    /// Subscribe to committed assistant/source changes; each wake requires a fresh authorized read.
    #[must_use]
    pub fn subscribe_changes(&self) -> watch::Receiver<u64> {
        self.updates.subscribe()
    }
    pub(crate) async fn recall_context(
        &self,
        text: &str,
        current_source: MemoryId,
    ) -> ChatResult<RecallContext> {
        let text = text.trim();
        // The current input is already committed for crash recovery, but is not
        // historical evidence for its own answer. Keep it out of the recall set.
        let mut end = text.len().min(4096);
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        let query = MemoryText::new(&text[..end]).map_err(memory_chat_error)?;
        let recalled = self.recall(query, 32).await.map_err(memory_chat_error)?;
        let mut evidence = Vec::new();
        // Recent initiative is conversational context even for a short reply
        // such as "yes" whose semantic query cannot recover the question.
        let notices = self
            .store
            .assistant_notices(&self.owner)
            .await
            .map_err(memory_chat_error)?;
        for notice in notices
            .into_iter()
            .filter(|notice| !notice.dismissed)
            .take(2)
        {
            let full = notice.text.as_str();
            let mut end = full.len().min(1024);
            while !full.is_char_boundary(end) {
                end -= 1;
            }
            evidence.push(serde_json::json!({"id":notice.id,"speaker":"assistant","initiated":true,"observed_at":notice.created_at,"quote":&full[..end],"partial":end<full.len()}));
        }
        for hit in recalled
            .sources
            .into_iter()
            .filter(|hit| hit.record.source.id != current_source)
            .take(12)
        {
            let source = &hit.record.source;
            if evidence
                .iter()
                .any(|quote| quote.get("id") == Some(&serde_json::json!(source.id)))
            {
                continue;
            }
            let full = source.text.as_str();
            let excerpt = full
                .get(hit.byte_start as usize..hit.byte_end as usize)
                .ok_or(ChatError::Memory(MemoryError::CorruptStore))?;
            let mut end = excerpt.len().min(1024);
            while !excerpt.is_char_boundary(end) {
                end -= 1;
            }
            evidence.push(serde_json::json!({"id":source.id,"revision":source.revision,"speaker":hit.record.role,"observed_at":hit.record.observed_at,"quote":&excerpt[..end],"partial":end<full.len()}));
        }
        Ok(RecallContext {
            quotes: evidence,
            semantic_available: recalled.semantic_available,
            lexical_truncated: recalled.lexical_truncated,
            query_truncated: recalled.query_truncated || end < text.len(),
        })
    }
}
pub(crate) fn memory_chat_error(error: MemoryError) -> ChatError {
    ChatError::Memory(error)
}

/// Composite resource owner. Required memory readiness precedes application
/// activation. Public handles have no shutdown authority.
#[derive(Debug)]
pub struct MemoryRuntime {
    access: MemoryAccess,
    store: Option<MemoryStore>,
    embedding: Option<EmbeddingWorker>,
    stop: Cancellation,
    indexer: Option<JoinHandle<MemoryResult<()>>>,
}
impl MemoryRuntime {
    /// Open storage and start the selected embedding worker before issuing access.
    /// # Errors
    /// Returns a sanitized storage, artifact or native startup error.
    pub async fn start(
        engine: &'static ModelEngine,
        config: MemoryRuntimeConfig,
    ) -> MemoryResult<Self> {
        let store = MemoryStore::open(config.storage).await?;
        if let Err(error) = store.handle().use_source_graph(&store.owner()).await {
            store.shutdown().await?;
            return Err(error);
        }
        let embedding = match EmbeddingWorker::start(engine, config.embedding).await {
            Ok(worker) => worker,
            Err(error) => {
                store.shutdown().await?;
                return Err(error);
            }
        };
        let changed = Arc::new(Notify::new());
        let (health, status) = watch::channel(None);
        let access = MemoryAccess {
            store: store.handle(),
            owner: store.owner(),
            embedding: embedding.handle(),
            changed: changed.clone(),
            updates: watch::channel(0).0,
            health: status,
        };
        let stop = Cancellation::default();
        let task_access = access.clone();
        let task_stop = stop.clone();
        let indexer = tokio::spawn(async move {
            loop {
                // Check backlog on startup and every committed-source wake. The
                // stored byte cursor permits restart without replaying completed chunks.
                let result = drain_index(&task_access, &task_stop).await;
                match result {
                    Ok(()) => {
                        health.send_replace(None);
                    }
                    Err(MemoryError::Conflict | MemoryError::NotFound) => continue,
                    Err(error) => {
                        health.send_replace(Some(match error {
                            MemoryError::Capacity => "index capacity",
                            _ => "index unavailable",
                        }));
                    }
                }
                let (failures, retry) = task_access
                    .store
                    .embedding_failures(&task_access.owner)
                    .await?;
                if failures > 0 {
                    health.send_replace(Some("partial index; sources need retry or review"));
                }
                tokio::select! {
                    ()=task_stop.cancelled()=>break,
                    ()=changed.notified()=>{},
                    ()=async { match retry { Some(delay)=>tokio::time::sleep(delay).await, None=>std::future::pending().await } }=>{},
                }
            }
            Ok(())
        });
        Ok(Self {
            access,
            store: Some(store),
            embedding: Some(embedding),
            stop,
            indexer: Some(indexer),
        })
    }
    /// Issue the local host's owner capability. Adapters must authenticate before
    /// exposing any operation through this handle.
    #[must_use]
    pub fn owner_access(&self) -> MemoryAccess {
        self.access.clone()
    }
    /// Stop and join optional indexing, destroy native ownership, then close DB.
    /// # Errors
    /// Reports a failed join/native/storage shutdown after attempting all cleanup.
    pub async fn shutdown(mut self) -> MemoryResult<()> {
        self.stop.cancel();
        let mut failed = None;
        if let Some(task) = self.indexer.take() {
            match task.await {
                Ok(Ok(())) => {}
                Ok(Err(error)) => failed = Some(error),
                Err(_) => failed = Some(MemoryError::Unavailable),
            }
        }
        if let Some(worker) = self.embedding.take()
            && let Err(error) = worker.shutdown().await
        {
            failed = Some(error);
        }
        if let Some(store) = self.store.take()
            && let Err(error) = store.shutdown().await
        {
            failed = Some(error);
        }
        if let Some(error) = failed {
            return Err(error);
        }
        Ok(())
    }
}
impl Drop for MemoryRuntime {
    fn drop(&mut self) {
        self.stop.cancel();
    }
}

async fn drain_index(access: &MemoryAccess, stop: &Cancellation) -> MemoryResult<()> {
    while !stop.is_cancelled() {
        let Some(task) = access.store.next_embedding(&access.owner).await? else {
            return Ok(());
        };
        let text = task.source.text.as_str();
        let window = access
            .embedding
            .passage_window(task.source.text.clone(), task.covered_bytes)
            .await;
        let (start, end, vector) = match window {
            Ok((start, end)) => {
                let input = MemoryText::new(
                    text.get(start as usize..end as usize)
                        .ok_or(MemoryError::CorruptStore)?,
                )?;
                (
                    start,
                    end,
                    access
                        .embedding
                        .embed(EmbeddingPurpose::Passage, input)
                        .await,
                )
            }
            Err(error) => (0, 0, Err(error)),
        };
        let vector = match vector {
            Ok(vector) => vector,
            Err(MemoryError::InvalidInput(_) | MemoryError::Deadline) => {
                access
                    .store
                    .embedding_failed(
                        &access.owner,
                        SourceRef {
                            id: task.source.id,
                            revision: task.source.revision,
                        },
                    )
                    .await?;
                continue;
            }
            Err(error) => return Err(error),
        };
        access
            .store
            .put_embedding(
                &access.owner,
                SourceRef {
                    id: task.source.id,
                    revision: task.source.revision,
                },
                task.covered_bytes,
                start,
                end,
                vector,
            )
            .await?;
        tokio::task::yield_now().await;
    }
    Ok(())
}
