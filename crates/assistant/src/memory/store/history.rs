//! Durable visible exchanges, independent of working context and event cursors.
//! Readers hydrate one short SQLite snapshot into owned bounded pages. Suppressed
//! exchanges are excluded in SQL; legacy operations explicitly lack client keys.
//! Stable autoincrement ordinals survive purge, restart and database maintenance.

use super::{MemoryHandle, OwnerCapability, database::decode};
use crate::memory::{
    ClientRequestKey, HistoryPage, MemoryError, MemoryId, MemoryResult, OperationExchange, Revision,
};
use rusqlite::params;

impl MemoryHandle {
    /// Read one eligible exchange and its visible sources in one snapshot.
    /// # Errors
    /// Rejects foreign authority, forgotten/absent operations or storage failure.
    pub async fn exchange(
        &self,
        owner: &OwnerCapability,
        id: MemoryId,
    ) -> MemoryResult<OperationExchange> {
        self.authorize(owner)?;
        self.reader().call(move |db| db.exchange(id)).await
    }

    /// Read newest-first visible exchanges before an exclusive stable cursor.
    /// Pages contain at most 32 exchanges and one MiB of visible source text.
    /// # Errors
    /// Rejects invalid bounds, foreign authority, unavailable storage or corruption.
    pub async fn history(
        &self,
        owner: &OwnerCapability,
        before: Option<Revision>,
        limit: usize,
    ) -> MemoryResult<HistoryPage> {
        self.authorize(owner)?;
        if !(1..=32).contains(&limit) {
            return Err(MemoryError::InvalidInput("history limit"));
        }
        self.reader()
            .call(move |db| {
                // Pin metadata and both sources together; end the snapshot before
                // returning owned data to an async caller.
                let tx = db.connection.unchecked_transaction()?;
                let mut statement = tx.prepare(
                    "SELECT h.ordinal,h.operation_id FROM operation_history h
                 JOIN product_operations p ON p.principal=h.principal AND p.id=h.operation_id
                 WHERE h.principal=?1 AND p.redacted=0 AND (?2 IS NULL OR h.ordinal<?2)
                 ORDER BY h.ordinal DESC LIMIT ?3",
                )?;
                let candidates = statement
                    .query_map(
                        params![
                            db.owner.principal().to_string(),
                            before.map(Revision::get),
                            i64::try_from(limit + 1).map_err(|_| MemoryError::Capacity)?
                        ],
                        |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)),
                    )?
                    .collect::<Result<Vec<_>, _>>()?;
                drop(statement);
                let mut exchanges = Vec::new();
                let mut bytes: usize = 0;
                let mut last = None;
                let mut more = false;
                for (ordinal, id) in candidates {
                    if exchanges.len() >= limit {
                        more = true;
                        break;
                    }
                    let exchange = db.exchange_in_snapshot(MemoryId::parse(&id)?)?;
                    let cost = exchange.input.source.text.as_str().len()
                        + exchange
                            .assistant
                            .as_ref()
                            .map_or(0, |s| s.source.text.as_str().len());
                    if bytes + cost > 1024 * 1024 {
                        more = true;
                        break;
                    }
                    bytes += cost;
                    last = Some(Revision::new(ordinal)?);
                    exchanges.push(exchange);
                }
                tx.commit()?;
                Ok(HistoryPage {
                    exchanges,
                    next_before: if more { last } else { None },
                })
            })
            .await
    }
}

impl super::database::Database {
    fn exchange(&self, id: MemoryId) -> MemoryResult<OperationExchange> {
        let tx = self.connection.unchecked_transaction()?;
        let result = self.exchange_in_snapshot(id)?;
        tx.commit()?;
        Ok(result)
    }

    fn exchange_in_snapshot(&self, id: MemoryId) -> MemoryResult<OperationExchange> {
        let operation = super::operations::operation(
            &self.connection,
            &self.owner.principal().to_string(),
            id,
        )?;
        let key: Option<String> = self.connection.query_row(
            "SELECT client_key FROM operation_history WHERE principal=?1 AND operation_id=?2",
            params![self.owner.principal().to_string(), id.to_string()],
            |r| r.get(0),
        )?;
        let client_key: Option<ClientRequestKey> = key.as_deref().map(decode).transpose()?;
        let input = self.source_record(operation.input.id)?;
        let assistant = operation
            .assistant
            .map(|source| self.source_record(source.id))
            .transpose()?;
        Ok(OperationExchange {
            operation,
            client_key,
            input,
            assistant,
        })
    }
}
