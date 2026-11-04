# Web UI and runtime services

Status: implemented, 2026-11-04. The machine-readable HTTP contract is
[`web-api.yaml`](web-api.yaml); the architectural decision is
[`0003`](decisions/0003-runtime-services-and-web.md).

## Product and command contract

One `assistant` binary owns the resident runtime and its compiled plugins.
`assistant-web` is a library, not another executable. Server mode is the default:

```sh
cargo run --release -- --model /path/to/model.gguf
cargo run --release -- --model /path/to/model.gguf --host 127.0.0.1 --port 8080
cargo run --release -- --model /path/to/model.gguf --host ::1 --port 8080
```

Default listener: `127.0.0.1:8080`. Port zero requests an allocated port, printed
after activation. Stdin EOF has no effect in server mode. `--daemon` remains a
compatibility alias for resident operation. `--host` accepts IP literals.

`--no-web` disables registration of the entire web plugin. There is no HTTP
socket, UI serving, SSE subscription or web task in this mode. It does not remove
embedded assets from the compiled executable.

One-shot prompts are for testing/diagnostics and **require `--no-web`**. They
complete one interaction, print its output and stop the runtime. Interactive
stdin and raw decoder diagnostics also require disabling web:

```sh
cargo run --release -- --model /path/to/model.gguf --no-web --prompt 'Hello'
cargo run --release -- --model /path/to/model.gguf --no-web --interactive
cargo run --release -- --model /path/to/model.gguf --no-web --raw --prompt 'Once upon a time'
cargo run --release -- --model /path/to/model.gguf --no-web
```

The last command is a headless resident runtime. Explicit listener flags conflict
with `--no-web`; prompt/interactive modes conflict with each other. CLI trace,
diagnostics and required-tool test controls require `--no-web` too. Invalid
combinations fail before model or listener I/O. GPU features and model/cache
configuration keep their existing meanings.

## Plugin lifecycle

`Plugin::configure` registers factories, dependencies, tools, hooks or middleware
without I/O. `PluginRegistrar::runtime_service` installs an identified
`RuntimeServiceFactory`. Required runtime service names must be unique.

Startup: dependency services → prepare runtime services → resident workers/chat
→ application coordinator → activate runtime services. Prepared resources are
owned handles released on rollback. Partial preparation must be cancellation
safe; failed activation must clean up any tasks it started. Activation is awaited
to completion. Runtime context contains `AssistantHandle`, typed dependencies and a read-only
shutdown notification.

Shutdown: close application admission → quiesce runtime services → cancel and
finalize application interactions → close/join runtime services → stop workers
→ stop dependency services in reverse order. Failures are aggregated. Service
drop requests stop, while explicit shutdown joins tasks. Native work is joined
without freeing resources while a native call executes.

Web preparation reserves its listener before model loading. Activation begins
serving only after application readiness. Web termination is observable through
the runtime supervisor. Existing dependency-service startup semantics and
request middleware semantics remain distinct from this runtime-ready lifecycle.

## Shared application contract

Product transports use `AssistantHandle::{submit,snapshot,subscribe,run,cancel}`.
The current implementation exposes one primary chat and one active interaction.
Native inference and tools still run through the existing chat pipeline.

The coordinator owns accepted work independently of awaiting callers and browser
connections. It rejects concurrent input with `chat_busy`. Subscription creation
and snapshot capture are one operation. A lagged subscription is closed; the
consumer replaces its view from a new snapshot. No event replay store exists.

The display transcript, native working context and future durable history are
different states. Partial output carries its terminal status. Tools/reasoning
do not become answer text. Model-turn boundary events keep continuation messages
separate. The awaited core result finalizes each interaction, including failures
that occurred before normal event delivery.

| Resource | Bound |
| --- | --- |
| New input | 16 KiB UTF-8; identifiers 128 ASCII characters |
| One assistant message | 128 KiB UTF-8 |
| Visible transcript | 128 messages and 1 MiB text; oldest complete runs evicted |
| Submission records | 128; at least 10 minutes retained, capacity rejected |
| Application command inbox | 32 commands |
| Native-to-application progress inbox | 256 items and 256 KiB text |
| Shared subscriber ring | 64 updates, shared immutable payloads |
| Application subscribers | 32 |
| Web SSE streams | 4 |
| Web TCP connections | 64, including unfinished requests |
| HTTP JSON body | 128 KiB |
| Request processing/body read | 10 seconds |
| Idle TCP I/O | 30 seconds; SSE heartbeat every 15 seconds |
| HTTP shutdown drain | 5 seconds before forced socket closure |

Core run limits additionally bound model turns, output bytes, tokens, context,
tools and duration. Deduplication keys are `(transport scope, client_id,
client_request_id)`. Equal input returns the original run; different input
conflicts. Reconnect never automatically repeats a POST. Old instance chat IDs
are invalid after restart. Retained recent run metadata resolves lost HTTP
acknowledgments; expired records are not restart recovery.

## Browser boundary

No authentication is implemented. Default binding is loopback. Explicit network
binding changes who can connect; all native clients reaching it are unauthenticated.
Host validation accepts the actual destination IP/port, or `localhost` when that
destination is loopback. Forwarded headers are not trusted. Browser mutation
requests require JSON, `X-Assistant-Client: webui`, and an exact origin match when
Origin is present. Missing Origin is supported through the non-simple header
requirement. Cross-origin access is not enabled.

The HTML uses per-response CSP nonces, no remote assets, no interpreted message
HTML and no persistent browser storage. Error responses use safe machine codes.
SSE begins with `snapshot`, followed by `update` events; IDs are instance-scoped
strings. Serialization failure or lag closes the stream. Each reconnect starts
with authoritative state rather than replaying `Last-Event-ID`.

## Single-file UI and verification

`crates/assistant-web/webui.html` contains all production HTML/CSS/JavaScript.
Transport, state transitions and rendering are separate functions within one
strict-mode closure. The language floor is ES2017. Target browsers are Chromium
80+, Firefox 78+, and Safari 13.1-era WebKit; modern viewport features have
fallbacks. Current-engine tests do not establish minimum-version certification.

Application text deltas coalesce for at most 20 ms or 16 KiB and flush at
semantic boundaries/snapshot capture. Browser streaming appends text on animation frames and preserves existing message nodes.
The transcript is bounded; input sizing changes only when input or viewport
changes. Manual scroll position is respected. Status and completed-message
announcements avoid per-token screen-reader updates. Enter-to-send respects
composition, with Shift+Enter for newline. Responses are plain text in v1.

Browser-only development tools are isolated from the production asset:

```sh
cd crates/assistant-web/tests/browser
npm ci
npx playwright install chromium firefox webkit
npm run check
npm test
```

Core tests use scripted evaluation without model files. HTTP boundary tests cover
authority, structured errors and nonce templating. Browser fixtures exercise
send/cancel, IME, reconnect, long streaming, stable nodes and responsive layouts.
Actual minimum engines/mobile keyboards still require deployment-device checks.

The real-daemon smoke test uses existing local weights and verifies HTTP policy,
SSE generation, deduplication and SIGTERM with an open subscription:

```sh
cargo build -p assistant-daemon
python3 scripts/test-web-smoke.py --require-model
python3 scripts/test-web-smoke.py --require-model --signal SIGINT
```

The default model is the ignored `target/test-models/stories260K.gguf`; supply
`--model` for another compatible local model. Absence skips without
`--require-model`. No model is downloaded. On this development host, current
WebKit testing needs compatible Ubuntu libraries for Playwright's Linux build;
those test-only libraries are outside the repository and production binary.

## Systemd

The binary runs in the foreground and handles SIGTERM and SIGINT. An example
user unit is in `packaging/systemd/assistant.service`; replace its executable and
model paths before installing it. `Type=exec` confirms execution, not model
readiness. The ready URL is printed only after the web service activates.
Do not choose a stop timeout assuming native decode/load can be forcibly unwound.
