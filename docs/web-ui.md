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
different states. Run metadata/measurements stay retained while their exchanges
are visible, even after the minimum deduplication window. Partial output carries its terminal status. A cutoff/failure before visible text
gets an empty terminal display entry, so its host status remains visible without
fabricating model content. Tools/reasoning
do not become answer text. Model-turn boundary events keep continuation messages
separate. The awaited core result finalizes each interaction, including failures
that occurred before normal event delivery.

| Resource | Bound |
| --- | --- |
| New input | 16 KiB UTF-8; identifiers 128 ASCII characters |
| One assistant message | 128 KiB UTF-8 |
| Visible transcript | 128 messages and 1 MiB text; oldest complete runs evicted |
| Submission records | 128; at least 10 minutes and while visible, capacity rejected |
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

The HTML loads embedded assets under a same-origin CSP, with no remote assets,
interpreted message HTML or persistent browser storage. HTML revalidates and
content-hashed JS/CSS/fonts/workers use immutable caching; gzip is prebuilt. Error responses use safe machine codes.
SSE begins with `snapshot`, followed by `update` events; IDs are instance-scoped
strings. Serialization failure or lag closes the stream. Each reconnect starts
with authoritative state rather than replaying `Last-Event-ID`.

## Typed frontend and verification

Production source lives in `crates/assistant-web/frontend`. React/strict TypeScript
uses Vite 8/Rolldown/Oxc, the Oxc React plugin, Oxlint with type-aware rules,
Oxfmt, `tsc` and Steiger. Dependencies are pinned. FSD layers are `app`,
`pages/chat`, `entities/{chat,telemetry}` and `shared`; unique chat actions remain
in the page until reuse justifies feature slices. See [ADR 0004](decisions/0004-typed-frontend-and-measurements.md).

Prepare assets before Cargo (also for CPU-only daemon builds):

```sh
cd crates/assistant-web/frontend
npm ci
npm run check
npm test
npm run build
cd ../../..
cargo build -p assistant-daemon
```

Node 22.18+ or 24.12+ is a build/development prerequisite. No Node process or
separate asset directory is needed after deployment. Cargo verifies source/input
checksums and embeds its own frozen copy; missing/stale assets fail with a build
instruction. Cargo never invokes npm or uses a stale fallback. Generated dist is
ignored. The build enforces a 120 KiB gzip initial JS/CSS budget; fonts, Markdown
worker and syntax grammars load separately. Runtime dependency/font notices are
available through the generated HTML's local license link.

`npm run dev` runs a loopback Vite server proxying `/api` to the daemon at
`127.0.0.1:8080`. Its development-only proxy rewrites Host/Origin together so
production request policy stays intact. Node/HMR are not part of the binary.

Target floors: Chromium 100+, Firefox 102 ESR+, Safari 15.4-era WebKit. Linux
WebKitGTK 2.36 is a compatibility target, not an assertion that Safari and GTK
versions are interchangeable. Explicit JS/CSS targets override Vite defaults.
Current browser tests do not establish oldest WebView/device certification.

Native application deltas coalesce for at most 20 ms/16 KiB. A single visible-tab
connection batches updates on animation frames. Hidden tabs close their stream;
runs continue on the daemon and visibility restores an authoritative snapshot.
Keyed subscriptions preserve unrelated rows. Worker parsing is throttled, with
one in-flight job and one latest pending version per mounted message. Limits:
128 KiB input, 12,000 tokens/nodes, 48 recursive conversion levels and a 1.5 s
worker watchdog. Failure displays literal source. Raw HTML stays text, unsafe
links are suppressed, and images never load automatically. Completed messages
receive lazy allowlisted syntax highlighting; code has Copy/Wrap controls.

The normal textarea keeps native selection, multiline and IME behavior:

| Action | Binding |
| --- | --- |
| Send | Enter or Ctrl/Cmd+Enter |
| Newline | Shift+Enter |
| Previous accepted prompt | Up in an empty draft or while recalling |
| Next prompt / restore draft | Down while recalling |
| Exit recall / close actions / otherwise stop | Escape |
| Quick actions | Ctrl/Cmd+K |

Recall creates a new draft; it never rewrites an earlier turn. History is bounded
at 64 accepted local prompts and resets on daemon instance change. Ambiguous
submissions retry only with the same key within a conservative monotonic retention
window. Explicit rejections preserve the draft. Neither wall-clock changes nor
late acknowledgments can create a new key or revive an old run.

The responsive dark layout uses local Inter and JetBrains Mono, tabular metrics,
short transform/opacity animations and reduced-motion support. Manual scrolling
is respected; token streaming never starts repeated smooth scrolling. Status
announcements avoid reading every token.

```sh
cd crates/assistant-web/frontend
npx playwright install chromium firefox webkit
npm run test:browser
```

The browser JSON report (`frontend/test-results/results.json`) includes measured
input-to-frame p95 samples while streaming. The 100 ms automated regression
ceiling on current engines is separate from the intended 50 ms deployment-device
target; no minimum-hardware certification is claimed.

Browser fixtures serve the built production assets and CSP through real HTTP/SSE.
They cover Markdown/XSS, IME/recall, lost/late acknowledgments, reconnect recovery,
320/768/2560 px layouts, bounded streaming, stable rows and manual scroll. Native
unit tests cover token allowances and measured active time, including a deliberate
consumer pause. Current WebKit on this development host needs test-only Ubuntu
libraries; `ASSISTANT_TEST_WEBKIT_EXECUTABLE` optionally selects a compatible
Playwright launcher without changing production assets.

The real-daemon smoke test requires existing local weights:

```sh
python3 scripts/test-web-smoke.py --require-model
python3 scripts/test-web-smoke.py --require-model --signal SIGINT
```

Default model: ignored `target/test-models/stories260K.gguf`; use `--model` for
another local model. Absence skips unless `--require-model`. No model is downloaded. To check the live Rust-to-TypeScript contract too:

```sh
python3 scripts/test-web-smoke.py --require-model --model /path/to/model.gguf --contract-output /tmp/assistant-contract.json
ASSISTANT_TEST_CONTRACT=/tmp/assistant-contract.json npm --prefix crates/assistant-web/frontend test
```

The optional contract case skips cleanly when no artifact is supplied.

## Output limits and measurements

Daemon defaults are `--ctx-size 8192 --max-tokens 4096 --run-max-tokens 8192
--timeout-seconds 600`. Output and total-run counts include hidden reasoning and
protocol, across tool continuations. Library `LLMConfig` retains its smaller
experimental defaults; callers should choose their own capacity/output policy.
A requested turn is clamped to the remaining run allowance and, for chat, to
actual remaining context capacity after safe old-exchange eviction. A smaller
`--ctx-size` can therefore retain the larger requested output ceiling; the actual
turn still reserves only what fits. Raw diagnostics fit their entire input without
history eviction. Context does
not expand automatically and there is no semantic compaction. The UI explains
`output_tokens`, `run_tokens` or `context_capacity` truncation. Oversized protected
input still fails; deadlines/errors are separate outcomes. Use explicit ceilings
that leave input room; larger contexts also require more cache memory.

By default the header and assistant-message footer show tokens/s and VRAM only.
Expand them for CPU, RSS/PSS, timings, counts and source details. Header throughput
uses the measured active run while generating, otherwise weighted lifetime native
throughput (`sum sampled tokens / sum active generation time`). It is never a mean
of individual rates. The footer refers to its one model turn, including hidden
sampled output. Prefill and first sampled-token latency are separate from visible
text latency. Synthetic backends report timing unavailable.

Native active times sum completed owner-thread steps, excluding scheduling and
consumer waits. Timings/counters survive failures as measured prefixes;
`measurement_complete=false` explicitly marks interruption before terminal native
accounting. An unreported final native step is not counted or fabricated.

CPU uses all daemon threads over monotonic phase boundaries; 100% is one core
and multicore work can exceed 100%. It includes other daemon work in the same
window. Linux RSS/PSS come from `/proc/self/smaps_rollup`. VRAM covers process
residency **only on GPUs with actual non-host allocations
for the primary model or its contexts**. The native owner publishes owned PCI
identities from model/cache/compute allocation snapshots; available-device lists
and instantaneous utilization never select the scope. A model on one card counts
only that card; a model split across cards counts each participant once. CPU-only
models report zero even when GPU backends registered/probed other cards. Resident
weights/cache still count while generation is idle.

NVIDIA uses per-PID NVML queries for the matching PCI device, deduplicating
compute/graphics process records. AMD uses Linux DRM fdinfo resident VRAM, with
client-ID deduplication; the documented amdgpu legacy residency alias is accepted.
Multiple resident clients with unknown/shared attribution cannot produce a precise
total. The aggregate is **Unavailable** if placement or any participating device's
counter is unavailable, rather than silently publishing a partial NVIDIA/AMD sum.
The expandable panel identifies the model GPUs, PCI addresses and counter sources.
Other allocations on unrelated GPUs are excluded. Process residency on a selected
GPU can still include other daemon work; it is not an exclusive allocation cost
for a message/model. Native buffer bytes establish device participation and are
never substituted for driver residency. Unified memory
is not added to RAM as fictitious dedicated VRAM. Memory windows retain baseline,
last observation and maximum observed bytes, not exclusive per-message cost or
continuous peak allocation. Resident model/cache memory can stay flat after a run.

The transport-neutral core sampler uses one blocking operation at a time, shared
by all tabs: 250 ms active, one second visible-idle, no idle polling with no
subscribers. Per-turn/run aggregates and a latest sample are bounded; full sample
histories are not retained. Native generations and process providers expose only
owned snapshots to async code. `snapshot.resources`, `snapshot.inference`,
`run.performance`, `run.resources`, `run.limit_reason` and `telemetry` SSE updates
are additive v1 fields documented in OpenAPI.

## Systemd

The binary runs in the foreground and handles SIGTERM and SIGINT. An example
user unit is in `packaging/systemd/assistant.service`; replace its executable and
model paths before installing it. `Type=exec` confirms execution, not model
readiness. The ready URL is printed only after the web service activates.
Do not choose a stop timeout assuming native decode/load can be forcibly unwound.
