//! Real local-artifact memory preflight. Default tests do not require weights;
//! explicit execution with `ASSISTANT_REQUIRE_MEMORY_MODEL=1` fails if unavailable.
//! This is development evidence, not held-out quality qualification.
#![cfg(target_os = "linux")]
use assistant::{
    AssistantRuntime,
    chat::{RequestOptions, ToolUse},
    engine::{ExecutionMode, LLMConfig, ModelEngine},
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
#[ignore = "semantic qualification gate: current local chat model fails this case"]
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
    let prompt = r#"Extract explicit personal knowledge into the constrained JSON object.
Rules: version=1. coverage is complete only if all offered text is examined; otherwise partial or none.
Entities: introduce n0..n7 for new named entities; kind must describe the label. Each needs source=e0 and an exact quotation proving its presence. u is the user. Do not use a or x handles: none offered.
Claims: use action=add and target=null. NO existing c handles were offered, so corrections are forbidden. subject and reference objects must resolve to u or a declared n handle. Family sibling_of connects two people. has_occupation goes from the person who has the occupation to an occupation entity. Preserve direction.
Polarity is positive unless explicitly negated. Modality is asserted for direct statements, reported for attributed reports, hypothetical only for possibilities, question for questions, fiction for fiction.
valid_from and valid_to are null unless the source explicitly supplies a validity time. Source handles are not dates. conditions is [] unless a condition is explicitly stated. Evidence quotes exact unique substrings. unresolved is [] when no ambiguity remains. Do not infer unstated claims.
Example source: I prefer tea.
Example output: {"version":1,"coverage":"complete","entities":[],"claims":[{"action":"add","target":null,"subject":"u","predicate":"prefers","object":{"kind":"text","value":"tea"},"polarity":"positive","modality":"asserted","valid_from":null,"valid_to":null,"conditions":[],"evidence":[{"source":"e0","quote":"I prefer tea."}]}],"unresolved":[]}
Now extract source e0 (speaker=user): My sister Lena is a veterinarian."#;
    let response = runtime
        .chat()
        .request(prompt)
        .options(RequestOptions {
            json_schema: Some(include_str!("../../../docs/memory-extraction.schema.json").into()),
            max_new_tokens: Some(512),
            tool_use: ToolUse::Disabled,
            ..RequestOptions::default()
        })
        .run()
        .await;
    let stopped = runtime.shutdown().await;
    let response = response.expect("real constrained extraction");
    stopped.expect("join chat");
    eprintln!(
        "extraction status={:?} sampled={} output={}",
        response.status, response.usage.generated_tokens, response.message.content
    );
    let proposal: serde_json::Value =
        serde_json::from_str(&response.message.content).expect("unrepaired JSON");
    assert_eq!(proposal["version"], 1);
    let entities = proposal["entities"].as_array().expect("entities");
    let lena = entities
        .iter()
        .find(|e| e["label"] == "Lena" && e["kind"] == "person")
        .expect("Lena identity")["handle"]
        .as_str()
        .expect("handle");
    let occupation = entities
        .iter()
        .find(|e| e["label"] == "veterinarian" && e["kind"] == "occupation")
        .expect("occupation identity")["handle"]
        .as_str()
        .expect("handle");
    let claims = proposal["claims"].as_array().expect("claims");
    assert_eq!(
        claims.len(),
        2,
        "exactly the two supported development facts"
    );
    assert!(
        claims.iter().any(|c| c["subject"] == "u"
            && c["predicate"] == "sibling_of"
            && c["object"]["value"] == lena),
        "family direction"
    );
    assert!(
        claims.iter().any(|c| c["subject"] == lena
            && c["predicate"] == "has_occupation"
            && c["object"]["value"] == occupation),
        "occupation attribution"
    );
    for claim in claims {
        assert_eq!(claim["action"], "add");
        assert!(claim["target"].is_null());
        assert_eq!(claim["modality"], "asserted");
        assert_eq!(claim["polarity"], "positive");
        assert!(claim["valid_from"].is_null() && claim["valid_to"].is_null());
    }
}
