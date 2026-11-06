//! Revocable owner application access shared by transport adapters. Trusted host
//! code issues grants according to its transport access policy. Tokens and wire framing
//! stay outside the core; capabilities cannot be deserialized. Revoking a grant
//! blocks new reads/commands and wakes subscribers, without rolling back work
//! already owned by the application. Every returned read is checked again.

use super::{ApplicationError, AssistantHandle, RunView, Snapshot, Submission, Subscription};
use crate::{
    engine::Cancellation,
    memory::{ClientRequestKey, HistoryPage, MemoryId, Revision},
};
use sha2::{Digest, Sha256};

/// One owner's revocable product access. All clones share revocation.
#[derive(Debug, Clone)]
pub struct OwnerHandle {
    assistant: AssistantHandle,
    namespace: String,
    revoked: Cancellation,
}
impl AssistantHandle {
    /// Issue owner access according to the trusted host's transport policy.
    /// The namespace is host policy, never a client-selected permission.
    /// # Errors
    /// Rejects invalid namespaces, memory-disabled mode or shutdown.
    pub fn owner_access(&self, namespace: &str) -> Result<OwnerHandle, ApplicationError> {
        if !super::Coordinator::identifier(namespace) {
            return Err(ApplicationError::InvalidInput);
        }
        if self.memory.is_none() {
            return Err(ApplicationError::StorageUnavailable);
        }
        if self.closed.load(std::sync::atomic::Ordering::Acquire) {
            return Err(ApplicationError::Closed);
        }
        Ok(OwnerHandle {
            assistant: self.clone(),
            namespace: namespace.into(),
            revoked: Cancellation::default(),
        })
    }
}
impl OwnerHandle {
    fn memory(&self) -> Result<&crate::memory::service::MemoryAccess, ApplicationError> {
        self.check()?;
        self.assistant
            .memory
            .as_ref()
            .ok_or(ApplicationError::StorageUnavailable)
    }
    /// Read personality and initiative settings, independent of any transport.
    /// # Errors
    /// Reports authorization or storage failure.
    pub async fn assistant_profile(
        &self,
    ) -> Result<crate::memory::AssistantProfile, ApplicationError> {
        let result = self
            .memory()?
            .store
            .assistant_profile(&self.memory()?.owner)
            .await
            .map_err(super::memory_application_error);
        self.check()?;
        result
    }
    /// Replace owner settings with optimistic concurrency.
    /// # Errors
    /// Reports stale edits, invalid bounds, authorization or storage failure.
    pub async fn update_assistant(
        &self,
        expected: Revision,
        settings: crate::memory::AssistantSettings,
    ) -> Result<crate::memory::AssistantProfile, ApplicationError> {
        let memory = self.memory()?;
        let result = memory
            .store
            .update_assistant(
                &memory.owner,
                expected,
                settings,
                crate::memory::session::utc_millis().map_err(super::memory_application_error)?,
            )
            .await
            .map_err(super::memory_application_error);
        memory.wake();
        self.check()?;
        result
    }
    /// Read durable assistant-initiated messages.
    /// # Errors
    /// Reports authorization or storage failure.
    pub async fn notices(&self) -> Result<Vec<crate::memory::AssistantNotice>, ApplicationError> {
        let memory = self.memory()?;
        let result = memory
            .store
            .assistant_notices(&memory.owner)
            .await
            .map_err(super::memory_application_error);
        self.check()?;
        result
    }
    /// Dismiss a message without forgetting its content.
    /// # Errors
    /// Reports authorization, absent messages or storage failure.
    pub async fn dismiss_notice(&self, id: MemoryId) -> Result<(), ApplicationError> {
        let memory = self.memory()?;
        let result = memory
            .store
            .dismiss_notice(&memory.owner, id)
            .await
            .map_err(super::memory_application_error);
        memory.wake();
        self.check()?;
        result
    }
    /// Subscribe to changes. Consumers must recheck this grant before publication.
    /// # Errors
    /// Reports authorization or shutdown.
    pub fn assistant_changes(&self) -> Result<tokio::sync::watch::Receiver<u64>, ApplicationError> {
        Ok(self.memory()?.subscribe_changes())
    }
    /// Page source connections without losing neighbors at a high-degree node.
    /// # Errors
    /// Reports invalid cursors/entries, authorization or storage failure.
    pub async fn connections(
        &self,
        start: MemoryId,
        cursor: Option<String>,
    ) -> Result<crate::memory::SourceGraph, ApplicationError> {
        let memory = self.memory()?;
        let result = memory
            .store
            .source_page(&memory.owner, start, cursor)
            .await
            .map_err(super::memory_application_error);
        self.check()?;
        result
    }
    /// Walk universal conversation/note links, independent of typed claims.
    /// # Errors
    /// Reports invalid bounds/references, authorization or storage failure.
    pub async fn follow_sources(
        &self,
        start: MemoryId,
        depth: usize,
    ) -> Result<crate::memory::SourceGraph, ApplicationError> {
        let memory = self.memory()?;
        let result = memory
            .store
            .follow_sources(&memory.owner, start, depth)
            .await
            .map_err(super::memory_application_error);
        self.check()?;
        result
    }
    /// Inspect bounded factual adjacency with original direction and provenance.
    /// Without a node handle, start at the authenticated owner's person node.
    /// # Errors
    /// Reports authorization, invalid depth, absent nodes or storage failure.
    pub async fn follow(
        &self,
        start: Option<MemoryId>,
        depth: usize,
    ) -> Result<crate::memory::GraphBundle, ApplicationError> {
        self.check()?;
        if !(1..=8).contains(&depth) {
            return Err(ApplicationError::InvalidInput);
        }
        let memory = self
            .assistant
            .memory
            .as_ref()
            .ok_or(ApplicationError::StorageUnavailable)?;
        let result = memory
            .store
            .follow(
                &memory.owner,
                start.unwrap_or_else(|| memory.owner.principal()),
                depth,
            )
            .await
            .map_err(super::memory_application_error);
        self.check()?;
        result
    }
    /// Save an explicit owner note without creating a chat exchange or inference.
    /// # Errors
    /// Reports invalid text/keys, conflicts, authority or storage failure.
    pub async fn remember(
        &self,
        request: &str,
        text: crate::memory::MemoryText,
    ) -> Result<crate::memory::MemoryReceipt<crate::memory::Source>, ApplicationError> {
        let memory = self.memory()?;
        if !super::Coordinator::identifier(request) {
            return Err(ApplicationError::InvalidInput);
        }
        let digest = Sha256::digest(
            serde_json::to_vec(&(&self.namespace, request))
                .map_err(|_| ApplicationError::InvalidInput)?,
        );
        let key = crate::memory::OperationKey::new("owner-note", format!("{digest:x}"))
            .map_err(super::memory_application_error)?;
        let result = memory
            .store
            .remember_observed(
                &memory.owner,
                key,
                text,
                crate::memory::session::utc_millis().map_err(super::memory_application_error)?,
            )
            .await
            .map_err(super::memory_application_error);
        memory.wake();
        self.check()?;
        result
    }
    /// Preserve an accepted source without creating another evidence copy.
    /// # Errors
    /// Reports authorization, revision/key conflicts or storage failure.
    pub async fn preserve(
        &self,
        request: &str,
        source: crate::memory::SourceRef,
    ) -> Result<crate::memory::MemoryReceipt<crate::memory::Source>, ApplicationError> {
        self.check()?;
        if !super::Coordinator::identifier(request) {
            return Err(ApplicationError::InvalidInput);
        }
        let digest = Sha256::digest(
            serde_json::to_vec(&(&self.namespace, request))
                .map_err(|_| ApplicationError::InvalidInput)?,
        );
        let key = crate::memory::OperationKey::new("owner-preserve", format!("{digest:x}"))
            .map_err(super::memory_application_error)?;
        let memory = self
            .assistant
            .memory
            .as_ref()
            .ok_or(ApplicationError::StorageUnavailable)?;
        let result = memory
            .store
            .preserve_source(&memory.owner, key, source)
            .await
            .map_err(super::memory_application_error);
        self.check()?;
        result
    }
    /// Revoke this grant and every clone; admitted work keeps its settlement owner.
    pub fn revoke(&self) {
        self.revoked.cancel();
    }
    /// Whether this grant has been revoked.
    #[must_use]
    pub fn is_revoked(&self) -> bool {
        self.revoked.is_cancelled()
    }
    /// Wait for revocation without polling.
    pub async fn revoked(&self) {
        self.revoked.cancelled().await;
    }
    /// Check immediately before a transport's controlled publication boundary.
    /// # Errors
    /// Returns `Unauthorized` after revocation, or `Closed` after shutdown.
    pub fn check(&self) -> Result<(), ApplicationError> {
        if self.is_revoked() {
            return Err(ApplicationError::Unauthorized);
        }
        if self
            .assistant
            .closed
            .load(std::sync::atomic::Ordering::Acquire)
        {
            return Err(ApplicationError::Closed);
        }
        Ok(())
    }
    /// Submit through the shared durable product path.
    /// # Errors
    /// Reports authorization, validation, capacity or durable admission failure.
    pub async fn submit(&self, input: Submission) -> Result<RunView, ApplicationError> {
        self.check()?;
        let result = self.assistant.submit(&self.namespace, input).await;
        self.check()?;
        result
    }
    /// Reconcile an original key without resubmitting input or executing tools.
    /// # Errors
    /// Reports authorization, absent/forgotten keys or storage failure.
    pub async fn lookup(&self, client: &str, request: &str) -> Result<RunView, ApplicationError> {
        self.check()?;
        if !super::Coordinator::identifier(client) || !super::Coordinator::identifier(request) {
            return Err(ApplicationError::InvalidInput);
        }
        let committing = self
            .assistant
            .call(|reply| {
                super::Command::Pending(
                    self.namespace.clone(),
                    client.into(),
                    request.into(),
                    reply,
                )
            })
            .await?;
        if committing {
            return Err(ApplicationError::StorageUnavailable);
        }
        let memory = self
            .assistant
            .memory
            .as_ref()
            .ok_or(ApplicationError::StorageUnavailable)?;
        let key = ClientRequestKey::new(&self.namespace, client, request)
            .map_err(super::memory_application_error)?;
        let record = memory
            .store
            .operation_by_key(&memory.owner, key)
            .await
            .map_err(super::memory_application_error)?;
        let result = self.assistant.run(&record.id.to_string()).await;
        self.check()?;
        result
    }
    /// Read current persisted or active operation state.
    /// # Errors
    /// Reports authorization, absent/forgotten identity or storage failure.
    pub async fn operation(&self, id: &str) -> Result<RunView, ApplicationError> {
        self.check()?;
        let result = self.assistant.run(id).await;
        self.check()?;
        result
    }
    /// Read a bounded atomic presentation snapshot.
    /// # Errors
    /// Reports authorization, coordinator capacity or shutdown.
    pub async fn snapshot(&self) -> Result<Snapshot, ApplicationError> {
        self.check()?;
        let result = self.assistant.snapshot().await;
        self.check()?;
        result
    }
    /// Register a snapshot and subsequent bounded events atomically.
    /// Subscribers must also observe [`Self::revoked`] and check before sending.
    /// # Errors
    /// Reports authorization, subscription capacity or shutdown.
    pub async fn subscribe(&self) -> Result<Subscription, ApplicationError> {
        self.check()?;
        let result = self.assistant.subscribe().await;
        self.check()?;
        result
    }
    /// Request cancellation explicitly; disconnecting never calls this method.
    /// # Errors
    /// Reports authorization, absent operation or shutdown.
    pub async fn cancel(&self, id: &str) -> Result<RunView, ApplicationError> {
        self.check()?;
        let result = self.assistant.cancel(id).await;
        self.check()?;
        result
    }
    /// Read newest-first durable visible history without altering working context.
    /// # Errors
    /// Reports authorization, invalid pagination or storage failure.
    pub async fn history(
        &self,
        before: Option<Revision>,
        limit: usize,
    ) -> Result<HistoryPage, ApplicationError> {
        self.check()?;
        let memory = self
            .assistant
            .memory
            .as_ref()
            .ok_or(ApplicationError::StorageUnavailable)?;
        let result = memory
            .store
            .history(&memory.owner, before, limit)
            .await
            .map_err(super::memory_application_error);
        self.check()?;
        result
    }
    /// Inspect a literal source with host attribution; its ID grants no authority.
    /// # Errors
    /// Reports authorization, suppressed/absent evidence or storage failure.
    pub async fn evidence(
        &self,
        id: MemoryId,
    ) -> Result<crate::memory::SourceRecord, ApplicationError> {
        self.check()?;
        let memory = self
            .assistant
            .memory
            .as_ref()
            .ok_or(ApplicationError::StorageUnavailable)?;
        let result = memory
            .source(id)
            .await
            .map_err(super::memory_application_error);
        self.check()?;
        result
    }
    /// Search source evidence, keeping channel failures explicit.
    /// # Errors
    /// Reports authorization, query bounds or required storage failure.
    pub async fn search(
        &self,
        text: crate::memory::MemoryText,
        limit: usize,
    ) -> Result<crate::memory::service::RecallBundle, ApplicationError> {
        self.check()?;
        let memory = self
            .assistant
            .memory
            .as_ref()
            .ok_or(ApplicationError::StorageUnavailable)?;
        let result = memory
            .recall(text, limit)
            .await
            .map_err(super::memory_application_error);
        self.check()?;
        result
    }
    /// Inspect component status without personal text or native paths.
    /// # Errors
    /// Reports authorization or unavailable required storage.
    pub async fn memory_health(
        &self,
    ) -> Result<crate::memory::service::MemoryHealth, ApplicationError> {
        self.check()?;
        let memory = self
            .assistant
            .memory
            .as_ref()
            .ok_or(ApplicationError::StorageUnavailable)?;
        let result = memory
            .health()
            .await
            .map_err(super::memory_application_error);
        self.check()?;
        result
    }
}
