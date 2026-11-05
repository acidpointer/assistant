//! Durable event consumers. Reading copies a short snapshot; acknowledgment
//! checks the cursor and creates/coalesces dependent formation work in the same
//! writer transaction. Wake signals need not be durable. Bookkeeping does not
//! emit further events, avoiding recursive self-consumption. No event GC yet.

use super::{
    MemoryHandle, OwnerCapability,
    database::{Database, revision},
    operations::{schedule_formation, tag},
};
use crate::memory::{
    ConsumerRequirement, EventPage, MemoryChange, MemoryError, MemoryId, MemoryReceipt,
    MemoryResult, OperationKey, Revision, SourceRef,
};
use rusqlite::{Transaction, params};

impl Database {
    fn register_consumer(
        &mut self,
        key: &OperationKey,
        name: &str,
        requirement: ConsumerRequirement,
    ) -> MemoryResult<MemoryReceipt<String>> {
        let principal = self.owner.principal().to_string();
        let name = name.to_owned();
        self.mutate(
            key,
            &(name.clone(), requirement),
            "consumer_register",
            move |tx, _, _| {
                let exists: bool = tx.query_row(
                    "SELECT EXISTS(SELECT 1 FROM event_consumers WHERE principal=?1 AND name=?2)",
                    params![principal, name],
                    |r| r.get(0),
                )?;
                if exists {
                    return Err(MemoryError::Conflict);
                }
                tx.execute(
                    "INSERT INTO event_consumers(principal,name,requirement) VALUES(?1,?2,?3)",
                    params![principal, name, tag(&requirement)?],
                )?;
                Ok(name)
            },
        )
    }
    fn consumer_events(&mut self, name: &str, limit: usize) -> MemoryResult<EventPage> {
        let principal = self.owner.principal().to_string();
        let tx = self.connection.transaction()?;
        let cursor: i64 = tx.query_row(
            "SELECT cursor FROM event_consumers WHERE principal=?1 AND name=?2",
            params![principal, name],
            |r| r.get(0),
        )?;
        let mut statement=tx.prepare("SELECT seq,kind,root_cause FROM change_events WHERE principal=?1 AND seq>?2 ORDER BY seq LIMIT ?3")?;
        let rows = statement
            .query_map(
                params![
                    principal,
                    cursor,
                    i64::try_from(limit + 1)
                        .map_err(|_| MemoryError::InvalidInput("event limit"))?
                ],
                |r| {
                    Ok((
                        r.get::<_, i64>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                    ))
                },
            )?
            .collect::<Result<Vec<_>, _>>()?;
        let truncated = rows.len() > limit;
        let events = rows
            .into_iter()
            .take(limit)
            .map(|(seq, kind, root)| {
                Ok(MemoryChange {
                    sequence: revision(seq)?,
                    kind,
                    root_cause: MemoryId::parse(&root)?,
                })
            })
            .collect::<MemoryResult<Vec<_>>>()?;
        Ok(EventPage {
            after: if cursor == 0 {
                None
            } else {
                Some(revision(cursor)?)
            },
            events,
            truncated,
        })
    }
    fn acknowledge_events(
        &mut self,
        key: &OperationKey,
        name: &str,
        expected: Option<Revision>,
        through: Revision,
        sources: Vec<SourceRef>,
    ) -> MemoryResult<MemoryReceipt<Revision>> {
        let principal = self.owner.principal().to_string();
        let limit = self.pending_job_limit;
        let name = name.to_owned();
        let digest_input = (name.clone(), expected, through, sources.clone());
        let apply = move |tx: &Transaction<'_>, _: MemoryId, _: Revision| {
            let cursor: i64 = tx.query_row(
                "SELECT cursor FROM event_consumers WHERE principal=?1 AND name=?2",
                params![principal, name],
                |r| r.get(0),
            )?;
            if cursor != expected.map_or(0, Revision::get) || through.get() <= cursor {
                return Err(MemoryError::Conflict);
            }
            let (count,last):(i64,Option<i64>)=tx.query_row("SELECT count(*),max(seq) FROM change_events WHERE principal=?1 AND seq>?2 AND seq<=?3",params![principal,cursor,through.get()],|r|Ok((r.get(0)?,r.get(1)?)))?;
            if count > 32 || last != Some(through.get()) {
                return Err(MemoryError::InvalidInput("event range"));
            }
            for source in sources {
                let valid:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM sources WHERE principal=?1 AND id=?2 AND revision=?3 AND suppressed=0)",params![principal,source.id.to_string(),source.revision.get()],|r|r.get(0))?;
                if !valid {
                    return Err(MemoryError::Conflict);
                }
                schedule_formation(tx, &principal, source, limit)?;
            }
            tx.execute(
                "UPDATE event_consumers SET cursor=?1 WHERE principal=?2 AND name=?3",
                params![through.get(), principal, name],
            )?;
            Ok(through)
        };
        self.mutate(key, &digest_input, "consumer_ack", apply)
    }
}

impl MemoryHandle {
    /// Register a named consumer at the beginning of retained event history.
    /// # Errors
    /// Rejects invalid/duplicate names, key conflicts, foreign authority or storage failure.
    pub async fn register_consumer(
        &self,
        owner: &OwnerCapability,
        key: OperationKey,
        name: String,
        requirement: ConsumerRequirement,
    ) -> MemoryResult<MemoryReceipt<String>> {
        self.authorize(owner)?;
        OperationKey::new("consumer", &name)?;
        self.0
            .writer
            .call(move |db| db.register_consumer(&key, &name, requirement))
            .await
    }
    /// Copy at most 32 events after a consumer's durable cursor.
    /// # Errors
    /// Rejects invalid limits, unknown consumers, foreign authority or storage failure.
    pub async fn consumer_events(
        &self,
        owner: &OwnerCapability,
        name: String,
        limit: usize,
    ) -> MemoryResult<EventPage> {
        self.authorize(owner)?;
        OperationKey::new("consumer", &name)?;
        if !(1..=32).contains(&limit) {
            return Err(MemoryError::InvalidInput("event limit"));
        }
        self.reader()
            .call(move |db| db.consumer_events(&name, limit))
            .await
    }
    /// Advance a cursor atomically with up to 32 dependent formation requests.
    ///
    /// Sources beyond optional job quota stay deferred rather than disappearing.
    /// # Errors
    /// Rejects stale cursors, invalid ranges/sources, key conflicts or storage failure.
    pub async fn acknowledge_events(
        &self,
        owner: &OwnerCapability,
        key: OperationKey,
        name: String,
        expected: Option<Revision>,
        through: Revision,
        sources: Vec<SourceRef>,
    ) -> MemoryResult<MemoryReceipt<Revision>> {
        self.authorize(owner)?;
        OperationKey::new("consumer", &name)?;
        if sources.len() > 32 {
            return Err(MemoryError::InvalidInput("consumer jobs"));
        }
        self.0
            .writer
            .call(move |db| db.acknowledge_events(&key, &name, expected, through, sources))
            .await
    }
}
