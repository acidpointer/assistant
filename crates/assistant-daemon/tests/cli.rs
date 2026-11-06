//! Validate process modes before model loading. The missing model must never mask
//! an invalid combination or accidentally start a listener in diagnostic mode.
use std::process::Command;

fn rejects(args: &[&str], expected: &str) {
    let output = Command::new(env!("CARGO_BIN_EXE_assistant"))
        .args(["--model", "missing.gguf"])
        .args(args)
        .output()
        .expect("run CLI");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains(expected), "{stderr}");
    assert!(
        !stderr.contains("failed to load model"),
        "arguments must fail first: {stderr}"
    );
}
#[test]
fn one_shot_and_interactive_require_disabling_web() {
    rejects(&["--prompt", "Hello"], "--no-web");
    rejects(&["--interactive"], "--no-web");
    rejects(
        &["--no-web", "--prompt", "Hello", "--interactive"],
        "cannot be used",
    );
}
#[test]
fn listener_settings_conflict_with_no_web_and_invalid_addresses_fail_early() {
    rejects(&["--no-web", "--port", "8081"], "cannot be used");
    rejects(&["--no-web", "--host", "127.0.0.1"], "cannot be used");
    rejects(&["--host", "not-an-address"], "invalid value");
    rejects(&["--port", "65536"], "invalid value");
}
#[test]
fn help_describes_normal_server_and_testing_modes() {
    let output = Command::new(env!("CARGO_BIN_EXE_assistant"))
        .arg("--help")
        .output()
        .expect("help");
    assert!(output.status.success());
    let help = String::from_utf8(output.stdout).expect("UTF-8 help");
    for flag in [
        "--no-web",
        "--host",
        "--port",
        "--memory-dir",
        "--embedding-model",
        "--interactive",
        "Diagnostic/testing",
    ] {
        assert!(help.contains(flag), "{help}");
    }
    assert!(!help.contains("--memory-config"), "{help}");
}

#[test]
fn memory_flags_require_each_other_and_reject_incompatible_modes_before_loading() {
    rejects(&["--memory-dir", "private-memory"], "--embedding-model");
    rejects(&["--embedding-model", "e5.gguf"], "--memory-dir");
    rejects(&["--memory-config", "obsolete.json"], "unexpected argument");
    rejects(
        &[
            "--memory-dir",
            "private-memory",
            "--embedding-model",
            "e5.gguf",
            "--no-web",
            "--raw",
            "--prompt",
            "Hello",
        ],
        "cannot be used",
    );
    rejects(
        &[
            "--memory-dir",
            "private-memory",
            "--embedding-model",
            "e5.gguf",
            "--no-web",
            "--require-tool",
        ],
        "cannot be used",
    );
    rejects(
        &["--no-web", "--prompt", "Hello", "--request-id", "retry"],
        "--memory-dir",
    );
}

#[test]
fn pairing_command_is_removed() {
    rejects(&["pair"], "unexpected argument");
}
