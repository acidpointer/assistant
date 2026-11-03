# Local assistant

Rust workspace with local llama.cpp bindings and a tool-using assistant.

| Crate | Purpose |
| --- | --- |
| `assistant` | Annotated inference example with local tool calls and a final answer |
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

From the workspace root, using the existing model filename:

```sh
cargo run --release -- --prompt "What time is it?" --trace
```

Or choose another model and token budget:

```sh
cargo run --release -- --model /path/to/model.gguf --ctx-size 4096 --max-tokens 512
```

For a CPU-only build without the Vulkan SDK:

```sh
cargo run --release --no-default-features -- --gpu-layers 0 --prompt "Hello!"
```

Other GPU backends can be selected with `--no-default-features --features cuda`
or `--no-default-features --features metal`. Each needs its platform toolkit.
Use `--help` for all options. `--thinking` enables reasoning in supported chat
templates; `--raw` bypasses chat formatting and tools for completion models.

## Learn how tools work

[crates/assistant/src/main.rs](crates/assistant/src/main.rs) is the complete
"learn in ~20 minutes" walkthrough. It explains model/context lifetimes,
tokenization, batches, logits, sampling, grammar constraints, chat templates,
tool-call parsing, argument validation, execution, and conversation history.
All application/tool code is in that file; there is no agent framework.

The two built-in tools are:

| Tool | Implementation | Example question |
| --- | --- | --- |
| `current_time` | Runs the system `date` command with no arguments | What time is it? |
| `calculate` | Checked signed 64-bit addition, subtraction, and multiplication in Rust | What is 123 multiplied by 456? |

```sh
cargo run -- --prompt "What time is it?" --trace
cargo run -- --prompt "What is 123 multiplied by 456?" --trace
cargo run -- --prompt "What time is it, and what is 19 times 23?" --trace
cargo run -- --prompt "What time is it?" --require-tool --trace
cargo run -- --prompt "Explain what a token is." --no-tools
```

The model chooses a function and arguments; Rust validates and executes it,
appends a `role: tool` result linked to the assistant request, and runs the
model again. It repeats until there is a final answer. Tool requests and exact
results appear on stderr. `--trace` also shows prompts, raw model protocol text,
and normalized assistant messages. The parsed final answer goes to stdout;
raw completion mode streams text instead. `--verbose` enables native logs.

Tool schemas become llama.cpp grammar constraints, and typed Rust arguments
validate requests again before execution. The host accepts only the listed
functions. Grammar constrains format; it does not prove the model interpreted
the question correctly. `--require-tool` forces a call on the first turn and
is useful for studying the difference between automatic and required choice.

To add your own tool, follow `calculate`: add its definition in
`tool_definitions()`, define a typed argument struct, implement a Rust function,
and register its exact name in `execute_tool()`. Add an argument-validation test
and a model conversation test. The model never supplies executable shell text.

Each model turn gets its own `--max-tokens` budget (default 512).
`--max-tool-calls` bounds executed calls, including failures (default 4).
The complete history plus new output must fit `--ctx-size` (default 4096).
For clarity the example clears the KV cache and prefills the full history each
turn; the model weights stay loaded. Long prompts use batches of up to 512
tokens. A truncated tool-enabled turn is rejected without executing its calls;
increase the output/context budget when needed. Model files and build artifacts
are ignored by Git.

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

The assistant tests also exercise real LFM3 tool calls when the default local
model file is present. Alternatively set `ASSISTANT_TEST_MODEL` to an absolute
GGUF path for a model with a compatible tool-capable chat template:

```sh
cargo test -p assistant -- --test-threads=1
```

They check arithmetic, exact `date` output, and a conversation needing both
tools. These tests use CPU inference and do not download model weights.

## Upstream sources

The Rust crates are ported from
[eugenehp/llama-cpp-rs](https://github.com/eugenehp/llama-cpp-rs/commit/1d62841020af8a309ab749b64ac3688919f383df)
(version 0.9.0). Their MIT and Apache-2.0 licenses are retained in each crate.

llama.cpp is pinned to
[`a34de8618b25dfc32ee43749db47d767a9f0a25d`](https://github.com/ggml-org/llama.cpp/commit/a34de8618b25dfc32ee43749db47d767a9f0a25d)
(`b12015`), upstream HEAD on October 30, 2026, including the LFM3 architecture.
The sys crate applies four binding patches for speculative state, decode
lifecycle, context-local rotation, and native logging safety to a build-directory
copy; the submodule stays unmodified. The runtime-safety patch appends a context
parameter and adds a required ABI guard. See [binding compatibility notes](docs/bindings-fork.md#ownership-allocation-and-api-compatibility-2026-11-03)
for the fallible batch/ggml APIs and lifetime-bound native handles.
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
