//! Explicit selected-model extraction evaluation. Synthetic authored fixtures and
//! actual model output are compared separately from storage/seeded graph tests.
//! Default tests need no model. Reports bind corpus, algorithm and model hashes;
//! a filtered or development run never qualifies the full memory feature.
#![cfg(all(target_os = "linux", feature = "experimental-extraction"))]

use assistant::{
    AssistantRuntime,
    engine::{Cancellation, ExecutionMode, LLMConfig, ModelEngine},
    memory::{
        ClaimObject, FormationState, MemoryId, MemoryText, Revision, Source, SourceRecord,
        SourceRole,
        extraction::Fact,
        formation::{FormationCause, extract_source},
    },
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, io::Read, path::PathBuf};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Corpus {
    version: u32,
    track: String,
    scope: String,
    cases: Vec<Case>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Case {
    id: String,
    source: String,
    expected: Vec<Fact>,
    unresolved: bool,
}
#[derive(Serialize)]
struct ResultRow {
    id: String,
    actual: Vec<Fact>,
    error: Option<String>,
    true_positive: usize,
    false_positive: usize,
    false_negative: usize,
    exact: bool,
    input_tokens: Option<u32>,
    output_tokens: Option<u32>,
    native_millis: Option<u32>,
    passes: Option<u8>,
    failure_stage: Option<assistant::memory::formation::FormationStage>,
    measurement_complete: bool,
    observed_output: Option<String>,
}
fn hash_file(path: &std::path::Path) -> String {
    let mut file = std::fs::File::open(path).expect("required artifact");
    let mut hash = Sha256::new();
    let mut buffer = [0; 65536];
    loop {
        let size = file.read(&mut buffer).expect("read artifact");
        if size == 0 {
            break;
        }
        hash.update(&buffer[..size]);
    }
    format!("{:x}", hash.finalize())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "explicit local-model quality evaluation; development and held-out tracks are distinct"]
async fn selected_model_extraction_corpus() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let model = root.join("LFM3-1.7B-Heretic-Abliterated-Q6_K.gguf");
    assert!(
        model.is_file(),
        "explicit evaluation requires the existing selected model"
    );
    let model_hash = hash_file(&model);
    assert_eq!(
        model_hash, "616775686c9abcbc1a0d302f75dc0a1f12498f272170bd7cda556a706c5606e1",
        "do not substitute models to pass this gate"
    );
    let path = std::env::var_os("ASSISTANT_MEMORY_EVAL_CORPUS").map_or_else(
        || root.join("docs/memory-extraction-development.json"),
        PathBuf::from,
    );
    let bytes = std::fs::read(path).expect("explicit corpus");
    let corpus: Corpus = serde_json::from_slice(&bytes).expect("bounded fixture contract");
    assert_eq!(corpus.version, 1);
    assert!(corpus.cases.len() <= 1000);
    let filter = std::env::var("ASSISTANT_MEMORY_EVAL_CASE").ok();
    let selected = corpus
        .cases
        .iter()
        .filter(|case| filter.as_ref().is_none_or(|filter| filter == &case.id))
        .collect::<Vec<_>>();
    assert!(!selected.is_empty(), "no matching evaluation case");
    let mut ids = std::collections::BTreeSet::new();
    for case in &corpus.cases {
        assert!(ids.insert(&case.id), "duplicate case ID");
        assert!(case.source.len() <= 2048 && case.expected.len() <= 4);
        assert!(!case.unresolved || case.expected.is_empty());
    }
    if ModelEngine::get_instance().is_err() {
        ModelEngine::init().expect("backend");
    }
    let engine = ModelEngine::get_instance().expect("engine");
    let mut config = LLMConfig::new(model);
    config.execution = ExecutionMode::Cpu;
    config.gpu_layers = 0;
    config.context_size = 4096;
    config.batch_size = 512;
    config.max_new_tokens = 512;
    config.threads = 4;
    let runtime = AssistantRuntime::builder(engine)
        .model("main", config)
        .build()
        .expect("runtime configuration")
        .start()
        .await
        .expect("resident selected model");
    let inference = runtime
        .internal_inference("main")
        .expect("restricted host evaluator");
    let principal = MemoryId::new().expect("fixture principal");
    let mut results = Vec::new();
    for case in selected {
        let source = SourceRecord {
            source: Source {
                id: MemoryId::new().expect("fixture source"),
                revision: Revision::new(1).expect("version"),
                text: MemoryText::new(&case.source).expect("literal fixture"),
                preserved: false,
            },
            role: SourceRole::User,
            operation_id: None,
            observed_at: None,
            formation: FormationState::Queued,
        };
        // This explicit evaluation is isolated from product submissions/storage.
        // Each case has a fresh root allowance; no output is promoted to memory.
        let evaluated =
            extract_source(&inference, &source, principal, Cancellation::default()).await;
        let (
            actual,
            error,
            usage,
            passes,
            semantic_rejection,
            failure_stage,
            measurement_complete,
            observed_output,
        ) = match evaluated {
            Ok(report) => {
                let mut labels = BTreeMap::from([(principal, "user".to_owned())]);
                labels.extend(
                    report
                        .changes
                        .entities
                        .into_iter()
                        .map(|entity| (entity.id, entity.label.as_str().to_owned())),
                );
                let facts = report
                    .changes
                    .claims
                    .into_iter()
                    .map(|claim| Fact {
                        subject: labels
                            .get(&claim.subject)
                            .expect("host-resolved subject")
                            .clone(),
                        relation: claim.predicate,
                        object: match claim.object {
                            ClaimObject::Reference(id) => {
                                labels.get(&id).expect("host-resolved object").clone()
                            }
                            ClaimObject::Text(value) => value.as_str().to_owned(),
                            _ => panic!("unsupported evaluation object"),
                        },
                        negated: claim.negative,
                        mode: claim.modality,
                        qualification: None,
                    })
                    .collect::<Vec<_>>();
                (
                    facts,
                    None,
                    Some(report.usage),
                    Some(report.passes),
                    false,
                    None,
                    true,
                    None,
                )
            }
            Err(error) => {
                let semantic_rejection = matches!(&error.cause,FormationCause::Validation(assistant::memory::MemoryError::InvalidInput(reason)) if !matches!(*reason,"unmeasured formation"|"incomplete formation"));
                (
                    Vec::new(),
                    Some(error.code().to_owned()),
                    Some(error.accounting.usage),
                    Some(error.accounting.passes),
                    semantic_rejection,
                    Some(error.stage),
                    error.measurement_complete,
                    error.output.map(|output| {
                        format!(
                            "{} complete={}: {}",
                            output.stage,
                            output.complete,
                            output.json.as_str()
                        )
                    }),
                )
            }
        };
        let tp = case
            .expected
            .iter()
            .filter(|fact| actual.contains(fact))
            .count();
        let fp = actual.len() - tp;
        let missing = case
            .expected
            .iter()
            .filter(|fact| !actual.contains(fact))
            .count();
        let exact = if case.unresolved {
            semantic_rejection
        } else {
            error.is_none() && fp == 0 && missing == 0
        };
        eprintln!(
            "case={} exact={exact} tp={tp} fp={fp} missing={missing}",
            case.id
        );
        results.push(ResultRow {
            id: case.id.clone(),
            actual,
            error,
            true_positive: tp,
            false_positive: fp,
            false_negative: missing,
            exact,
            input_tokens: usage.map(|usage| usage.input_tokens),
            output_tokens: usage.map(|usage| usage.output_tokens),
            native_millis: usage.map(|usage| usage.native_millis),
            passes,
            failure_stage,
            measurement_complete,
            observed_output,
        });
    }
    runtime.shutdown().await.expect("join native model owner");
    let complete = filter.is_none() && results.len() == corpus.cases.len();
    let passed = results.iter().all(|row| row.exact);
    let report = serde_json::json!({"track":corpus.track,"scope":corpus.scope,"complete_corpus":complete,"model_sha256":model_hash,"corpus_sha256":format!("{:x}",Sha256::digest(&bytes)),"algorithm_sha256":format!("{:x}",Sha256::digest(include_bytes!("../src/memory/extraction.rs"))),"formation_sha256":format!("{:x}",Sha256::digest(include_bytes!("../src/memory/formation.rs"))),"all_exact":passed,"full_memory_qualified":false,"results":results});
    let encoded = serde_json::to_vec_pretty(&report).expect("evaluation report");
    if let Some(path) = std::env::var_os("ASSISTANT_MEMORY_EVAL_REPORT") {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut file = std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(path)
            .expect("new report path; never overwrite evidence");
        file.write_all(&encoded).expect("write report");
    }
    println!("{}", String::from_utf8(encoded).expect("JSON UTF-8"));
    assert!(
        passed,
        "semantic evaluation failed; inspect report and revise the approach using development data"
    );
}
