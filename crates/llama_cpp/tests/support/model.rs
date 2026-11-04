//! Locate and load GGUF files for integration tests.
//!
//! Several test binaries declare `mod support;` and each uses only a subset of
//! these helpers, so unused items here are expected rather than dead.
#![allow(dead_code)]

use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

use llama_cpp::llama_backend::LlamaBackend;
use llama_cpp::model::params::LlamaModelParams;
use llama_cpp::model::LlamaModel;

/// Serialize tests that share mutable native configuration or fixture state.
/// The maintained logging patch contains fitting callbacks on their thread;
/// this lock does not substitute for individual native ownership contracts.
pub static LLAMA_LOCK: Mutex<()> = Mutex::new(());

/// Acquire the global llama.cpp test lock, recovering from a poisoned mutex
/// after a prior test panic.
pub fn llama_guard() -> std::sync::MutexGuard<'static, ()> {
    LLAMA_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

static BACKEND: OnceLock<LlamaBackend> = OnceLock::new();

/// Default tiny model used in CI (`ggml-org/models` / llama.cpp server tests).
pub const DEFAULT_TEST_MODEL: &str = "stories260K.gguf";

/// Process-wide backend initialisation (llama.cpp allows one init per process).
pub fn backend() -> &'static LlamaBackend {
    BACKEND.get_or_init(|| LlamaBackend::init().expect("initialize the process test backend"))
}

/// Resolved on-disk model file.
#[derive(Debug, Clone)]
pub struct ModelFixture {
    pub path: PathBuf,
    /// `true` when only tokenizer weights are present (no inference).
    pub vocab_only: bool,
}

/// Find a GGUF for tests. See module docs for search order.
pub fn find_test_model() -> Option<ModelFixture> {
    if let Some(path) = std::env::var_os("LLAMA_TEST_MODEL") {
        let p = PathBuf::from(path);
        if p.is_file() {
            return Some(ModelFixture {
                path: p,
                vocab_only: false,
            });
        }
        panic!("LLAMA_TEST_MODEL must name an existing GGUF file");
    }

    if let Some(path) = default_cached_test_model() {
        if path.is_file() {
            return Some(ModelFixture {
                path,
                vocab_only: false,
            });
        }
    }

    assert!(
        std::env::var_os("LLAMA_TEST_REQUIRE_MODEL").is_none(),
        "full-model coverage requires LLAMA_TEST_MODEL or the cached tiny model"
    );
    vocab_only_fixture()
}

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn default_cached_test_model() -> Option<PathBuf> {
    let path = workspace_root()
        .join("target/test-models")
        .join(DEFAULT_TEST_MODEL);
    path.is_file().then_some(path)
}

fn vocab_only_fixture() -> Option<ModelFixture> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../llama_cpp_sys/llama.cpp/models/ggml-vocab-llama-bpe.gguf");
    path.is_file().then_some(ModelFixture {
        path,
        vocab_only: true,
    })
}

/// Load a model for tests. Returns `(model, vocab_only)`.
pub fn load_model() -> Option<(LlamaModel, bool)> {
    let fixture = find_test_model()?;
    let mut params = LlamaModelParams::default();
    if fixture.vocab_only {
        params = params.with_vocab_only(true);
    }
    let params = std::pin::pin!(params);
    let model = LlamaModel::load_from_file(backend(), &fixture.path, &params)
        .expect("the selected GGUF fixture must load; load failures are not skips");
    Some((model, fixture.vocab_only))
}

/// Load a full (non-vocab-only) checkpoint, or `None` when unavailable.
pub fn load_full_model() -> Option<LlamaModel> {
    let (model, vocab_only) = load_model()?;
    if vocab_only {
        eprintln!("SKIP: full model required (set LLAMA_TEST_MODEL)");
        return None;
    }
    Some(model)
}

/// Path to the loaded test model, if any.
pub fn test_model_path() -> Option<PathBuf> {
    find_test_model().map(|f| f.path)
}

/// Skip helper — prints guidance when no model is available.
pub fn skip_no_model() {
    eprintln!("SKIP: no test model available");
    eprintln!("  export LLAMA_TEST_MODEL=/path/to/model.gguf");
}
