//! Temporary-store tests for durable receipts, temporal evidence, authorization,
//! restart and forgetting. No local language model or machine path is required.

#[path = "operation_tests.rs"]
mod operation_tests;

use super::*;
use std::{fs, os::unix::fs::DirBuilderExt, path::PathBuf};

struct Directory(PathBuf);
impl Directory {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "assistant-memory-{}",
            MemoryId::new().expect("test identity")
        ));
        fs::DirBuilder::new()
            .mode(0o700)
            .create(&path)
            .expect("private test directory");
        Self(path)
    }
    fn config(&self) -> MemoryConfig {
        MemoryConfig::new(&self.0)
    }
}
impl Drop for Directory {
    fn drop(&mut self) {
        let _cleanup = fs::remove_dir_all(&self.0);
    }
}
fn key(name: &str) -> OperationKey {
    OperationKey::new("test", name).expect("valid test key")
}
fn text(value: &str) -> MemoryText {
    MemoryText::new(value).expect("bounded fixture text")
}
fn version() -> Revision {
    Revision::new(1).expect("positive version")
}
/// Stable fake-clock origin near the actual epoch, so production completion's
/// wall-clock expiry checks and explicit test lease advances share one scale.
fn job_clock() -> i64 {
    static BASE: std::sync::OnceLock<i64> = std::sync::OnceLock::new();
    *BASE.get_or_init(|| super::session::utc_millis().expect("test clock"))
}

#[tokio::test]
async fn product_recall_bounds_long_questions_and_accepts_punctuation_without_losing_storage_errors()
 {
    let dir = Directory::new();
    let store = MemoryStore::open(dir.config()).await.expect("store");
    let access = super::service::MemoryAccess::for_test(&store);
    access
        .remember(key("source"), text("My bicycle is blue."))
        .await
        .expect("source");
    let query = (0..40)
        .map(|index| format!("word{index}"))
        .collect::<Vec<_>>()
        .join(" ");
    let result = access
        .recall(text(&query), 8)
        .await
        .expect("a long question is not an invalid storage query");
    assert!(result.query_truncated);
    let result = access
        .recall(text("???"), 8)
        .await
        .expect("punctuation-only product input is valid");
    assert!(result.sources.is_empty());
    store.shutdown().await.expect("close required storage");
    assert!(
        access.recall(text("???"), 8).await.is_err(),
        "optional query channels cannot hide closed required storage"
    );
}
fn claim(
    subject: MemoryId,
    predicate: Predicate,
    object: ClaimObject,
    source: &Source,
    quote: &str,
) -> ClaimInput {
    ClaimInput {
        subject,
        predicate,
        object,
        negative: false,
        modality: ClaimModality::Asserted,
        valid_from: None,
        valid_to: None,
        conditions: vec![],
        evidence: vec![ClaimEvidence {
            source_id: source.id,
            source_revision: source.revision,
            quote: text(quote),
        }],
    }
}

#[test]
fn typed_ids_versions_and_debug_do_not_leak_text() {
    let id = MemoryId::new().expect("identity");
    assert_eq!(
        MemoryId::parse(&id.to_string()).expect("parse identity"),
        id
    );
    assert!(MemoryId::parse("FFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFF").is_err());
    assert!(serde_json::from_str::<Revision>("\"01\"").is_err());
    assert!(serde_json::from_str::<Revision>("0").is_err());
    assert_eq!(
        serde_json::to_string(&version()).expect("serialize version"),
        "\"1\""
    );
    assert!(!format!("{:?}", text("private secret")).contains("private secret"));
}

#[tokio::test]
async fn remember_replays_and_survives_restart_without_a_model() {
    let dir = Directory::new();
    let store = MemoryStore::open(dir.config()).await.expect("open store");
    let handle = store.handle();
    let owner = store.owner();
    let first = handle
        .remember(&owner, key("remember"), text("Я віддаю перевагу чаю."))
        .await
        .expect("commit source");
    let same = handle
        .remember(&owner, key("remember"), text("Я віддаю перевагу чаю."))
        .await
        .expect("replay receipt");
    assert!(same.replayed);
    assert_eq!(first.operation_id, same.operation_id);
    assert_eq!(first.value.id, same.value.id);
    assert!(matches!(
        handle
            .remember(&owner, key("remember"), text("different"))
            .await,
        Err(MemoryError::Conflict)
    ));
    let results = handle
        .search(&owner, text("чаю"), 10, Some(first.sequence))
        .await
        .expect("search committed source");
    assert_eq!(results.sources.len(), 1);
    store.shutdown().await.expect("join store");
    assert!(matches!(
        handle.source(&owner, first.value.id).await,
        Err(MemoryError::Unavailable)
    ));
    let store = MemoryStore::open(dir.config()).await.expect("reopen store");
    let handle = store.handle();
    let owner = store.owner();
    assert_eq!(
        handle
            .source(&owner, first.value.id)
            .await
            .expect("persisted source")
            .text
            .as_str(),
        "Я віддаю перевагу чаю."
    );
    assert!(
        handle
            .remember(&owner, key("remember"), text("Я віддаю перевагу чаю."))
            .await
            .expect("durable replay")
            .replayed
    );
    store.shutdown().await.expect("join reopened store");
}

#[tokio::test]
async fn capabilities_and_directory_lease_are_enforced() {
    let first_dir = Directory::new();
    let second_dir = Directory::new();
    let first = MemoryStore::open(first_dir.config())
        .await
        .expect("first store");
    assert!(matches!(
        MemoryStore::open(first_dir.config()).await,
        Err(MemoryError::Busy)
    ));
    let second = MemoryStore::open(second_dir.config())
        .await
        .expect("second store");
    assert!(matches!(
        first
            .handle()
            .remember(&second.owner(), key("foreign"), text("forged owner"))
            .await,
        Err(MemoryError::Forbidden)
    ));
    first.shutdown().await.expect("first shutdown");
    second.shutdown().await.expect("second shutdown");
}

#[tokio::test]
async fn personal_graph_correction_retains_history_and_new_current_path() {
    let dir = Directory::new();
    let store = MemoryStore::open(dir.config()).await.expect("store");
    let h = store.handle();
    let o = store.owner();
    let source = h
        .remember(
            &o,
            key("family"),
            text("My sister Lena is a veterinarian in Lviv."),
        )
        .await
        .expect("family evidence")
        .value;
    let lena = h
        .create_entity(&o, key("lena"), EntityKind::Person, text("Lena"))
        .await
        .expect("person")
        .value;
    let vet = h
        .create_entity(&o, key("vet"), EntityKind::Occupation, text("veterinarian"))
        .await
        .expect("profession")
        .value;
    let lviv = h
        .create_entity(&o, key("lviv"), EntityKind::Place, text("Lviv"))
        .await
        .expect("location")
        .value;
    let quote = source.text.as_str();
    h.add_claim(
        &o,
        key("sister"),
        claim(
            o.principal(),
            Predicate::SiblingOf,
            ClaimObject::Reference(lena.id),
            &source,
            quote,
        ),
    )
    .await
    .expect("family relationship");
    h.add_claim(
        &o,
        key("profession"),
        claim(
            lena.id,
            Predicate::HasOccupation,
            ClaimObject::Reference(vet.id),
            &source,
            quote,
        ),
    )
    .await
    .expect("occupation");
    let location = h
        .add_claim(
            &o,
            key("residence"),
            claim(
                lena.id,
                Predicate::LivesIn,
                ClaimObject::Reference(lviv.id),
                &source,
                quote,
            ),
        )
        .await
        .expect("residence")
        .value;
    let bundle = h
        .follow(&o, o.principal(), 3)
        .await
        .expect("follow family chain");
    assert_eq!(bundle.claims.len(), 3);
    assert!(!bundle.truncated);
    let update = h
        .remember(&o, key("move"), text("Lena moved to Kyiv."))
        .await
        .expect("new evidence")
        .value;
    let kyiv = h
        .create_entity(&o, key("kyiv"), EntityKind::Place, text("Kyiv"))
        .await
        .expect("new place")
        .value;
    let input = claim(
        lena.id,
        Predicate::LivesIn,
        ClaimObject::Reference(kyiv.id),
        &update,
        update.text.as_str(),
    );
    let corrected = h
        .correct_claim(&o, key("correct"), location.id, version(), input.clone())
        .await
        .expect("correction");
    assert_eq!(corrected.value.revision.get(), 2);
    assert!(matches!(
        h.correct_claim(&o, key("stale"), location.id, version(), input)
            .await,
        Err(MemoryError::Conflict)
    ));
    let history = h.claim_history(&o, location.id).await.expect("history");
    assert_eq!(history.len(), 2);
    assert!(history[0].recorded_to.is_some());
    let bundle = h.follow(&o, o.principal(), 3).await.expect("current graph");
    let current = bundle
        .claims
        .iter()
        .find(|c| c.id == location.id)
        .expect("current location");
    assert!(matches!(current.input.object,ClaimObject::Reference(id) if id==kyiv.id));
    store.shutdown().await.expect("shutdown");
}

#[tokio::test]
async fn invalid_evidence_rolls_back_and_quotes_require_unique_utf8_spans() {
    let dir = Directory::new();
    let store = MemoryStore::open(dir.config()).await.expect("store");
    let h = store.handle();
    let o = store.owner();
    let source = h
        .remember(&o, key("source"), text("Я люблю чай. Я люблю чай. aaaa"))
        .await
        .expect("source")
        .value;
    for (name, quote) in [
        ("repeated", "Я люблю чай."),
        ("overlap", "aaa"),
        ("absent", "coffee"),
    ] {
        let input = claim(
            o.principal(),
            Predicate::Prefers,
            ClaimObject::Text(text("tea")),
            &source,
            quote,
        );
        assert!(matches!(
            h.add_claim(&o, key(name), input).await,
            Err(MemoryError::InvalidInput(_))
        ));
    }
    let input = claim(
        o.principal(),
        Predicate::Prefers,
        ClaimObject::Text(text("tea")),
        &source,
        "Я люблю чай. Я люблю чай.",
    );
    let receipt = h
        .add_claim(&o, key("repeated"), input)
        .await
        .expect("failed transaction did not consume key");
    assert_eq!(receipt.sequence.get(), 2);
    store.shutdown().await.expect("shutdown");
}

#[tokio::test]
async fn suppression_hides_evidence_and_purge_prevents_receipt_resurrection() {
    let dir = Directory::new();
    let store = MemoryStore::open(dir.config()).await.expect("store");
    let h = store.handle();
    let o = store.owner();
    let source = h
        .remember(&o, key("source"), text("I prefer tea."))
        .await
        .expect("source")
        .value;
    let c = h
        .add_claim(
            &o,
            key("claim"),
            claim(
                o.principal(),
                Predicate::Prefers,
                ClaimObject::Text(text("tea")),
                &source,
                "I prefer tea.",
            ),
        )
        .await
        .expect("claim")
        .value;
    h.suppress_source(&o, key("forget"), source.id, source.revision)
        .await
        .expect("journaled suppression");
    assert!(matches!(
        h.remember(&o, key("source"), text("I prefer tea.")).await,
        Err(MemoryError::Gone)
    ));
    assert!(matches!(
        h.source(&o, source.id).await,
        Err(MemoryError::NotFound)
    ));
    assert!(
        h.search(&o, text("tea"), 10, None)
            .await
            .expect("search")
            .sources
            .is_empty()
    );
    assert!(matches!(
        h.claim_history(&o, c.id).await,
        Err(MemoryError::NotFound)
    ));
    assert!(
        h.purge_source_batch(&o, source.id)
            .await
            .expect("purge complete")
    );
    assert!(matches!(
        h.remember(&o, key("source"), text("I prefer tea.")).await,
        Err(MemoryError::Gone)
    ));
    store.shutdown().await.expect("shutdown");
    let store = MemoryStore::open(dir.config())
        .await
        .expect("reopen with tombstone");
    assert!(
        store
            .handle()
            .search(&store.owner(), text("tea"), 10, None)
            .await
            .expect("search restored store")
            .sources
            .is_empty()
    );
    store.shutdown().await.expect("shutdown");
}

#[tokio::test]
async fn literal_fts_operators_do_not_become_a_query_program() {
    let dir = Directory::new();
    let store = MemoryStore::open(dir.config()).await.expect("store");
    let h = store.handle();
    let o = store.owner();
    h.remember(&o, key("source"), text("I prefer tea."))
        .await
        .expect("source");
    assert!(
        h.search(&o, text("NOT secret* OR \"near\""), 10, None)
            .await
            .expect("literal query")
            .sources
            .is_empty()
    );
    store.shutdown().await.expect("shutdown");
}

#[tokio::test]
async fn reclaimed_fence_cannot_publish_and_completion_is_atomic_and_idempotent() {
    let dir = Directory::new();
    let store = MemoryStore::open(dir.config()).await.expect("store");
    let h = store.handle();
    let o = store.owner();
    let source = h
        .remember(&o, key("source"), text("I prefer tea."))
        .await
        .expect("source")
        .value;
    let old = h
        .claim_extraction_job(&o, job_clock())
        .await
        .expect("claim attempt")
        .expect("pending job");
    let current = h
        .claim_extraction_job(&o, job_clock() + 60_001)
        .await
        .expect("reclaim expired job")
        .expect("reclaimed job");
    let changes = ExtractionChanges {
        entities: vec![],
        claims: vec![claim(
            o.principal(),
            Predicate::Prefers,
            ClaimObject::Text(text("tea")),
            &source,
            "I prefer tea.",
        )],
    };
    let usage = MemoryJobUsage {
        input_tokens: 128,
        output_tokens: 64,
        native_millis: 100,
    };
    assert!(matches!(
        h.finish_extraction_job(&o, old, changes.clone(), ExtractionOutcome::Complete, usage)
            .await,
        Err(MemoryError::Conflict)
    ));
    let first = h
        .finish_extraction_job(
            &o,
            current.clone(),
            changes.clone(),
            ExtractionOutcome::Complete,
            usage,
        )
        .await
        .expect("current fenced completion");
    let replay = h
        .finish_extraction_job(
            &o,
            current.clone(),
            changes.clone(),
            ExtractionOutcome::Complete,
            usage,
        )
        .await
        .expect("replayed completion");
    assert_eq!(first[0].id, replay[0].id);
    assert_eq!(
        h.follow(&o, o.principal(), 1)
            .await
            .expect("graph")
            .claims
            .len(),
        1
    );
    h.suppress_source(&o, key("suppress"), source.id, source.revision)
        .await
        .expect("suppress completed source");
    assert!(matches!(
        h.finish_extraction_job(&o, current, changes, ExtractionOutcome::Complete, usage)
            .await,
        Err(MemoryError::Conflict)
    ));
    store.shutdown().await.expect("shutdown");
}

#[tokio::test]
async fn failed_claim_application_rolls_back_job_and_budget_completion() {
    let dir = Directory::new();
    let store = MemoryStore::open(dir.config()).await.expect("store");
    let h = store.handle();
    let o = store.owner();
    let source = h
        .remember(&o, key("source"), text("I prefer tea."))
        .await
        .expect("source")
        .value;
    let lease = h
        .claim_extraction_job(&o, job_clock())
        .await
        .expect("claim attempt")
        .expect("pending");
    let valid = claim(
        o.principal(),
        Predicate::Prefers,
        ClaimObject::Text(text("tea")),
        &source,
        "I prefer tea.",
    );
    let mut invalid = valid.clone();
    invalid.evidence[0].quote = text("not present");
    assert!(
        h.finish_extraction_job(
            &o,
            lease.clone(),
            ExtractionChanges {
                entities: vec![],
                claims: vec![valid.clone(), invalid]
            },
            ExtractionOutcome::Complete,
            MemoryJobUsage::default()
        )
        .await
        .is_err()
    );
    assert!(
        h.follow(&o, o.principal(), 1)
            .await
            .expect("graph after rollback")
            .claims
            .is_empty()
    );
    let result = h
        .finish_extraction_job(
            &o,
            lease,
            ExtractionChanges {
                entities: vec![],
                claims: vec![valid],
            },
            ExtractionOutcome::Complete,
            MemoryJobUsage::default(),
        )
        .await
        .expect("same lease remains usable after atomic rejection");
    assert_eq!(result.len(), 1);
    store.shutdown().await.expect("shutdown");
}

#[tokio::test]
async fn known_valid_time_and_negation_survive_graph_retrieval() {
    let dir = Directory::new();
    let store = MemoryStore::open(dir.config()).await.expect("store");
    let h = store.handle();
    let o = store.owner();
    let source = h
        .remember(
            &o,
            key("source"),
            text("I did not prefer tea during that period."),
        )
        .await
        .expect("source")
        .value;
    let mut input = claim(
        o.principal(),
        Predicate::Prefers,
        ClaimObject::Text(text("tea")),
        &source,
        source.text.as_str(),
    );
    input.negative = true;
    input.valid_from = Some(100);
    input.valid_to = Some(200);
    h.add_claim(&o, key("negative"), input)
        .await
        .expect("qualified negative claim");
    assert!(
        h.follow_at(&o, o.principal(), 1, 99)
            .await
            .expect("before")
            .claims
            .is_empty()
    );
    assert!(
        h.follow_at(&o, o.principal(), 1, 100)
            .await
            .expect("within")
            .claims[0]
            .input
            .negative
    );
    assert!(
        h.follow_at(&o, o.principal(), 1, 200)
            .await
            .expect("exclusive end")
            .claims
            .is_empty()
    );
    store.shutdown().await.expect("shutdown");
}

#[tokio::test]
async fn old_backup_restore_applies_newer_deletion_journal_before_reads() {
    let dir = Directory::new();
    let store = MemoryStore::open(dir.config()).await.expect("store");
    let h = store.handle();
    let o = store.owner();
    let source = h
        .remember(&o, key("source"), text("I prefer tea."))
        .await
        .expect("source")
        .value;
    h.backup(&o, dir.0.join("backup"))
        .await
        .expect("online backup");
    h.suppress_source(&o, key("forget"), source.id, source.revision)
        .await
        .expect("newer deletion");
    let restored = h
        .restore_candidate(&o, dir.0.join("backup"), dir.0.join("restored"))
        .await
        .expect("reconciled candidate");
    let rh = restored.handle();
    let ro = restored.owner();
    assert!(matches!(
        rh.source(&ro, source.id).await,
        Err(MemoryError::NotFound)
    ));
    assert!(
        rh.search(&ro, text("tea"), 10, None)
            .await
            .expect("restored search")
            .sources
            .is_empty()
    );
    assert!(matches!(
        rh.remember(&ro, key("source"), text("I prefer tea.")).await,
        Err(MemoryError::Gone)
    ));
    assert!(
        rh.purge_source_batch(&ro, source.id)
            .await
            .expect("restored purge")
    );
    restored.shutdown().await.expect("candidate shutdown");
    store.shutdown().await.expect("original shutdown");
}

#[tokio::test]
async fn altered_backup_and_existing_destination_are_rejected() {
    use std::io::Write;
    let dir = Directory::new();
    let store = MemoryStore::open(dir.config()).await.expect("store");
    let h = store.handle();
    let o = store.owner();
    h.remember(&o, key("source"), text("Original source."))
        .await
        .expect("source");
    h.backup(&o, dir.0.join("backup")).await.expect("backup");
    assert!(h.backup(&o, dir.0.join("backup")).await.is_err());
    fs::OpenOptions::new()
        .append(true)
        .open(dir.0.join("backup/memory.sqlite3"))
        .expect("backup file")
        .write_all(b"altered")
        .expect("inject tampering");
    assert!(matches!(
        h.restore_candidate(&o, dir.0.join("backup"), dir.0.join("candidate"))
            .await,
        Err(MemoryError::CorruptStore)
    ));
    store.shutdown().await.expect("shutdown");
}

#[tokio::test]
async fn memory_crash_child() {
    let Some(path) = std::env::var_os("ASSISTANT_MEMORY_CRASH_TEST_DIRECTORY") else {
        return;
    };
    let path = PathBuf::from(path);
    let store = MemoryStore::open(MemoryConfig::new(&path))
        .await
        .expect("child store");
    let receipt = store
        .handle()
        .remember(
            &store.owner(),
            key("crash"),
            text("This acknowledged memory survives abrupt termination."),
        )
        .await
        .expect("child durable acknowledgment");
    fs::write(path.join("ready.tmp"), receipt.value.id.to_string()).expect("write child receipt");
    fs::rename(path.join("ready.tmp"), path.join("ready")).expect("publish child readiness");
    std::future::pending::<()>().await;
}

#[tokio::test]
async fn committed_memory_survives_process_kill_without_destructors() {
    use std::process::{Command, Stdio};
    let dir = Directory::new();
    let mut child = Command::new(std::env::current_exe().expect("test executable"))
        .args([
            "--exact",
            "memory::tests::memory_crash_child",
            "--nocapture",
        ])
        .env("ASSISTANT_MEMORY_CRASH_TEST_DIRECTORY", &dir.0)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn crash fixture");
    let started = std::time::Instant::now();
    while !dir.0.join("ready").exists() {
        if let Some(status) = child.try_wait().expect("observe fixture") {
            panic!("fixture exited before durable receipt: {status}");
        }
        if started.elapsed() > std::time::Duration::from_secs(10) {
            let _stop = child.kill();
            let _join = child.wait();
            panic!("crash fixture readiness timed out");
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    child.kill().expect("abruptly stop child");
    child.wait().expect("reap child");
    let source =
        MemoryId::parse(&fs::read_to_string(dir.0.join("ready")).expect("recorded receipt"))
            .expect("durable source identity");
    let store = MemoryStore::open(dir.config())
        .await
        .expect("recover killed store");
    assert_eq!(
        store
            .handle()
            .source(&store.owner(), source)
            .await
            .expect("acknowledged source survived")
            .text
            .as_str(),
        "This acknowledged memory survives abrupt termination."
    );
    store.shutdown().await.expect("shutdown recovered store");
}

#[tokio::test]
async fn crashed_attempts_keep_compute_reservations_across_restarts() {
    let dir = Directory::new();
    let store = MemoryStore::open(dir.config()).await.expect("store");
    store
        .handle()
        .remember(&store.owner(), key("source"), text("I prefer tea."))
        .await
        .expect("source");
    store
        .handle()
        .claim_extraction_job(&store.owner(), job_clock())
        .await
        .expect("first reservation")
        .expect("job");
    store.shutdown().await.expect("shutdown without completion");
    let store = MemoryStore::open(dir.config()).await.expect("restart");
    store
        .handle()
        .claim_extraction_job(&store.owner(), job_clock() - 100_000)
        .await
        .expect("second reservation despite backward clock")
        .expect("recovered job");
    store.shutdown().await.expect("shutdown without completion");
    let store = MemoryStore::open(dir.config())
        .await
        .expect("second restart");
    assert!(matches!(
        store
            .handle()
            .claim_extraction_job(&store.owner(), job_clock() - 200_000)
            .await,
        Err(MemoryError::Capacity)
    ));
    store.shutdown().await.expect("shutdown");
}

#[tokio::test]
async fn staged_jobs_reserve_all_passes_and_only_rebate_measured_completion() {
    let dir = Directory::new();
    let store = MemoryStore::open(dir.config()).await.expect("store");
    let h = store.handle();
    let o = store.owner();
    h.remember(&o, key("staged"), text("My sister Lena is a veterinarian."))
        .await
        .expect("source");
    let lease = h
        .claim_extraction_job_with_budget(&o, job_clock(), ExtractionBudget::StagedRoot)
        .await
        .expect("reserve staged")
        .expect("lease");
    let connection =
        rusqlite::Connection::open(dir.0.join("memory.sqlite3")).expect("inspect accounting");
    let charged: (i64, i64, i64, i64) = connection
        .query_row(
            "SELECT passes,input_tokens,output_tokens,native_millis FROM job_attempts",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .expect("durable before inference");
    assert_eq!(charged, (4, 4096, 1024, 120000));
    let accounting = ExtractionAccounting {
        passes: 3,
        usage: MemoryJobUsage {
            input_tokens: 800,
            output_tokens: 130,
            native_millis: 500,
        },
    };
    assert!(matches!(
        h.finish_extraction_job_accounted(
            &o,
            lease.clone(),
            ExtractionChanges::default(),
            ExtractionOutcome::Complete,
            ExtractionAccounting {
                usage: MemoryJobUsage {
                    input_tokens: 4097,
                    ..accounting.usage
                },
                ..accounting
            }
        )
        .await,
        Err(MemoryError::InvalidInput(_))
    ));
    h.finish_extraction_job_accounted(
        &o,
        lease.clone(),
        ExtractionChanges::default(),
        ExtractionOutcome::Complete,
        accounting,
    )
    .await
    .expect("reconcile measured usage");
    h.finish_extraction_job_accounted(
        &o,
        lease,
        ExtractionChanges::default(),
        ExtractionOutcome::Complete,
        accounting,
    )
    .await
    .expect("lost completion reply replays");
    let actual: (i64, i64, i64, i64) = connection
        .query_row(
            "SELECT passes,input_tokens,output_tokens,native_millis FROM job_attempts",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .expect("actual totals");
    assert_eq!(actual, (3, 800, 130, 500));
    drop(connection);
    store.shutdown().await.expect("join storage");
}

#[tokio::test]
async fn heartbeat_never_revives_expiry_or_extends_the_hard_end() {
    let dir = Directory::new();
    let store = MemoryStore::open(dir.config()).await.expect("store");
    let h = store.handle();
    let o = store.owner();
    h.remember(&o, key("source"), text("My sister Lena is a veterinarian."))
        .await
        .expect("source");
    let clock = job_clock();
    let lease = h
        .claim_extraction_job_with_budget(&o, clock, ExtractionBudget::StagedRoot)
        .await
        .expect("claim")
        .expect("lease");
    for advance in [20_000, 40_000, 60_000, 80_000, 100_000] {
        h.heartbeat_extraction_job(&o, lease.clone(), clock + advance)
            .await
            .expect("renew within lease and hard end");
    }
    let connection =
        rusqlite::Connection::open(dir.0.join("memory.sqlite3")).expect("inspect hard end");
    let until: i64 = connection
        .query_row("SELECT lease_until FROM jobs", [], |r| r.get(0))
        .expect("lease deadline");
    assert_eq!(until, clock + 120_000);
    assert!(matches!(
        h.heartbeat_extraction_job(&o, lease.clone(), clock + 120_000)
            .await,
        Err(MemoryError::Conflict)
    ));
    assert!(
        h.claim_extraction_job_with_budget(&o, clock + 120_001, ExtractionBudget::StagedRoot)
            .await
            .expect("exhausted root becomes unresolved")
            .is_none()
    );
    assert!(matches!(
        h.finish_extraction_job_accounted(
            &o,
            lease,
            ExtractionChanges::default(),
            ExtractionOutcome::Complete,
            ExtractionAccounting {
                usage: MemoryJobUsage::default(),
                passes: 1
            }
        )
        .await,
        Err(MemoryError::Conflict)
    ));
    let charged: i64 = connection
        .query_row("SELECT sum(input_tokens) FROM job_attempts", [], |r| {
            r.get(0)
        })
        .expect("crashed work remains charged");
    assert_eq!(charged, 4096);
    drop(connection);
    store.shutdown().await.expect("join storage");
}

#[tokio::test]
async fn graph_follows_eight_edges_after_restart_without_conversation_history() {
    let dir = Directory::new();
    let store = MemoryStore::open(dir.config()).await.expect("store");
    let h = store.handle();
    let o = store.owner();
    let source = h
        .remember(
            &o,
            key("source"),
            text("Each recorded project depends on the next project in this chain."),
        )
        .await
        .expect("source")
        .value;
    let mut nodes = Vec::new();
    for index in 0..9 {
        nodes.push(
            h.create_entity(
                &o,
                key(&format!("node-{index}")),
                EntityKind::Project,
                text(&format!("Project {index}")),
            )
            .await
            .expect("project")
            .value
            .id,
        );
    }
    for (index, pair) in nodes.windows(2).enumerate() {
        h.add_claim(
            &o,
            key(&format!("edge-{index}")),
            claim(
                pair[0],
                Predicate::DependsOn,
                ClaimObject::Reference(pair[1]),
                &source,
                source.text.as_str(),
            ),
        )
        .await
        .expect("host-authored chain edge");
    }
    store.shutdown().await.expect("shutdown");
    let store = MemoryStore::open(dir.config()).await.expect("restart");
    let bundle = store
        .handle()
        .follow_at(&store.owner(), nodes[0], 8, 1000)
        .await
        .expect("eight-edge traversal");
    assert_eq!(bundle.claims.len(), 8);
    assert!(!bundle.truncated);
    let reverse = store
        .handle()
        .follow_at(&store.owner(), nodes[8], 8, 1000)
        .await
        .expect("inverse navigation");
    assert_eq!(
        reverse.claims.len(),
        8,
        "inverse adjacency does not manufacture additional facts"
    );
    assert_eq!(reverse.entities.len(), 9);
    assert_eq!(reverse.sources.len(), 1);
    assert_eq!(reverse.sources[0].role, SourceRole::User);
    assert!(
        reverse.sources[0].observed_at.is_none(),
        "legacy/standalone observation time is unknown"
    );
    for path in &reverse.paths {
        assert!(!path.steps.is_empty() && path.steps.len() <= 8);
        assert_eq!(path.steps[0].from, nodes[8]);
        for adjacent in path.steps.windows(2) {
            assert_eq!(adjacent[0].to, Some(adjacent[1].from));
        }
        assert!(bundle.claims.iter().any(|claim| claim.id == path.claim));
    }
    store.shutdown().await.expect("shutdown");
}

#[tokio::test]
async fn inverse_parent_navigation_keeps_original_direction_and_suppressed_labels_private() {
    let dir = Directory::new();
    let store = MemoryStore::open(dir.config()).await.expect("store");
    let h = store.handle();
    let o = store.owner();
    let source = h
        .remember(&o, key("father"), text("My father Oleh is a teacher."))
        .await
        .expect("source")
        .value;
    let lease = h
        .claim_extraction_job(&o, job_clock())
        .await
        .expect("lease")
        .expect("job");
    let parent = Entity {
        id: MemoryId::new().expect("parent ID"),
        kind: EntityKind::Person,
        label: text("Oleh"),
    };
    let job = Entity {
        id: MemoryId::new().expect("occupation ID"),
        kind: EntityKind::Occupation,
        label: text("teacher"),
    };
    let claims = h
        .finish_extraction_job(
            &o,
            lease,
            ExtractionChanges {
                entities: vec![parent.clone(), job.clone()],
                claims: vec![
                    claim(
                        parent.id,
                        Predicate::ParentOf,
                        ClaimObject::Reference(o.principal()),
                        &source,
                        source.text.as_str(),
                    ),
                    claim(
                        parent.id,
                        Predicate::HasOccupation,
                        ClaimObject::Reference(job.id),
                        &source,
                        source.text.as_str(),
                    ),
                ],
            },
            ExtractionOutcome::Complete,
            MemoryJobUsage::default(),
        )
        .await
        .expect("authored graph fixture, not model evidence");
    let graph = h
        .follow_at(&o, o.principal(), 2, job_clock())
        .await
        .expect("navigate incoming parent and outgoing occupation");
    assert_eq!(graph.claims.len(), 2);
    assert!(
        graph
            .claims
            .iter()
            .any(|edge| edge.input.subject == parent.id
                && edge.input.predicate == Predicate::ParentOf)
    );
    let occupation = claims
        .iter()
        .find(|edge| edge.input.predicate == Predicate::HasOccupation)
        .expect("occupation claim");
    let path = graph
        .paths
        .iter()
        .find(|path| path.claim == occupation.id)
        .expect("full navigation provenance");
    assert_eq!(path.steps.len(), 2);
    assert_eq!(path.steps[0].from, o.principal());
    assert_eq!(path.steps[0].to, Some(parent.id));
    assert_eq!(path.steps[1].to, Some(job.id));
    h.suppress_source(&o, key("forget-father"), source.id, source.revision)
        .await
        .expect("storage suppression");
    let graph = h
        .follow_at(&o, parent.id, 2, job_clock())
        .await
        .expect("read after suppression, before physical purge");
    assert!(graph.claims.is_empty() && graph.paths.is_empty() && graph.sources.is_empty());
    assert_eq!(
        graph.entities[0].label.as_str(),
        "[forgotten]",
        "source-backed label eligibility is checked before purge"
    );
    store.shutdown().await.expect("join store");
}

#[tokio::test]
async fn insecure_directory_ancestors_and_symlinked_store_files_fail_closed() {
    use std::os::unix::fs::{PermissionsExt, symlink};
    let dir = Directory::new();
    fs::set_permissions(&dir.0, fs::Permissions::from_mode(0o777))
        .expect("simulate replaceable parent");
    assert!(matches!(
        MemoryStore::open(MemoryConfig::new(dir.0.join("child"))).await,
        Err(MemoryError::Forbidden)
    ));
    fs::set_permissions(&dir.0, fs::Permissions::from_mode(0o700))
        .expect("restore test cleanup permissions");
    let target = dir.0.join("target");
    fs::write(&target, b"must not be opened as a database").expect("target fixture");
    fs::set_permissions(&target, fs::Permissions::from_mode(0o600)).expect("target permissions");
    symlink(&target, dir.0.join("memory.sqlite3")).expect("malicious database symlink");
    assert!(MemoryStore::open(dir.config()).await.is_err());
    assert_eq!(
        fs::read(&target).expect("unchanged target"),
        b"must not be opened as a database"
    );
}

#[tokio::test]
async fn exclusive_status_requires_identified_correction_instead_of_last_write_wins() {
    let dir = Directory::new();
    let store = MemoryStore::open(dir.config()).await.expect("store");
    let h = store.handle();
    let o = store.owner();
    let source = h
        .remember(&o, key("source"), text("The project is active."))
        .await
        .expect("source")
        .value;
    let project = h
        .create_entity(&o, key("project"), EntityKind::Project, text("Project"))
        .await
        .expect("project")
        .value;
    let first = h
        .add_claim(
            &o,
            key("active"),
            claim(
                project.id,
                Predicate::Status,
                ClaimObject::Text(text("active")),
                &source,
                source.text.as_str(),
            ),
        )
        .await
        .expect("first status")
        .value;
    let update = h
        .remember(&o, key("update"), text("The project is completed."))
        .await
        .expect("update")
        .value;
    let replacement = claim(
        project.id,
        Predicate::Status,
        ClaimObject::Text(text("completed")),
        &update,
        update.text.as_str(),
    );
    assert!(matches!(
        h.add_claim(&o, key("second"), replacement.clone()).await,
        Err(MemoryError::Conflict)
    ));
    h.correct_claim(&o, key("correction"), first.id, first.revision, replacement)
        .await
        .expect("identified status correction");
    assert_eq!(
        h.follow(&o, project.id, 1)
            .await
            .expect("current status")
            .claims
            .len(),
        1
    );
    store.shutdown().await.expect("shutdown");
}

#[tokio::test]
async fn dropped_reply_does_not_cancel_an_already_queued_memory_write() {
    let dir = Directory::new();
    let store = MemoryStore::open(dir.config()).await.expect("store");
    let h = store.handle();
    let o = store.owner();
    let (started, ready) = tokio::sync::oneshot::channel();
    let (release, receiver) = std::sync::mpsc::channel();
    let writer = h.clone();
    let barrier = tokio::spawn(async move { writer.test_writer_barrier(started, receiver).await });
    ready.await.expect("writer held at known boundary");
    let request_handle = h.clone();
    let request_owner = o.clone();
    let request = tokio::spawn(async move {
        request_handle
            .remember(
                &request_owner,
                key("lost-reply"),
                text("Persist even when the caller loses its reply."),
            )
            .await
    });
    let deadline = std::time::Instant::now();
    while h.test_writer_pending() < 2 {
        assert!(
            deadline.elapsed() < std::time::Duration::from_secs(2),
            "write did not enqueue"
        );
        tokio::task::yield_now().await;
    }
    request.abort();
    let _cancelled = request.await;
    release.send(()).expect("release writer");
    barrier
        .await
        .expect("barrier task")
        .expect("barrier completed");
    let replay = h
        .remember(
            &o,
            key("lost-reply"),
            text("Persist even when the caller loses its reply."),
        )
        .await
        .expect("reconcile original key");
    assert!(replay.replayed);
    store.shutdown().await.expect("shutdown");
}

#[tokio::test]
async fn exact_vectors_follow_source_revision_suppression_and_purge() {
    use super::embedding::Embedding;
    let dir = Directory::new();
    let store = MemoryStore::open(dir.config()).await.expect("store");
    let handle = store.handle();
    let owner = store.owner();
    let first = handle
        .remember(
            &owner,
            key("vector-a"),
            text("My sister Lena is a veterinarian."),
        )
        .await
        .expect("source")
        .value;
    let second = handle
        .remember(
            &owner,
            key("vector-b"),
            text("The database stores transactions."),
        )
        .await
        .expect("source")
        .value;
    let vector = |index| {
        let mut values = vec![0.0; 384];
        values[index] = 1.0;
        Embedding::from_values(values).expect("unit vector")
    };
    for (index, source) in [&first, &second].into_iter().enumerate() {
        let task = handle
            .next_embedding(&owner)
            .await
            .expect("task")
            .expect("pending");
        assert_eq!(task.source.id, source.id);
        let end = u32::try_from(source.text.as_str().len()).expect("length");
        handle
            .put_embedding(
                &owner,
                SourceRef {
                    id: source.id,
                    revision: source.revision,
                },
                0,
                0,
                end,
                vector(index),
            )
            .await
            .expect("index");
        assert!(
            matches!(
                handle
                    .put_embedding(
                        &owner,
                        SourceRef {
                            id: source.id,
                            revision: source.revision
                        },
                        0,
                        0,
                        end,
                        vector(index)
                    )
                    .await,
                Err(MemoryError::Conflict)
            ),
            "stale cursor cannot publish"
        );
    }
    assert!(
        handle
            .next_embedding(&owner)
            .await
            .expect("coverage")
            .is_none()
    );
    let hits = handle
        .semantic_search(&owner, vector(0), 2)
        .await
        .expect("exact search");
    assert_eq!(hits[0].source.id, first.id);
    assert_eq!(hits[0].score, 1.0);
    handle
        .suppress_source(&owner, key("vector-forget"), first.id, first.revision)
        .await
        .expect("journal suppression");
    let hits = handle
        .semantic_search(&owner, vector(0), 2)
        .await
        .expect("eligible search");
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].source.id, second.id);
    while !handle
        .purge_source_batch(&owner, first.id)
        .await
        .expect("bounded purge")
    {}
    store.shutdown().await.expect("shutdown");
    let connection = rusqlite::Connection::open(dir.0.join("memory.sqlite3")).expect("inspect");
    assert_eq!(
        connection
            .query_row(
                "SELECT count(*) FROM embedding_chunks WHERE source_id=?1",
                [first.id.to_string()],
                |r| r.get::<_, i64>(0)
            )
            .expect("vector purge"),
        0
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT count(*) FROM embedding_coverage WHERE source_id=?1",
                [first.id.to_string()],
                |r| r.get::<_, i64>(0)
            )
            .expect("coverage purge"),
        0
    );
}

#[tokio::test]
async fn universal_memory_keeps_interleaved_private_preferences_links_and_corrections_after_restart()
 {
    let dir = Directory::new();
    let store = MemoryStore::open(dir.config()).await.expect("store");
    let h = store.handle();
    let owner = store.owner();
    let persona = h
        .remember(
            &owner,
            key("persona"),
            text("Please be a playful anime companion, but speak plainly when we work on Rust."),
        )
        .await
        .expect("arbitrary persona request")
        .value;
    let preference=h.remember(&owner,key("private"),text("My private interests include adult fiction. Keep recommendations private; I prefer romance to horror.")).await.expect("arbitrary private preference").value;
    h.remember(
        &owner,
        key("unrelated"),
        text("The daemon project should also serve a Unix socket someday."),
    )
    .await
    .expect("interleaved engineering conversation");
    let original=h.remember_note(&owner,key("note"),text("The owner prefers private romantic adult fiction recommendations and a playful companion persona."),vec![(SourceRef{id:preference.id,revision:preference.revision},text("preference")),(SourceRef{id:persona.id,revision:persona.revision},text("presentation"))],job_clock()).await.expect("freeform interpretation").value;
    let correction=h.remember(&owner,key("correction"),text("Actually, keep recommendations literary rather than anime themed. The companion persona is just how I want you to talk.")).await.expect("later correction").value;
    let revised=h.remember_note(&owner,key("revised"),text("Recommendation subject and assistant persona are separate: literary romance recommendations, playful companion voice."),vec![(SourceRef{id:original.id,revision:original.revision},text("clarifies")),(SourceRef{id:correction.id,revision:correction.revision},text("based_on"))],job_clock()).await.expect("source-backed correction").value;
    assert_eq!(
        h.source_record(&owner, original.id)
            .await
            .expect("attribution")
            .role,
        SourceRole::Assistant
    );
    let profile = h.assistant_profile(&owner).await.expect("default profile");
    h.update_assistant(&owner,profile.revision,AssistantSettings{instructions:text("Be a playful anime companion. Use plain technical language for work. Respect the owner's private literary preferences."),initiative_interval_seconds:Some(1800)},job_clock()).await.expect("persist arbitrary role");
    store.shutdown().await.expect("close first process");
    let store = MemoryStore::open(dir.config()).await.expect("reopen");
    let h = store.handle();
    let owner = store.owner();
    let graph = h
        .follow_sources(&owner, preference.id, 4)
        .await
        .expect("walk incoming and outgoing links");
    assert!(
        graph
            .sources
            .iter()
            .any(|record| record.source.id == revised.id)
    );
    assert!(graph.links.iter().any(|link| link.from.id == revised.id
        && link.to.id == original.id
        && link.relation.as_str() == "clarifies"));
    assert!(
        h.assistant_profile(&owner)
            .await
            .expect("durable personality")
            .settings
            .instructions
            .as_str()
            .contains("playful anime companion")
    );
    h.suppress_source(
        &owner,
        key("forget-private"),
        preference.id,
        preference.revision,
    )
    .await
    .expect("forget private evidence and derived interpretations");
    assert!(h.source(&owner, original.id).await.is_err());
    assert!(
        h.source(&owner, revised.id).await.is_err(),
        "transitive interpretation cannot resurrect forgotten evidence"
    );
    assert!(
        h.source(&owner, persona.id).await.is_ok(),
        "unrelated experiences remain independently usable"
    );
    while !h
        .purge_next_batch(&owner)
        .await
        .expect("bounded graph-label/receipt/source purge")
    {}
    store.shutdown().await.expect("close");
}

#[tokio::test]
async fn initiative_claims_mailbox_profile_edits_and_forgetting_are_durable() {
    let dir = Directory::new();
    let store = MemoryStore::open(dir.config()).await.expect("store");
    let h = store.handle();
    let owner = store.owner();
    let now = job_clock();
    let source = h
        .remember(
            &owner,
            key("project"),
            text("I'm building a garden bench this weekend; ask me later how it went."),
        )
        .await
        .expect("ongoing life context")
        .value;
    let profile = h.assistant_profile(&owner).await.expect("profile");
    assert!(
        h.claim_attention(&owner, now + 100_000)
            .await
            .expect("disabled is idle")
            .is_none()
    );
    let enabled = h
        .update_assistant(
            &owner,
            profile.revision,
            AssistantSettings {
                instructions: text(
                    "Be warmly curious about my projects. Ask about the bench when appropriate.",
                ),
                initiative_interval_seconds: Some(60),
            },
            now,
        )
        .await
        .expect("enable initiative");
    assert!(
        h.update_assistant(&owner, profile.revision, enabled.settings.clone(), now)
            .await
            .is_err(),
        "stale editors cannot silently overwrite personality"
    );
    assert!(
        h.claim_attention(&owner, now + 59_999)
            .await
            .expect("before due")
            .is_none()
    );
    let claimed = h
        .claim_attention(&owner, now + 60_000)
        .await
        .expect("due")
        .expect("one durable claim");
    assert!(
        h.claim_attention(&owner, now + 60_001)
            .await
            .expect("already claimed")
            .is_none()
    );
    h.settle_attention(
        &owner,
        claimed.revision,
        Some(text("How did your garden bench turn out?")),
        vec![SourceRef {
            id: source.id,
            revision: source.revision,
        }],
        now + 60_002,
        "completed",
    )
    .await
    .expect("assistant-only delivery");
    assert!(
        h.history(&owner, None, 8)
            .await
            .expect("product history")
            .exchanges
            .is_empty(),
        "no fictitious user exchange was created"
    );
    store.shutdown().await.expect("close");
    let store = MemoryStore::open(dir.config()).await.expect("restart");
    let h = store.handle();
    let owner = store.owner();
    let notices = h.assistant_notices(&owner).await.expect("durable inbox");
    assert_eq!(notices.len(), 1);
    assert_eq!(
        h.source_record(&owner, notices[0].id)
            .await
            .expect("notice role")
            .role,
        SourceRole::Assistant
    );
    assert!(
        h.claim_attention(&owner, now + 60_003)
            .await
            .expect("restart no duplicate")
            .is_none()
    );
    h.dismiss_notice(&owner, notices[0].id)
        .await
        .expect("dismiss");
    assert!(h.assistant_notices(&owner).await.expect("dismiss state")[0].dismissed);
    let current = h.assistant_profile(&owner).await.expect("current");
    h.update_assistant(
        &owner,
        current.revision,
        AssistantSettings {
            instructions: text("Be quiet unless I ask."),
            initiative_interval_seconds: None,
        },
        now + 70_000,
    )
    .await
    .expect("disable/revise");
    assert!(
        h.settle_attention(
            &owner,
            claimed.revision,
            Some(text("stale personality message")),
            Vec::new(),
            now + 70_001,
            "completed"
        )
        .await
        .is_err()
    );
    h.suppress_source(&owner, key("forget-project"), source.id, source.revision)
        .await
        .expect("forget evidence");
    assert!(
        h.assistant_notices(&owner)
            .await
            .expect("read-time suppression")
            .is_empty()
    );
    store.shutdown().await.expect("close");
}

#[tokio::test]
async fn graph_pages_reach_every_neighbor_without_enlarging_the_model_context() {
    let dir = Directory::new();
    let store = MemoryStore::open(dir.config()).await.expect("store");
    let h = store.handle();
    let owner = store.owner();
    let root = h
        .remember(
            &owner,
            key("hub"),
            text("A collection of independent projects and preferences."),
        )
        .await
        .expect("entry point")
        .value;
    let mut expected = std::collections::BTreeSet::new();
    for index in 0..23 {
        let note = h
            .remember_note(
                &owner,
                key(&format!("neighbor-{index}")),
                text(&format!("Project note {index} with its own topic.")),
                vec![(
                    SourceRef {
                        id: root.id,
                        revision: root.revision,
                    },
                    text("related_to"),
                )],
                job_clock(),
            )
            .await
            .expect("neighbor")
            .value;
        expected.insert(note.id);
    }
    let mut found = std::collections::BTreeSet::new();
    let mut cursor = None;
    let mut pages = 0;
    loop {
        let page = h
            .source_page(&owner, root.id, cursor)
            .await
            .expect("bounded adjacency page");
        for link in &page.links {
            assert_eq!(link.to.id, root.id);
            found.insert(link.from.id);
        }
        assert!(
            page.sources
                .iter()
                .all(|record| record.role == SourceRole::Assistant || record.source.id == root.id)
        );
        pages += 1;
        assert!(pages <= 23, "cursor always makes progress");
        cursor = page.next_cursor;
        if cursor.is_none() {
            break;
        }
    }
    assert!(pages > 1);
    assert_eq!(
        found, expected,
        "no high-degree connection is permanently discarded"
    );
    assert!(
        h.source_page(&owner, root.id, Some("malformed cursor".into()))
            .await
            .is_err()
    );
    store.shutdown().await.expect("close");
}
