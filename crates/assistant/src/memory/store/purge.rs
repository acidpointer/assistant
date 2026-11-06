//! Bounded physical redaction after journaled suppression. A batch touches at
//! most 256 logical payload records and one MiB of accounted payload, including
//! retained tool arguments. Small FK/index maintenance is covered by a fixed
//! final-cleanup allowance; this is not a bound on `SQLite`'s physical I/O. Keys,
//! causal identities and charged compute survive. The writer owns each batch.

use super::database::Database;
use crate::memory::{MemoryError, MemoryId, MemoryResult};
use rusqlite::{Connection, TransactionBehavior, params};

fn nonnegative(value: i64) -> MemoryResult<u64> {
    u64::try_from(value).map_err(|_| MemoryError::CorruptStore)
}

fn payload_bytes(value: i64) -> MemoryResult<u64> {
    let bytes = nonnegative(value)?;
    if bytes > 1024 * 1024 {
        return Err(MemoryError::CorruptStore);
    }
    Ok(bytes)
}

struct Budget {
    records: u64,
    bytes: u64,
}

fn purge_embeddings(
    connection: &Connection,
    principal: &str,
    source: &str,
    budget: &mut Budget,
) -> MemoryResult<bool> {
    let mut statement = connection.prepare("SELECT rowid,length(vector) FROM embedding_chunks WHERE principal=?1 AND source_id=?2 LIMIT 128")?;
    let rows = statement
        .query_map(params![principal, source], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    for (row, bytes) in rows {
        if !budget.take(1, payload_bytes(bytes)?) {
            break;
        }
        connection.execute("DELETE FROM embedding_chunks WHERE rowid=?1", [row])?;
    }
    Ok(connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM embedding_chunks WHERE principal=?1 AND source_id=?2)",
        params![principal, source],
        |r| r.get(0),
    )?)
}
fn purge_connections(
    connection: &Connection,
    principal: &str,
    source: &str,
    budget: &mut Budget,
) -> MemoryResult<bool> {
    // Labels may contain private prose. Remove them under the same record/byte
    // allowance as claims, before source FK cleanup.
    for (table, predicate, size) in [
        (
            "memory_links",
            "from_id=?2 OR to_id=?2",
            "length(CAST(relation AS BLOB))",
        ),
        ("note_dependencies", "note_id=?2 OR source_id=?2", "64"),
    ] {
        let mut statement = connection.prepare(&format!(
            "SELECT rowid,{size} FROM {table} WHERE principal=?1 AND ({predicate}) LIMIT 128"
        ))?;
        let rows = statement
            .query_map(params![principal, source], |r| {
                Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        for (row, bytes) in rows {
            if !budget.take(1, payload_bytes(bytes)?) {
                break;
            }
            connection.execute(&format!("DELETE FROM {table} WHERE rowid=?1"), [row])?;
        }
        if connection.query_row(
            &format!("SELECT EXISTS(SELECT 1 FROM {table} WHERE principal=?1 AND ({predicate}))"),
            params![principal, source],
            |r| r.get::<_, bool>(0),
        )? {
            return Ok(true);
        }
    }
    Ok(false)
}
impl Budget {
    fn take(&mut self, records: u64, bytes: u64) -> bool {
        if records > self.records || bytes > self.bytes {
            return false;
        }
        self.records -= records;
        self.bytes -= bytes;
        true
    }
}

fn purge_arguments(
    connection: &Connection,
    principal: &str,
    source: &str,
    budget: &mut Budget,
) -> MemoryResult<bool> {
    let mut statement = connection.prepare(
        "SELECT rowid,length(CAST(arguments AS BLOB)) FROM tool_invocations WHERE principal=?1
             AND arguments IS NOT NULL AND operation_id IN(SELECT operation_id FROM source_metadata
             WHERE principal=?1 AND source_id=?2) LIMIT 32",
    )?;
    let rows = statement
        .query_map(params![principal, source], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    for (row, bytes) in rows {
        if !budget.take(1, payload_bytes(bytes)?) {
            break;
        }
        connection.execute(
            "UPDATE tool_invocations SET arguments=NULL WHERE rowid=?1",
            [row],
        )?;
    }
    Ok(connection.query_row("SELECT EXISTS(SELECT 1 FROM tool_invocations WHERE principal=?1 AND arguments IS NOT
             NULL AND operation_id IN(SELECT operation_id FROM source_metadata WHERE principal=?1 AND
             source_id=?2))",params![principal,source],|r|r.get(0))?)
}

fn purge_claims(
    connection: &Connection,
    principal: &str,
    source: &str,
    budget: &mut Budget,
) -> MemoryResult<bool> {
    let mut statement = connection.prepare(
        "SELECT DISTINCT c.id,c.revision,length(CAST(c.payload AS BLOB)) FROM claim_versions c
             JOIN evidence e ON e.principal=c.principal AND e.claim_id=c.id AND
             e.claim_revision=c.revision WHERE e.principal=?1 AND e.source_id=?2 ORDER BY
             c.id,c.revision LIMIT 128",
    )?;
    let rows = statement
        .query_map(params![principal, source], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, i64>(2)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    for (id, revision, bytes) in rows {
        if !budget.take(1, payload_bytes(bytes)?) {
            break;
        }
        connection.execute(
            "DELETE FROM claim_versions WHERE principal=?1 AND id=?2 AND revision=?3",
            params![principal, id, revision],
        )?;
    }
    Ok(connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM evidence WHERE principal=?1 AND source_id=?2)",
        params![principal, source],
        |r| r.get(0),
    )?)
}

fn purge_receipts(
    connection: &Connection,
    principal: &str,
    source: &str,
    budget: &mut Budget,
) -> MemoryResult<bool> {
    let mut statement = connection.prepare(
        "SELECT c.id,length(CAST(c.receipt AS BLOB))+coalesce(length(c.digest),0) FROM commands c
             JOIN command_sources d ON d.principal=c.principal AND d.command_id=c.id WHERE
             d.principal=?1 AND d.source_id=?2 AND c.receipt IS NOT NULL ORDER BY c.id LIMIT 128",
    )?;
    let rows = statement
        .query_map(params![principal, source], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    for (id, bytes) in rows {
        if !budget.take(1, payload_bytes(bytes)?) {
            break;
        }
        connection.execute(
            "UPDATE commands SET digest=NULL,receipt=NULL WHERE principal=?1 AND id=?2",
            params![principal, id],
        )?;
    }
    Ok(connection.query_row("SELECT EXISTS(SELECT 1 FROM command_sources d JOIN commands c ON c.principal=d.principal
             AND c.id=d.command_id WHERE d.principal=?1 AND d.source_id=?2 AND c.receipt IS NOT NULL)",params![principal,source],|r|r.get(0))?)
}

fn cleanup(
    connection: &Connection,
    principal: &str,
    source: &str,
    budget: &mut Budget,
) -> MemoryResult<bool> {
    let bytes:i64=connection.query_row("SELECT coalesce(sum(length(CAST(text AS BLOB))),0) FROM sources WHERE principal=?1 AND id=?2",params![principal,source],|r|r.get(0))?;
    let labels: i64 = connection.query_row(
        "SELECT count(*) FROM node_sources WHERE principal=?1 AND source_id=?2",
        params![principal, source],
        |r| r.get(0),
    )?;
    let bytes = nonnegative(bytes)?;
    let labels = nonnegative(labels)?;
    if bytes > 131_072 || labels > 128 {
        return Err(MemoryError::CorruptStore);
    }
    // Four text/index copies plus bounded metadata and 512-byte entity labels.
    if !budget.take(8 + labels, 4 * bytes + 65_536 + 512 * labels) {
        return Ok(false);
    }
    connection.execute(
        "UPDATE nodes SET label='[forgotten]' WHERE principal=?1 AND id IN(SELECT node_id FROM
             node_sources WHERE principal=?1 AND source_id=?2) AND NOT EXISTS(SELECT 1 FROM
             node_sources n JOIN sources s ON s.principal=n.principal AND s.id=n.source_id AND
             s.revision=n.source_revision WHERE n.principal=nodes.principal AND n.node_id=nodes.id
             AND s.suppressed=0)",
        params![principal, source],
    )?;
    connection.execute("DELETE FROM search_fts WHERE rowid IN(SELECT rowid FROM search_documents WHERE principal=?1 AND source_id=?2)",params![principal,source])?;
    connection.execute(
        "DELETE FROM search_documents WHERE principal=?1 AND source_id=?2",
        params![principal, source],
    )?;
    connection.execute(
        "DELETE FROM jobs WHERE principal=?1 AND source_id=?2",
        params![principal, source],
    )?;
    connection.execute(
        "UPDATE commands SET digest=NULL,receipt=NULL WHERE principal=?1 AND id IN(SELECT
             command_id FROM sources WHERE principal=?1 AND id=?2)",
        params![principal, source],
    )?;
    connection.execute("DELETE FROM product_outcomes WHERE principal=?1 AND operation_id IN(SELECT operation_id FROM source_metadata WHERE principal=?1 AND source_id=?2)",params![principal,source])?;
    connection.execute(
        "DELETE FROM assistant_notices WHERE principal=?1 AND source_id=?2",
        params![principal, source],
    )?;
    connection.execute(
        "DELETE FROM sources WHERE principal=?1 AND id=?2",
        params![principal, source],
    )?;
    connection.execute(
        "UPDATE forget_operations SET purged=1 WHERE principal=?1 AND source_id=?2",
        params![principal, source],
    )?;
    Ok(true)
}

impl Database {
    pub(super) fn purge_source_batch(&mut self, id: MemoryId) -> MemoryResult<bool> {
        let principal = self.owner.principal().to_string();
        let id = id.to_string();
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (journaled, purged): (bool, bool) = tx.query_row(
            "SELECT journaled,purged FROM forget_operations WHERE principal=?1 AND source_id=?2",
            params![principal, id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        if !journaled {
            return Err(MemoryError::Unavailable);
        }
        if purged {
            return Ok(true);
        }
        let mut budget = Budget {
            records: 256,
            bytes: 1024 * 1024,
        };
        let arguments = purge_arguments(&tx, &principal, &id, &mut budget)?;
        let embeddings = purge_embeddings(&tx, &principal, &id, &mut budget)?;
        let claims = purge_claims(&tx, &principal, &id, &mut budget)?;
        let connections = purge_connections(&tx, &principal, &id, &mut budget)?;
        let receipts = purge_receipts(&tx, &principal, &id, &mut budget)?;
        let complete = !arguments
            && !embeddings
            && !claims
            && !receipts
            && !connections
            && cleanup(&tx, &principal, &id, &mut budget)?;
        tx.commit()?;
        Ok(complete)
    }
}
