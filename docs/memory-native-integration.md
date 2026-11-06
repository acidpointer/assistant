# Native memory integration progress

Current access contract: the owner requested CLI-only startup and direct local
WebUI access. Pairing, bearer/session authentication and its Unix socket have
been removed. Open the loopback URL to connect, including after reload/restart.
The benchmark manifests below retain historical verification of the earlier
access mechanism; they are not startup instructions for the current service.
Direct-access recheck passed: workspace formatting/check, daemon/web package
tests, 14 frontend unit checks, 17 Chromium journeys, and the real-daemon browser
smoke with reload and process restart. The real smoke ran without
`XDG_RUNTIME_DIR` and reused completed synthetic request keys, avoiding repeated
chat generation while verifying the changed transport.

Current implementation, 2026-11-06: [ADR 0007](decisions/0007-universal-assistant-memory.md)
is implemented: conversation/note graphs, semantic search and paged traversal
tools, freeform personality, a durable optional initiative loop, local
WebUI settings, standalone notes, graph browsing and an SSE mailbox. The core
owner API is independent of HTTP. See [using memory](assistant-memory.md).

[Versioned journey evidence](benchmarks/memory-journey-2026-11-06.json) binds the
current local LFM3/E5 files and tested executable. No weights were downloaded or
replaced. Real CPU/native checks passed pairing, browser use, source connections,
personality persistence, process restart/reconciliation, and retrieval of a
standalone satin-finish note absent from chat inputs. Two actual model-initiated
messages were committed and dismissed; the resumed check required a new ID and
verified no fictitious user exchange. Foreground keys are reused on retry to avoid
repeating already committed inference.

Verification: workspace formatting/check; full serialized workspace suite 675
passed/16 ignored; after the final explicit-note addition, application packages
131 passed/2 ignored and workspace check passed. Frontend type/lint/build and
14 unit checks passed (one optional fixture skipped), as did 17 Chromium fixture
journeys and a separate real-daemon Chromium journey. The optional historical
extraction feature also compiles; product builds exclude it. Native-build helper
checks passed and equivalent configurations imported verified native libraries.

The initial CPU smoke timeout and a browser assumption about the first semantic
hit's relationship label were corrected. These are development checks, not
exhaustive topic/personality qualification. CPU prefills observed around 55–127
seconds; GPU latency was not measured. Whole-memory restore and end-to-end
forgetting controls are not yet exposed in WebUI. The specialized extraction
experiments below are historical and do not run in production.

## Historical checkpoint and extraction experiments

Status: opt-in E5 recall, durable core/history and authenticated WebUI implemented;
**the final memory-system delivery is incomplete and unqualified**, 2026-11-06.
The complete contract remains [the final integration plan](memory-integration-plan.md).
This document records executable progress and failures, not a replacement scope.
Artifact identities and measured observations are recorded in the
[preflight manifest](benchmarks/memory-preflight-2026-11-05.json).

Planning update, 2026-11-06: the owner requested completing the feature with
WebUI as the first client and transport-independent core commands. The
[refreshed delivery plan](memory-integration-plan.md#core-boundary-and-webui-delivery-refinement--2026-11-06)
supersedes the pause for current planning. No new implementation or model/test
results are claimed by this documentation update. The checkpoint below records
the state at the historical stop; its two source edits and manifest are now
tracked in `1fca696`, but their recorded verification limits still apply.

## Current implementation and evidence — 2026-11-06

The owner authorized implementation and requires the existing selected models.
No alternative model or model download is part of this work. The protocol was
changed to fit LFM3: identify source-local names, classify fixed family dimensions,
then extract occupations. Host code supplies record IDs, types and evidence spans;
the model never chooses authority or database identities. The original unchanged
family/occupation semantic assertions now pass the real staged development
preflight. This is **one development case, not held-out qualification**.

Artifact provenance discrepancy: an independent `sha256sum` and the evaluation
harness both hash the current local `LFM3-1.7B-Heretic-Abliterated-Q6_K.gguf` as
`616775686c9abcbc1a0d302f75dc0a1f12498f272170bd7cda556a706c5606e1`.
The historical 2026-11-05 manifest records `077f001f…` for that filename. This work
did not edit or replace weights. The cause of the mismatch is unestablished;
current results bind to the current file and do not qualify the older checksum
or isolate algorithm changes from that historical provenance discrepancy.

Implemented since the checkpoint:

- Transport-independent revocable `OwnerHandle`; durable lookup/history and
  restart projection with explicit legacy metadata availability. Admission runs
  outside the coordinator task, remains owned after a lost waiter and only
  publishes acceptance after commit. Activation awaits history hydration.
- Mandatory bounded capture of all visible assistant/tool-continuation text,
  independently of optional observers. Hidden reasoning is excluded.
- Restricted internal inference over resident weights, isolated from product
  hooks/history/tools. Separate bounded queue/context capacity and foreground
  priority use native scheduling boundaries; no native preemption is claimed.
- Authenticated loopback WebUI with private same-UID pairing, one-use expiring
  codes, bounded hashed sessions, revocation-aware fetch SSE, reload-safe opaque
  request identities, history browsing, source search/preservation and health.
- Source retrieval uses rank fusion and excludes the active input from its own
  historical evidence. Answer policy is separate from untrusted quotations.
- E5 passage windows now use measured tokenizer counts (384 including prefix and
  special tokens, up to 64 overlap) with literal UTF-8 mappings and atomic byte
  coverage. The generation identity includes artifact/preprocessing/window policy;
  previous byte-window coverage is not reused as current coverage. Failed sources
  have persistent 5/30-second retries and review status after three failures.
  Rust vector scoring has elapsed/visited bounds and reports timeout rather than
  returning a partial ranking as exact.

Completed checks at this point: 103 core library tests, 5 web library tests;
frontend type/lint/build checks; 12 frontend unit tests (one optional contract
fixture skipped); 14 Chromium browser cases including v1 regression and owner
pairing/reload/revocation/logout. Real selected-E5 tests verify multilingual
recall after restart and multi-window literal coverage. The real two-daemon
[`check-memory-web.py`](../scripts/check-memory-web.py) passes saved chat,
authorization, conflict, history, E5 search, fresh-process retry reconciliation
and an E5-backed generated answer. CPU-only, local selected artifacts, synthetic
temporary stores; no production daemon or user history is used.

The answer smoke initially hit its token limit while discussing the active
question as a recalled source. Excluding that source and separating answer policy
from quotation data fixed the unchanged completion check. The harness also now
drains daemon diagnostics throughout its lifetime to avoid pipe backpressure.

Open work remains the complete A–G contract: durable staged job reservations,
heartbeat/coverage/read sets and automatic formation; graph aliases/inverses/
proof paths and corrections; situation/personality/attention/inbox; publication,
forgetting/export/restore barriers; frozen held-out corpus/reference parity,
load/resource/security recovery qualification and timed opt-in initiative.
Automatic graph formation remains inactive and health explicitly says
`unqualified`. Existing storage tests do not establish those product contracts.

The full workspace test gate is currently blocked by pre-existing untracked
native-build tests referring to unavailable dev dependencies. Those files and
the native submodule were left unchanged; narrow application gates are recorded
separately. No commit has been made.

## Stop checkpoint — 2026-11-05

**Historical stop: paused at the owner's request on 2026-11-05.**
Committed implementation anchor at that stop: `0dae27c` (`wip: semantic graph memory`).
The working tree also contains the documentation/manifest updates and two small
source edits described below. Nothing was committed as part of this checkpoint.

| State | What exists at this stop |
| --- | --- |
| Done | Selected E5 Q8 artifact verification and scoped metadata overrides; safe embedding access; exception-contained tokenization; resident CPU embedding worker; source-bound vector chunks/coverage, exact semantic search and bounded vector purge. |
| Done | Opt-in durable runtime/headless path: committed admission, stable operation IDs/retries, evidence recall, private operation authority, owned tool settlement before finalization, terminal details/usage availability and diagnostic-history isolation. |
| Verified | Real E5 multilingual retrieval, batch isolation, newline handling and close/reopen recall; deterministic durable admission/retry/cancellation tests; real daemon save followed by recall in a fresh process. The tokenizer abort was diagnosed and fixed; the corrected daemon exited with status 0. |
| Failed qualification | The current local chat model's named-JSON extraction passes syntax but fails semantic checks in two development attempts. The explicit semantic preflight remains failing and ignored by the default suite. |
| Not implemented | Automatic graph-formation orchestration and full proposal validation/qualification; graph proof-path/inverse/alias integration, chunked/fenced correction/read sets and situation state. Existing storage primitives are not a running memory-formation loop. |
| Not implemented | Personality/disposition, attention and durable inbox/initiative; owner pairing/authenticated v2 web/frontend and complete owner command surfaces; complete publication, forgetting and active-restore barriers. |
| Not qualified | Reference tokenizer/vector parity, held-out extraction/recall/path quality, supported large-store latency/foreground interference, browser/recovery races and live-use gates. |

**Work in progress when interrupted:** recording the evidence manifest and final
verification notes, plus these uncommitted source changes:

- `crates/assistant/src/memory/service.rs`: trim surrounding whitespace before
  selecting the bounded recall-query prefix, so leading whitespace cannot hide
  the actual query. This last edit has not been retested.
- `crates/assistant-daemon/src/cli.rs`: reject `--require-tool` together with
  `--memory-config` rather than silently ignore a flag not propagated into the
  durable application defaults. This last edit has not been retested.
- `docs/benchmarks/memory-preflight-2026-11-05.json`: newly written, untracked
  evidence manifest; retain it with the documentation changes when reviewed.

**Last completed checks:** `cargo check --workspace --offline` passed;
`cargo test --workspace --offline` recorded **649 passed, 0 failed, 16 ignored**.
The explicit native E5 service test passed separately. Clippy completed with
warnings; it was not a warning-free gate. These checks preceded the two final
source edits above. The last `cargo fmt --all --check` failed on the newly added
unformatted trim line; formatting and verification remain pending. The semantic
extraction preflight is a separate known failure, not part of the green default
suite or proof of production quality.

**Temporary shutdown:** the test/build/debugger sessions used for this work have
exited. Both smoke daemon processes are stopped, and their native model workers
are no longer resident. Diagnostic logs and the synthetic smoke store/config
remain under `/tmp` (notably `/tmp/assistant-memory-e5-smoke/`); they were retained
as evidence rather than deleted. Local GGUF files were preserved. No new service
was installed or left running by this work.

**Resume order:** first review the two pending source edits, format and rerun
their checks. Then continue the unchanged A–G delivery contract in
[the final plan](memory-integration-plan.md): resolve extraction semantics and
build the frozen quality harness; finish durable identity/options/lookup and
foreground/internal-work contracts; implement the graph/job/read-set path;
personality/attention; authenticated adapters/owner controls; publication,
forgetting and restore; then full native/headless/browser/load qualification.
Do not weaken semantic assertions, activate automatic graph writes on the
current evidence, infer authorization for another model download, or describe
the current partial integration as the completed brain-map feature.

## Selected model and native contract

Use the owner-supplied `multilingual-e5-small-q8_0.gguf` from
[cstr/multilingual-e5-small-GGUF](https://huggingface.co/cstr/multilingual-e5-small-GGUF).
Startup requires exactly 131,624,960 bytes with SHA-256
`0a34067a40f25d3149b36885faa62bee0e5284d0f9edc102acfc00e115d953e8`.
The daemon never downloads or converts weights. No Python runtime, package or
additional model is needed by this implementation. The downloaded bytes stay
unchanged and ignored by Git.

The file declares BERT, 384 output dimensions, a 512-token context, mean pooling,
and a SentencePiece vocabulary. Two missing metadata keys require explicit
**artifact-specific** load overrides: `tokenizer.ggml.token_type_count=2`
(matches its `[384,2]` token-type tensor), and `bert.attention.causal=false`.
These are not global defaults or an architecture relabeling. Prefixes are
`query: ` / `passage: `; the host L2-normalizes finite, nonzero outputs.
Whitespace in the encoder copy is collapsed; retained literal text and byte
citation offsets are unchanged. More than 512 tokens is rejected, never silently
truncated. The indexer splits sources into overlapping UTF-8 spans and advances
coverage atomically with each vector.

Two real daemon processes saved and recalled a blue-bicycle fact with fresh
working history. After the guard below, the recall process exited cleanly (0).
An earlier real daemon run exposed an additional failure: missing byte-fallback vocabulary
entries could make `llama_tokenize` throw through the C ABI, aborting Rust.
The new repository-owned tokenizer shim catches native exceptions and returns
a typed failure for both tokenization and counting. An unsupported input stays
literally saved and reports an indexing fault; it cannot abort the daemon or be
reported as fully indexed. A failed source currently stops that indexer drain
until another wake; per-source retry isolation remains outstanding.

A dedicated native CPU thread owns both E5 model and context, with two compute
threads and separate bounded queues (eight foreground queries, eight passages).
A query takes precedence over queued indexing; one already-running passage is
not preempted. Idle workers await commands without native polling. Explicit
shutdown joins indexing, then E5 ownership, then storage. No submodule or pinned
upstream revision changed.

The binding now checks last-successful output eligibility and output dimensions
before creating embedding slices. Invalid/negative indices return errors rather
than reaching the native debug abort. `get_embeddings` borrows the first selected
token row; pooled output requires `embeddings_seq_ith`.
Metadata override insertion is fallible, supports multiple keys, preserves the
native sentinel on vector growth, and rejects duplicates/oversized keys and
unterminated strings. Callers must handle the new `Result`.

## Implemented application path

On Linux, `RuntimeBuilder::memory(MemoryRuntimeConfig)` requires storage and E5
readiness before application activation. The default remains memory-disabled.
The required path commits user input and settlement reservation before acceptance,
uses stable operation IDs and reconciles retries across daemon restarts, recalls
literal/semantic evidence before the first model turn, and commits visible
terminal output before returning the terminal result. Migrations 1/2 remain
immutable; migration 3 adds source-bound vectors/coverage and migration 4 adds
bounded terminal metadata. Truncation/deadline and measured usage have explicit
persisted fields; legacy/unrecoverable details remain absent. Presentation
includes availability flags rather than asserting missing counters are measured.

Private operation authority is independent of mutable middleware drafts. Effective
tool arguments are validated before durable intent; owned execution/settlement
continues if a waiting future is dropped. Finalization joins those tasks before
entering the writer's higher-priority settlement lane. Unknown effects stay
unknown and are not automatically replayed. Calculator/time retain bounded,
attributed arguments/results; process tools retain identity/outcome only.
Unclassified third-party tools default to ephemeral capture and external effects.

`RunningAssistant::chat()` returns independent diagnostic history when memory is
required. Personal product interactions use the application handle. Direct
memory access is a host-issued owner capability, never a model-deserializable ID.

Semantic search scans eligible normalized vectors exactly, deduplicates sources,
and returns source IDs/revisions and supporting spans. Similarity is not factual
entailment. Suppression excludes vectors immediately; bounded purge accounts for
vector payloads before deleting source/coverage. Large-scale latency/capacity is
not qualified. Recall currently packs at most twelve bounded source excerpts;
proof paths, synchronous correction dependencies and publication barriers are
not yet integrated.

## Headless use

Supply memory paths as CLI flags, relative to the working directory:

```sh
cargo run -p assistant-daemon --offline -- \
  --model ./LFM3-1.7B-Heretic-Abliterated-Q6_K.gguf \
  --memory-dir "$HOME/.local/share/assistant/memory" \
  --embedding-model ./multilingual-e5-small-q8_0.gguf --no-web \
  --prompt 'Remember this fact: my bicycle is blue.' \
  --request-id bicycle-1 --cpu --no-tools
```

Run a fresh process with a different request key and ask about the bicycle. A
retry must reuse the original key **and exact input**; it reconciles the original
operation without generating or executing tools again. `--interactive` uses the
same durable path. `--raw` conflicts with required memory. Both memory flags are
required together; configuration files are not used. Explicit `--require-tool` conflicts with `--memory-dir` until durable
client-option identity is integrated; automatic registered tools remain available.

The existing unauthenticated v1 web adapter **rejects memory-enabled activation**.
It continues working with memory disabled. Pairing, authenticated v2 adapters,
owner controls and the updated frontend remain to be implemented; binding to
loopback alone is not owner authentication.

## Verification and limits

The native E5 binding test checks finite 384-dimensional output, English and
Ukrainian ordering, mixed-batch isolation, invalid sequences/indices, failed
batch invalidation and tokenizer exception containment. The service test indexes
real E5 passages (including a newline), closes/reopens storage and E5, and recalls
without working chat history. Deterministic application tests verify commit
before acceptance/terminal publication, stable retry after restart, and unknown
tool settlement after dropping the waiter. Storage tests cover vector cursor
conflicts, suppression and physical payload purge.

```sh
cargo test -p llama_cpp --test test_embeddings --test test_kv_overrides --offline
ASSISTANT_REQUIRE_MEMORY_MODEL=1 cargo test -p assistant --no-default-features \
  --test memory_native selected_e5_persists_recall --offline -- --ignored --nocapture
```

These are smoke/development results. Token-ID/reference-vector parity,
quantization tolerance, held-out retrieval quality, supported large-store latency
and foreground interference remain unmeasured. Do not turn the publisher's
accuracy statement into a local qualification result.

The real local LFM3 Q6 extraction preflight **fails**. In two development attempts,
constrained JSON syntax succeeded but semantics failed: the first invented a
correction target and validity times and assigned the occupation to the user;
the revised policy emitted a text object for a family edge and omitted the
occupation edge. The opt-in failing test keeps both supported claims mandatory;
no syntax repair or relaxed assertion turns this into a pass.

```sh
ASSISTANT_REQUIRE_MEMORY_MODEL=1 cargo test -p assistant --no-default-features \
  --test memory_native lfm_named_json_semantic_preflight --offline -- --ignored --nocapture
```

Automatic graph formation is not activated. Durable formation jobs remain queued
and literal/semantic recall continues independently. The entire final integration
plan still needs its graph/job/correction/read-set work, personality/disposition,
attention/inbox, owner authentication/UI/controls, full publication/forget/restore
barriers, held-out/native/browser/load qualification and live-use gates. No new
model download is assumed or authorized by this failed preflight.
