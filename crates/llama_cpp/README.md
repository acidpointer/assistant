# llama_cpp

Local safe Rust bindings to llama.cpp, ported from
[eugenehp/llama-cpp-rs](https://github.com/eugenehp/llama-cpp-rs)
revision `1d62841020af8a309ab749b64ac3688919f383df` (0.9.0).
See the [workspace README](../../README.md) for the native revision and setup.

```rust
use llama_cpp::prelude::*;

fn load() -> Result<(), Box<dyn std::error::Error>> {
    let backend = LlamaBackend::init()?;
    let model = LlamaModel::load_from_file(
        &backend,
        "model.gguf",
        &LlamaModelParams::default(),
    )?;
    let tokens = model.str_to_token("Hello!", AddBos::Always)?;
    println!("{} tokens", tokens.len());
    Ok(())
}
```

For a full chat-template, batching, sampling, and streamed detokenization example,
see [the standalone assistant source](../assistant/src/example.rs) (currently outside the executable target). The API includes model/context
management, sampler chains, chat templates, structured output, embeddings,
quantization, tensor capture, speculative decoding, native log callback
management, and optional multimodal/RPC support. Rustdoc contains detailed
examples: `cargo doc -p llama_cpp --no-deps`.

| Feature | Default | Purpose |
| --- | --- | --- |
| `openmp` | yes | CPU multithreading via OpenMP |
| `mtmd` | yes | Multimodal vision/audio bindings |
| `ggml` | yes | Rust tensor and graph API |
| `rpc` | yes | Remote compute backend |
| `vulkan` | no | Vulkan GPU backend |
| `cuda` | no | NVIDIA CUDA backend |
| `metal` | no | Apple Metal backend |
| `blas`, `opencl`, `hip`, `webgpu` | no | Additional native backends |
| `native` | no | CPU tuning for the build machine |
| `dynamic-link` | no | Build shared libraries instead of static libraries |
| `q1` | no | Compatibility no-op; Q1_0 is supported unconditionally |
| `turboquant` | no | Experimental PQ3 reference and restricted CPU/Vulkan cache inference; state I/O/position shifts unavailable |
| `prebuilt` | no | Falls back to source while the required patches are active |

Use `default-features = false` to select only the backends/APIs you need.
Native features are forwarded to the local `llama_cpp_sys` crate.
Dynamic linking requires configuring the runtime library search path on Linux;
static linking is the workspace default.

The original MIT and Apache-2.0 license texts are included in this directory.

## Safety contracts and migration

Backend handles have private constructors and can be cloned to share one
initialization lease. Every model retains its backend through native model
destruction. `QuantModelDesc::build`, `load_from_file_ptr`, and `init_from_user`
now require a backend argument.

`LlamaBatch::new(512, 1)?` and `LlamaBatch::get_one(tokens)?` are fallible.
Batch arrays are Rust-owned; capacity checks precede every token/membership
write. `BatchInitError` and `BatchAddError` distinguish invalid sizes,
allocation failures, and rejected additions.

Vocabulary, tensor, and graph handles carry their owner's lifetime. ggml graph
dependencies must share an arena. ggml constructors and metadata operations
return `GgmlError`, and tensor names are returned as owned strings. Backend
allocation/execution methods are unsafe because external tensor buffers must
remain initialized and alive through every later use of their native addresses.

The managed native logger supports concurrent installation and replacement;
callbacks run outside locks and Rust unwinding is contained. Rotation defaults
are atomic preferences for new Rust contexts and never rewrite the environment.
See [fork maintenance](../../docs/bindings-fork.md) for the mandatory native
patch, ABI guard, compatibility details, and regression-test contracts.

Typed cache settings live in `context::cache::KvCacheConfig` (FP16, Q8_0,
Q4_0; full/compact SWA; flash and rotation policy). Validate against
`LlamaModel::cache_topology()` before allocating. `LlamaContext::cache_info()`
returns effective arena geometry and rotation state; `try_memory_breakdown()`
returns complete allocation accounting or an error. Use these fallible reports
for diagnostics rather than the legacy best-effort collector. Context/compute
bytes are per context; weight bytes refer to shared model allocations.
`try_synchronize()` provides an exception-contained measurement boundary.
See the [cache baseline](../../docs/kv-cache-baseline.md) for limitations and
model/GPU validation coverage.

The experimental [PQ3 reference contract](../../docs/pq3-reference-contract.md)
freezes its native block/graph ABI, supported CPU/Vulkan inference and restrictions.
`cargo test -p llama_cpp --no-default-features --features turboquant --test test_pq3_reference`
exercises the codec without a model or GPU.


The [measured PQ3 candidate](../../docs/pq3-inference-evaluation.md) remains
opt-in: it fails the fixed retrieval/KL gates and repeats a near-full 4K
throughput regression versus Q4. AMD/Intel are unvalidated; QJL is deferred.

Embedding access checks last-successful output rows, pooling and output dimensions;
negative/unselected indices return errors. `get_embeddings` returns the first
selected token row; use `embeddings_seq_ith` for pooled output. Native tokenizer
exceptions return `StringToTokenError::Native` for tokenization/counting rather
than crossing FFI. `LlamaModelParams::append_kv_override` is now fallible and
supports multiple checked keys; callers must handle its `Result`.
