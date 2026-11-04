# Local assistant

Rust workspace with local llama.cpp bindings and a tool-using assistant.

| Crate | Purpose |
| --- | --- |
| `assistant` | Persistent runtime, high-level chat API, typed tools and CLI |
| `llama_cpp` | Safe Rust API, ported from eugenehp/llama-cpp-rs |
| `llama_cpp_sys` | Generated FFI, native build, and C++ shims |

## Build and run

Initialize the pinned native source after cloning:

```sh
git submodule update --init --recursive
```

Build prerequisites: Rust 1.88+, a C/C++ compiler, CMake, libclang for bindgen,
and OpenMP. Ninja is used when available. The assistant uses Vulkan by default;
install Vulkan headers, the Vulkan loader, a GPU driver, and `glslc` as well.
On Arch Linux, these are provided by `base-devel cmake ninja clang vulkan-headers
vulkan-icd-loader shaderc` and your GPU's Vulkan driver package.

Choose a local GGUF explicitly; model paths are never committed:

```sh
cargo run --release -- --model /path/to/model.gguf --prompt "What time is it?" --trace
```

Without `--prompt`, the CLI reads interactions from stdin until EOF, sharing one
conversation and resident model. `--daemon` starts the runtime and waits for
Ctrl-C; external transports are future work.

```sh
cargo run --release -- --model /path/to/model.gguf
cargo run --release -- --model /path/to/model.gguf --daemon
cargo run --release --no-default-features -- --model /path/to/model.gguf --gpu-layers 0 --prompt "Hello!"
```

Other GPU backends use `--no-default-features --features cuda` or
`--no-default-features --features metal` and their platform toolkits. Disabling
default features now actually disables Vulkan. `--thinking` enables supported
template reasoning; `--raw` bypasses chat/tools for diagnostic completion.
`--ctx-size`, `--batch-size`, `--max-tokens` and `--threads` control native budgets.
`--timeout-seconds` sets the per-interaction deadline (default 120 seconds).
Cache controls include `--cache-type-k`, `--cache-type-v`, `--swa-cache`,
`--flash-attn`, `--attn-rotation`, and `--ubatch-size`. Defaults remain FP16/full
SWA. `--diagnostics` prints native allocation/settings JSON to stderr; `--cpu`
also disables attention/operator offloading. See the
[cache baseline](docs/kv-cache-baseline.md) for contracts, measurements, and
the repeatable benchmark/placement workflow.

## Library and extensions

[The API guide](docs/llm-api.md) describes `Chat::generate`, request overrides,
streaming, cancellation, typed tools, plugins, ordered hooks and middleware.
[The architecture](docs/architecture.md) explains ownership and working context.
The binary is a thin adapter over this library. `src/example.rs` is a retained
historical walkthrough and is not compiled.

The CLI opts into two example tools:

| Tool | Implementation |
| --- | --- |
| `current_time` | Fixed system `date` command with no model-supplied arguments |
| `calculate` | Checked signed 64-bit arithmetic, including integer division |

The model requests registered functions and structured arguments. Rust validates
and executes them, appends correlated results and continues until a final answer
or a configured bound. `--trace` prints exact tool requests/results to stderr;
user-visible text streams to stdout. Prompt bodies and raw protocol are not logged.
`--no-tools` disables capabilities. `--require-tool` requires a first-turn call.

The runtime stays resident between interactions and blocks while idle. Recent
history is bounded; oldest complete exchanges are evicted to fit message, byte
and native token budgets. It does not summarize or persist memory. Weights and
allocated KV caches still occupy memory while idle. External transports,
semantic/graph memory and voice remain separate future features.

## Checks

```sh
cargo fmt --all --check
cargo check --workspace
cargo test --workspace
```

The binding tests automatically use the tokenizer fixture in the native source.
For full inference coverage, place the upstream `stories260K.gguf` test model
in `target/test-models/`, or set `LLAMA_TEST_MODEL` to an absolute GGUF path.
Tests requiring a full model skip when one is unavailable.

Live assistant tool-choice evaluations require `ASSISTANT_TEST_MODEL`, an
absolute GGUF path with a compatible tool-capable chat template. Set
`ASSISTANT_TEST_REQUIRE_MODEL=1` to fail rather than skip when absent:

```sh
cargo test -p assistant -- --test-threads=1
```

They check arithmetic, exact `date` output, and a conversation needing both
tools. `ASSISTANT_TEST_GPU_LAYERS` selects weight offloading (default zero).
Cache evaluations use separate required-model/GPU flags described in the
[baseline guide](docs/kv-cache-baseline.md#test-categories-and-reproduction).
Tests do not download model weights.

## Upstream sources

The Rust crates are ported from
[eugenehp/llama-cpp-rs](https://github.com/eugenehp/llama-cpp-rs/commit/1d62841020af8a309ab749b64ac3688919f383df)
(version 0.9.0). Their MIT and Apache-2.0 licenses are retained in each crate.

llama.cpp is pinned to
[`a34de8618b25dfc32ee43749db47d767a9f0a25d`](https://github.com/ggml-org/llama.cpp/commit/a34de8618b25dfc32ee43749db47d767a9f0a25d)
(`b12015`), upstream HEAD on October 30, 2026, including the LFM3 architecture.
The sys crate always applies five binding patches for speculative state, decode
lifecycle, context-local rotation, native logging safety, and Vulkan resource
lifetime to a build-directory copy; the submodule stays unmodified. The runtime-safety patch appends a context
parameter and adds a required ABI guard. See [binding compatibility notes](docs/bindings-fork.md#ownership-allocation-and-api-compatibility-2026-11-03)
for the fallible batch/ggml APIs and lifetime-bound native handles.
The Vulkan patch retires unused devices before driver finalization and preserves
buffer-type descriptors across cache recreation; see the
[Vulkan lifetime contract](docs/bindings-fork.md#vulkan-resource-lifetime).
The local port also adapts the chat capabilities shim to upstream's JSON API
and keeps quantization preview parameters alive for the native state's lifetime.

The local crate names are `llama_cpp` and `llama_cpp_sys`. Linking is static by
default; `llama_cpp/dynamic-link` remains available explicitly. The assistant
enables only the binding features it needs. The library's own defaults also
include multimodal, ggml, and RPC APIs.

To update the native revision deliberately:

```sh
git -C crates/llama_cpp_sys/llama.cpp fetch origin master
git -C crates/llama_cpp_sys/llama.cpp checkout --detach origin/master
cargo test --workspace
git add crates/llama_cpp_sys/llama.cpp
```

Update the revision above and adapt the shims or patches if upstream changes
their APIs. Builds use the pinned source and do not fetch llama.cpp themselves.

The opt-in `turboquant` feature adds ordered native patches
[0008](crates/llama_cpp_sys/patches/0008-pq3-cpu-format.patch),
[0009](crates/llama_cpp_sys/patches/0009-pq3-kv-graph-state.patch), and
[0010](crates/llama_cpp_sys/patches/0010-pq3-vulkan-cache-attention.patch).
It enables experimental matched `pq3_0` K/V on restricted Llama/LFM3 CPU or
Vulkan contexts. Defaults remain FP16/full SWA. State I/O and positional shifts
are unavailable for PQ3; see the [format/support contract](docs/pq3-reference-contract.md)
and [fixed quality jobs](docs/cache-quality-evaluation.md). The
[measured candidate](docs/pq3-inference-evaluation.md) fails quality gates and
regresses near-full 4K speed; it remains opt-in.

```sh
cargo run --features turboquant -- --model ./model.gguf \
  --cache-type-k pq3_0 --cache-type-v pq3_0 --swa-cache compact --require-vulkan
```
