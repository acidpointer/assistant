//! Mandatory durable participant for one authorized product interaction. This
//! private host context cannot be replaced by middleware or deserialized from a
//! model. Admission precedes construction; settlement runs in an owned task even
//! when a client drops its waiter. Stable keys make terminal writes reconcilable.
use super::{
    MemoryError, MemoryResult, MemoryText, OperationFinalization, OperationKey, OperationRecord,
    TerminalOutcome, service::MemoryAccess,
};
use crate::chat::{ChatResponse, ChatResult, RunFailure, RunStatus};
use std::sync::Arc;

#[derive(Debug)]
pub(crate) struct MemoryOperation {
    pending: std::sync::Mutex<Vec<tokio::task::JoinHandle<MemoryResult<()>>>>,
    ordinal: std::sync::atomic::AtomicU16,
    unknown: std::sync::atomic::AtomicBool,
    pub(crate) record: OperationRecord,
    pub(crate) access: MemoryAccess,
}
impl MemoryOperation {
    pub(crate) fn new(record: OperationRecord, access: MemoryAccess) -> Arc<Self> {
        Arc::new(Self {
            record,
            access,
            pending: std::sync::Mutex::new(Vec::new()),
            ordinal: std::sync::atomic::AtomicU16::new(0),
            unknown: std::sync::atomic::AtomicBool::new(false),
        })
    }
    pub(crate) async fn execute_tool(
        self: &Arc<Self>,
        tools: crate::tools::ToolRegistry,
        call: crate::chat::ToolCall,
        allowed: Vec<String>,
        context: crate::tools::ToolContext,
        deadline: std::time::Instant,
    ) -> ChatResult<crate::chat::ToolReceipt> {
        let (effect, retention) = tools.durable_dispatch(&call, &allowed)?;
        let ordinal = self
            .ordinal
            .fetch_add(1, std::sync::atomic::Ordering::AcqRel)
            .checked_add(1)
            .ok_or(MemoryError::Capacity)?;
        let capture = match retention {
            crate::tools::ToolRetention::Ephemeral => super::ToolCapture::Ephemeral,
            crate::tools::ToolRetention::Retained => {
                let arguments = serde_json::to_string(&call.arguments)?;
                if arguments.len() > 16_384 {
                    return Err(crate::chat::ChatError::Limit("durable tool argument bytes"));
                }
                super::ToolCapture::Retained {
                    arguments: MemoryText::new(arguments)?,
                }
            }
        };
        let intent = super::ToolIntent {
            ordinal,
            handler: call.name.clone(),
            effect,
            capture,
        };
        let operation = self.clone();
        let (reply, receipt) = tokio::sync::oneshot::channel();
        // Register ownership without yielding. Cancellation cannot strand an
        // unregistered child whose critical writes finalization could overtake.
        {
            let mut pending = self.pending.lock().map_err(|_| MemoryError::Unavailable)?;
            pending.push(tokio::spawn(async move {
                operation.access.store.tool_intent(&operation.access.owner,OperationKey::new("product-tool-intent",format!("{}-{ordinal}",operation.record.id))?,operation.record.id,intent).await?;
                let cancellation=context.cancellation.clone();
                let (mut result, outcome)=tokio::select! {
                    biased;
                    ()=cancellation.cancelled()=>(crate::chat::ToolReceipt{call:call.clone(),result:crate::tools::failure("interrupted","no confirmed tool outcome")},super::ToolOutcome::Unknown),
                    ()=tokio::time::sleep_until(deadline.into())=>(crate::chat::ToolReceipt{call:call.clone(),result:crate::tools::failure("deadline","no confirmed tool outcome")},super::ToolOutcome::Unknown),
                    result=tools.execute(call.clone(),&allowed,context)=>{
                        let outcome=if result.result.get("ok").and_then(serde_json::Value::as_bool)==Some(true){super::ToolOutcome::Succeeded}else{super::ToolOutcome::Failed};
                        (result,outcome)
                    }
                };
                if outcome==super::ToolOutcome::Unknown {operation.unknown.store(true,std::sync::atomic::Ordering::Release);}
                let retained=match retention {
                    crate::tools::ToolRetention::Ephemeral=>None,
                    crate::tools::ToolRetention::Retained=>{
                        let mut serialized=serde_json::to_string(&result.result)?;
                        if serialized.len()>16_384 {
                            result.result=crate::tools::failure("result_too_large","tool completed; result exceeded durable byte allowance");
                            serialized=serde_json::to_string(&result.result)?;
                        }
                        Some(MemoryText::new(serialized)?)
                    }
                };
                operation.access.store.tool_result(&operation.access.owner,OperationKey::new("product-tool-result",format!("{}-{ordinal}",operation.record.id))?,operation.record.id,ordinal,outcome,retained).await?;
                let _=reply.send(result);
                Ok(())
            }));
        }
        receipt
            .await
            .map_err(|_| crate::chat::ChatError::Memory(MemoryError::Unavailable))
    }
    pub(crate) async fn prepare(&self, input: &str) -> ChatResult<String> {
        self.access
            .store
            .start_operation(
                &self.access.owner,
                OperationKey::new("product-start", self.record.id.to_string())?,
                self.record.id,
            )
            .await?;
        self.access.recall_context(input).await
    }
    pub(crate) async fn settle(
        &self,
        result: &Result<ChatResponse, RunFailure>,
    ) -> MemoryResult<()> {
        let pending =
            { std::mem::take(&mut *self.pending.lock().map_err(|_| MemoryError::Unavailable)?) };
        for task in pending {
            task.await.map_err(|_| MemoryError::Unavailable)??;
        }
        let response = match result {
            Ok(response) => response,
            Err(failure) => &failure.partial,
        };
        let outcome = if self.unknown.load(std::sync::atomic::Ordering::Acquire) {
            TerminalOutcome::Interrupted
        } else {
            match response.status {
                RunStatus::Completed | RunStatus::Truncated => TerminalOutcome::Completed,
                RunStatus::Cancelled => TerminalOutcome::Cancelled,
                RunStatus::Deadline | RunStatus::Failed => TerminalOutcome::Failed,
            }
        };
        let assistant = if response.message.content.trim().is_empty() {
            None
        } else {
            Some(MemoryText::new(response.message.content.clone())?)
        };
        let kind = if self.unknown.load(std::sync::atomic::Ordering::Acquire) {
            super::CompletionKind::Interrupted
        } else {
            match response.status {
                RunStatus::Completed => super::CompletionKind::Completed,
                RunStatus::Truncated => super::CompletionKind::Truncated,
                RunStatus::Cancelled => super::CompletionKind::Cancelled,
                RunStatus::Deadline => super::CompletionKind::Deadline,
                RunStatus::Failed => super::CompletionKind::Failed,
            }
        };
        let limit = match response.limit_reason {
            None => None,
            Some("output_tokens") => Some(super::CompletionLimit::OutputTokens),
            Some("context_capacity") => Some(super::CompletionLimit::ContextCapacity),
            Some("run_tokens") => Some(super::CompletionLimit::RunTokens),
            Some(_) => return Err(MemoryError::InvalidInput("terminal limit")),
        };
        let details = super::OperationDetails {
            kind,
            limit,
            input_tokens: Some(
                u64::try_from(response.usage.input_tokens).map_err(|_| MemoryError::Capacity)?,
            ),
            generated_tokens: Some(
                u64::try_from(response.usage.generated_tokens)
                    .map_err(|_| MemoryError::Capacity)?,
            ),
        };
        self.access
            .store
            .finalize_operation_detailed(
                &self.access.owner,
                OperationKey::new("product-finish", self.record.id.to_string())?,
                self.record.id,
                OperationFinalization { outcome, assistant },
                details,
            )
            .await?;
        self.access.wake();
        Ok(())
    }
}

impl MemoryAccess {
    pub(crate) async fn admit_product(
        &self,
        scope: &str,
        client: &str,
        request: &str,
        text: &str,
    ) -> MemoryResult<(OperationRecord, bool)> {
        let key = super::ClientRequestKey::new(scope, client, request)?;
        match self.store.operation_by_key(&self.owner, key.clone()).await {
            Ok(record) => {
                let source = self.store.source(&self.owner, record.input.id).await?;
                if source.text.as_str() != text {
                    return Err(MemoryError::Conflict);
                }
                return Ok((record, false));
            }
            Err(MemoryError::NotFound) => {}
            Err(error) => return Err(error),
        }
        let observed_at = utc_millis()?;
        let receipt = self
            .store
            .admit_operation(
                &self.owner,
                super::OperationAdmission {
                    key,
                    input: MemoryText::new(text)?,
                    observed_at,
                    limits: super::OperationLimits::new(131_072, 32, 16_384)?,
                },
            )
            .await?;
        self.wake();
        Ok((receipt.value, !receipt.replayed))
    }
}
pub(crate) fn utc_millis() -> MemoryResult<i64> {
    i64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|_| MemoryError::InvalidInput("host clock"))?
            .as_millis(),
    )
    .map_err(|_| MemoryError::InvalidInput("host clock"))
}
