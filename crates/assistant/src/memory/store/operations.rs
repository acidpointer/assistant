//! Product operations, source attribution and required tool settlement. Only the
//! writer mutates these records; operation/source/receipt/event changes commit
//! together. Recovery interrupts active work without replaying tools. Prose is
//! retained only through the declared capture policy, and suppressed operations
//! cannot replay their old receipts or accept late results.

use super::{
    MemoryHandle, OwnerCapability,
    database::{Database, MARGIN, WriteBudget, decode, json, reserved_bytes, revision},
};
use crate::memory::{
    ClientRequestKey, MemoryError, MemoryId, MemoryReceipt, MemoryResult, MemoryText,
    OperationAdmission, OperationFinalization, OperationKey, OperationLimits, OperationRecord,
    OperationState, Source, SourceRecord, SourceRef, SourceRole, StorageHealth, StorageStatus,
    ToolCapture, ToolIntent, ToolInvocation, ToolOutcome,
};
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use serde::{Serialize, de::DeserializeOwned};
use std::sync::atomic::Ordering;

/// Persist enum values as their explicit serde string vocabulary.
pub(super) fn tag(value: &impl Serialize) -> MemoryResult<String> {
    serde_json::to_value(value)?
        .as_str()
        .map(str::to_owned)
        .ok_or(MemoryError::CorruptStore)
}
fn read_tag<T: DeserializeOwned>(value: &str) -> MemoryResult<T> {
    decode(&json(&value)?)
}
fn source_ref(id: &str) -> MemoryResult<SourceRef> {
    Ok(SourceRef {
        id: MemoryId::parse(id)?,
        revision: revision(1)?,
    })
}

#[derive(Clone, Copy)]
/// Host-selected attribution and reserved source slot, never inferred from prose.
pub(super) enum SourceOrigin {
    ExplicitMemory,
    User {
        operation: MemoryId,
        observed_at: i64,
    },
    Assistant {
        operation: MemoryId,
    },
    Tool {
        operation: MemoryId,
        ordinal: u16,
    },
}

pub(super) fn insert_source(
    tx: &Transaction<'_>,
    principal: &str,
    command: MemoryId,
    source: &Source,
    origin: SourceOrigin,
    job_limit: u32,
) -> MemoryResult<()> {
    let (role, operation, ordinal, observed, form) = match origin {
        SourceOrigin::ExplicitMemory => (SourceRole::User, None, None, None, true),
        SourceOrigin::User {
            operation,
            observed_at,
        } => (
            SourceRole::User,
            Some(operation),
            Some(0),
            Some(observed_at),
            true,
        ),
        SourceOrigin::Assistant { operation } => {
            (SourceRole::Assistant, Some(operation), Some(1), None, true)
        }
        SourceOrigin::Tool { operation, ordinal } => (
            SourceRole::Tool,
            Some(operation),
            Some(i64::from(ordinal) + 1),
            None,
            false,
        ),
    };
    tx.execute("INSERT INTO sources(principal,id,revision,command_id,text,preserved) VALUES(?1,?2,?3,?4,?5,?6)", params![principal,source.id.to_string(),source.revision.get(),command.to_string(),source.text.as_str(),source.preserved])?;
    tx.execute("INSERT INTO
             source_metadata(principal,source_id,source_revision,role,operation_id,ordinal,observed_at,formation)
             VALUES(?1,?2,?3,?4,?5,?6,?7,'not_applicable')", params![principal,source.id.to_string(),source.revision.get(),tag(&role)?,operation.map(|id|id.to_string()),ordinal,observed])?;
    tx.execute("INSERT INTO search_documents(principal,source_id,source_revision,text) VALUES(?1,?2,?3,?4)", params![principal,source.id.to_string(),source.revision.get(),source.text.as_str()])?;
    let row = tx.last_insert_rowid();
    tx.execute(
        "INSERT INTO search_fts(rowid,text) VALUES(?1,?2)",
        params![row, source.text.as_str()],
    )?;
    if form {
        schedule_formation(
            tx,
            principal,
            SourceRef {
                id: source.id,
                revision: source.revision,
            },
            job_limit,
        )?;
    }
    Ok(())
}

pub(super) fn schedule_formation(
    tx: &Transaction<'_>,
    principal: &str,
    source: SourceRef,
    limit: u32,
) -> MemoryResult<()> {
    let existing: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM jobs WHERE principal=?1 AND source_id=?2 AND source_revision=?3)", params![principal,source.id.to_string(),source.revision.get()], |r|r.get(0))?;
    if existing {
        return Ok(());
    }
    let count: i64 = tx.query_row(
        "SELECT count(*) FROM jobs WHERE state IN('ready','leased')",
        [],
        |r| r.get(0),
    )?;
    let state = if count < i64::from(limit) {
        tx.execute("INSERT INTO jobs(principal,id,job_key,source_id,source_revision,state) VALUES(?1,?2,?3,?4,?5,'ready')", params![principal,MemoryId::new()?.to_string(),format!("extract:{}:{}:v1",source.id,source.revision.get()),source.id.to_string(),source.revision.get()])?;
        "queued"
    } else {
        "deferred"
    };
    tx.execute("UPDATE source_metadata SET formation=?1 WHERE principal=?2 AND source_id=?3 AND source_revision=?4", params![state,principal,source.id.to_string(),source.revision.get()])?;
    Ok(())
}

fn operation(
    connection: &Connection,
    principal: &str,
    id: MemoryId,
) -> MemoryResult<OperationRecord> {
    let (version, state, input, assistant, unknown, redacted): (
        i64,
        String,
        String,
        Option<String>,
        bool,
        bool,
    ) = connection.query_row(
        "SELECT revision,state,input_source,assistant_source,unknown_tool_outcome,redacted FROM
             product_operations WHERE principal=?1 AND id=?2",
        params![principal, id.to_string()],
        |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
            ))
        },
    )?;
    if redacted {
        return Err(MemoryError::Gone);
    }
    let details: Option<String> = connection
        .query_row(
            "SELECT details FROM product_outcomes WHERE principal=?1 AND operation_id=?2",
            params![principal, id.to_string()],
            |r| r.get(0),
        )
        .optional()?;
    Ok(OperationRecord {
        details: details.as_deref().map(decode).transpose()?,
        id,
        revision: revision(version)?,
        state: read_tag(&state)?,
        input: source_ref(&input)?,
        assistant: assistant.as_deref().map(source_ref).transpose()?,
        unknown_tool_outcome: unknown,
    })
}
fn limits(connection: &Connection, principal: &str, id: MemoryId) -> MemoryResult<OperationLimits> {
    let value: String = connection.query_row(
        "SELECT limits_json FROM product_operations WHERE principal=?1 AND id=?2",
        params![principal, id.to_string()],
        |r| r.get(0),
    )?;
    decode(&value)
}
fn active(connection: &Connection, principal: &str, id: MemoryId) -> MemoryResult<OperationRecord> {
    let record = operation(connection, principal, id)?;
    if !matches!(
        record.state,
        OperationState::Accepted | OperationState::Running
    ) {
        return Err(MemoryError::Conflict);
    }
    Ok(record)
}
fn invocation(
    connection: &Connection,
    principal: &str,
    id: MemoryId,
    ordinal: u16,
) -> MemoryResult<ToolInvocation> {
    operation(connection, principal, id)?;
    let (handler, effect, state, result): (String, String, String, Option<String>) = connection
        .query_row(
            "SELECT handler,effect,state,result_source FROM tool_invocations WHERE principal=?1 AND
             operation_id=?2 AND ordinal=?3",
            params![principal, id.to_string(), ordinal],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )?;
    Ok(ToolInvocation {
        operation_id: id,
        ordinal,
        handler,
        effect: read_tag(&effect)?,
        outcome: if state == "intent" {
            None
        } else {
            Some(read_tag(&state)?)
        },
        result: result.as_deref().map(source_ref).transpose()?,
    })
}

pub(super) fn suppression_sources(
    connection: &Connection,
    principal: &str,
    id: MemoryId,
) -> MemoryResult<Vec<(String, i64)>> {
    // A product output/receipt may repeat any part of its own exchange. Until
    // finer dependencies exist, forgetting one source suppresses that bounded
    // operation's other sources too. Independent exchanges remain independent.
    let operation: Option<String> = connection.query_row(
        "SELECT operation_id FROM source_metadata WHERE principal=?1 AND source_id=?2",
        params![principal, id.to_string()],
        |r| r.get(0),
    )?;
    let mut statement = connection.prepare(
        "SELECT s.id,s.revision FROM sources s JOIN source_metadata m ON m.principal=s.principal
             AND m.source_id=s.id AND m.source_revision=s.revision WHERE s.principal=?1 AND (s.id=?2
             OR (?3 IS NOT NULL AND m.operation_id=?3)) ORDER BY s.id LIMIT 35",
    )?;
    let sources = statement
        .query_map(params![principal, id.to_string(), operation], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    if sources.len() > 34 {
        return Err(MemoryError::CorruptStore);
    }
    Ok(sources)
}

pub(super) fn redact_suppressed_operations(connection: &Connection) -> MemoryResult<()> {
    connection.execute("UPDATE product_operations SET redacted=1,reserved_bytes=0,revision=revision+1,state=CASE
             WHEN state IN('accepted','running') THEN 'interrupted' ELSE state END WHERE redacted=0
             AND EXISTS(SELECT 1 FROM source_metadata m JOIN sources s ON s.principal=m.principal AND
             s.id=m.source_id AND s.revision=m.source_revision WHERE
             m.principal=product_operations.principal AND m.operation_id=product_operations.id AND
             s.suppressed=1)",[])?;
    connection.execute(
        "UPDATE tool_invocations SET state='unknown' WHERE state='intent' AND EXISTS(SELECT 1
             FROM product_operations p WHERE p.principal=tool_invocations.principal AND
             p.id=tool_invocations.operation_id AND p.redacted=1)",
        [],
    )?;
    Ok(())
}

pub(super) fn redact_operation_for_source(
    connection: &Connection,
    principal: &str,
    source: MemoryId,
) -> MemoryResult<()> {
    connection.execute("UPDATE product_operations SET redacted=1,reserved_bytes=0,revision=revision+1,state=CASE
             WHEN state IN('accepted','running') THEN 'interrupted' ELSE state END WHERE principal=?1
             AND redacted=0 AND id IN(SELECT operation_id FROM source_metadata WHERE principal=?1 AND
             source_id=?2)",params![principal,source.to_string()])?;
    connection.execute(
        "UPDATE tool_invocations SET state='unknown' WHERE principal=?1 AND state='intent' AND
             operation_id IN(SELECT operation_id FROM source_metadata WHERE principal=?1 AND
             source_id=?2)",
        params![principal, source.to_string()],
    )?;
    Ok(())
}

impl Database {
    fn admit_operation(
        &mut self,
        input: &OperationAdmission,
    ) -> MemoryResult<MemoryReceipt<OperationRecord>> {
        let principal = self.owner.principal().to_string();
        let key = input.key.storage_key()?;
        let input = input.clone();
        let reservation = input.limits.reservation(input.input.as_str().len())?;
        let limit = self.pending_job_limit;
        // A reconnect observes a different host time without changing the client
        // request. Preserve the first observation instead of conflicting on retry.
        let digest_input = (input.key.clone(), input.input.clone(), input.limits);
        let apply = move |tx: &Transaction<'_>, id: MemoryId, _: crate::memory::Revision| {
            let busy:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM product_operations WHERE principal=?1 AND state IN('accepted','running'))",[&principal],|r|r.get(0))?;
            if busy {
                return Err(MemoryError::Busy);
            }
            let source = Source {
                id: MemoryId::new()?,
                revision: revision(1)?,
                text: input.input,
                preserved: false,
            };
            tx.execute("INSERT INTO
             product_operations(principal,id,input_source,state,observed_at,limits_json,reserved_bytes)
             VALUES(?1,?2,?3,'accepted',?4,?5,?6)", params![principal,id.to_string(),source.id.to_string(),input.observed_at,json(&input.limits)?,i64::try_from(reservation).map_err(|_|MemoryError::Capacity)?])?;
            insert_source(
                tx,
                &principal,
                id,
                &source,
                SourceOrigin::User {
                    operation: id,
                    observed_at: input.observed_at,
                },
                limit,
            )?;
            operation(tx, &principal, id)
        };
        self.mutate_budget(
            &key,
            &digest_input,
            "operation_admit",
            WriteBudget::Admission(reservation),
            apply,
        )
    }

    fn operation_by_key(&self, key: &ClientRequestKey) -> MemoryResult<OperationRecord> {
        let key = key.storage_key()?;
        let (namespace, request) = key.parts();
        let id: String = self.connection.query_row(
            "SELECT id FROM commands WHERE principal=?1 AND namespace=?2 AND request_key=?3",
            params![self.owner.principal().to_string(), namespace, request],
            |r| r.get(0),
        )?;
        operation(
            &self.connection,
            &self.owner.principal().to_string(),
            MemoryId::parse(&id)?,
        )
    }

    fn start_operation(
        &mut self,
        key: &OperationKey,
        id: MemoryId,
    ) -> MemoryResult<MemoryReceipt<OperationRecord>> {
        let principal = self.owner.principal().to_string();
        let digest_input = id;
        let apply = move |tx: &Transaction<'_>, _: MemoryId, _: crate::memory::Revision| {
            let record = active(tx, &principal, id)?;
            if record.state != OperationState::Accepted {
                return Err(MemoryError::Conflict);
            }
            tx.execute("UPDATE product_operations SET state='running',revision=revision+1 WHERE principal=?1 AND id=?2",params![principal,id.to_string()])?;
            operation(tx, &principal, id)
        };
        self.mutate_budget(
            key,
            &digest_input,
            "operation_start",
            WriteBudget::Reserved(id),
            apply,
        )
    }

    fn tool_intent(
        &mut self,
        key: &OperationKey,
        id: MemoryId,
        input: &ToolIntent,
    ) -> MemoryResult<MemoryReceipt<ToolInvocation>> {
        let principal = self.owner.principal().to_string();
        let input = input.clone();
        let digest_input = (id, input.clone());
        let apply = move |tx: &Transaction<'_>, _: MemoryId, _: crate::memory::Revision| {
            if active(tx, &principal, id)?.state != OperationState::Running {
                return Err(MemoryError::Conflict);
            }
            let limits = limits(tx, &principal, id)?;
            if input.ordinal == 0 || input.ordinal > limits.tool_calls {
                return Err(MemoryError::InvalidInput("tool ordinal"));
            }
            let arguments = match input.capture {
                ToolCapture::Ephemeral => None,
                ToolCapture::Retained { arguments } => {
                    if arguments.as_str().len() > limits.receipt_bytes as usize {
                        return Err(MemoryError::InvalidInput("tool argument bytes"));
                    }
                    Some(arguments)
                }
            };
            let exists:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM tool_invocations WHERE principal=?1 AND operation_id=?2 AND ordinal=?3)",params![principal,id.to_string(),input.ordinal],|r|r.get(0))?;
            if exists {
                return Err(MemoryError::Conflict);
            }
            tx.execute(
                "INSERT INTO
             tool_invocations(principal,operation_id,ordinal,handler,effect,capture,arguments,state)
             VALUES(?1,?2,?3,?4,?5,?6,?7,'intent')",
                params![
                    principal,
                    id.to_string(),
                    input.ordinal,
                    input.handler,
                    tag(&input.effect)?,
                    if arguments.is_some() {
                        "retained"
                    } else {
                        "ephemeral"
                    },
                    arguments.as_ref().map(MemoryText::as_str)
                ],
            )?;
            tx.execute(
                "UPDATE product_operations SET revision=revision+1 WHERE principal=?1 AND id=?2",
                params![principal, id.to_string()],
            )?;
            invocation(tx, &principal, id, input.ordinal)
        };
        self.mutate_budget(
            key,
            &digest_input,
            "tool_intent",
            WriteBudget::Reserved(id),
            apply,
        )
    }

    fn tool_result(
        &mut self,
        key: &OperationKey,
        id: MemoryId,
        ordinal: u16,
        outcome: ToolOutcome,
        result: Option<MemoryText>,
    ) -> MemoryResult<MemoryReceipt<ToolInvocation>> {
        let principal = self.owner.principal().to_string();
        // Check capture before serializing/digesting payloads. Ephemeral raw data
        // is never part of a durable command, even when the host misuses this API.
        operation(&self.connection, &principal, id)?;
        let capture:String=self.connection.query_row("SELECT capture FROM tool_invocations WHERE principal=?1 AND operation_id=?2 AND ordinal=?3",params![principal,id.to_string(),ordinal],|r|r.get(0))?;
        if capture == "ephemeral" && result.is_some() {
            return Err(MemoryError::InvalidInput("ephemeral tool result"));
        }
        let limit = self.pending_job_limit;
        let digest_input = (id, ordinal, outcome, result.clone());
        let apply = move |tx: &Transaction<'_>, command: MemoryId, _: crate::memory::Revision| {
            active(tx, &principal, id)?;
            let old = invocation(tx, &principal, id, ordinal)?;
            if old.outcome.is_some() {
                return Err(MemoryError::Conflict);
            }
            let limits = limits(tx, &principal, id)?;
            let source = if let Some(text) = result {
                if text.as_str().len() > limits.receipt_bytes as usize {
                    return Err(MemoryError::InvalidInput("tool result bytes"));
                }
                let source = Source {
                    id: MemoryId::new()?,
                    revision: revision(1)?,
                    text,
                    preserved: false,
                };
                insert_source(
                    tx,
                    &principal,
                    command,
                    &source,
                    SourceOrigin::Tool {
                        operation: id,
                        ordinal,
                    },
                    limit,
                )?;
                Some(source.id.to_string())
            } else {
                None
            };
            tx.execute("UPDATE tool_invocations SET state=?1,result_source=?2 WHERE principal=?3 AND operation_id=?4 AND ordinal=?5",params![tag(&outcome)?,source,principal,id.to_string(),ordinal])?;
            tx.execute("UPDATE product_operations SET revision=revision+1,unknown_tool_outcome=EXISTS(SELECT 1
             FROM tool_invocations WHERE principal=?1 AND operation_id=?2 AND state='unknown') WHERE
             principal=?1 AND id=?2",params![principal,id.to_string()])?;
            invocation(tx, &principal, id, ordinal)
        };
        self.mutate_budget(
            key,
            &digest_input,
            "tool_result",
            WriteBudget::Reserved(id),
            apply,
        )
    }

    fn finalize_operation(
        &mut self,
        key: &OperationKey,
        id: MemoryId,
        input: &OperationFinalization,
    ) -> MemoryResult<MemoryReceipt<OperationRecord>> {
        self.finalize_operation_details(key, id, input, None)
    }
    fn finalize_operation_details(
        &mut self,
        key: &OperationKey,
        id: MemoryId,
        input: &OperationFinalization,
        details: Option<crate::memory::OperationDetails>,
    ) -> MemoryResult<MemoryReceipt<OperationRecord>> {
        let principal = self.owner.principal().to_string();
        let input = input.clone();
        let limit = self.pending_job_limit;
        let kind = if details.is_some() {
            "operation_finalize_v2"
        } else {
            "operation_finalize"
        };
        let digest_input = if let Some(details) = &details {
            serde_json::to_value((id, &input, details))?
        } else {
            serde_json::to_value((id, &input))?
        };
        let apply = move |tx: &Transaction<'_>, command: MemoryId, _: crate::memory::Revision| {
            active(tx, &principal, id)?;
            let limits = limits(tx, &principal, id)?;
            let state = tag(&input.outcome)?;
            let unknown:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM tool_invocations WHERE principal=?1 AND operation_id=?2 AND
             state IN('intent','unknown'))",params![principal,id.to_string()],|r|r.get(0))?;
            if state == "completed" && unknown {
                return Err(MemoryError::Conflict);
            }
            let source = if let Some(text) = input.assistant {
                if text.as_str().len() > limits.assistant_bytes as usize {
                    return Err(MemoryError::InvalidInput("assistant bytes"));
                }
                let source = Source {
                    id: MemoryId::new()?,
                    revision: revision(1)?,
                    text,
                    preserved: false,
                };
                insert_source(
                    tx,
                    &principal,
                    command,
                    &source,
                    SourceOrigin::Assistant { operation: id },
                    limit,
                )?;
                Some(source.id.to_string())
            } else {
                None
            };
            tx.execute("UPDATE tool_invocations SET state='unknown' WHERE principal=?1 AND operation_id=?2 AND state='intent'",params![principal,id.to_string()])?;
            tx.execute("UPDATE product_operations SET
             state=?1,revision=revision+1,reserved_bytes=0,assistant_source=?2,unknown_tool_outcome=?3
             WHERE principal=?4 AND id=?5",params![state,source,unknown,principal,id.to_string()])?;
            if let Some(details) = details {
                tx.execute(
                    "INSERT INTO product_outcomes(principal,operation_id,details) VALUES(?1,?2,?3)",
                    params![principal, id.to_string(), json(&details)?],
                )?;
            }
            operation(tx, &principal, id)
        };
        self.mutate_budget(key, &digest_input, kind, WriteBudget::Reserved(id), apply)
    }

    pub(super) fn recover_operations(&mut self) -> MemoryResult<()> {
        let principal = self.owner.principal().to_string();
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let id:Option<String>=tx.query_row("SELECT id FROM product_operations WHERE principal=?1 AND state IN('accepted','running')",[&principal],|r|r.get(0)).optional()?;
        if let Some(id) = id {
            tx.execute("UPDATE tool_invocations SET state='unknown' WHERE principal=?1 AND operation_id=?2 AND state='intent'",params![principal,id])?;
            tx.execute("UPDATE product_operations SET
             state='interrupted',revision=revision+1,reserved_bytes=0,unknown_tool_outcome=EXISTS(SELECT
             1 FROM tool_invocations WHERE principal=?1 AND operation_id=?2 AND state='unknown')
             WHERE principal=?1 AND id=?2",params![principal,id])?;
            tx.execute("UPDATE meta SET commit_seq=commit_seq+1", [])?;
            tx.execute(
                "INSERT INTO change_events(principal,seq,kind,root_cause) SELECT
             ?1,commit_seq,'operation_recovery',?2 FROM meta",
                params![principal, id],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    fn source_record(&self, id: MemoryId) -> MemoryResult<SourceRecord> {
        let source = self.source(id)?;
        let (role,op,observed,formation):(String,Option<String>,Option<i64>,String)=self.connection.query_row("SELECT role,operation_id,observed_at,formation FROM source_metadata WHERE principal=?1
             AND source_id=?2 AND source_revision=?3",params![self.owner.principal().to_string(),id.to_string(),source.revision.get()],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?)))?;
        Ok(SourceRecord {
            source,
            role: read_tag(&role)?,
            operation_id: op.map(|id| MemoryId::parse(&id)).transpose()?,
            observed_at: observed,
            formation: read_tag(&formation)?,
        })
    }

    fn storage_health(&self) -> MemoryResult<StorageHealth> {
        let used = self.directory.used_bytes()?;
        let reserved = reserved_bytes(&self.connection)?;
        let pending: i64 = self.connection.query_row(
            "SELECT count(*) FROM jobs WHERE state IN('ready','leased')",
            [],
            |r| r.get(0),
        )?;
        let deferred: i64 = self.connection.query_row(
            "SELECT count(*) FROM source_metadata WHERE formation='deferred'",
            [],
            |r| r.get(0),
        )?;
        Ok(StorageHealth {
            status: if used.saturating_add(reserved).saturating_add(MARGIN) >= self.quota {
                StorageStatus::CapacityBlocked
            } else {
                StorageStatus::Ready
            },
            used_bytes: used,
            quota_bytes: self.quota,
            reserved_bytes: reserved,
            pending_jobs: u64::try_from(pending).map_err(|_| MemoryError::CorruptStore)?,
            deferred_sources: u64::try_from(deferred).map_err(|_| MemoryError::CorruptStore)?,
        })
    }
}

impl MemoryHandle {
    /// Commit a product input and its worst-case settlement reservation atomically.
    /// # Errors
    /// Rejects invalid bounds/time, foreign authority, quota, another active run or key conflict.
    pub async fn admit_operation(
        &self,
        owner: &OwnerCapability,
        input: OperationAdmission,
    ) -> MemoryResult<MemoryReceipt<OperationRecord>> {
        self.authorize(owner)?;
        if input.observed_at < 0 || input.input.as_str().len() > 16 * 1024 {
            return Err(MemoryError::InvalidInput("operation input"));
        }
        self.0
            .writer
            .call(move |db| db.admit_operation(&input))
            .await
    }
    /// Look up the current operation state by the original client key after timeout.
    /// # Errors
    /// Rejects unknown/forgotten keys, foreign authority or unavailable storage.
    pub async fn operation_by_key(
        &self,
        owner: &OwnerCapability,
        key: ClientRequestKey,
    ) -> MemoryResult<OperationRecord> {
        self.authorize(owner)?;
        self.reader()
            .call(move |db| db.operation_by_key(&key))
            .await
    }
    /// Inspect current persisted state, rather than an earlier replay receipt.
    /// # Errors
    /// Rejects unknown/forgotten operations, foreign authority or storage failure.
    pub async fn operation(
        &self,
        owner: &OwnerCapability,
        id: MemoryId,
    ) -> MemoryResult<OperationRecord> {
        self.authorize(owner)?;
        self.reader()
            .call(move |db| operation(&db.connection, &db.owner.principal().to_string(), id))
            .await
    }
    /// Record execution beginning using the admitted operation's critical lane.
    /// # Errors
    /// Rejects invalid state, key conflict, foreign authority or write failure.
    pub async fn start_operation(
        &self,
        owner: &OwnerCapability,
        key: OperationKey,
        id: MemoryId,
    ) -> MemoryResult<MemoryReceipt<OperationRecord>> {
        self.authorize(owner)?;
        self.0
            .writer
            .critical(move |db| db.start_operation(&key, id))
            .await
    }
    /// Persist typed dispatch intent before executing a registered tool handler.
    /// # Errors
    /// Rejects invalid names/bounds/state, duplicate ordinals or unavailable storage.
    pub async fn tool_intent(
        &self,
        owner: &OwnerCapability,
        key: OperationKey,
        id: MemoryId,
        input: ToolIntent,
    ) -> MemoryResult<MemoryReceipt<ToolInvocation>> {
        self.authorize(owner)?;
        OperationKey::new("handler", &input.handler)?;
        if let ToolCapture::Retained { arguments } = &input.capture
            && arguments.as_str().len() > 16_384
        {
            return Err(MemoryError::InvalidInput("tool argument bytes"));
        }
        self.0
            .writer
            .critical(move |db| db.tool_intent(&key, id, &input))
            .await
    }
    /// Commit a captured result or uncertainty before further product publication.
    /// # Errors
    /// Rejects ephemeral raw data, payload bounds, invalid state or changed/reused keys.
    pub async fn tool_result(
        &self,
        owner: &OwnerCapability,
        key: OperationKey,
        id: MemoryId,
        ordinal: u16,
        outcome: ToolOutcome,
        result: Option<MemoryText>,
    ) -> MemoryResult<MemoryReceipt<ToolInvocation>> {
        self.authorize(owner)?;
        if result
            .as_ref()
            .is_some_and(|text| text.as_str().len() > 16_384)
        {
            return Err(MemoryError::InvalidInput("tool result bytes"));
        }
        self.0
            .writer
            .critical(move |db| db.tool_result(&key, id, ordinal, outcome, result))
            .await
    }
    /// Inspect an invocation without implicitly replaying its handler.
    /// # Errors
    /// Rejects unknown/forgotten invocations, foreign authority or storage failure.
    pub async fn tool_invocation(
        &self,
        owner: &OwnerCapability,
        id: MemoryId,
        ordinal: u16,
    ) -> MemoryResult<ToolInvocation> {
        self.authorize(owner)?;
        self.reader()
            .call(move |db| {
                invocation(
                    &db.connection,
                    &db.owner.principal().to_string(),
                    id,
                    ordinal,
                )
            })
            .await
    }
    /// Atomically persist visible output and terminal state, releasing reservations.
    /// # Errors
    /// Rejects oversized output, uncertain successful completion, stale state or write failure.
    pub async fn finalize_operation(
        &self,
        owner: &OwnerCapability,
        key: OperationKey,
        id: MemoryId,
        input: OperationFinalization,
    ) -> MemoryResult<MemoryReceipt<OperationRecord>> {
        self.authorize(owner)?;
        self.0
            .writer
            .settle(move |db| db.finalize_operation(&key, id, &input))
            .await
    }
    /// Commit the exact host terminal boundary atomically with visible output.
    /// # Errors
    /// Rejects inconsistent outcome/detail pairs, stale operations and storage failure.
    pub async fn finalize_operation_detailed(
        &self,
        owner: &OwnerCapability,
        key: OperationKey,
        id: MemoryId,
        input: OperationFinalization,
        details: crate::memory::OperationDetails,
    ) -> MemoryResult<MemoryReceipt<OperationRecord>> {
        self.authorize(owner)?;
        use crate::memory::{CompletionKind, TerminalOutcome};
        let compatible = matches!(
            (input.outcome, details.kind),
            (
                TerminalOutcome::Completed,
                CompletionKind::Completed | CompletionKind::Truncated
            ) | (TerminalOutcome::Cancelled, CompletionKind::Cancelled)
                | (
                    TerminalOutcome::Failed,
                    CompletionKind::Failed | CompletionKind::Deadline
                )
                | (TerminalOutcome::Interrupted, CompletionKind::Interrupted)
        );
        if !compatible {
            return Err(MemoryError::InvalidInput("terminal details"));
        }
        self.0
            .writer
            .settle(move |db| db.finalize_operation_details(&key, id, &input, Some(details)))
            .await
    }

    /// Read literal evidence together with host attribution and formation state.
    /// # Errors
    /// Rejects unknown/suppressed sources, foreign authority or storage failure.
    pub async fn source_record(
        &self,
        owner: &OwnerCapability,
        id: MemoryId,
    ) -> MemoryResult<SourceRecord> {
        self.authorize(owner)?;
        self.reader().call(move |db| db.source_record(id)).await
    }
    /// Inspect storage counters even when ordinary writer admission is blocked.
    /// # Errors
    /// Rejects foreign authority, closed storage or failed reader inspection.
    pub async fn storage_health(&self, owner: &OwnerCapability) -> MemoryResult<StorageHealth> {
        self.authorize(owner)?;
        let mut health = self.reader().call(|db| db.storage_health()).await?;
        if self.0.writer.blocked.load(Ordering::Acquire) {
            health.status = StorageStatus::Blocked;
        }
        Ok(health)
    }
}
