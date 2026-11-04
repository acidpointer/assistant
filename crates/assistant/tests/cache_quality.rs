//! Explicit model-quality characterization with authored data and fixed tools.
//! Model absence skips unless required. Case failures are recorded as scores;
//! infrastructure/native failures fail the job. The companion script applies
//! the declared candidate gates and verifies SHA256 identities before a run.
use assistant::{
    AssistantRuntime,
    chat::ToolUse,
    engine::{KvCacheType, LLMConfig, ModelEngine, SwaCacheMode},
    tools::{
        Tool, ToolContext,
        builtin::{Calculator, CurrentTime, TimeArgs},
    },
};
use llama_cpp::model::{AddBos, LlamaModel, params::LlamaModelParams};
use serde_json::{Value, json};
use std::{collections::BTreeSet, path::PathBuf};

const DATA: &str = include_str!("fixtures/cache-quality-v1.json");
const MANIFEST: &str = include_str!("fixtures/cache-quality-manifest.json");

fn fixtures() -> Value {
    serde_json::from_str(DATA).expect("authored quality fixtures")
}
fn fnv(bytes: &[u8]) -> String {
    let hash = bytes.iter().fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x100_0000_01b3)
    });
    format!("{hash:016x}")
}

#[test]
fn fixture_manifest_and_tasks_are_consistent_without_a_model() {
    let manifest: Value = serde_json::from_str(MANIFEST).expect("manifest JSON");
    assert_eq!(manifest["files"][0]["fnv1a64"], fnv(DATA.as_bytes()));
    let data = fixtures();
    assert_eq!(data["version"], 1);
    let mut ids = BTreeSet::new();
    for case in data["tasks"].as_array().expect("task list") {
        assert!(ids.insert(case["id"].as_str().expect("task ID")));
        assert!(!case["prompt"].as_str().expect("task prompt").is_empty());
        assert!(case["expected"].is_string() || case["integer"].is_i64());
    }
    assert!(data["texts"].as_array().expect("numerical corpus").len() >= 4);
    assert_eq!(data["acceptance"]["fp16_repeats"], 2);
    assert_eq!(data["retrieval"]["target_user_tokens"], json!([1024, 3500]));
}

#[derive(Debug)]
struct FixedClock {
    value: String,
    description: String,
}
impl Tool for FixedClock {
    const NAME: &'static str = "current_time";
    type Args = TimeArgs;
    type Output = Value;
    type Error = std::io::Error;
    fn description(&self) -> &str {
        &self.description
    }
    async fn call(&self, context: ToolContext, _: TimeArgs) -> std::io::Result<Value> {
        if context.cancellation.is_cancelled() {
            return Err(std::io::Error::other("cancelled"));
        }
        Ok(json!({"local_time":self.value}))
    }
}

#[derive(Debug, Clone)]
struct Case {
    id: String,
    prompt: String,
    expected: Option<String>,
    integer: Option<i64>,
    tools: Vec<String>,
    kind: String,
}

fn text<'a>(value: &'a Value, key: &str) -> &'a str {
    value[key].as_str().expect("fixture string field")
}
// Retrieval and literal-reading tasks require an answer, not a quoted needle
// inside unfinished reasoning. Harmless quote/Markdown wrappers are accepted.
fn exact_answer(answer: &str, expected: &str) -> bool {
    answer.trim().trim_matches(['"', '\'', '`', '*']).trim() == expected
}

#[test]
fn exact_answers_reject_reasoning_echoes_and_accept_literal_wrappers() {
    for answer in [
        "ORCHID-72",
        " \"ORCHID-72\" ",
        "`ORCHID-72`",
        "**ORCHID-72**",
    ] {
        assert!(exact_answer(answer, "ORCHID-72"));
    }
    for answer in [
        "The prompt says ORCHID-72, but I need to find the record...",
        "ORCHID-72 or COBALT-19",
        "The access phrase is ORCHID-72.",
    ] {
        assert!(!exact_answer(answer, "ORCHID-72"));
    }
}

fn integer_in(answer: &str, expected: i64) -> bool {
    answer
        .replace(',', "")
        .split(|c: char| !c.is_ascii_digit() && c != '-')
        .any(|word| word.parse::<i64>().ok() == Some(expected))
}

fn archive(record: &Value, filler: &str, position: &str, entries: usize) -> String {
    let needle = format!(
        "Only record {} has the access phrase {}.\n",
        text(record, "key"),
        text(record, "value")
    );
    let at = match position {
        "start" => 0,
        "middle" => entries / 2,
        "end" => entries,
        _ => panic!("unknown authored position"),
    };
    let mut result = String::from("Read the archive data below.\n");
    for i in 0..=entries {
        if i == at {
            result.push_str(&needle);
        }
        if i < entries {
            result.push_str(
                &filler
                    .replace("{index}", &i.to_string())
                    .replace("{number}", &(i * 17 + 3).to_string()),
            );
        }
    }
    result.push_str(&format!(
        "\nWhat access phrase is assigned to record {}? Return only the exact phrase.",
        text(record, "key")
    ));
    result
}

fn cases(data: &Value, tokenizer: &LlamaModel, full: bool) -> Vec<Case> {
    let mut cases = Vec::new();
    for item in data["tasks"].as_array().expect("tasks") {
        if !full && item["smoke"] != true {
            continue;
        }
        cases.push(Case {
            id: text(item, "id").into(),
            prompt: text(item, "prompt").into(),
            expected: item["expected"].as_str().map(str::to_owned),
            integer: item["integer"].as_i64(),
            tools: item["tools"]
                .as_array()
                .expect("required tools")
                .iter()
                .map(|v| v.as_str().expect("tool name").to_owned())
                .collect(),
            kind: text(item, "kind").into(),
        });
    }
    let retrieval = &data["retrieval"];
    let sizes = if full {
        "target_user_tokens"
    } else {
        "smoke_target_user_tokens"
    };
    let positions = if full { "positions" } else { "smoke_positions" };
    for size in retrieval[sizes].as_array().expect("retrieval sizes") {
        let size =
            usize::try_from(size.as_u64().expect("positive target")).expect("bounded token target");
        for position in retrieval[positions].as_array().expect("positions") {
            for record in retrieval["records"].as_array().expect("records") {
                let position = position.as_str().expect("position");
                let (mut low, mut high) = (0, size / 8 + 2);
                while low < high {
                    let mid = low + (high - low).div_ceil(2);
                    let prompt = archive(record, text(retrieval, "filler"), position, mid);
                    let count = tokenizer
                        .str_to_token(&prompt, AddBos::Always)
                        .expect("fixture tokenization")
                        .len();
                    if count <= size {
                        low = mid;
                    } else {
                        high = mid - 1;
                    }
                }
                cases.push(Case {
                    id: format!("retrieve-{}-{size}-{position}", text(record, "key")),
                    prompt: archive(record, text(retrieval, "filler"), position, low),
                    expected: Some(text(record, "value").into()),
                    integer: None,
                    tools: Vec::new(),
                    kind: "retrieval".into(),
                });
            }
        }
    }
    cases
}

async fn evaluate_profile(
    engine: &'static ModelEngine,
    path: &PathBuf,
    data: &Value,
    cases: &[Case],
    profile: &str,
    swa: SwaCacheMode,
) -> anyhow::Result<()> {
    let mut config = LLMConfig::new(path);
    config.max_new_tokens = 256;
    config.cache.swa = swa;
    match profile {
        "f16" => {}
        "q8" => {
            config.cache.key = KvCacheType::Q8_0;
            config.cache.value = KvCacheType::Q8_0;
        }
        "q4" => {
            config.cache.key = KvCacheType::Q4_0;
            config.cache.value = KvCacheType::Q4_0;
        }
        _ => anyhow::bail!("unsupported reference profile"),
    }
    let runtime = AssistantRuntime::builder(engine)
        .model("quality", config)
        .system(text(data, "system"))
        .tool(Calculator)
        .tool(FixedClock {
            value: text(data, "clock").into(),
            description: CurrentTime.description().into(),
        })
        .build()?
        .start()
        .await?;
    let result=async {
        let chat=runtime.chat();
        for case in cases {
            chat.clear_working_history().await;
            let mut request=chat.request(case.prompt.clone());
            if case.tools.is_empty() {request=request.tools(Vec::<String>::new()).tool_use(ToolUse::Disabled);}
            let response=request.run().await?;
            let successful:BTreeSet<_>=response.tools.iter().filter(|r|r.result["ok"]==true).map(|r|r.call.name.as_str()).collect();
            let correct_tools=case.tools.iter().all(|t|successful.contains(t.as_str()));
            let correct_answer=case.expected.as_ref().is_none_or(|expected| if case.kind == "retrieval" || case.kind == "answer" { exact_answer(response.text(), expected) } else { response.text().contains(expected) }) && case.integer.is_none_or(|expected|integer_in(response.text(),expected));
            let correct_value=case.integer.is_none_or(|expected|response.tools.iter().any(|r|r.call.name=="calculate" && r.result["ok"]==true && r.result["result"]["value"]==expected));
            let passed=correct_tools && correct_answer && (case.tools.is_empty() || correct_value);
            eprintln!("CACHE_TASK_QUALITY {}",json!({"stage":"baseline","profile":profile,"swa":format!("{swa:?}"),"case":case.id,"kind":case.kind,"passed":passed,"answer":response.text(),"successful_tools":successful,"input_tokens":response.usage.input_tokens,"generated_tokens":response.usage.generated_tokens}));
        }
        let diagnostics=runtime.cache_diagnostics().await?;
        let contexts=&diagnostics["quality"].contexts;
        if std::env::var_os("ASSISTANT_QUALITY_REQUIRE_VULKAN").is_some() {anyhow::ensure!(contexts.iter().any(|c|c.weight_devices.iter().any(|d|d.starts_with("Vulkan"))),"required Vulkan quality coverage unavailable");}
        eprintln!("CACHE_QUALITY_CONFIG {}",json!({"profile":profile,"swa":format!("{swa:?}"),"diagnostics":diagnostics["quality"].to_json()}));
        Ok::<_,anyhow::Error>(())
    }.await;
    let stopped = runtime.shutdown().await;
    match (result, stopped) {
        (Err(error), Err(cleanup)) => Err(anyhow::anyhow!("{error}; shutdown: {cleanup}")),
        (Err(error), _) => Err(error),
        (_, Err(error)) => Err(error.into()),
        (Ok(()), Ok(())) => Ok(()),
    }
}

#[tokio::test(flavor = "current_thread")]
async fn required_cache_task_quality_characterization() {
    let Some(path) = std::env::var_os("ASSISTANT_QUALITY_MODEL") else {
        assert!(
            std::env::var_os("ASSISTANT_QUALITY_REQUIRE_MODEL").is_none()
                && std::env::var_os("ASSISTANT_QUALITY_REQUIRE_VULKAN").is_none(),
            "required quality coverage needs ASSISTANT_QUALITY_MODEL"
        );
        eprintln!(
            "SKIP: use scripts/cache-quality-results.py run for explicit model-quality characterization"
        );
        return;
    };
    let path = PathBuf::from(path);
    assert!(path.is_file(), "quality model must exist");
    let checksum = std::env::var("ASSISTANT_QUALITY_MODEL_SHA256")
        .expect("run through scripts/cache-quality-results.py to verify model SHA256");
    assert!(checksum.len() == 64 && checksum.bytes().all(|b| b.is_ascii_hexdigit()));
    ModelEngine::init().expect("quality backend");
    let engine = ModelEngine::get_instance().expect("quality engine");
    let tokenizer = LlamaModel::load_from_file(
        engine.backend_ref(),
        &path,
        &LlamaModelParams::default().with_vocab_only(true),
    )
    .expect("model-matched tokenizer only, no second weight allocation");
    let data = fixtures();
    let full = std::env::var_os("ASSISTANT_QUALITY_FULL").is_some();
    let cases = cases(&data, &tokenizer, full);
    eprintln!(
        "CACHE_QUALITY_IDENTITY {}",
        json!({"model_sha256":checksum,"fixture_fnv1a64":fnv(DATA.as_bytes()),"vocabulary_size":tokenizer.n_vocab(),"full":full,"task_count":cases.len(),"acceptance":data["acceptance"]})
    );
    for (profile, swa) in [
        ("f16", SwaCacheMode::Full),
        ("f16", SwaCacheMode::Compact),
        ("q8", SwaCacheMode::Compact),
        ("q4", SwaCacheMode::Compact),
    ] {
        evaluate_profile(engine, &path, &data, &cases, profile, swa)
            .await
            .expect("evaluation must execute; accuracy is recorded per case");
    }
}
