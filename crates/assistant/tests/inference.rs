//! Exercise the binary's batched prompt decoding with the upstream tiny model.

use std::path::PathBuf;
use std::process::Command;

use serde_json::Value;

#[test]
fn generates_after_a_prompt_larger_than_one_batch() {
    let model =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/test-models/stories260K.gguf");
    if !model.is_file() {
        eprintln!(
            "SKIP: place stories260K.gguf in target/test-models to run the CLI inference test"
        );
        return;
    }

    let output = Command::new(env!("CARGO_BIN_EXE_assistant"))
        .arg("--model")
        .arg(model)
        .args([
            "--raw",
            "--gpu-layers",
            "0",
            "--max-tokens",
            "4",
            "--prompt",
        ])
        .arg("Once upon a time ".repeat(200))
        .output()
        .expect("run assistant");
    let logs = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "assistant failed: {logs}");
    let prompt_tokens: usize = logs
        .lines()
        .find_map(|line| line.strip_prefix("Prompt: "))
        .expect("prompt token count")
        .split_whitespace()
        .next()
        .expect("token count field")
        .parse()
        .expect("integer token count");
    assert!(prompt_tokens > 512, "test must exercise multiple batches");
    assert!(logs.contains("Generated 4 tokens"), "{logs}");
    let text = String::from_utf8(output.stdout).expect("valid streamed UTF-8");
    assert!(!text.trim().is_empty(), "generation must produce text");
}

// The tiny story model verifies decoding, but was not trained to call tools.
// These model-quality evaluations require an explicit instruction/tool model.
// Set ASSISTANT_TEST_MODEL; absence is a skip unless required mode is enabled.
fn tool_model() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("ASSISTANT_TEST_MODEL") {
        let path = PathBuf::from(path);
        assert!(
            path.is_file(),
            "ASSISTANT_TEST_MODEL must point to a GGUF file"
        );
        return Some(path);
    }
    assert!(
        std::env::var_os("ASSISTANT_TEST_REQUIRE_MODEL").is_none(),
        "model-quality coverage requires ASSISTANT_TEST_MODEL"
    );
    eprintln!("SKIP: set ASSISTANT_TEST_MODEL to run model-quality evaluations");
    None
}

fn ask_with_tools(prompt: &str) -> Option<(String, String)> {
    let model = tool_model()?;
    let profile = std::env::var("ASSISTANT_TEST_CACHE_PROFILE").unwrap_or_else(|_| "f16".into());
    let (key, value) = match profile.as_str() {
        "f16" => ("f16", "f16"),
        "q8" => ("q8_0", "q8_0"),
        "q8-q4" => ("q8_0", "q4_0"),
        "q4" => ("q4_0", "q4_0"),
        _ => panic!("unknown ASSISTANT_TEST_CACHE_PROFILE"),
    };
    let swa = std::env::var("ASSISTANT_TEST_SWA").unwrap_or_else(|_| "full".into());
    let gpu_layers = std::env::var("ASSISTANT_TEST_GPU_LAYERS").unwrap_or_else(|_| "0".into());
    let output = Command::new(env!("CARGO_BIN_EXE_assistant"))
        .arg("--model")
        .arg(model)
        .args([
            "--timeout-seconds",
            "300",
            "--trace",
            "--gpu-layers",
            &gpu_layers,
            "--cache-type-k",
            key,
            "--cache-type-v",
            value,
            "--swa-cache",
            &swa,
            "--prompt",
            prompt,
        ])
        .output()
        .expect("run assistant with tools");
    let logs = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(output.status.success(), "tool conversation failed: {logs}");
    Some((
        String::from_utf8(output.stdout).expect("UTF-8 answer"),
        logs,
    ))
}

#[test]
fn invalid_cache_policy_is_rejected_before_model_loading() {
    let output = Command::new(env!("CARGO_BIN_EXE_assistant"))
        .args([
            "--model",
            "missing.gguf",
            "--cache-type-v",
            "q4_0",
            "--flash-attn",
            "off",
            "--prompt",
            "unused",
        ])
        .output()
        .expect("run invalid cache policy");
    assert!(!output.status.success());
    let logs = String::from_utf8_lossy(&output.stderr);
    assert!(
        logs.contains("quantized value cache requires flash attention"),
        "{logs}"
    );
}

fn result_for(logs: &str, tool: &str) -> Value {
    let prefix = format!("[tool result]  {tool} ");
    let result: Value = logs
        .lines()
        .find_map(|line| line.strip_prefix(&prefix))
        .map_or_else(
            || panic!("model did not call {tool}: {logs}"),
            |json| serde_json::from_str(json).expect("JSON tool result"),
        );
    assert_eq!(result["ok"], true, "tool failed: {result}");
    result["result"].clone()
}

fn mentions_integer(answer: &str, expected: i64) -> bool {
    // Natural answers may display 56088 as 56,088. Check the complete integer,
    // including its sign, rather than rejecting harmless presentation changes
    // or accidentally accepting the number as a substring of another number.
    answer
        .replace(',', "")
        .split(|c: char| !c.is_ascii_digit() && c != '-')
        .any(|word| word.parse::<i64>().ok() == Some(expected))
}

#[test]
fn numeric_answer_check_accepts_formatting_but_preserves_value_and_sign() {
    assert!(mentions_integer("The answer is **56,088**.", 56088));
    assert!(!mentions_integer("156088", 56088));
    assert!(!mentions_integer("-56,088", 56088));
    assert!(!mentions_integer("56089", 56088));
}

#[test]
fn model_calls_calculator_and_uses_its_result() {
    let Some((answer, logs)) = ask_with_tools("What is 123 multiplied by 456?") else {
        return;
    };
    assert_eq!(result_for(&logs, "calculate")["value"], 56088);
    assert!(
        mentions_integer(&answer, 56088),
        "incorrect answer: {answer}"
    );
}

#[test]
fn model_calls_date_and_returns_the_actual_output() {
    let Some((answer, logs)) =
        ask_with_tools("What time is it? Return the date command's complete output verbatim.")
    else {
        return;
    };
    let result = result_for(&logs, "current_time");
    let actual = result["local_time"].as_str().expect("date output");
    assert!(!actual.is_empty());
    assert!(
        answer.contains(actual),
        "answer {answer:?} did not contain tool result {actual:?}"
    );
}

#[test]
fn model_can_use_two_different_tools_in_one_conversation() {
    let Some((answer, logs)) = ask_with_tools("What time is it, and what is 19 times 23?") else {
        return;
    };
    assert!(result_for(&logs, "current_time")["local_time"].is_string());
    assert_eq!(result_for(&logs, "calculate")["value"], 437);
    assert!(mentions_integer(&answer, 437), "incorrect answer: {answer}");
}
