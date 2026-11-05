//! Ordered, checksummed storage upgrades. The original schema is immutable
//! migration one, including stores created before migration history existed.
//! Each upgrade and its history/meta update share a writer transaction. Unknown
//! or modified history fails closed before recovery or reader startup.

use super::database::{hash, json};
use crate::memory::{EntityKind, MemoryError, MemoryId, MemoryResult};
use rusqlite::{Connection, TransactionBehavior, params};

const INITIAL: &str = include_str!("schema.sql");
const OPERATIONS: &str = include_str!("migrations/0002-operations.sql");
const EMBEDDINGS: &str = include_str!("migrations/0003-embeddings.sql");
const OUTCOMES: &str = include_str!("migrations/0004-product-outcomes.sql");
const MIGRATIONS: &[&str] = &[INITIAL, OPERATIONS, EMBEDDINGS, OUTCOMES];

pub(super) fn migrate(connection: &mut Connection) -> MemoryResult<()> {
    let existing: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='meta')",
        [],
        |row| row.get(0),
    )?;
    if !existing {
        let tables: i64 = connection.query_row(
            "SELECT count(*) FROM sqlite_master WHERE name NOT LIKE 'sqlite_%'",
            [],
            |r| r.get(0),
        )?;
        if tables != 0 {
            return Err(MemoryError::UnsupportedVersion);
        }
        let store = MemoryId::new()?;
        let principal = MemoryId::new()?;
        let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute_batch(INITIAL)?;
        tx.execute("INSERT INTO meta(singleton,schema_version,migration_hash,store_id,owner_id) VALUES(1,1,?1,?2,?3)", params![hash(INITIAL.as_bytes()), store.to_string(), principal.to_string()])?;
        tx.execute(
            "INSERT INTO nodes(principal,id,kind,label) VALUES(?1,?1,?2,'owner')",
            params![principal.to_string(), json(&EntityKind::Person)?],
        )?;
        tx.commit()?;
    }
    let (version, checksum): (i64, String) = connection.query_row(
        "SELECT schema_version,migration_hash FROM meta WHERE singleton=1",
        [],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    let version = usize::try_from(version).map_err(|_| MemoryError::UnsupportedVersion)?;
    if version == 0
        || version > MIGRATIONS.len()
        || checksum != hash(MIGRATIONS[version - 1].as_bytes())
    {
        return Err(MemoryError::UnsupportedVersion);
    }
    let history_exists: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='migration_history')",
        [],
        |r| r.get(0),
    )?;
    if version > 1 && !history_exists {
        return Err(MemoryError::UnsupportedVersion);
    }
    if version > 1 || history_exists {
        let mut statement = connection
            .prepare("SELECT version,checksum FROM migration_history ORDER BY version")?;
        let history = statement
            .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))?
            .collect::<Result<Vec<_>, _>>()?;
        if history.len() != version {
            return Err(MemoryError::UnsupportedVersion);
        }
        for (index, (number, checksum)) in history.iter().enumerate() {
            if usize::try_from(*number).ok() != Some(index + 1)
                || *checksum != hash(MIGRATIONS[index].as_bytes())
            {
                return Err(MemoryError::UnsupportedVersion);
            }
        }
    }
    for (index, sql) in MIGRATIONS.iter().enumerate().skip(version) {
        let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute_batch(sql)?;
        if index == 1 {
            tx.execute(
                "INSERT INTO migration_history(version,checksum) VALUES(1,?1)",
                [hash(INITIAL.as_bytes())],
            )?;
        }
        let next = i64::try_from(index + 1).map_err(|_| MemoryError::UnsupportedVersion)?;
        tx.execute(
            "INSERT INTO migration_history(version,checksum) VALUES(?1,?2)",
            params![next, hash(sql.as_bytes())],
        )?;
        tx.execute(
            "UPDATE meta SET schema_version=?1,migration_hash=?2 WHERE singleton=1",
            params![next, hash(sql.as_bytes())],
        )?;
        tx.commit()?;
    }
    Ok(())
}
