# Architecture

Status: early-stage snapshot. Last reviewed 2026-11-02.

This document describes the intended system shape and the constraints that the
application layer must respect. It is a guide for design and review, not a
substitute for reading the code. The application logic itself is human-owned;
see `../AGENTS.md` §2.

## 1. Product shape

The target is an on-device, long-running user daemon:

- one small model kept resident for low-latency local inference
- multiple local transports: HTTP, D-Bus, Unix socket, and possibly a CLI
- streaming chat responses, cancellation, and bounded resource use
- tool calling with host-side validation and execution
- session state and KV-cache reuse across turns
- no cloud dependency and no network requirement for inference

The workspace is not there yet. The binding fork is usable, while the
application and high-level LLM API are still being designed.

## 2. Workspace map

| Path | Role | Ownership |
| --- | --- | --- |
| `crates/llama_cpp_sys/` | FFI, build script, C++ shims, build-time patches, pinned `llama.cpp` submodule | Agent-maintained |
| `crates/llama_cpp/` | Safe Rust API over llama.cpp: model, context, sampling, chat, tools, grammar, speculative decoding, tensor access | Agent-maintained |
| `crates/assistant/` | High-level LLM wrapper, application logic, future daemon | Human-owned |
| `docs/` | Architecture, fork maintenance, decisions, security docs | Shared |

The two binding crates are a fork of `eugenehp/llama-cpp-rs` at revision
`1d62841020af8a309ab749b64ac3688919f383df` (0.9.0). Native source is pinned to
llama.cpp `a34de8618b25dfc32ee43749db47d767a9f0a25d` (`b12015`, upstream HEAD on
2026-10-30). See `bindings-fork.md` for the maintenance contract.

## 3. Current runtime path

As of the last audit:

```text
cargo run -p assistant
    -> crates/assistant/src/main.rs
        -> mod llm;          (compiled, but never called)
        -> fn main() { Ok(()) }

crates/assistant/src/example.rs   (746 lines, not declared as a module)
```

Consequences:

- `cargo run` exits successfully and prints nothing.
- The working tool-calling example is not part of any target.
- `crates/assistant/tests/inference.rs` spawns `CARGO_BIN_EXE_assistant` with
  CLI arguments, so its model-backed tests fail at the first missing log line.
- The README still describes the old CLI, including `--prompt`, `--trace`,
  `--raw`, and `--require-tool`.

This is a baseline regression, not a model-quality result. Restore a runnable
binary before treating assistant test failures as meaningful.

## 4. Known baseline issues

| Area | Issue | Evidence |
| --- | --- | --- |
| Entry point | Binary is a no-op; walkthrough is orphaned | `crates/assistant/src/main.rs`, `src/example.rs` |
| Tests | 4 of 5 assistant inference tests fail because the binary ignores arguments | `cargo test -p assistant --test inference` |
| Build features | `llama_cpp/vulkan` is enabled unconditionally on the dependency, so `--no-default-features` and `--features metal` still build Vulkan | `crates/assistant/Cargo.toml:12` |
| Docs | README run instructions and the `main.rs` walkthrough link are stale | `README.md:28-68` |
| Git | Repository has no commits; only `.gitmodules` and the submodule gitlink are staged | `git log`, `git status` |
| Formatting | `cargo fmt --all --check` fails on `main.rs` whitespace | `cargo fmt` |

The binding suite covers unit, chat/context/sampler, and model-backed tests.
Binding safety changes and API migration notes are recorded in
[`bindings-fork.md`](bindings-fork.md#ownership-allocation-and-api-compatibility-2026-11-03).
Passing tests are not a substitute for the documented FFI ownership contracts.

## 5. High-level LLM API state

`crates/assistant/src/llm/` currently contains a compiling skeleton:

- `LLMModel` owns `LlamaBackend`, `LlamaModel`, and model parameters.
- `LLMContext` owns a `LlamaContext` plus a borrow of `LLMModel`.
- `generate` performs prefill, greedy sampling, and detokenization.

It is not usable as an API yet:

| Problem | Impact |
| --- | --- |
| `generate` is private; `Generation` fields have no getters | No caller can generate or read a result |
| `LLMResult`/`LLMError` are `pub(crate)` but appear in `pub` signatures | Cannot become a public library API as-is |
| No `max_tokens`; `FinishReason::TokenLimit` is unreachable | Generation can run until EOG or a decode failure |
| No context-budget check | Overflow aborts decode and loses the generated text |
| No streaming or cancellation | Unusable for interactive local clients |
| Always clears the KV cache and re-prefills | Full history is recomputed every turn |
| Greedy sampling only | No temperature, top-k/top-p, penalties, or reasoning budget |
| No chat/tool orchestration | Caller must rebuild what `example.rs` already demonstrates |
| `LLMModel` stores `LlamaModelParams` by value | Raw pointers make `LLMModel` `!Send`, blocking worker-thread handoff |
| `LlamaBackend::init` is process-global | One live backend per process; per-model backends prevent hot-swap |
| `LLMContext::new` accepts any model/context pair | Tokenization and decoding can target different models |

These are design constraints for the next iteration, not a request to implement
the application layer on the human's behalf.

## 6. Target daemon shape

Start simple; add transports only after the engine contract is stable.

```text
 HTTP / D-Bus / Unix socket
          |
          v
 transport adapters  (thin, protocol-specific)
          |
          v
 request router + session store + cancellation
          |
          v
 inference worker thread
   - one resident model
   - one LlamaContext per active session
   - token budget, streaming sink, stop handling
          |
          v
 llama_cpp -> llama.cpp
```

Recommended properties:

- **Single inference worker first.** `LlamaContext` is `!Send`, and KV memory
  is the limiting resource. A queue plus one worker is easier to reason about
  than a context pool.
- **Tokio for I/O, blocking thread for inference.** Do not run llama.cpp
  decoding on an async executor thread.
- **One engine per process.** Own the `LlamaBackend` at engine level. Do not
  create a backend per model load.
- **Session state is explicit.** Each session records history and its current
  KV length so only new tokens are decoded.
- **Streaming and cancellation are part of the engine contract.** They are not
  transport features bolted on later.
- **Limits are security controls.** Bound prompt tokens, output tokens, queue
  depth, tool calls, and wall-clock time.

## 7. Invariants the engine should preserve

These are recommendations for the human-owned API; agents should review against
them rather than implement them unasked.

1. A request's prompt plus reserved output must fit `n_ctx` before decoding
   starts. The error must identify the budget and the overflow.
2. Every generation ends with an explicit reason: EOG, stop sequence, token
   limit, cancellation, or error.
3. A cancelled or truncated generation still returns the text produced so far,
   unless the caller explicitly discards it.
4. Chat rendering, tokenization, grammar creation, and output parsing all come
   from the same `ChatParams`; do not mix templates and parsers.
5. Tool arguments are validated by the host. Grammar constrains syntax, not
   intent or safety.
6. A session's KV cache is reused only for a verified common prefix. After a
   history rewrite or divergence, clear from the divergence point.
7. No `unwrap`/`expect` on request paths; all failures cross the API boundary as
   typed errors.
8. Inference state is thread-confined. Shared model weights are `Arc`-shared;
   contexts are not shared across threads.

## 8. Threading and lifetime constraints

- `LlamaModel` is explicitly `Send + Sync` (`crates/llama_cpp/src/model.rs`).
  Weights can be shared between workers.
- `LlamaContext` owns a `NonNull` native handle and is not `Send`. Pin it to one
  inference thread.
- `LlamaBackend` is process-global: `init` fails while any handle/model retains
  the current initialization lease. Cloning shares that lease; final shutdown
  finishes before initialization is permitted again. Every model retains a lease
  through native model destruction. Own one engine-level handle for the daemon.
- `LLMModel` is currently `!Send` because it retains `LlamaModelParams`, which
  contains raw pointers. Remove or encapsulate that field before relying on
  moving the engine between threads.
- `ChatTemplates` and `ChatParser` are `Send + Sync`; they can be shared.
- `CommonSampler` is `Send` but not `Sync`; give each session its own sampler.
- Tensor/decode callbacks require pinned callback state and strict drop ordering.
  See `../crates/llama_cpp/TENSOR_TRANSACTIONS_SAFETY.md`.

## 9. Open decisions

These need an explicit choice before daemon work expands:

1. Concurrency: single worker, small context pool, or multi-process.
2. HTTP protocol: OpenAI-compatible or a custom streaming JSON protocol.
3. D-Bus shape: service object with signals, or a thin RPC mapping.
4. Session persistence: in-memory only first, or durable from day one.
5. Model lifecycle: one resident model, hot-swap, or multiple profiles.
6. Tool execution: in-process executor vs. separate sandboxed worker.
7. Platform priority: Linux CPU/Vulkan first, or cross-platform from the start.

Record significant choices as ADRs in `docs/decisions/` and add them to
`docs-map.md`.

## 10. Keeping this document honest

Update this file when the entry point, crate boundaries, threading model, or
known baseline changes. If a claim here conflicts with the code, the code wins;
fix the document in the same change.
