# Assistant API foundation

Status: implemented, 2026-11-03. The simple API and its extension points share
one execution path. There is no permanent memory implementation yet.

## Basic use

```rust,no_run
use assistant::{AssistantRuntime, engine::{ModelEngine, LLMConfig}};

# async fn example() -> Result<(), Box<dyn std::error::Error>> {
ModelEngine::init()?;
let engine = ModelEngine::get_instance()?;
let runtime = AssistantRuntime::builder(engine)
    .model("chat", LLMConfig::new("/path/to/model.gguf"))
    .system("You are a helpful local assistant.")
    .build()?
    .start().await?;

let chat = runtime.chat();
let response = chat.generate("Hello!").await?;
println!("{}", response.text());

// The same conversation and resident weights remain available here.
let response = chat.request("Explain that briefly")
    .max_new_tokens(128)
    .tools(Vec::<String>::new())
    .context("Supporting data for this interaction only")
    .on_event(|event| { println!("{event:?}"); Ok(()) })
    .run().await?;
println!("{}", response.text());
runtime.shutdown().await?;
# Ok(()) }
```

Run inside Tokio. Initialization is explicit; repeated `ModelEngine::init` is an
error. The engine singleton lives until process exit. Configuration validates
native integer bounds and stores portable values; raw native parameters are
local to model loading. Model defaults include context/batch size, threads,
GPU layers, output allowance and supported template thinking. Request options
can override output allowance, thinking, tools, deadline and cancellation.
Greedy sampling is currently intentional; a sampler configuration API is deferred.

`Chat` clones share one serialized working conversation. `create_chat(model)`
creates independent state on resident weights, subject to native context limits.
`set_system`, `messages` and `clear_working_history` allow explicit inspection and
management. Request supporting context is retained through tool continuations
but does not become permanent chat history or trusted system policy.

## Product application and runtime services

The daemon registers `assistant_web::WebPlugin` by default. `--no-web` omits it;
`--prompt` is an explicit testing/diagnostic operation requiring `--no-web`.
Normal transports use `RunningAssistant::assistant()` and its shared
`AssistantHandle` for submission, snapshots, subscriptions, inspection and
cancellation. Low-level `Chat` remains available for direct library/diagnostic
use; those direct mutations do not update the application display view.

Plugins register runtime-ready factories through
`PluginRegistrar::runtime_service`. See `runtime::services` for preparation,
activation, quiescence, termination observation and shutdown contracts. The
runtime keeps these handles outside `Pipeline`, rolls back startup failures and
supervises unexpected termination through `wait_for_failure`. Existing dependency
services retain their pre-model lifecycle. Model-turn boundary events now identify
separate assistant messages across tool continuations.

See [web and lifecycle contracts](web-ui.md) and [the HTTP specification](web-api.yaml).

## Tools

A capability implements `tools::Tool`, declaring its stable name, argument type,
output type and error type. Arguments derive `serde::Deserialize` and
`schemars::JsonSchema`; use `#[serde(deny_unknown_fields)]` and validate domain
constraints inside the handler before effects. `ToolContext` supplies the host
invocation ID and cooperative cancellation. Register with `.tool(tool)` or a
plugin's `registrar.add_tool(tool)`.

Schemas are derived and cached once at registration. Selection refers only to
registered names. Dispatch rechecks selection and deserializes the actual JSON.
Bad arguments, unknown/disallowed names and domain failures produce structured
failed results. The model never chooses Rust code, executable paths or shell
text. `tools::builtin::{Calculator, CurrentTime}` are opt-in CLI examples, not
capabilities automatically installed by the library.

The runner parses a complete assistant message, dispatches bounded tool calls,
appends correlated results and evaluates again. It rejects duplicate IDs and
truncated tool requests. All tools are sequential initially. `ToolUse::RequireFirst`
requires a call on the first evaluation only; later turns may finish normally.
Output format and argument typing do not establish authorization: hosts must
choose the appropriate installed tools and per-request selection.

## Plugins, hooks and middleware

Implement `pipeline::Plugin::configure` to install a package of capabilities.
Registration performs no service or model I/O. Duplicate names/providers and
unresolved required service types fail at `build`. Register shared `Arc<T>`
handles with `provide`, declare dependencies with `require`, and register long-lived
startup/shutdown with `lifecycle`. Services own and stop their own background tasks.
They must clean up a partially failed or cancelled start.

Four stages have fixed global ordering:

1. `before_run`: once per interaction; choose tools or retrieve supporting data.
2. `before_model`: transform the draft before each evaluation.
3. `after_model`: inspect/transform fully parsed output before tool dispatch.
4. `on_run_finished`: observe the terminal response, including failures/receipts.

Hooks are async closures returning `BoxFuture`. Each has a unique name and may
specify `.before("name")` / `.after("name")` within its stage. Build validates
missing targets, cycles and cross-stage constraints. `pipeline().stages()` exposes
the resolved order. With no explicit constraints, registration order is stable.

`RunLayer`, `ModelLayer` and `ToolLayer` provide Express-style wrapping around a
run, a model evaluation or a tool invocation. They receive a consumed `Next*`
continuation. Registration order is outermost first. Layers may short-circuit or
await the next layer once and inspect its result; there is no automatic retry.
Core execution limits and capability checks run after draft modifications.
Plugins are trusted host code and must not block the async executor.

A future memory plugin can retrieve into `RunContext.context` before a run and
extract facts from `ChatResponse.messages` after completion. Those messages are
the current interaction, including its tool receipts, rather than an unbounded
archive. Memory persistence, retrieval correctness and extraction acknowledgments
belong to that service. Terminal observers receive at most one attempt; errors
and their one-second per-hook timeout are response warnings, never an automatic
reason to replay tools.

## Working context

Model-level cache configuration, microbatch resolution, owned worker diagnostics,
and the measured compact-SWA/Q8/Q4 baseline are documented in the
[cache guide](kv-cache-baseline.md). Defaults remain FP16/full SWA. Runtime
diagnostics inspect existing contexts on their owner threads without allocating
a new context or including prompt/model paths.

Three distinct states exist:

| State | Owner | Lifetime |
| --- | --- | --- |
| Recent typed messages and system prompt | `Chat` | Bounded conversation lifetime |
| Rendered tokens and KV state | Native worker context | Disposable inference optimization |
| Durable facts/semantic graph | Future memory service | Independent of chats/native contexts |

The host keeps a bounded recent window. Before each run it drops oldest complete
exchanges to meet message/payload bounds. Before decoding, the native worker renders
and tokenizes, reserves the requested output and drops oldest complete exchanges
until it fits. The newest user input and all its tool continuations are retained.
System instructions and request supporting context are not silently shortened.
If that minimum prompt cannot fit, the operation returns `EngineError::Budget`
without overflowing the native context. The recent transcript may contain more
messages than a particular native prompt. No running summary is produced.

KV reuse requires a verified evaluated-token prefix and usable final logits.
Any rewrite/divergence uses a full rebuild. This conservative policy works without
assuming arbitrary KV removal is valid for every model architecture. A native
failure invalidates bookkeeping and the next generation rebuilds. Prepared raw
or chat input stays model-bound; chat render, grammar, stops and parser are coherent.
Low-level `LLMContext::generate` remains available for engine tests and diagnostics.

## Events, cancellation and failures

Events distinguish text, reasoning, complete tool requests/results and terminal
status. Raw protocol markers and incomplete tool JSON are not user-visible deltas.
Delivered text cannot be retracted by an output hook. Callbacks are synchronous,
fallible and should return quickly; transports should enqueue bounded output
and return a delivery error if they cannot accept it. Worker event channels are
bounded. A slow worker consumer suspends that generation while other contexts
continue; idle/backpressured workers sleep on channel notifications.

`Cancellation` is cloneable and works for async waiters and native step checks.
Dropping a polled run future cancels its registered work; dropping an unpolled
request builder has no effects. Cancellation/deadlines can stop async work promptly
and native work between batches/tokens. A native call itself is not preemptible.
Partial results can lag buffered native progress when an outer deadline wins.

`RunFailure` carries the typed cause and a partial `ChatResponse`. Native failures
also preserve raw `GenerationProgress` in their source. Receipts are recorded
before result-delivery callbacks. Outstanding tool requests get explicit unknown
outcomes on interruption. A timeout cannot undo an external side effect; callers
must use invocation IDs and application-specific idempotency if retries are needed.

Output exhaustion without tool requests returns `RunStatus::Truncated`. Budget,
protocol and delivery errors are failures, not fabricated final answers. Terminal
observer failures are warnings because successful effects have already occurred.

## Runtime lifecycle and limits

`build` is inert and validates configuration. `start` must be awaited to completion:
services start in registration order, then dedicated model workers load weights.
Startup failure stops successful components, retaining cleanup diagnostics.
Lifecycle service operations use `shutdown_grace` as their timeout allowance.
Do not abandon the startup future while service initialization is in progress.

`shutdown` closes admission, cancels/waits for admitted runs, joins workers, then
shuts services down in reverse order. It attempts all cleanup steps and aggregates
failures. Grace limits cooperative run/service waits, not thread joins: blocking
native ownership must finish before handles can be freed. Dropping a running
runtime requests stop but cannot await service flushing.

Defaults: 16 admitted runs, 32 buffered native events per evaluation, 4 contexts
per resident model; each interaction allows 8 model turns, 8 tool calls, 4096
sampled tokens and 120 seconds. Request payload, raw output and individual tool
result limits are 256 KiB, 128 KiB and 16 KiB. Recent messages are capped at 128
before evaluation. Tools may still allocate internally; the host validates the
returned payload and substitutes a bounded error for oversized results. Trusted
extensions must implement their own streaming/pagination for large data.

Native models/KV caches remain allocated while idle. These are resident-memory
costs, not continuous inference. Memory persistence, wake-word/ASR/TTS, external
transports, dynamic model unloading and background autonomous work are future
features, not hidden work in this foundation.
