//! Durable product-boundary failure tests. Fixtures use private temporary stores,
//! explicit writer barriers, a real process kill and `SQLite` page exhaustion;
//! no model, network or application runtime is required.

use super::{Directory, claim, job_clock, key, text, version};
use crate::memory::{
    ClaimObject, ClientRequestKey, ConsumerRequirement, ExtractionChanges, ExtractionOutcome,
    FormationState, MemoryError, MemoryId, MemoryJobUsage, MemoryReceipt, MemoryStore,
    OperationAdmission, OperationFinalization, OperationLimits, OperationState, Predicate, Source,
    SourceRef, SourceRole, StorageStatus, TerminalOutcome, ToolCapture, ToolEffect, ToolIntent,
    ToolOutcome,
};
use std::{fs, os::unix::fs::PermissionsExt};

fn admission(request: &str) -> OperationAdmission {
    OperationAdmission {
        key: ClientRequestKey::new("test", "owner-client", request).expect("client key"),
        input: text("My sister Lena is a veterinarian in Lviv."),
        observed_at: 1_000_000,
        limits: OperationLimits::new(131_072, 2, 16_384).expect("limits"),
    }
}

#[tokio::test]
async fn preservation_keeps_one_source_and_job_and_replay_cannot_resurrect_forgetting() {
    let dir = Directory::new();
    let store = MemoryStore::open(dir.config()).await.expect("store");
    let handle = store.handle();
    let owner = store.owner();
    let admitted = handle
        .admit_operation(&owner, admission("preserve"))
        .await
        .expect("input committed")
        .value;
    assert!(
        !handle
            .source(&owner, admitted.input.id)
            .await
            .expect("source")
            .preserved
    );
    let before = handle.storage_health(&owner).await.expect("health");
    let saved = handle
        .preserve_source(&owner, key("preserve-source"), admitted.input)
        .await
        .expect("preserve existing");
    assert_eq!(saved.value.id, admitted.input.id);
    assert!(saved.value.preserved);
    assert!(
        handle
            .preserve_source(&owner, key("preserve-source"), admitted.input)
            .await
            .expect("replay")
            .replayed
    );
    assert_eq!(
        handle
            .storage_health(&owner)
            .await
            .expect("health")
            .pending_jobs,
        before.pending_jobs,
        "retention never grants model work"
    );
    assert_eq!(
        handle
            .history(&owner, None, 32)
            .await
            .expect("history")
            .exchanges
            .len(),
        1
    );
    handle
        .suppress_source(
            &owner,
            key("forget-preserved"),
            admitted.input.id,
            admitted.input.revision,
        )
        .await
        .expect("journaled suppression");
    assert!(matches!(
        handle
            .preserve_source(&owner, key("preserve-source"), admitted.input)
            .await,
        Err(MemoryError::Gone)
    ));
    assert!(
        handle
            .history(&owner, None, 32)
            .await
            .expect("eligible history")
            .exchanges
            .is_empty()
    );
    store.shutdown().await.expect("shutdown");
}
fn finalization(outcome: TerminalOutcome) -> OperationFinalization {
    OperationFinalization {
        outcome,
        assistant: Some(text("I will keep that in mind.")),
    }
}
fn retained(ordinal: u16) -> ToolIntent {
    ToolIntent {
        ordinal,
        handler: "calculator".into(),
        effect: ToolEffect::ReadOnly,
        capture: ToolCapture::Retained {
            arguments: text("{\"expression\":\"2+2\"}"),
        },
    }
}

fn legacy_fixture(dir: &Directory) -> Source {
    use sha2::{Digest, Sha256};
    let sql = include_str!("store/schema.sql");
    assert_eq!(
        format!("{:x}", Sha256::digest(sql.as_bytes())),
        "d487c3f2e3d43315d41da8de7a62b962e19d6c0197743951e4fa4f6c3f552619",
        "migration one must remain immutable"
    );
    let connection =
        rusqlite::Connection::open(dir.0.join("memory.sqlite3")).expect("legacy database");
    connection.execute_batch(sql).expect("original schema");
    let store = MemoryId::new().expect("store id");
    let principal = MemoryId::new().expect("principal id");
    let command = MemoryId::new().expect("command id");
    let source = Source {
        id: MemoryId::new().expect("source id"),
        revision: version(),
        text: text("Я віддаю перевагу чаю."),
        preserved: true,
    };
    let receipt = MemoryReceipt {
        operation_id: command,
        sequence: version(),
        value: source.clone(),
        replayed: false,
    };
    connection.execute("INSERT INTO meta(singleton,schema_version,migration_hash,store_id,owner_id,commit_seq) VALUES(1,1,?1,?2,?3,1)",rusqlite::params![format!("{:x}",Sha256::digest(sql.as_bytes())),store.to_string(),principal.to_string()]).expect("legacy meta");
    connection
        .execute(
            "INSERT INTO nodes(principal,id,kind,label) VALUES(?1,?1,'\"person\"','owner')",
            [principal.to_string()],
        )
        .expect("owner");
    let digest = Sha256::digest(
        serde_json::to_vec(&(1_u8, "remember", &source.text)).expect("canonical legacy digest"),
    )
    .to_vec();
    connection.execute("INSERT INTO commands(principal,id,namespace,request_key,digest,receipt) VALUES(?1,?2,'test','legacy',?3,?4)",rusqlite::params![principal.to_string(),command.to_string(),digest,serde_json::to_string(&receipt).expect("receipt")]).expect("legacy command");
    connection.execute("INSERT INTO sources(principal,id,revision,command_id,text,preserved) VALUES(?1,?2,1,?3,?4,1)",rusqlite::params![principal.to_string(),source.id.to_string(),command.to_string(),source.text.as_str()]).expect("legacy source");
    connection
        .execute(
            "INSERT INTO command_sources VALUES(?1,?2,?3)",
            rusqlite::params![
                principal.to_string(),
                command.to_string(),
                source.id.to_string()
            ],
        )
        .expect("source dependency");
    connection.execute("INSERT INTO search_documents(principal,source_id,source_revision,text) VALUES(?1,?2,1,?3)",rusqlite::params![principal.to_string(),source.id.to_string(),source.text.as_str()]).expect("legacy search document");
    connection
        .execute(
            "INSERT INTO search_fts(rowid,text) VALUES(?1,?2)",
            rusqlite::params![connection.last_insert_rowid(), source.text.as_str()],
        )
        .expect("legacy index");
    connection.close().expect("close legacy fixture");
    fs::set_permissions(
        dir.0.join("memory.sqlite3"),
        fs::Permissions::from_mode(0o600),
    )
    .expect("private legacy database");
    source
}

#[tokio::test]
async fn migration_preserves_legacy_data_and_rejects_tampered_history() {
    let dir = Directory::new();
    let old = legacy_fixture(&dir);
    let store = MemoryStore::open(dir.config()).await.expect("upgrade v1");
    let record = store
        .handle()
        .source_record(&store.owner(), old.id)
        .await
        .expect("attributed legacy source");
    assert_eq!(record.source.text.as_str(), old.text.as_str());
    assert_eq!(record.role, SourceRole::User);
    assert_eq!(record.observed_at, None, "must not fabricate an event date");
    assert!(
        store
            .handle()
            .remember(&store.owner(), key("legacy"), old.text)
            .await
            .expect("legacy replay")
            .replayed
    );
    assert_eq!(
        store
            .handle()
            .search(&store.owner(), text("чаю"), 1, None)
            .await
            .expect("old FTS")
            .sources
            .len(),
        1
    );
    store.shutdown().await.expect("shutdown");
    let connection =
        rusqlite::Connection::open(dir.0.join("memory.sqlite3")).expect("inspect migrations");
    assert_eq!(
        connection
            .query_row("SELECT count(*) FROM migration_history", [], |r| r
                .get::<_, i64>(0))
            .expect("history"),
        8
    );
    connection
        .execute(
            "UPDATE migration_history SET checksum='tampered' WHERE version=1",
            [],
        )
        .expect("inject modified history");
    connection.close().expect("close fixture");
    assert!(matches!(
        MemoryStore::open(dir.config()).await,
        Err(MemoryError::UnsupportedVersion)
    ));
}

#[tokio::test]
async fn failed_upgrade_rolls_back_schema_and_can_be_retried() {
    let dir = Directory::new();
    let old = legacy_fixture(&dir);
    let connection = rusqlite::Connection::open(dir.0.join("memory.sqlite3")).expect("fixture");
    connection
        .execute_batch("CREATE TABLE tool_invocations(conflict TEXT)")
        .expect("inject migration failure");
    connection.close().expect("close");
    assert!(MemoryStore::open(dir.config()).await.is_err());
    let connection =
        rusqlite::Connection::open(dir.0.join("memory.sqlite3")).expect("inspect rollback");
    assert_eq!(
        connection
            .query_row("SELECT schema_version FROM meta", [], |r| r
                .get::<_, i64>(0))
            .expect("version"),
        1
    );
    assert_eq!(connection.query_row("SELECT count(*) FROM sqlite_master WHERE name IN('product_operations','migration_history')",[],|r|r.get::<_,i64>(0)).expect("rolled back DDL"),0);
    connection
        .execute_batch("DROP TABLE tool_invocations")
        .expect("remove injected failure");
    connection.close().expect("close");
    let store = MemoryStore::open(dir.config())
        .await
        .expect("retry upgrade");
    assert_eq!(
        store
            .handle()
            .source(&store.owner(), old.id)
            .await
            .expect("source intact")
            .text
            .as_str(),
        old.text.as_str()
    );
    store.shutdown().await.expect("shutdown");
}

#[tokio::test]
#[allow(
    clippy::too_many_lines,
    reason = "Keep the durable exchange and restart trace in one test."
)]
async fn operation_receipts_capture_and_terminal_state_survive_restart() {
    let dir = Directory::new();
    let store = MemoryStore::open(dir.config()).await.expect("store");
    let h = store.handle();
    let o = store.owner();
    let request = admission("complete");
    let admitted = h
        .admit_operation(&o, request.clone())
        .await
        .expect("durable acceptance");
    let id = admitted.value.id;
    assert_eq!(
        h.source_record(&o, admitted.value.input.id)
            .await
            .expect("source attribution")
            .observed_at,
        Some(request.observed_at)
    );
    assert!(
        !h.source(&o, admitted.value.input.id)
            .await
            .expect("ordinary capture")
            .preserved
    );
    assert!(
        h.admit_operation(&o, request.clone())
            .await
            .expect("retry")
            .replayed
    );
    let mut changed = request.clone();
    changed.input = text("different");
    assert!(matches!(
        h.admit_operation(&o, changed).await,
        Err(MemoryError::Conflict)
    ));
    assert!(matches!(
        h.admit_operation(&o, admission("another")).await,
        Err(MemoryError::Busy)
    ));
    assert!(
        h.storage_health(&o)
            .await
            .expect("reservation")
            .reserved_bytes
            > 0
    );
    h.start_operation(&o, key("start"), id)
        .await
        .expect("start");
    h.tool_intent(&o, key("intent"), id, retained(1))
        .await
        .expect("durable intent");
    let result = h
        .tool_result(
            &o,
            key("result"),
            id,
            1,
            ToolOutcome::Succeeded,
            Some(text("4")),
        )
        .await
        .expect("confirmed receipt");
    let result_source = result.value.result.expect("retained source");
    assert_eq!(
        h.source_record(&o, result_source.id)
            .await
            .expect("tool attribution")
            .role,
        SourceRole::Tool
    );
    assert!(
        h.tool_result(
            &o,
            key("result"),
            id,
            1,
            ToolOutcome::Succeeded,
            Some(text("4"))
        )
        .await
        .expect("tool replay")
        .replayed
    );
    let done = h
        .finalize_operation(
            &o,
            key("done"),
            id,
            finalization(TerminalOutcome::Completed),
        )
        .await
        .expect("durable terminal");
    assert_eq!(done.value.state, OperationState::Completed);
    assert_eq!(
        h.source_record(&o, done.value.assistant.expect("assistant source").id)
            .await
            .expect("assistant attribution")
            .role,
        SourceRole::Assistant
    );
    assert_eq!(
        h.storage_health(&o).await.expect("release").reserved_bytes,
        0
    );
    store.shutdown().await.expect("shutdown");
    let store = MemoryStore::open(dir.config()).await.expect("reopen");
    let h = store.handle();
    let o = store.owner();
    assert_eq!(
        h.operation_by_key(&o, request.key)
            .await
            .expect("status after reconnect")
            .state,
        OperationState::Completed
    );
    assert!(
        h.finalize_operation(
            &o,
            key("done"),
            id,
            finalization(TerminalOutcome::Completed)
        )
        .await
        .expect("terminal replay")
        .replayed
    );
    assert_eq!(
        h.tool_invocation(&o, id, 1)
            .await
            .expect("durable result")
            .outcome,
        Some(ToolOutcome::Succeeded)
    );
    store.shutdown().await.expect("shutdown");
}

#[tokio::test]
async fn ephemeral_payloads_and_false_success_are_rejected() {
    let dir = Directory::new();
    let store = MemoryStore::open(dir.config()).await.expect("store");
    let h = store.handle();
    let o = store.owner();
    let id = h
        .admit_operation(&o, admission("ephemeral"))
        .await
        .expect("accept")
        .value
        .id;
    h.start_operation(&o, key("start"), id)
        .await
        .expect("start");
    h.tool_intent(
        &o,
        key("intent"),
        id,
        ToolIntent {
            ordinal: 1,
            handler: "process_top".into(),
            effect: ToolEffect::ReadOnly,
            capture: ToolCapture::Ephemeral,
        },
    )
    .await
    .expect("minimal intent");
    assert!(matches!(
        h.tool_result(
            &o,
            key("bad-result"),
            id,
            1,
            ToolOutcome::Succeeded,
            Some(text("private-process-secret"))
        )
        .await,
        Err(MemoryError::InvalidInput(_))
    ));
    assert!(matches!(
        h.finalize_operation(
            &o,
            key("false-success"),
            id,
            finalization(TerminalOutcome::Completed)
        )
        .await,
        Err(MemoryError::Conflict)
    ));
    let done = h
        .finalize_operation(
            &o,
            key("cancel"),
            id,
            finalization(TerminalOutcome::Cancelled),
        )
        .await
        .expect("cancel settlement");
    assert!(done.value.unknown_tool_outcome);
    assert_eq!(
        h.tool_invocation(&o, id, 1)
            .await
            .expect("uncertainty")
            .outcome,
        Some(ToolOutcome::Unknown)
    );
    assert!(
        h.search(&o, text("private-process-secret"), 8, None)
            .await
            .expect("no retained data")
            .sources
            .is_empty()
    );
    store.shutdown().await.expect("shutdown");
    let connection =
        rusqlite::Connection::open(dir.0.join("memory.sqlite3")).expect("inspect capture");
    assert_eq!(connection.query_row("SELECT count(*) FROM tool_invocations WHERE arguments IS NOT NULL OR result_source IS NOT NULL",[],|r|r.get::<_,i64>(0)).expect("ephemeral metadata"),0);
    assert_eq!(
        connection
            .query_row(
                "SELECT count(*) FROM commands WHERE request_key IN('bad-result','false-success')",
                [],
                |r| r.get::<_, i64>(0)
            )
            .expect("rollback invalid receipts"),
        0
    );
}

#[tokio::test]
async fn quota_blocks_optional_writes_but_allows_settlement_and_suppression() {
    let dir = Directory::new();
    let config = dir.config().with_quota(128 * 1024 * 1024).expect("quota");
    let store = MemoryStore::open(config).await.expect("store");
    let h = store.handle();
    let o = store.owner();
    let admitted = h
        .admit_operation(&o, admission("quota"))
        .await
        .expect("reserve acceptance");
    let before = h.storage_health(&o).await.expect("health");
    let filler = fs::File::create(dir.0.join("quota-fixture")).expect("sparse quota fixture");
    filler
        .set_len(before.quota_bytes - 64 * 1024 * 1024 - before.used_bytes + 1)
        .expect("reach ordinary threshold");
    assert_eq!(
        h.storage_health(&o).await.expect("blocked health").status,
        StorageStatus::CapacityBlocked
    );
    assert!(matches!(
        h.remember(&o, key("optional"), text("refuse new growth"))
            .await,
        Err(MemoryError::Capacity)
    ));
    let used = h
        .storage_health(&o)
        .await
        .expect("before finalization")
        .used_bytes;
    let done = h
        .finalize_operation(
            &o,
            key("done"),
            admitted.value.id,
            OperationFinalization {
                outcome: TerminalOutcome::Completed,
                assistant: Some(text(&"\"".repeat(131_072))),
            },
        )
        .await
        .expect("reserved terminal write");
    let after = h.storage_health(&o).await.expect("terminal health");
    assert_eq!(after.reserved_bytes, 0);
    assert!(
        after.used_bytes - used <= before.reserved_bytes,
        "fixture WAL growth fits conservative reservation"
    );
    h.suppress_source(
        &o,
        key("forget"),
        admitted.value.input.id,
        admitted.value.input.revision,
    )
    .await
    .expect("forget at ordinary quota threshold");
    assert!(matches!(
        h.operation(&o, admitted.value.id).await,
        Err(MemoryError::Gone)
    ));
    assert!(
        h.source(&o, done.value.assistant.expect("output").id)
            .await
            .is_err()
    );
    while !h.purge_next_batch(&o).await.expect("bounded purge") {}
    store.shutdown().await.expect("shutdown");
}

#[tokio::test]
async fn physical_page_exhaustion_never_acknowledges_a_terminal_write() {
    let dir = Directory::new();
    let store = MemoryStore::open(dir.config()).await.expect("store");
    let h = store.handle();
    let o = store.owner();
    let admitted = h
        .admit_operation(&o, admission("diskfull"))
        .await
        .expect("accept");
    h.test_limit_database_pages()
        .await
        .expect("inject page exhaustion");
    assert!(matches!(
        h.finalize_operation(
            &o,
            key("done"),
            admitted.value.id,
            OperationFinalization {
                outcome: TerminalOutcome::Completed,
                assistant: Some(text(&"z".repeat(131_072)))
            }
        )
        .await,
        Err(MemoryError::StorageFull)
    ));
    assert_eq!(
        h.operation(&o, admitted.value.id)
            .await
            .expect("no false completion")
            .state,
        OperationState::Accepted
    );
    assert_eq!(
        h.storage_health(&o).await.expect("writer blocked").status,
        StorageStatus::Blocked
    );
    assert!(matches!(
        h.admit_operation(&o, admission("another")).await,
        Err(MemoryError::Unavailable)
    ));
    store.shutdown().await.expect("shutdown");
    let store = MemoryStore::open(dir.config()).await.expect("recover");
    assert_eq!(
        store
            .handle()
            .operation(&store.owner(), admitted.value.id)
            .await
            .expect("interruption")
            .state,
        OperationState::Interrupted
    );
    store.shutdown().await.expect("shutdown");
}

#[tokio::test]
async fn finalization_has_capacity_and_priority_when_both_other_lanes_are_full() {
    let dir = Directory::new();
    let store = MemoryStore::open(dir.config()).await.expect("store");
    let h = store.handle();
    let o = store.owner();
    let id = h
        .admit_operation(&o, admission("saturation"))
        .await
        .expect("accept")
        .value
        .id;
    let (started, ready) = tokio::sync::oneshot::channel();
    let (release, receiver) = std::sync::mpsc::channel();
    let writer = h.clone();
    let barrier = tokio::spawn(async move { writer.test_writer_barrier(started, receiver).await });
    ready.await.expect("writer blocked");
    let mut ordinary = Vec::new();
    for i in 0..55 {
        let (h, o) = (h.clone(), o.clone());
        ordinary.push(tokio::spawn(async move {
            h.remember(
                &o,
                key(&format!("queued-{i}")),
                text("ordinary queued source"),
            )
            .await
        }));
    }
    let deadline = std::time::Instant::now();
    while h.test_writer_pending() < 56 {
        assert!(deadline.elapsed().as_secs() < 3, "ordinary enqueue");
        tokio::task::yield_now().await;
    }
    assert!(matches!(
        h.remember(&o, key("overflow"), text("overflow")).await,
        Err(MemoryError::Capacity)
    ));
    let mut critical = Vec::new();
    for i in 0..7 {
        let (h, o) = (h.clone(), o.clone());
        critical.push(tokio::spawn(async move {
            h.start_operation(&o, key(&format!("start-{i}")), id).await
        }));
    }
    while h.test_writer_critical_pending() < 7 {
        assert!(deadline.elapsed().as_secs() < 3, "critical enqueue");
        tokio::task::yield_now().await;
    }
    let (settler, owner) = (h.clone(), o.clone());
    let terminal = tokio::spawn(async move {
        settler
            .finalize_operation(
                &owner,
                key("done"),
                id,
                finalization(TerminalOutcome::Cancelled),
            )
            .await
    });
    while !h.test_settlement_pending() {
        assert!(deadline.elapsed().as_secs() < 3, "settlement enqueue");
        tokio::task::yield_now().await;
    }
    release.send(()).expect("release writer");
    barrier.await.expect("barrier task").expect("barrier");
    let terminal = terminal
        .await
        .expect("settlement task")
        .expect("reserved settlement");
    for task in critical {
        assert!(matches!(
            task.await.expect("critical task"),
            Err(MemoryError::Conflict)
        ));
    }
    for task in ordinary {
        assert!(
            task.await
                .expect("ordinary task")
                .expect("ordinary commit")
                .sequence
                > terminal.sequence
        );
    }
    store.shutdown().await.expect("drain shutdown");
}

#[tokio::test]
#[allow(
    clippy::too_many_lines,
    reason = "Keep cursor rollback and dependent job scheduling in one trace."
)]
async fn deferred_formation_and_consumer_cursor_commit_together() {
    let dir = Directory::new();
    let config = dir.config().with_pending_job_limit(1).expect("job limit");
    let store = MemoryStore::open(config.clone()).await.expect("store");
    let h = store.handle();
    let o = store.owner();
    h.remember(&o, key("one"), text("one source"))
        .await
        .expect("first job");
    let two = h
        .remember(&o, key("two"), text("second source remains searchable"))
        .await
        .expect("literal saved despite quota")
        .value;
    assert_eq!(
        h.source_record(&o, two.id)
            .await
            .expect("deferred")
            .formation,
        FormationState::Deferred
    );
    assert_eq!(
        h.search(&o, text("searchable"), 1, None)
            .await
            .expect("literal recall")
            .sources
            .len(),
        1
    );
    h.register_consumer(
        &o,
        key("consumer"),
        "formation".into(),
        ConsumerRequirement::Required,
    )
    .await
    .expect("consumer");
    let lease = h
        .claim_extraction_job(&o, job_clock())
        .await
        .expect("claim")
        .expect("job");
    h.finish_extraction_job(
        &o,
        lease,
        ExtractionChanges::default(),
        ExtractionOutcome::Unresolved,
        MemoryJobUsage::default(),
    )
    .await
    .expect("release pending job");
    let events = h
        .consumer_events(&o, "formation".into(), 32)
        .await
        .expect("events");
    let through = events.events.last().expect("factual changes").sequence;
    assert!(matches!(
        h.acknowledge_events(
            &o,
            key("bad-ack"),
            "formation".into(),
            None,
            through,
            vec![SourceRef {
                id: MemoryId::new().expect("absent source"),
                revision: version()
            }]
        )
        .await,
        Err(MemoryError::Conflict)
    ));
    assert_eq!(
        h.consumer_events(&o, "formation".into(), 32)
            .await
            .expect("cursor unchanged")
            .after,
        None
    );
    assert_eq!(
        h.source_record(&o, two.id)
            .await
            .expect("no partial scheduling")
            .formation,
        FormationState::Deferred
    );
    h.acknowledge_events(
        &o,
        key("ack"),
        "formation".into(),
        None,
        through,
        vec![SourceRef {
            id: two.id,
            revision: two.revision,
        }],
    )
    .await
    .expect("cursor and job commit");
    assert_eq!(
        h.source_record(&o, two.id)
            .await
            .expect("scheduled")
            .formation,
        FormationState::Queued
    );
    assert!(
        h.consumer_events(&o, "formation".into(), 32)
            .await
            .expect("no recursive bookkeeping events")
            .events
            .is_empty()
    );
    store.shutdown().await.expect("shutdown");
    let store = MemoryStore::open(config).await.expect("restart");
    assert_eq!(
        store
            .handle()
            .consumer_events(&store.owner(), "formation".into(), 32)
            .await
            .expect("durable cursor")
            .after,
        Some(through)
    );
    store.shutdown().await.expect("shutdown");
}

#[tokio::test]
async fn forgetting_revokes_all_exchange_receipts_and_rejects_late_results() {
    let dir = Directory::new();
    let store = MemoryStore::open(dir.config()).await.expect("store");
    let h = store.handle();
    let o = store.owner();
    let request = admission("forget");
    let accepted = h
        .admit_operation(&o, request.clone())
        .await
        .expect("accept")
        .value;
    h.start_operation(&o, key("start"), accepted.id)
        .await
        .expect("start");
    h.tool_intent(&o, key("intent"), accepted.id, retained(1))
        .await
        .expect("intent");
    let receipt = h
        .tool_result(
            &o,
            key("result"),
            accepted.id,
            1,
            ToolOutcome::Succeeded,
            Some(text("retained secret receipt")),
        )
        .await
        .expect("result");
    h.tool_intent(&o, key("late-intent"), accepted.id, retained(2))
        .await
        .expect("pending result");
    h.suppress_source(
        &o,
        key("forget"),
        receipt.value.result.expect("source").id,
        version(),
    )
    .await
    .expect("suppress exchange");
    assert!(matches!(
        h.admit_operation(&o, request.clone()).await,
        Err(MemoryError::Gone)
    ));
    assert!(matches!(
        h.start_operation(&o, key("start"), accepted.id).await,
        Err(MemoryError::Gone)
    ));
    assert!(matches!(
        h.tool_result(
            &o,
            key("late"),
            accepted.id,
            2,
            ToolOutcome::Succeeded,
            Some(text("late secret"))
        )
        .await,
        Err(MemoryError::Gone)
    ));
    assert!(h.source(&o, accepted.input.id).await.is_err());
    while !h.purge_next_batch(&o).await.expect("purge exchange") {}
    store.shutdown().await.expect("shutdown");
    let connection =
        rusqlite::Connection::open(dir.0.join("memory.sqlite3")).expect("inspect purge");
    assert_eq!(
        connection
            .query_row(
                "SELECT count(*) FROM tool_invocations WHERE arguments IS NOT NULL",
                [],
                |r| r.get::<_, i64>(0)
            )
            .expect("arguments purged"),
        0
    );
    connection.close().expect("close");
    let store = MemoryStore::open(dir.config()).await.expect("restart");
    assert!(matches!(
        store
            .handle()
            .operation_by_key(&store.owner(), request.key)
            .await,
        Err(MemoryError::Gone)
    ));
    store.shutdown().await.expect("shutdown");
}

#[tokio::test]
async fn history_over_one_page_has_an_explicit_continuation() {
    let dir = Directory::new();
    let store = MemoryStore::open(dir.config()).await.expect("store");
    let h = store.handle();
    let o = store.owner();
    let source = h
        .remember(&o, key("source"), text("I prefer tea."))
        .await
        .expect("source")
        .value;
    let input = claim(
        o.principal(),
        Predicate::Prefers,
        ClaimObject::Text(text("tea")),
        &source,
        source.text.as_str(),
    );
    let mut current = h
        .add_claim(&o, key("claim"), input.clone())
        .await
        .expect("claim")
        .value;
    for i in 0..256 {
        current = h
            .correct_claim(
                &o,
                key(&format!("correction-{i}")),
                current.id,
                current.revision,
                input.clone(),
            )
            .await
            .expect("version")
            .value;
    }
    assert!(matches!(
        h.claim_history(&o, current.id).await,
        Err(MemoryError::Capacity)
    ));
    let mut after = None;
    let mut count = 0;
    loop {
        let page = h
            .claim_history_page(&o, current.id, after, 128)
            .await
            .expect("page");
        count += page.claims.len();
        after = page.next_after;
        if after.is_none() {
            break;
        }
    }
    assert_eq!(count, 257);
    store.shutdown().await.expect("shutdown");
}

#[tokio::test]
async fn operation_crash_child() {
    let Some(path) = std::env::var_os("ASSISTANT_OPERATION_CRASH_DIRECTORY") else {
        return;
    };
    let path = std::path::PathBuf::from(path);
    let store = MemoryStore::open(crate::memory::MemoryConfig::new(&path))
        .await
        .expect("child store");
    let h = store.handle();
    let o = store.owner();
    let id = h
        .admit_operation(&o, admission("crash"))
        .await
        .expect("child admission")
        .value
        .id;
    h.start_operation(&o, key("start"), id)
        .await
        .expect("start");
    h.tool_intent(&o, key("first-intent"), id, retained(1))
        .await
        .expect("intent");
    h.tool_result(
        &o,
        key("first-result"),
        id,
        1,
        ToolOutcome::Succeeded,
        Some(text("4")),
    )
    .await
    .expect("result");
    h.tool_intent(
        &o,
        key("second-intent"),
        id,
        ToolIntent {
            ordinal: 2,
            handler: "external_action".into(),
            effect: ToolEffect::External,
            capture: ToolCapture::Ephemeral,
        },
    )
    .await
    .expect("external intent");
    fs::write(path.join("external-effect"), "performed once").expect("simulated effect");
    fs::write(path.join("ready.tmp"), id.to_string()).expect("receipt");
    fs::rename(path.join("ready.tmp"), path.join("ready")).expect("publish readiness");
    std::future::pending::<()>().await;
}

#[tokio::test]
async fn lost_admission_reply_reconciles_the_same_operation_after_restart() {
    let dir = Directory::new();
    let store = MemoryStore::open(dir.config()).await.expect("store");
    let h = store.handle();
    let o = store.owner();
    let (started, ready) = tokio::sync::oneshot::channel();
    let (release, receiver) = std::sync::mpsc::channel();
    let writer = h.clone();
    let barrier = tokio::spawn(async move { writer.test_writer_barrier(started, receiver).await });
    ready.await.expect("known admission boundary");
    let (submitter, owner) = (h.clone(), o.clone());
    let pending =
        tokio::spawn(async move { submitter.admit_operation(&owner, admission("lost")).await });
    let started = std::time::Instant::now();
    while h.test_writer_pending() < 2 {
        assert!(started.elapsed().as_secs() < 3, "admission queued");
        tokio::task::yield_now().await;
    }
    pending.abort();
    let _cancelled = pending.await;
    release.send(()).expect("release writer");
    barrier.await.expect("barrier task").expect("barrier");
    let mut retry = admission("lost");
    retry.observed_at += 3_600_000;
    let receipt = h
        .admit_operation(&o, retry)
        .await
        .expect("reconcile original key");
    assert!(receipt.replayed);
    assert_eq!(
        h.source_record(&o, receipt.value.input.id)
            .await
            .expect("original observation")
            .observed_at,
        Some(admission("lost").observed_at)
    );
    assert!(
        h.storage_health(&o)
            .await
            .expect("reservation retained")
            .reserved_bytes
            > 0
    );
    store.shutdown().await.expect("shutdown");
    let store = MemoryStore::open(dir.config()).await.expect("restart");
    let status = store
        .handle()
        .operation_by_key(&store.owner(), admission("lost").key)
        .await
        .expect("durable lookup");
    assert_eq!(status.id, receipt.value.id);
    assert_eq!(status.state, OperationState::Interrupted);
    store.shutdown().await.expect("shutdown");
}

#[tokio::test]
async fn foreign_authority_cannot_admit_or_settle_an_operation() {
    let first_dir = Directory::new();
    let second_dir = Directory::new();
    let first = MemoryStore::open(first_dir.config()).await.expect("first");
    let second = MemoryStore::open(second_dir.config())
        .await
        .expect("second");
    let h = first.handle();
    let id = h
        .admit_operation(&first.owner(), admission("owner"))
        .await
        .expect("owner admission")
        .value
        .id;
    assert!(matches!(
        h.admit_operation(&second.owner(), admission("foreign"))
            .await,
        Err(MemoryError::Forbidden)
    ));
    assert!(matches!(
        h.operation(&second.owner(), id).await,
        Err(MemoryError::Forbidden)
    ));
    assert!(matches!(
        h.finalize_operation(
            &second.owner(),
            key("foreign-done"),
            id,
            finalization(TerminalOutcome::Completed)
        )
        .await,
        Err(MemoryError::Forbidden)
    ));
    assert_eq!(
        h.operation(&first.owner(), id)
            .await
            .expect("unchanged")
            .state,
        OperationState::Accepted
    );
    first.shutdown().await.expect("first shutdown");
    second.shutdown().await.expect("second shutdown");
}

#[tokio::test]
async fn restore_preserves_configured_quota_and_deferred_job_bound() {
    let dir = Directory::new();
    let config = dir
        .config()
        .with_quota(128 * 1024 * 1024)
        .expect("quota")
        .with_pending_job_limit(1)
        .expect("jobs");
    let store = MemoryStore::open(config).await.expect("store");
    let h = store.handle();
    let o = store.owner();
    h.remember(&o, key("before"), text("before backup"))
        .await
        .expect("source");
    let backup = dir.0.join("backup");
    h.backup(&o, backup.clone()).await.expect("backup");
    let candidate = h
        .restore_candidate(&o, backup, dir.0.join("candidate"))
        .await
        .expect("candidate");
    let source = candidate
        .handle()
        .remember(
            &candidate.owner(),
            key("after"),
            text("saved despite bounded jobs"),
        )
        .await
        .expect("literal")
        .value;
    let health = candidate
        .handle()
        .storage_health(&candidate.owner())
        .await
        .expect("health");
    assert_eq!(health.quota_bytes, 128 * 1024 * 1024);
    assert_eq!(health.pending_jobs, 1);
    assert_eq!(health.deferred_sources, 1);
    assert_eq!(
        candidate
            .handle()
            .source_record(&candidate.owner(), source.id)
            .await
            .expect("deferred")
            .formation,
        FormationState::Deferred
    );
    candidate.shutdown().await.expect("candidate shutdown");
    store.shutdown().await.expect("store shutdown");
}

#[tokio::test]
async fn purge_respects_payload_bytes_as_well_as_record_count() {
    let dir = Directory::new();
    let store = MemoryStore::open(dir.config()).await.expect("store");
    let h = store.handle();
    let o = store.owner();
    // Escaping makes each qualified claim/receipt much larger than its raw words.
    let raw = "\u{0001}".repeat(2048);
    let source = h
        .remember(&o, key("large-source"), text(&raw))
        .await
        .expect("source")
        .value;
    let mut input = claim(
        o.principal(),
        Predicate::Prefers,
        ClaimObject::Text(text(&raw)),
        &source,
        &raw,
    );
    input.conditions = vec![text(&"\u{0001}".repeat(1024)); 4];
    for i in 0..32 {
        h.add_claim(&o, key(&format!("large-claim-{i}")), input.clone())
            .await
            .expect("large claim");
    }
    h.suppress_source(&o, key("forget-large"), source.id, source.revision)
        .await
        .expect("suppression");
    let connection =
        rusqlite::Connection::open(dir.0.join("memory.sqlite3")).expect("read-only measurements");
    let remaining = || -> u64 {
        connection.query_row("SELECT (SELECT coalesce(sum(length(CAST(payload AS BLOB))),0) FROM claim_versions)+(SELECT coalesce(sum(length(CAST(receipt AS BLOB))),0) FROM commands)",[],|r|r.get::<_,i64>(0)).expect("remaining private payload").try_into().expect("nonnegative payload bytes")
    };
    let mut previous = remaining();
    assert!(previous > 1024 * 1024, "exercise the byte bound");
    let mut batches = 0;
    loop {
        let complete = h
            .purge_source_batch(&o, source.id)
            .await
            .expect("bounded batch");
        let current = remaining();
        assert!(
            previous - current <= 1024 * 1024,
            "one MiB payload bound per batch"
        );
        previous = current;
        batches += 1;
        assert!(batches < 20, "bounded forward progress");
        if complete {
            break;
        }
    }
    assert!(batches > 1, "large payload requires multiple batches");
    connection.close().expect("close reader");
    store.shutdown().await.expect("shutdown");
}

#[tokio::test]
async fn rejected_proposal_constraints_do_not_block_healthy_storage() {
    let dir = Directory::new();
    let store = MemoryStore::open(dir.config()).await.expect("store");
    let h = store.handle();
    let o = store.owner();
    h.remember(&o, key("source"), text("Lena is my sister."))
        .await
        .expect("source");
    let lease = h
        .claim_extraction_job(&o, job_clock())
        .await
        .expect("lease")
        .expect("job");
    let entity = crate::memory::Entity {
        id: MemoryId::new().expect("entity id"),
        kind: crate::memory::EntityKind::Person,
        label: text("Lena"),
    };
    let changes = ExtractionChanges {
        entities: vec![entity.clone(), entity.clone()],
        claims: vec![],
    };
    assert!(matches!(
        h.finish_extraction_job(
            &o,
            lease.clone(),
            changes,
            ExtractionOutcome::Complete,
            MemoryJobUsage::default()
        )
        .await,
        Err(MemoryError::Conflict)
    ));
    assert_eq!(
        h.storage_health(&o)
            .await
            .expect("healthy after rejected proposal")
            .status,
        StorageStatus::Ready
    );
    assert!(
        matches!(h.follow(&o, entity.id, 1).await, Err(MemoryError::NotFound)),
        "entity insert rolled back"
    );
    h.remember(
        &o,
        key("still-admitting"),
        text("Another literal is still accepted."),
    )
    .await
    .expect("ordinary admission remains open");
    h.finish_extraction_job(
        &o,
        lease,
        ExtractionChanges::default(),
        ExtractionOutcome::Unresolved,
        MemoryJobUsage::default(),
    )
    .await
    .expect("settle original attempt");
    store.shutdown().await.expect("shutdown");
}

#[tokio::test]
async fn literal_identifiers_are_not_dependencies_or_cause_authority() {
    let dir = Directory::new();
    let store = MemoryStore::open(dir.config()).await.expect("store");
    let h = store.handle();
    let o = store.owner();
    let operation = h
        .admit_operation(&o, admission("identity-text"))
        .await
        .expect("admit")
        .value;
    h.finalize_operation(
        &o,
        key("done"),
        operation.id,
        finalization(TerminalOutcome::Completed),
    )
    .await
    .expect("complete");
    h.register_consumer(
        &o,
        key("audit-consumer"),
        "audit".into(),
        ConsumerRequirement::Required,
    )
    .await
    .expect("consumer");
    let literal_id = text(&operation.id.to_string());
    let literal_source = text(&operation.input.id.to_string());
    let id_receipt = h
        .remember(&o, key("literal-operation-id"), literal_id)
        .await
        .expect("literal identifier");
    h.remember(&o, key("literal-source-id"), literal_source.clone())
        .await
        .expect("literal source identifier");
    let page = h
        .consumer_events(&o, "audit".into(), 32)
        .await
        .expect("events");
    let event = page
        .events
        .iter()
        .find(|e| e.sequence == id_receipt.sequence)
        .expect("literal change event");
    assert_eq!(
        event.root_cause, id_receipt.operation_id,
        "prose cannot select an existing root cause"
    );
    h.suppress_source(
        &o,
        key("forget-operation"),
        operation.input.id,
        operation.input.revision,
    )
    .await
    .expect("suppress exchange");
    assert!(
        h.remember(&o, key("literal-source-id"), literal_source)
            .await
            .expect("literal is independent of the record it names")
            .replayed
    );
    store.shutdown().await.expect("shutdown");
}

#[tokio::test]
async fn killed_operation_recovers_as_interrupted_without_tool_replay() {
    use std::process::{Command, Stdio};
    let dir = Directory::new();
    let mut child = Command::new(std::env::current_exe().expect("test executable"))
        .args([
            "--exact",
            "memory::tests::operation_tests::operation_crash_child",
            "--nocapture",
        ])
        .env("ASSISTANT_OPERATION_CRASH_DIRECTORY", &dir.0)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("crash fixture");
    let start = std::time::Instant::now();
    while !dir.0.join("ready").exists() {
        if let Some(status) = child.try_wait().expect("child status") {
            panic!("fixture failed: {status}");
        }
        if start.elapsed().as_secs() > 10 {
            let _stop = child.kill();
            let _join = child.wait();
            panic!("fixture readiness timeout");
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    child.kill().expect("kill without destructors");
    child.wait().expect("reap child");
    let id = MemoryId::parse(
        &fs::read_to_string(dir.0.join("ready")).expect("durable operation receipt"),
    )
    .expect("id");
    let store = MemoryStore::open(dir.config()).await.expect("recover");
    let h = store.handle();
    let o = store.owner();
    let record = h.operation(&o, id).await.expect("durable operation");
    assert_eq!(record.state, OperationState::Interrupted);
    assert!(record.unknown_tool_outcome);
    assert_eq!(
        h.tool_invocation(&o, id, 1)
            .await
            .expect("confirmed result")
            .outcome,
        Some(ToolOutcome::Succeeded)
    );
    assert_eq!(
        h.tool_invocation(&o, id, 2)
            .await
            .expect("unknown effect")
            .outcome,
        Some(ToolOutcome::Unknown)
    );
    assert_eq!(
        h.storage_health(&o)
            .await
            .expect("released reservation")
            .reserved_bytes,
        0
    );
    assert_eq!(
        h.admit_operation(&o, admission("crash"))
            .await
            .expect("retry old key")
            .value
            .id,
        id
    );
    assert_eq!(
        fs::read_to_string(dir.0.join("external-effect")).expect("effect receipt"),
        "performed once"
    );
    store.shutdown().await.expect("shutdown");
}
