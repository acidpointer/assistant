//! Real local-artifact memory preflight. Default tests do not require weights;
//! explicit execution with `ASSISTANT_REQUIRE_MEMORY_MODEL=1` fails if unavailable.
//! This is development evidence, not held-out quality qualification.
#![cfg(target_os = "linux")]
#[cfg(feature = "experimental-extraction")]
use assistant::{
    AssistantRuntime,
    engine::{ExecutionMode, LLMConfig},
};
use assistant::{
    engine::ModelEngine,
    memory::{
        MemoryConfig, MemoryId, MemoryText, OperationKey,
        embedding::EmbeddingConfig,
        service::{MemoryRuntime, MemoryRuntimeConfig},
    },
};
use std::{os::unix::fs::DirBuilderExt, path::PathBuf, time::Duration};
#[derive(Debug)]
struct TestDirectory(PathBuf);
impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "explicit native development preflight"]
async fn selected_e5_persists_recall() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let e5 = root.join("multilingual-e5-small-q8_0.gguf");
    if !e5.is_file() {
        assert!(
            std::env::var_os("ASSISTANT_REQUIRE_MEMORY_MODEL").is_none(),
            "required selected local artifacts missing"
        );
        eprintln!("skip: optional memory model artifacts absent");
        return;
    }
    let _guard = MODEL_LOCK.lock().expect("native test lock");
    if ModelEngine::get_instance().is_err() {
        ModelEngine::init().expect("backend");
    }
    let engine = ModelEngine::get_instance().expect("engine");
    let directory = TestDirectory(std::env::temp_dir().join(format!(
        "assistant-memory-native-{}",
        MemoryId::new().expect("identity")
    )));
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&directory.0)
        .expect("private dir");
    let config = MemoryRuntimeConfig {
        storage: MemoryConfig::new(&directory.0),
        embedding: EmbeddingConfig::e5_q8(&e5),
    };
    let memory = MemoryRuntime::start(engine, config.clone())
        .await
        .expect("memory startup");
    let owner = memory.owner_access();
    let saved = owner
        .remember(
            OperationKey::new("test", "sister").expect("key"),
            MemoryText::new("My sister Lena is a veterinarian.\nShe works with animals.")
                .expect("source"),
        )
        .await
        .expect("remember");
    owner
        .remember(
            OperationKey::new("test", "other").expect("key"),
            MemoryText::new("The database stores financial transactions.").expect("source"),
        )
        .await
        .expect("remember unrelated");
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let found = owner
            .search(
                MemoryText::new("Хто в моїй родині працює з тваринами?").expect("query"),
                2,
            )
            .await
            .expect("semantic recall");
        if found.len() == 2 {
            assert_eq!(found[0].source.id, saved.value.id);
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "semantic coverage deadline; fault={:?}",
            owner.indexing_fault()
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    memory.shutdown().await.expect("join memory");
    let memory = MemoryRuntime::start(engine, config).await.expect("restart");
    let found = memory
        .owner_access()
        .search(
            MemoryText::new("Who in my family works with animals?").expect("query"),
            2,
        )
        .await
        .expect("recall after restart");
    assert_eq!(
        found[0].source.id, saved.value.id,
        "no working history exists"
    );
    memory.shutdown().await.expect("join restart");
}

static MODEL_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "explicit native tokenizer/window development check"]
async fn selected_e5_token_windows_preserve_literal_coverage() {
    use assistant::memory::embedding::{EmbeddingPurpose, EmbeddingWorker};
    let e5 =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../multilingual-e5-small-q8_0.gguf");
    if !e5.is_file() {
        assert!(
            std::env::var_os("ASSISTANT_REQUIRE_MEMORY_MODEL").is_none(),
            "required selected artifact absent"
        );
        return;
    }
    let _guard = MODEL_LOCK.lock().expect("native test lock");
    if ModelEngine::get_instance().is_err() {
        ModelEngine::init().expect("backend");
    }
    let worker = EmbeddingWorker::start(
        ModelEngine::get_instance().expect("engine"),
        EmbeddingConfig::e5_q8(e5),
    )
    .await
    .expect("E5 worker");
    let handle = worker.handle();
    let text = "Марія працює з тваринами. Lena is a veterinarian.\n".repeat(40);
    let source = MemoryText::new(text.clone()).expect("literal source");
    let mut cursor = 0;
    let mut chunks = 0;
    while (cursor as usize) < text.len() {
        let (start, end) = handle
            .passage_window(source.clone(), cursor)
            .await
            .expect("token measured window");
        assert!(start <= cursor && end > cursor);
        let span = text
            .get(start as usize..end as usize)
            .expect("exact UTF-8 span");
        let vector = handle
            .embed(
                EmbeddingPurpose::Passage,
                MemoryText::new(span).expect("literal chunk"),
            )
            .await
            .expect("native chunk");
        assert!(
            vector.tokens <= 384,
            "prefix and special tokens are charged"
        );
        assert_eq!(vector.values().len(), 384);
        cursor = end;
        chunks += 1;
    }
    assert!(chunks > 1);
    assert_eq!(cursor as usize, text.len());
    worker.shutdown().await.expect("join native owner");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "explicit staged extraction development preflight; not held-out qualification"]
#[cfg(feature = "experimental-extraction")]
async fn lfm_named_json_semantic_preflight() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let chat = std::env::var_os("ASSISTANT_TEST_MODEL").map_or_else(
        || root.join("LFM3-1.7B-Heretic-Abliterated-Q6_K.gguf"),
        PathBuf::from,
    );
    if !chat.is_file() {
        assert!(
            std::env::var_os("ASSISTANT_REQUIRE_MEMORY_MODEL").is_none(),
            "required chat artifact absent"
        );
        eprintln!("skip: optional chat model absent");
        return;
    }
    let _guard = MODEL_LOCK.lock().expect("native test lock");
    if ModelEngine::get_instance().is_err() {
        ModelEngine::init().expect("backend");
    }
    let engine = ModelEngine::get_instance().expect("engine");
    let mut config = LLMConfig::new(chat);
    config.execution = ExecutionMode::Cpu;
    config.gpu_layers = 0;
    config.context_size = 4096;
    config.batch_size = 512;
    config.max_new_tokens = 512;
    config.threads = 4;
    let runtime=AssistantRuntime::builder(engine).model("main",config).system("Extract only explicitly stated facts. Return the requested JSON object, with no commentary.").build().expect("config").start().await.expect("chat startup");
    use assistant::memory::{
        ClaimModality, ExtractionAccounting, ExtractionBudget, ExtractionOutcome, MemoryStore,
        Predicate,
    };
    let directory = TestDirectory(std::env::temp_dir().join(format!(
        "assistant-formation-native-{}",
        MemoryId::new().expect("identity")
    )));
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&directory.0)
        .expect("private store");
    let store = MemoryStore::open(MemoryConfig::new(&directory.0))
        .await
        .expect("store");
    let handle = store.handle();
    let owner = store.owner();
    let source = handle
        .remember(
            &owner,
            OperationKey::new("preflight", "family-job").expect("key"),
            MemoryText::new("My sister Lena is a veterinarian.").expect("source"),
        )
        .await
        .expect("commit source")
        .value;
    let clock = || {
        i64::try_from(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_millis(),
        )
        .expect("clock bounds")
    };
    let lease = handle
        .claim_extraction_job_with_budget(&owner, clock(), ExtractionBudget::StagedRoot)
        .await
        .expect("reserve staged root")
        .expect("source lease");
    assert_eq!(lease.source.id, source.id);
    let record = handle
        .source_record(&owner, source.id)
        .await
        .expect("host attribution");
    let internal = runtime
        .internal_inference("main")
        .expect("internal capability");
    let evaluation = assistant::memory::formation::extract_source(
        &internal,
        &record,
        owner.principal(),
        assistant::engine::Cancellation::default(),
    );
    tokio::pin!(evaluation);
    let mut heartbeat = tokio::time::interval_at(
        tokio::time::Instant::now() + Duration::from_secs(20),
        Duration::from_secs(20),
    );
    let report = loop {
        tokio::select! {
            result = &mut evaluation => break result.expect("bounded staged semantic formation"),
            _ = heartbeat.tick() => handle.heartbeat_extraction_job(&owner,lease.clone(),clock()).await.expect("renew live fence without extending hard end"),
        }
    };
    assert!(
        report.usage.input_tokens <= 4096
            && report.usage.output_tokens <= 1024
            && report.usage.native_millis <= 120000
    );
    assert_eq!(report.passes, 3);
    eprintln!(
        "staged development accounting: {:?}, passes={}",
        report.usage, report.passes
    );
    let principal = owner.principal();
    let changes = report.changes;
    assert_eq!(
        changes.claims.len(),
        2,
        "exactly the two supported development facts"
    );
    let lena = changes
        .entities
        .iter()
        .find(|entity| {
            entity.label.as_str() == "Lena" && entity.kind == assistant::memory::EntityKind::Person
        })
        .expect("Lena identity");
    let occupation = changes
        .entities
        .iter()
        .find(|entity| {
            entity.label.as_str() == "veterinarian"
                && entity.kind == assistant::memory::EntityKind::Occupation
        })
        .expect("occupation identity");
    assert!(
        changes.claims.iter().any(|c| c.subject == principal
            && c.predicate == Predicate::SiblingOf
            && matches!(c.object,assistant::memory::ClaimObject::Reference(id) if id==lena.id)),
        "family direction"
    );
    assert!(changes.claims.iter().any(|c|c.subject==lena.id && c.predicate==Predicate::HasOccupation && matches!(c.object,assistant::memory::ClaimObject::Reference(id) if id==occupation.id)),"occupation attribution");
    for claim in &changes.claims {
        assert_eq!(claim.modality, ClaimModality::Asserted);
        assert!(!claim.negative);
        assert!(
            claim.conditions.is_empty() && claim.valid_from.is_none() && claim.valid_to.is_none(),
            "no invented conditions or validity times"
        );
        assert_eq!(claim.evidence[0].source_id, source.id);
        assert_eq!(
            claim.evidence[0].quote.as_str(),
            "My sister Lena is a veterinarian."
        );
    }
    // This explicit development test promotes only after its authored semantic
    // assertions. It does not activate an unqualified production formation loop.
    handle
        .finish_extraction_job_accounted(
            &owner,
            lease,
            changes,
            ExtractionOutcome::Complete,
            ExtractionAccounting {
                usage: report.usage,
                passes: report.passes,
            },
        )
        .await
        .expect("atomic fenced graph completion");
    let graph = handle
        .follow_at(&owner, principal, 2, clock())
        .await
        .expect("model-produced graph read");
    assert_eq!(graph.claims.len(), 2);
    runtime.shutdown().await.expect("join native owner");
    store.shutdown().await.expect("close store");
    let store = MemoryStore::open(MemoryConfig::new(&directory.0))
        .await
        .expect("reopen graph");
    let graph = store
        .handle()
        .follow_at(&store.owner(), principal, 2, clock())
        .await
        .expect("fresh store graph proof");
    assert_eq!(graph.claims.len(), 2);
    store.shutdown().await.expect("close reopened store");
}
