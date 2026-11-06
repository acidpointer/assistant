//! `SQLite` transactions, replay receipts and raw-source indexing. Migration
//! checksums are verified before traffic. Authoritative writes and job/event
//! handoff commit together. Source suppression survives worker cancellation.

use super::super::{
    MemoryConfig, MemoryError, MemoryId, MemoryReceipt, MemoryResult, MemoryText, OperationKey,
    Revision, SearchBundle, Source, SuppressionReceipt,
};
use super::{
    OwnerCapability,
    private_dir::{PrivateDirectory, private_file},
};
use rusqlite::{
    Connection, OpenFlags, OptionalExtension, Transaction, TransactionBehavior, params,
};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use sha2::{Digest, Sha256};
use std::{
    io::{BufRead, BufReader, Read, Seek, SeekFrom, Write},
    time::Duration,
};

pub(super) const MARGIN: u64 = 64 * 1024 * 1024;

/// Admission reserves future writes; settlement and revocation can use headroom.
#[derive(Clone, Copy)]
pub(super) enum WriteBudget {
    Ordinary,
    Admission(u64),
    Reserved(MemoryId),
    Revocation,
}

pub(super) fn reserved_bytes(connection: &Connection) -> MemoryResult<u64> {
    let bytes: i64 = connection.query_row(
        "SELECT coalesce(sum(reserved_bytes),0) FROM product_operations WHERE reserved_bytes>0",
        [],
        |r| r.get(0),
    )?;
    u64::try_from(bytes).map_err(|_| MemoryError::CorruptStore)
}

pub(super) fn check_budget(
    connection: &Connection,
    used: u64,
    quota: u64,
    budget: WriteBudget,
) -> MemoryResult<()> {
    let allowance = match budget {
        WriteBudget::Ordinary => MARGIN.saturating_add(reserved_bytes(connection)?),
        WriteBudget::Admission(bytes) => MARGIN
            .saturating_add(reserved_bytes(connection)?)
            .saturating_add(bytes),
        // The active operation's maximum is already durable. Revocation payloads
        // are bounded independently and may use the 64-MiB emergency margin.
        WriteBudget::Reserved(_) | WriteBudget::Revocation => 0,
    };
    if used.saturating_add(allowance) >= quota {
        return Err(MemoryError::Capacity);
    }
    Ok(())
}

pub(super) struct Database {
    pub(super) connection: Connection,
    pub(super) directory: PrivateDirectory,
    pub(super) owner: OwnerCapability,
    pub(super) quota: u64,
    pub(super) pending_job_limit: u32,
}

pub(super) fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

pub(super) fn json<T: Serialize>(value: &T) -> MemoryResult<String> {
    Ok(serde_json::to_string(value)?)
}
pub(super) fn decode<T: DeserializeOwned>(value: &str) -> MemoryResult<T> {
    serde_json::from_str(value).map_err(|_| MemoryError::CorruptStore)
}
pub(super) fn revision(value: i64) -> MemoryResult<Revision> {
    Revision::new(value).map_err(|_| MemoryError::CorruptStore)
}

impl Database {
    pub(super) fn open(directory: &PrivateDirectory, config: &MemoryConfig) -> MemoryResult<Self> {
        if rusqlite::version_number() < 3_051_003 {
            return Err(MemoryError::UnsupportedVersion);
        }
        let mut connection = Connection::open_with_flags(
            directory.database(),
            OpenFlags::SQLITE_OPEN_READ_WRITE
                | OpenFlags::SQLITE_OPEN_NO_MUTEX
                | OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )?;
        connection.busy_timeout(Duration::from_millis(100))?;
        connection.pragma_update(None, "journal_mode", "WAL")?;
        connection.pragma_update(None, "synchronous", "FULL")?;
        connection.pragma_update(None, "foreign_keys", "ON")?;
        connection.pragma_update(None, "trusted_schema", "OFF")?;
        verify_pragmas(&connection)?;
        super::migrations::migrate(&mut connection)?;
        let (store, principal): (String, String) = connection.query_row(
            "SELECT store_id,owner_id FROM meta WHERE singleton=1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        let page_size: i64 = connection.pragma_query_value(None, "page_size", |r| r.get(0))?;
        let max_pages =
            config.quota_bytes / u64::try_from(page_size).map_err(|_| MemoryError::CorruptStore)?;
        connection.pragma_update(
            None,
            "max_page_count",
            i64::try_from(max_pages).map_err(|_| MemoryError::Capacity)?,
        )?;
        Ok(Self {
            connection,
            directory: directory.clone(),
            owner: OwnerCapability {
                store: MemoryId::parse(&store)?,
                principal: MemoryId::parse(&principal)?,
            },
            quota: config.quota_bytes,
            pending_job_limit: config.pending_job_limit,
        })
    }
    pub(super) fn reader(
        directory: &PrivateDirectory,
        config: &MemoryConfig,
        owner: &OwnerCapability,
    ) -> MemoryResult<Self> {
        let connection = Connection::open_with_flags(
            directory.database(),
            OpenFlags::SQLITE_OPEN_READ_ONLY
                | OpenFlags::SQLITE_OPEN_NO_MUTEX
                | OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )?;
        connection.busy_timeout(Duration::from_millis(100))?;
        connection.pragma_update(None, "foreign_keys", "ON")?;
        connection.pragma_update(None, "query_only", "ON")?;
        Ok(Self {
            connection,
            directory: directory.clone(),
            owner: owner.clone(),
            quota: config.quota_bytes,
            pending_job_limit: config.pending_job_limit,
        })
    }
    pub(super) fn authority(&self) -> OwnerCapability {
        self.owner.clone()
    }
    pub(super) fn arm_deadline(&self, allowance: Duration) -> MemoryResult<()> {
        let start = std::time::Instant::now();
        self.connection
            .progress_handler(1000, Some(move || start.elapsed() > allowance))?;
        Ok(())
    }
    pub(super) fn mutate<T: Serialize + DeserializeOwned>(
        &mut self,
        key: &OperationKey,
        input: &impl Serialize,
        kind: &str,
        write: impl FnOnce(&Transaction<'_>, MemoryId, Revision) -> MemoryResult<T>,
    ) -> MemoryResult<MemoryReceipt<T>> {
        self.mutate_budget(key, input, kind, WriteBudget::Ordinary, write)
    }
    pub(super) fn mutate_budget<T: Serialize + DeserializeOwned>(
        &mut self,
        key: &OperationKey,
        input: &impl Serialize,
        kind: &str,
        budget: WriteBudget,
        write: impl FnOnce(&Transaction<'_>, MemoryId, Revision) -> MemoryResult<T>,
    ) -> MemoryResult<MemoryReceipt<T>> {
        let principal = self.owner.principal.to_string();
        let (namespace, request) = key.parts();
        let digest = Sha256::digest(serde_json::to_vec(&(1_u8, kind, input))?).to_vec();
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let existing:Option<(Option<Vec<u8>>,Option<String>)>=tx.query_row("SELECT digest,receipt FROM commands WHERE principal=?1 AND namespace=?2 AND request_key=?3",params![principal,namespace,request],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
        if let Some((old, receipt)) = existing {
            let Some(old) = old else {
                return Err(MemoryError::Gone);
            };
            if old != digest {
                return Err(MemoryError::Conflict);
            }
            let suppressed:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM command_sources c JOIN forget_operations f ON f.principal=c.principal AND f.source_id=c.source_id WHERE c.principal=?1 AND c.command_id=(SELECT id FROM commands WHERE principal=?1 AND namespace=?2 AND request_key=?3))",params![principal,namespace,request],|r|r.get(0))?;
            if suppressed {
                return Err(MemoryError::Gone);
            }
            let mut receipt: MemoryReceipt<T> = decode(&receipt.ok_or(MemoryError::CorruptStore)?)?;
            receipt.replayed = true;
            return Ok(receipt);
        }
        check_budget(&tx, self.directory.used_bytes()?, self.quota, budget)?;
        let seq: i64 = tx.query_row("SELECT commit_seq FROM meta WHERE singleton=1", [], |r| {
            r.get(0)
        })?;
        let next = seq.checked_add(1).ok_or(MemoryError::Capacity)?;
        let seq = revision(next)?;
        let id = MemoryId::new()?;
        tx.execute("INSERT INTO commands(principal,id,namespace,request_key,digest) VALUES(?1,?2,?3,?4,?5)",params![principal,id.to_string(),namespace,request,digest])?;
        let value = write(&tx, id, seq)?;
        let receipt = MemoryReceipt {
            operation_id: id,
            sequence: seq,
            value,
            replayed: false,
        };
        let mut identifiers = std::collections::BTreeSet::new();
        if kind != "suppress" {
            collect_identifiers(&serde_json::to_value(&receipt.value)?, &mut identifiers);
        }
        let root = match budget {
            WriteBudget::Reserved(operation) => operation,
            _ => id,
        };
        for identifier in identifiers {
            tx.execute("INSERT OR IGNORE INTO command_sources(principal,command_id,source_id) SELECT ?1,?2,id FROM sources WHERE principal=?1 AND id=?3",params![principal,id.to_string(),identifier.to_string()])?;
        }
        // The typed host operation selects this dependency/root; prose cannot.
        if let WriteBudget::Reserved(operation) = budget {
            tx.execute("INSERT OR IGNORE INTO command_sources(principal,command_id,source_id) SELECT principal,?2,input_source FROM product_operations WHERE principal=?1 AND id=?3", params![principal,id.to_string(),operation.to_string()])?;
        }
        tx.execute(
            "UPDATE commands SET receipt=?1 WHERE principal=?2 AND id=?3",
            params![json(&receipt)?, principal, id.to_string()],
        )?;
        tx.execute("UPDATE meta SET commit_seq=?1 WHERE singleton=1", [next])?;
        // Consumer bookkeeping is not a factual event: emitting an event for
        // every acknowledgment would create an endless self-consumption loop.
        if !matches!(kind, "consumer_register" | "consumer_ack") {
            tx.execute(
                "INSERT INTO change_events(principal,seq,kind,root_cause) VALUES(?1,?2,?3,?4)",
                params![principal, next, kind, root.to_string()],
            )?;
        }
        tx.commit()?;
        Ok(receipt)
    }
    pub(super) fn remember(
        &mut self,
        key: &OperationKey,
        text: &MemoryText,
    ) -> MemoryResult<MemoryReceipt<Source>> {
        self.remember_origin(key, text, super::operations::SourceOrigin::ExplicitMemory)
    }
    pub(super) fn remember_observed(
        &mut self,
        key: &OperationKey,
        text: &MemoryText,
        now: i64,
    ) -> MemoryResult<MemoryReceipt<Source>> {
        self.remember_origin(
            key,
            text,
            super::operations::SourceOrigin::ObservedMemory { observed_at: now },
        )
    }
    fn remember_origin(
        &mut self,
        key: &OperationKey,
        text: &MemoryText,
        origin: super::operations::SourceOrigin,
    ) -> MemoryResult<MemoryReceipt<Source>> {
        let principal = self.owner.principal.to_string();
        let payload = text.clone();
        let limit = self.pending_job_limit;
        self.mutate(key, text, "remember", move |tx, operation, _| {
            let source = Source {
                id: MemoryId::new()?,
                revision: revision(1)?,
                text: payload,
                preserved: true,
            };
            super::operations::insert_source(tx, &principal, operation, &source, origin, limit)?;
            Ok(source)
        })
    }
    pub(super) fn source(&self, id: MemoryId) -> MemoryResult<Source> {
        let (rev,text,preserved):(i64,String,bool)=self.connection.query_row("SELECT revision,text,preserved FROM sources WHERE principal=?1 AND id=?2 AND suppressed=0 ORDER BY revision DESC LIMIT 1",params![self.owner.principal.to_string(),id.to_string()],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?;
        Ok(Source {
            id,
            revision: revision(rev)?,
            text: MemoryText::new(text).map_err(|_| MemoryError::CorruptStore)?,
            preserved,
        })
    }
    pub(super) fn search(
        &mut self,
        query: &MemoryText,
        limit: usize,
        min_sequence: Option<Revision>,
    ) -> MemoryResult<SearchBundle> {
        let terms = query
            .as_str()
            .split(|c: char| !c.is_alphanumeric() && c != '_')
            .filter(|s| !s.is_empty())
            .take(17)
            .collect::<Vec<_>>();
        if terms.is_empty() || terms.len() > 16 {
            return Err(MemoryError::InvalidInput("search terms"));
        }
        let literal = terms
            .iter()
            .map(|s| format!("\"{}\"", s.replace('"', "\"\"")))
            .collect::<Vec<_>>()
            .join(" OR ");
        let tx = self.connection.transaction()?;
        let seq: i64 = tx.query_row("SELECT commit_seq FROM meta", [], |r| r.get(0))?;
        if min_sequence.is_some_and(|min| seq < min.get()) {
            return Err(MemoryError::Unavailable);
        }
        let mut statement=tx.prepare("SELECT s.id,s.revision,s.text,s.preserved FROM search_fts f JOIN search_documents d ON d.rowid=f.rowid JOIN sources s ON s.principal=d.principal AND s.id=d.source_id AND s.revision=d.source_revision WHERE search_fts MATCH ?1 AND d.principal=?2 AND s.suppressed=0 ORDER BY bm25(search_fts),s.id LIMIT ?3")?;
        let rows = statement.query_map(
            params![
                literal,
                self.owner.principal.to_string(),
                i64::try_from(limit + 1).map_err(|_| MemoryError::Capacity)?
            ],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, bool>(3)?,
                ))
            },
        )?;
        let mut sources = Vec::new();
        for row in rows {
            let (id, rev, text, preserved) = row?;
            sources.push(Source {
                id: MemoryId::parse(&id)?,
                revision: revision(rev)?,
                text: MemoryText::new(text).map_err(|_| MemoryError::CorruptStore)?,
                preserved,
            });
        }
        let truncated = sources.len() > limit;
        sources.truncate(limit);
        Ok(SearchBundle {
            sequence: if seq == 0 { None } else { Some(revision(seq)?) },
            sources,
            truncated,
        })
    }
    pub(super) fn suppress_source(
        &mut self,
        key: &OperationKey,
        id: MemoryId,
        expected: Revision,
    ) -> MemoryResult<MemoryReceipt<SuppressionReceipt>> {
        let principal = self.owner.principal.to_string();
        let receipt=self.mutate_budget(key,&(id,expected),"suppress",WriteBudget::Revocation,move|tx,_,_|{
            let revision_found:Option<i64>=tx.query_row("SELECT revision FROM sources WHERE principal=?1 AND id=?2",params![principal,id.to_string()],|r|r.get(0)).optional()?;
            if revision_found!=Some(expected.get()){return Err(MemoryError::Conflict)}
            tx.execute("UPDATE meta SET deletion_generation=deletion_generation+1,derived_epoch=derived_epoch+1 WHERE singleton=1",[])?;
            let generation:i64=tx.query_row("SELECT deletion_generation FROM meta",[],|r|r.get(0))?;
            let affected = super::operations::suppression_sources(tx, &principal, id)?;
            for (source_id, source_revision) in affected {
                tx.execute("UPDATE sources SET suppressed=1 WHERE principal=?1 AND id=?2",params![principal,source_id])?;
                tx.execute("DELETE FROM search_fts WHERE rowid IN(SELECT rowid FROM search_documents WHERE principal=?1 AND source_id=?2)",params![principal,source_id])?;
                tx.execute("UPDATE jobs SET state='cancelled',fence=fence+1,lease_instance=NULL,lease_until=NULL WHERE principal=?1 AND source_id=?2",params![principal,source_id])?;
                tx.execute("UPDATE job_attempts SET result_json=NULL,completion_digest=NULL WHERE principal=?1 AND job_id IN(SELECT id FROM jobs WHERE principal=?1 AND source_id=?2)",params![principal,source_id])?;
                tx.execute("UPDATE source_metadata SET formation='not_applicable' WHERE principal=?1 AND source_id=?2",params![principal,source_id])?;
                tx.execute("INSERT INTO forget_operations(principal,source_id,source_revision,deletion_generation) VALUES(?1,?2,?3,?4) ON CONFLICT(principal,source_id,source_revision) DO UPDATE SET deletion_generation=excluded.deletion_generation,journaled=0",params![principal,source_id,source_revision,generation])?;
            }
            super::operations::redact_operation_for_source(tx, &principal, id)?;
            Ok(SuppressionReceipt{source_id:id,deletion_generation:revision(generation)?})
        })?;
        self.flush_journal()?;
        Ok(receipt)
    }
    pub(super) fn flush_journal(&mut self) -> MemoryResult<()> {
        let mut statement=self.connection.prepare("SELECT source_id,source_revision,deletion_generation FROM forget_operations WHERE principal=?1 AND journaled=0 ORDER BY deletion_generation")?;
        let rows = statement
            .query_map([self.owner.principal.to_string()], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, i64>(2)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        drop(statement);
        if rows.is_empty() {
            return Ok(());
        }
        let mut file = private_file(&self.directory.journal())?;
        file.seek(SeekFrom::End(0))?;
        for (source, source_revision, generation) in rows {
            let entry = JournalEntry {
                store: self.owner.store,
                principal: self.owner.principal,
                source: MemoryId::parse(&source)?,
                source_revision: revision(source_revision)?,
                generation: revision(generation)?,
            };
            let mut bytes = serde_json::to_vec(&entry)?;
            bytes.push(b'\n');
            file.write_all(&bytes)?;
            file.sync_all()?;
            self.connection.execute("UPDATE forget_operations SET journaled=1 WHERE principal=?1 AND source_id=?2 AND source_revision=?3 AND deletion_generation=?4",params![self.owner.principal.to_string(),source,source_revision,generation])?;
        }
        Ok(())
    }
    pub(super) fn recover(&mut self) -> MemoryResult<()> {
        let applied_generation: i64 =
            self.connection
                .query_row("SELECT deletion_generation FROM meta", [], |r| r.get(0))?;
        let file = private_file(&self.directory.journal())?;
        if file.metadata()?.len() > 64 * 1024 * 1024 {
            return Err(MemoryError::Capacity);
        }
        let mut reader = BufReader::new(file);
        let mut offset = 0_u64;
        loop {
            let mut line = Vec::new();
            let read = reader.by_ref().take(1025).read_until(b'\n', &mut line)?;
            if read == 0 {
                break;
            }
            if line.len() > 1024 {
                return Err(MemoryError::CorruptStore);
            }
            if !line.ends_with(b"\n") {
                reader.get_mut().set_len(offset)?;
                reader.get_mut().sync_all()?;
                break;
            }
            let entry: JournalEntry =
                serde_json::from_slice(&line).map_err(|_| MemoryError::CorruptStore)?;
            if entry.store != self.owner.store || entry.principal != self.owner.principal {
                return Err(MemoryError::CorruptStore);
            }
            if entry.generation.get() <= applied_generation {
                offset += u64::try_from(read).map_err(|_| MemoryError::Capacity)?;
                continue;
            }
            self.connection.execute("INSERT INTO forget_operations(principal,source_id,source_revision,deletion_generation,journaled) VALUES(?1,?2,?3,?4,1) ON CONFLICT(principal,source_id,source_revision) DO UPDATE SET deletion_generation=max(deletion_generation,excluded.deletion_generation),journaled=1",params![entry.principal.to_string(),entry.source.to_string(),entry.source_revision.get(),entry.generation.get()])?;
            self.connection.execute(
                "UPDATE sources SET suppressed=1 WHERE principal=?1 AND id=?2 AND revision=?3",
                params![
                    entry.principal.to_string(),
                    entry.source.to_string(),
                    entry.source_revision.get()
                ],
            )?;
            self.connection.execute("UPDATE meta SET deletion_generation=max(deletion_generation,?1),derived_epoch=derived_epoch+1 WHERE singleton=1",[entry.generation.get()])?;
            offset += u64::try_from(read).map_err(|_| MemoryError::Capacity)?;
        }
        self.connection.execute("DELETE FROM search_fts WHERE rowid IN(SELECT d.rowid FROM search_documents d JOIN sources s ON s.principal=d.principal AND s.id=d.source_id AND s.revision=d.source_revision WHERE s.suppressed=1)",[])?;
        self.connection.execute("UPDATE jobs SET state='ready',fence=fence+1,lease_instance=NULL,lease_until=NULL WHERE state='leased'",[])?;
        self.connection.execute("UPDATE jobs SET state='cancelled',fence=fence+1,lease_instance=NULL,lease_until=NULL WHERE EXISTS(SELECT 1 FROM sources s WHERE s.principal=jobs.principal AND s.id=jobs.source_id AND s.revision=jobs.source_revision AND s.suppressed=1)",[])?;
        self.flush_journal()?;
        super::operations::redact_suppressed_operations(&self.connection)?;
        self.recover_operations()?;
        let integrity: String = self
            .connection
            .query_row("PRAGMA quick_check", [], |r| r.get(0))?;
        if integrity != "ok" {
            return Err(MemoryError::CorruptStore);
        }
        if self
            .connection
            .prepare("PRAGMA foreign_key_check")?
            .exists([])?
        {
            return Err(MemoryError::CorruptStore);
        }
        Ok(())
    }
}

fn collect_identifiers(
    value: &serde_json::Value,
    result: &mut std::collections::BTreeSet<MemoryId>,
) {
    match value {
        serde_json::Value::Array(values) => {
            for value in values {
                collect_identifiers(value, result);
            }
        }
        serde_json::Value::Object(values) => {
            for (field, value) in values {
                // Only registered typed identity fields carry dependencies.
                // A quotation/text that looks like an ID is still literal data.
                if matches!(field.as_str(), "id" | "source_id") {
                    if let Some(value) = value.as_str()
                        && let Ok(id) = MemoryId::parse(value)
                    {
                        result.insert(id);
                    }
                } else if value.is_object() || value.is_array() {
                    collect_identifiers(value, result);
                }
            }
        }
        _ => {}
    }
}

#[derive(Serialize, Deserialize)]
struct JournalEntry {
    store: MemoryId,
    principal: MemoryId,
    source: MemoryId,
    source_revision: Revision,
    generation: Revision,
}

fn verify_pragmas(connection: &Connection) -> MemoryResult<()> {
    let journal: String = connection.pragma_query_value(None, "journal_mode", |r| r.get(0))?;
    let sync: i64 = connection.pragma_query_value(None, "synchronous", |r| r.get(0))?;
    let foreign: bool = connection.pragma_query_value(None, "foreign_keys", |r| r.get(0))?;
    if journal != "wal" || sync != 2 || !foreign {
        return Err(MemoryError::Unavailable);
    }
    Ok(())
}
