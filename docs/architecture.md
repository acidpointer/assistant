# Architecture

Status: resident runtime and default web plugin, 2026-11-04. Application behavior is human-owned;
this foundation was explicitly requested. See [the API contract](llm-api.md)
and [the runtime decision](decisions/0001-persistent-runtime.md).

## Product shape

`assistant` is an on-device, long-running assistant. A persistent runtime owns
resident model weights and extension services. A completed interaction returns a
response while the runtime stays online. Idle inference workers block on bounded
channels; no inference or periodic model polling runs while idle.

One primary `Chat` represents the user's ongoing conversation. Additional chats
are optional independent working states. Permanent memory will be a separate
service, with retrieval and knowledge extraction integrated through hooks. Recent
messages and native KV state are disposable working memory; neither is a durable
archive. Summarization is not required for continued operation.

## Workspace boundaries

| Path | Responsibility |
| --- | --- |
| `crates/assistant/src/engine/` | Backend singleton, loaded model defaults, bounded prepared input and resumable generation, native cache validity |
| `crates/assistant/src/runtime/` | Service lifecycle, admission limits, resident worker threads and native context ownership |
| `crates/assistant/src/chat/` | Typed messages, operation overrides, semantic events, bounded model/tool runner and recent working history |
| `crates/assistant/src/tools/` | Typed schemas, argument deserialization and registered Rust dispatch; opt-in CLI tools |
| `crates/assistant/src/pipeline/` | Plugin configuration, typed services, ordered hooks and one-shot middleware continuations |
| `crates/assistant/src/application/` | Bounded product admission, run ownership, display snapshots and subscriptions |
| `crates/assistant-web/` | Runtime-ready HTTP/WebUI plugin; verified embedded React/TypeScript assets |
| `crates/assistant-daemon/` | Single `assistant` executable, CLI modes, signals and supervision |
| `crates/llama_cpp/` | Maintained safe binding fork |
| `crates/llama_cpp_sys/` | FFI, guarded shims, build-directory patches and pinned native submodule |

The library is the sole owner of engine statics. The binary imports it rather
than compiling another copy of the engine modules. `src/example.rs` is a retained
historical walkthrough, outside the build; current behavior is defined by the
library and CLI.

## Ownership and execution

```text
WebPlugin / future transports
          |
       AssistantHandle (bounded application coordinator)
          |
       Chat::request(...).run()
          |
 admission + conversation lock + deadline/cancellation
          |
 run middleware -> hooks -> bounded model/tool loop
          |                         |
 model middleware             tool middleware
          |                         |
 bounded worker channel       selected, typed Rust handler
          |
 dedicated model thread
   ModelEngine -> LLMModel -> native contexts and generation cursors
          |
 llama_cpp -> guarded shims -> pinned llama.cpp
```

The process-wide `ModelEngine` singleton is deliberate. Models borrow the engine;
contexts and cursors borrow their model. No native context is moved across
threads, and no unsafe `Send`/`Sync` workaround is used. Each model worker loads
its weights locally and owns the contexts borrowing them. One scheduling step is
one prefill batch or one sampled token. Active chats on a worker rotate between
steps. Native calls themselves cannot be preempted.

The default product is a foreground daemon with WebUI. `--no-web` skips the web
plugin entirely; explicit diagnostic `--prompt` and `--interactive` require it.
See [the web contract](web-ui.md) and [runtime-service decision](decisions/0003-runtime-services-and-web.md).

Async plugin services and tools run on Tokio. Registration does no I/O. Startup
starts dependency services, prepares runtime services, starts model workers and
the application coordinator, then activates runtime services. Failure rolls
back successfully started resources and retains rollback diagnostics. Shutdown
closes admission, quiesces runtime services, finalizes runs, closes/joins runtime
services, joins native workers, then stops dependency services in reverse order. Explicit shutdown is required to await flushing; dropping the
runtime only requests stop.

## Context and resource invariants

- Before native mutation, rendered input plus the reserved output must fit the
  actual single-sequence capacity. An oversized current interaction fails.
- Recent history is bounded by messages and payload bytes. Old complete user
  exchanges are removed first; native rendering additionally removes old complete
  exchanges until the token budget fits. System instructions, supporting context,
  the newest user input and its tool continuation remain intact.
- The retained recent transcript can be longer than the current native prompt.
  Clearing it is explicit. No automatic summary or permanent fact extraction runs.
- KV reuse requires that all successfully evaluated cached tokens are an exact
  prefix of the new prepared input. Divergence rebuilds the context. Native errors
  invalidate reuse. A cursor cannot advance after another generation or reset.
- Chat rendering, tokens, grammar, stop metadata and output parser belong to the
  same prepared render. Raw completion is an explicit diagnostic path.
- Queues, active requests, native contexts, model turns, tool calls, tokens, bytes
  and interaction duration have limits. Slow output consumers backpressure their
  native stream without blocking unrelated native contexts.
- Tools run only after complete parsing and typed deserialization. Model output
  selects registered names and data, never executable code or shell commands.
- Tool receipts survive delivery failures and cancellation. Interrupted calls get
  explicit unknown-outcome records; neither retries nor rollback are implicit.

## Extension boundaries

Plugins configure tools, service handles, lifecycle operations, hooks and
middleware. Named hooks are topologically sorted at build time within fixed
stages. Missing dependencies, missing ordering targets, duplicates and cycles are
configuration errors. Middleware wraps a whole run, model turn or tool call;
its continuation is consumed, preventing accidental double invocation.

Extensions are trusted Rust host code. Core bounds are checked after their input
modifications at execution boundaries. They must remain cooperative and avoid
blocking Tokio threads. Text already streamed cannot be retracted by output
hooks. Terminal observers have a one-second allowance each; their failures become
warnings because completed tool effects must not be replayed.

Supporting context is literal model input, distinct from trusted system policy.
This separation does not make retrieved data immune to prompt injection. Host
capability selection and typed validation remain necessary.

## Frontend and measurement boundary

The compiled web plugin serves a prepared FSD frontend. Oxc-based tooling builds
local assets that Cargo verifies and embeds. Rendering, connection lifecycle,
chat state and native/process accounting remain separate. The application actor
owns bounded per-turn metrics and one demand-driven process sampler; browsers
never infer native tokens or daemon CPU/memory from display text. See
[the measurement contract](web-ui.md#output-limits-and-measurements) and
[ADR 0004](decisions/0004-typed-frontend-and-measurements.md).

## Deliberate limits

Inference currently uses greedy sampling. There is no memory database, restart
recovery, voice pipeline, additional transport implementations, scheduler for autonomous background
model work, process sandbox for tools or GPU model unloading policy. Resident
weights and allocated KV caches consume memory while idle. Native load/decode
cannot be forcibly cancelled; shutdown waits for native owners before freeing
resources. See [binding maintenance](bindings-fork.md) for native safety contracts.
