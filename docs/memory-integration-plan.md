# Memory system: final implementation delivery

Status: final remaining implementation plan, 2026-11-05.
Audited baseline: `0205229` (`feat: initial comprehensive memory system`).
This change is research/documentation; application and binding changes below
have not been implemented.

The next implementation task delivers the complete remaining memory experience
in one integrated delivery. The order below is an internal build/test sequence,
not another series of storage-only milestones. Completion requires working
headless and authenticated web journeys, real model evidence and owner controls.
Do not stop after registering a plugin, wiring literal storage or passing mock
model tests.

The [implementation specification](memory-implementation-spec.md) remains the
v1 behavior authority. This plan replaces the earlier incremental integration
sequence and closes its remaining execution details. The
[storage contract](memory-storage.md) describes the implemented foundation;
[original research](memory-system-plan.md) supplies rationale and alternatives.

## Final delivery contract

| Experience | Required finished behavior |
| --- | --- |
| Remember | Accepted dialogue and explicit preservation survive restart. Confirmation identifies the committed literal source and distinguishes structuring/indexing coverage. |
| Understand current context | Every authorized product interaction recalls relevant people, preferences, goals, stated reasons, questions and unfinished commitments before its first model turn. Recent referents remain bounded. |
| Connect knowledge | Exact, lexical, semantic and typed graph recall produce complete supporting paths, including inverse navigation, attribution and time. Missing premises lead to qualification or abstention. |
| Update knowledge | Identified corrections take effect synchronously; ambiguous corrections remain visible and unresolved. Old jobs and active answers cannot overwrite or continue publishing invalidated evidence. |
| Have a stable character | Editable personality and expiring disposition compile deterministically; one behavior snapshot is pinned per operation. Voice changes do not change facts or permissions. |
| Take useful initiative | Due commitments and resolved blockers produce durable, deduplicated inbox items. Qualified, owner-enabled live notices obey quiet hours, activity deferral and quotas. |
| Give the owner control | Inspection, correction, preservation, forget preview/progress, export, backup and safe restore work through authorized core commands and the installed adapters. |
| Recover honestly | Lost replies, cancellation, storage failure and restart preserve durable identity and known tool outcomes. Uncertain effects remain uncertain; tools are never automatically replayed. |

Complete v1 retains one Linux owner, one assistant identity, one active product
interaction, SQLite authority, exact vector search and named JSON proposals.
HTTP is optional; memory/background/inbox behavior must work without it.
Memory-disabled behavior remains supported. Keep bounded working history for
immediate referents and tool protocol; prove permanent recall with that history
cleared. No graph server, ANN migration, compact language, recurring calendar,
external autonomous action, dynamic plugin loader or unrelated native update is
needed to finish this delivery.

## What the code actually contains

The working tree was clean at the start of this review. The previous milestone
records 645 passing workspace tests, 14 ignored, and 37 Rust 1.88 storage tests.
Those are existing storage/runtime regression results, not memory-model or
end-to-end qualification. This research did not rerun that suite.

| Area | Implemented baseline | Remaining integration |
| --- | --- | --- |
| Storage | Migrations 1/2, durable product/tool commands, attributed sources, priority/reservations, suppression/journal/purge, FTS/qualified edges, fenced jobs, consumer cursors and backup candidates. | New domain records, eligibility/dependency queries, richer jobs and product outcomes; reuse the existing writer. |
| Product coordinator | [`Coordinator::admit`](../crates/assistant/src/application/mod.rs) accepts and deduplicates in memory, then creates instance-scoped run IDs. | Required durable admission/status/finalization; stable operation identity and reconstructed views. |
| Chat/tools | [`Request::execute`](../crates/assistant/src/chat/mod.rs) selects over cancellation; [`RunContext::execute_tool`](../crates/assistant/src/chat/runner.rs) has no durable authority/receipt participant. | Mandatory persistence outside optional middleware; effective dispatch/capture policy and owned settlement. |
| Lifecycle | [`activate_services` / `shutdown`](../crates/assistant/src/runtime/mod.rs) start the application before service activation and close services before native worker joins. | Composite readiness, supervision and a storage-last drain barrier. |
| Internal inference | [`runtime/worker.rs`](../crates/assistant/src/runtime/worker.rs) has rotating active contexts and synchronous preparation, but no execution class or internal structured job API. | Reserved foreground capacity, priority before preparation/prefill, isolated JSON jobs and measured accounting. |
| Knowledge | Current jobs offer whole sources, fixed input reservation and add-only changes. Graph reads return outgoing edges. | Chunk coverage, correction/read sets, aliases, paths, semantic vectors, situation and attention. |
| Personality | No implemented personality/state module. | Versioned repository, compiler/reducer, profile editing and behavior snapshots. |
| Web | Authority/origin checks in [`http.rs`](../crates/assistant-web/src/http.rs); no owner sessions. Frontend connection uses `EventSource`; submission abandons restart recovery and limits retries to nine minutes. | Pairing/authentication, v2 contracts, authenticated fetch streams, durable reconciliation and owner UI. |
| Daemon | [`cli.rs`](../crates/assistant-daemon/src/cli.rs) requires a model for the existing command shape; prompt/stdin paths use ephemeral `Chat`. | Memory configuration, a pairing command without model startup, and authorized durable headless interactions. |

Audit anchors at this commit: application admission line 360; chat execution
line 289 and tool dispatch line 325; runtime activation line 493 and shutdown
line 588; worker prompt preparation line 507. The frontend connection constructs
`EventSource` at line 83 of
[`assistant-connection.tsx`](../crates/assistant-web/frontend/src/app/connection/assistant-connection.tsx);
[`submission.ts`](../crates/assistant-web/frontend/src/pages/chat/model/submission.ts)
contains the restart reset and retry-window checks around lines 49-65. Embedding
sequence slices start at `context.rs:413`; converter vocabulary selection starts
at `conversion/bert.py:38`, and native WordPiece selection at
`src/llama-vocab.cpp:2283` inside the pinned submodule.

The full specification still requires application work in human-owned crates.
Implementation should proceed only when requested, then carry this entire scope
through acceptance rather than treating the storage baseline as the product.

## Research findings that change the implementation order

### Qualify the actual model path first

The workspace inventory contains the local LFM3-1.7B Q6 derivative and no E5 GGUF
or memory embedding manifest. The recorded chat artifact SHA-256 is
`077f001fa1ba64e49190e4b2a83035e886e3c22f74d0a331843925b1d7fa3a2f`;
recheck it at implementation time. The existing
[tokenizer artifact](benchmarks/compact-protocol-tokens-2026-11-05.json) explicitly
contains no generation or extraction-quality measurement.

E5-small's published configuration names `BertModel` and
`XLMRobertaTokenizer`. The pinned converter's
[`BertModel.set_vocab`](../crates/llama_cpp_sys/llama.cpp/conversion/bert.py)
emits `tokenizer_model=bert`, which the pinned native vocabulary loader selects
as WordPiece. Separate XLM-Roberta vocabulary helpers exist. This is a concrete
conversion compatibility risk, not proof of a failed conversion experiment:
no E5 conversion was run in this review.
[Published E5 configuration](https://huggingface.co/intfloat/multilingual-e5-small/raw/main/config.json).

Therefore require token-ID, normalization, special-token and position-embedding
parity before vector parity. An adapter must preserve this model's BERT position
semantics; relabeling its architecture as XLM-Roberta is not a qualification.
Keep conversion changes in repository-owned tooling or build-copy patches, never
inside the pinned submodule. Do not silently switch model families to avoid the
problem. The selected E5 contract remains mean pooling, L2 normalization,
384 dimensions, query/passage prefixes and at most 512 total input tokens.
[E5 model card](https://huggingface.co/intfloat/multilingual-e5-small).

The Rust embedding accessors in
[`context.rs`](../crates/llama_cpp/src/context.rs) still construct slices from
`n_embd`; native output dimensions/rank counts and invalid-index behavior require
an audit before inference. Existing model APIs already expose `n_embd_out` and
`n_cls_out`; reuse them and add a checked shim only where native guards require it.

### Close product contracts before connecting callbacks

These choices are required in the final implementation, not future API research:

1. **Authority:** host-issued owner/session scope is required for personal reads,
   writes, subscriptions and tool dispatch. A string such as `web` is a client
   namespace, not authentication. No global recall hook grants private access.
2. **Client identity:** hash a versioned canonical client command, excluding host
   observation time, current default budgets and newly resolved profile state.
   Explicit client options belong in the digest; the originally resolved limits,
   behavior and policy revisions are stored at first admission. Retries after a
   config/profile change return the original operation, not a fresh run/conflict
   caused solely by new defaults. Version the digest format and support legacy
   stored requests explicitly; preserve existing receipt/spent-key semantics.
3. **Lookup:** add authorized lookup by original client key, because a lost
   admission reply leaves the client without an operation ID. Old acceptance
   receipts identify an operation; current status comes from the operation store.
4. **Outcome:** persist terminal reason, truncation/deadline status, limit reason
   and honest bounded usage/measurement availability. The current coarse storage
   state cannot reconstruct every `RunView` outcome. Do not invent zero metrics
   or a successful full answer after restart.
5. **Tool ordering:** all confirmed receipt/unknown-outcome writes settle before
   finalization is enqueued. The writer prioritizes finalization over other
   critical work, so cross-queue FIFO must never be assumed.
6. **Actual dispatch:** validate effective arguments after trusted middleware,
   persist intent before the real handler, and persist its bounded result before
   publication/continuation. A middleware short circuit is recorded as such;
   it must not pretend an external handler executed. Apply capture policy before
   serialization, including failure and cancellation paths.
7. **Read sets:** persist operation evidence/source dependencies, predicate-scope
   revisions and derived eligibility epochs. Source-only deletion checks cannot
   protect an answer based on changed blockers or an older conclusion.
8. **Preservation:** preserving an already accepted source is a retention command,
   not another copy of the whole conversation or a new model-work allowance.
   Coalesce source-family evidence and jobs; assistant prose is not independent
   corroboration of a user fact.

## One implementation pass, ordered internally

```mermaid
flowchart TD
    A[Artifact and model preflight] --> B[Authorized durable runtime]
    B --> C[Formation, hybrid recall and situation]
    C --> D[Personality and attention]
    B --> E[Authenticated adapters and owner controls]
    D --> F[Complete publication, forgetting and recovery]
    E --> F
    F --> G[Native, headless, browser and load acceptance]
    A --> G
```

Every section below belongs to the same final delivery. Small reviewable code
changes are appropriate; a new planning/approval cycle between them is not an
implementation requirement. Keep independent work moving if an empirical gate
fails, but do not report an unqualified channel as complete.

### A. Artifact preflight and executable qualification harness

Touch binding wrappers/shims as necessary, repository-owned model-preparation
and evaluation tooling, memory fixtures and benchmark manifests.

- Read [binding maintenance](bindings-fork.md); preserve the native pin and
  patches. Fix embedding dimension/pooling/rank/index/lifetime contracts, with
  error containment and real embedding tests. No unsafe slice from a guessed
  shape; invalid native indices must not abort the daemon.
- Prepare the selected E5 artifact from a fixed upstream revision. Record source,
  license, converter/options, tokenizer/model hashes, pooling/precision/prefixes
  and reference-vector fixtures. Development/reference Python dependencies may
  be isolated in preparation tooling; the daemon remains native Rust/llama.cpp.
  Do not commit weights or machine-local paths; startup never downloads models.
- Check tokenizer parity on English/Ukrainian, punctuation, normalization,
  special tokens, maximum input and mixed batches. Require matched-FP32 vector
  cosine >=0.999, correct dimension/norm, and batch isolation.
- Run real named-JSON extraction with the selected local chat artifact using
  the binding's existing template/parser/schema/grammar path. Inspect complete
  output, termination reason, exact quotation entailment, identity and correction
  behavior. Fix prompt/chunk/handle selection within the selected budgets;
  do not lower semantic validation or count repaired syntax as semantic success.
- Build the complete quality harness now. Freeze development versus held-out
  data before tuning. Seeded graphs, scripted models and actual model-produced
  memory are different test tracks and receive separate results.

This stage uses development cases for early preflight. Full held-out quality is
measured through the integrated implementation in G, not inferred from that
preflight or the converter's supported-architecture registry.

If the current chat artifact cannot pass Q4, compare a more capable explicitly
configured local extraction model within the same runtime/resource contracts.
An unresolved artifact/quality failure is a release blocker, not permission to
omit semantic memory or rename literal search as the final brain.

### B. Authorized durable runtime and conversation

Touch `application`, `chat`, `tools`, `runtime/{mod,services,worker}`, daemon
assembly and compiled memory/personality plugin glue.

- Select `MemoryMode::Disabled/Required`; exactly one mandatory persistence
  participant lives outside cancellable generation and optional hooks. Keep
  restricted handles inert until the composite factory completes recovery,
  required model/profile validation and consumer activation.
- Add private `OperationContext`, pinned `BehaviorSnapshot`, `EvidenceLease` and
  publication generation to the run. Restrict diagnostic `Chat` to an isolated
  ephemeral context without private history, persona, memory tools or recall;
  it must not alias a product chat containing personal working state.
- Make coordinator admission asynchronous and owned. Track the pending key as
  well as the active run so cancellation/status/health remain responsive.
  Acknowledge/publish acceptance after the atomic storage commit only. Lost
  receivers do not cancel admitted work or generate a new request key.
- Wire `admit_operation`, start, effective tool intent/result and terminal
  settlement. Cover early backend admission failure, lock-wait cancellation,
  middleware short circuits, tool/model errors and shutdown. Await receipt
  settlement before terminal settlement; await terminal commit before durable
  `Finished`. Streamed deltas remain provisional until then.
- Pass bounded visible-output/captured-receipt DTOs to persistence; never serialize
  raw `ChatResponse.messages`, hidden reasoning, system prompts or working context
  as a convenient terminal transcript.
- Introduce typed internal inference with a distinct execution class and owned
  completion. It uses resident model weights and isolated contexts, never the
  primary chat, ordinary tools, product submit or recursive transcript ingestion.
- Give foreground work priority before dispatch, allocation, rendering and every
  native prefill/decode boundary. Reserve an interactive context/permit and one
  query-embedding slot. Count paused context residency. If resources cannot fit,
  expose background unavailability rather than consuming interactive capacity.
- Prepare storage before model startup; release readiness before enabling
  authorized traffic. Supervise required consumers/writers/model users. On
  shutdown close admission/claiming, cancel generation, settle owned work, join
  native users, then close storage. A timeout never abandons finalization owners.

Prove an authorized headless exchange, lost reply, restart reconciliation,
confirmed/unknown tool outcome and unchanged memory-disabled behavior before
building further product surfaces. Continue the same implementation task.

### C. Form, retrieve and maintain the knowledge state

Touch `memory/{extract,retrieve,situation}`, job scheduling, store transactions,
model-worker preparation and typed memory tools.

- Generalize jobs to include kind, next due, attempts, lease heartbeat/hard end,
  attributed source excerpt, absolute spans, coverage cursor, read set and
  dependency/policy versions. Current whole-source/add-only leases are a starting
  point, not the extraction runtime contract.
- Count the actual full rendered input before compute reservation, including
  instructions, handles, schema/template and retry prefill. Keep the selected
  1,024-token excerpt ceiling, 128 overlap, four-claim/512-output bound, one repair,
  four passes/4,096 input/1,024 output per root and daily 60K/10K/300 seconds.
  Shrink excerpts when the full prompt cannot fit. Record model/tokenizer identity
  with usage; embedding work also consumes its declared compute allowance.
- Enforce 60-second leases, 20-second heartbeat, 120-second hard lifetime and two
  transient retries at 5/30 seconds. Expiry/reclaim advances fences. Complete
  source/graph/scope/generation checks and effects in one writer transaction.
  Partial coverage never advances an unlocated prefix; truncation applies no
  proposal. Deferred/unresolved literals remain searchable.
- Implement ambiguity-preserving aliases, source-family deduplication, typed
  inverse navigation and proof paths. Search exact/FTS/E5 candidates, fuse ranks
  using the specified constant 60, and investigate finite patterns under cumulative
  node/edge/depth/token limits. No arbitrary SQL or model-defined graph code.
- Implement versioned vector manifests/generations, finite normalized f32 storage,
  bounded exact scans and authoritative eligibility hydration. Expose index
  coverage/watermarks, building/unavailable/partial states and rebuild/switch
  behavior. Keep full source/span mappings through E5's 384/64-token chunks.
- Run E5 through a dedicated native owner worker with noncausal batch decode and
  pooling, initially on CPU. Give query work priority over document batches and
  copy vectors into owned results before crossing threads. Do not send an
  embedding model through the generative chat-template/`Generate` path.
- Build `EvidenceBundle` and `SituationSnapshot` before each product interaction,
  without a planning-model call for normal recall. Pack through the actual chat
  tokenizer: reserve current input, active tool protocol and minimum output,
  then the persona and 1,024-token evidence allowance. Keep complete premises,
  qualifications and pending corrections; never retain a stale fact alone.
- Offer scoped `memory.search`/`memory.follow` investigation and current-source
  preservation. Host-issued handles/intent control correction and destructive
  actions. Recalled prose cannot grant authority, edit personality or execute code.
- Apply identified corrections synchronously. Keep ambiguous targets unresolved;
  do not confidently present the old value while hiding pending correction words.
  Derive active goals, explicit reasons, blockers, questions and commitments from
  checked premises. Connectivity alone proves neither causality nor transitivity.

All named entities, typed claims and conclusions retain source role, evidence,
valid/recorded time and revisions. Assistant promises can create assistant
commitments, not new purported user facts. Unknown motives stay unknown.

### D. Personality, situation-driven attention and delivery

Touch `personality`, `memory/attention`, revisioned repositories, scheduler and
application events. Use one agent identity/repository and the same writer.

- Implement the specification's bounded profile, integer traits, compiler,
  disposition reducer/expiry and replacement generations. Default automatic
  disposition changes off; profile prose remains within 512 chat tokens.
  Pin revisions at admission. Neutral behavior works without a personality
  plugin; reject incompatible persistence-requiring configuration explicitly.
- Make profile/disposition edit, replacement and activation modes real owner
  commands. `interrupt_active` uses cancellation, mandatory finalization and the
  publication barrier. It never retries completed tool effects or deletes facts.
- Implement only the selected notice rules: due pending one-shot commitment or
  resolved blocker of an active goal. Check every remaining blocker. Civil-time
  resolution uses `chrono`/`chrono-tz`; ambiguous/nonexistent local times require
  clarification. Reject recurrence rather than making an unsupported promise.
- Persist subscriptions, fan-out cursors, cause/occurrence identity, notice state,
  draft versions, expiry, seen/dismiss/snooze and delivery records. Publish the
  notice plus quota/dedup state atomically. Disconnect/restart/redrafting cannot
  create another cause or refill its budget.
- Wake on durable changes/due time/resume; no periodic model inference. Native
  drafting runs only after deterministic eligibility/budget checks. Revalidate
  premise/profile/policy revisions before publication. Template factual inbox
  cards handle drafting failure; unqualified prose never reaches live delivery.
- Deliver typed assistant notices without manufacturing user turns. Preserve
  inbox items while no transport is connected. UI deduplicates stable IDs;
  published, client-acknowledged and owner-dismissed states remain distinct.
- Enforce default live opt-out, owner-capped 0/1/2 per rolling day, two-hour gap,
  quiet hours, five-minute activity deferral, expiry and permanent same-cause
  dismissal. Background telemetry/heartbeats do not count as owner activity.

The feature is fully implemented even while its live promotion gate is closed;
there must be no placeholder timer, fake canned proactive conversation or
unimplemented user action left in this delivery.

### E. Optional authenticated web and durable headless adapters

Touch `assistant-web`, frontend FSD features/contracts, daemon CLI, OpenAPI and
packaging. The core above has no dependency on web types or browser sessions.

- Add opt-in startup memory configuration with explicit data/model manifest,
  budgets, timezone, profile and owner attention policy. Keep existing default
  invocation compatibility. In enabled mode, `--no-web --prompt` and
  `--no-web --interactive` use authorized durable product commands; raw decoder
  diagnostics require memory disabled. Headless resident jobs/inbox keep working.
- Parse `assistant pair` without requiring/loading a model. Install its private
  0600 Unix pairing socket under 0700 runtime storage only with the web adapter;
  check peer UID. General-purpose Unix/D-Bus transport remains outside scope.
  Offline maintenance cannot open a second writer while the daemon owns the store.
- Implement the selected one-use 256-bit/5-minute pairing code and in-memory
  bearer sessions (8-hour absolute, 30-minute idle), one outstanding code and
  five exchange attempts/minute. Never put credentials in URLs, logs or browser
  persistent storage. Expiry/logout/revoke closes subscriptions and queues.
- Implement every v2 endpoint family in the specification, including lookup by
  original key, forget status and bounded export. Authenticate all personal
  reads, bootstrap, submission, status, cancellation and streams. Disable or
  equally protect legacy routes; no memory access through the v1 path.
- Replace `EventSource` with bounded authenticated fetch-SSE. Handle UTF-8/chunk
  boundaries, aborted streams, token expiry, lag and instance changes. Preserve
  atomic snapshot/stream registration and distinguish stream cursors from stable
  operation IDs. SSE heartbeat traffic must not renew idle expiry by itself.
- Replace the frontend's instance/nine-minute retry assumptions with durable
  reconciliation. Preserve opaque pending identity across reload if needed,
  without persisting a bearer or conversation text. After re-pairing, look up
  the original operation; never automatically replay it or mint a new key.
  A committing/indeterminate request is not a definite rejection.
- Add complete owner UI: memory/evidence/path inspection, preserve/correct,
  forget preview/progress, profile/disposition editor, inbox actions, component
  health, export and recovery controls. Keep technical counters in inspection;
  the conversation shows useful context, truthful saved state and source links.
- Update Rust schemas, OpenAPI, TypeScript validators, reducers and browser
  fixtures together. Preserve the current composer/streaming/Markdown behavior.

The browser must feel like the existing working chat with durable context and
owner controls, not a collection of disconnected demonstration pages.

### F. Publication, forgetting, export and restore across all features

Touch application publication, every personal reader/transport, dependency
storage, maintenance, backup/restore and owner recovery commands.

- Add authoritative source/claim dependencies for operation outputs, retained
  receipts, situation, appraisals, notices and exports. The storage baseline's
  own-exchange suppression is insufficient for text derived in later exchanges.
- Revalidate selected revisions before model dispatch and publication. Direct
  facts use their own revisions; derived conclusions also use scope/eligibility
  epochs, including inserts that invalidate absence-based reasoning. Do not
  hold a SQL transaction throughout inference.
- Implement durable forget progress: reviewed selection -> SQL suppression ->
  journal acknowledgment -> publication barrier -> bounded physical purge.
  Acknowledge logical forgetting only after the first three execution barriers
  settle. Invalidate affected prompts/history and conservatively mask derived
  copies until revalidated. Reject late job fences/results.
- Cover model deltas, tool replies, queued SSE, snapshots, evidence/search,
  inbox and export. Tag publications with store/deletion generation and check at
  the final controlled send boundary; drain/close queues that cannot be revoked.
  Already delivered bytes and downloaded external copies cannot be retracted.
- Implement bounded versioned JSONL export, authorized completion/download and
  generation recheck. Backup retention/expiry failures remain visible; journal
  retention covers every retained backup. Explicit preservation is not silently
  evicted to free space.
- Implement restore activation, not merely candidate creation: quiesce admission,
  verify manifests/checksums, reconcile newer deletions, validate integrity,
  rebuild/catch up indexes, revoke sessions/leases, reset working contexts and
  atomically choose the new store. Hold stable daemon ownership across the switch;
  use a durable activation marker/pointer and directory fsync, not a fragile
  in-process handle replacement. Crash recovery determines old/new activation
  without mixing them. Failed restore preserves the old directory.
- Preserve migration 1 and migration 2 byte-for-byte. Add ordered migrations for
  the new records and old-store tests. Never promote a backup that cannot prove
  deletion-history coverage; retain it quarantined for explicit owner reconciliation.

### G. Qualification, packaging and finished owner walkthrough

Build acceptance around the running product, with separate isolated native and
scripted failure fixtures. Add reproducible commands/reports for model quality,
headless/browser journeys and performance. No fabricated benchmark numbers.

| Evidence | Required result |
| --- | --- |
| Q1/Q2 platform/storage | Locked Rust 1.88 build, selected features, distributable Linux startup, v1/v2 upgrades and process-kill/atomicity/fence/quota/reservation tests for all new transactions. |
| Q3 embeddings | Safe shape/index/rank APIs; checksummed artifact/tokenizer parity; matched FP32 cosine >=0.999, normalization, batch isolation and generation-switch/deletion coverage. |
| Q4 extraction/answers | At least 120 held-out cases plus separate development cases; extraction precision >=95%, recall >=90%, zero critical identity/attribution/correction errors; supported answers >=90% and unknown abstention >=95%. |
| Retrieval | Direct/paraphrase evidence Recall@10 >=95%; complete-path recall >=90% for three/six/eight-edge cases, with missing/reversed edges, cycles/hubs and current/historical queries. |
| Q5 resources | Paired cold/warm foreground runs under queued background load; p95/p99, CPU/RSS/VRAM, embedding/prefill time and oldest backlog. Target p95 initial recall <250 ms at 100K records and <10% foreground first-token regression; report supported capacity if a target fails. |
| Q6 personality | Deterministic edit/reducer/rebuild replay and paired fixed-evidence profiles; factual conclusions/permissions unchanged, tone adherence reviewed, in-flight edits safe. |
| Q7 initiative | Fake-clock timezone/DST/quiet/dismissal/restart races; >=90% useful reviewed candidates with missed opportunities reported; opt-in local-use week before live promotion. |
| Q8 security/UI/recovery | Unauthorized/expired/revoked route and stream matrix; injection, lost replies, restart resnapshot, deletion during blocked delivery/export, older-backup restore and atomic activation failures. |

Use six held-out groups of at least 20 cases: personal facts/attribution;
English/Ukrainian paraphrase; corrections/time; chains/missing premises;
goals/reasons/commitments; unknown/injection/trust boundaries. Review/freeze
expected evidence and category counts before tuning. The nineteen development
examples and nine-step narrative are not held-out or executed native results.
Score extraction, retrieval and answer reading separately, with human-reviewed
ambiguous cases; a model judge alone is not the authority.

Default tests use temporary stores, scripted engines/embeddings and injected
time. Dedicated native runs require the declared artifacts and fail if absent.
Use isolated quality fixtures while retaining production budgets in each case;
separate load tests prove the real rolling-day/backlog limits. Inventory 10K,
100K and 1M stores where resource budgets permit; one million 384-float vectors
already require 1.536 GB before metadata, text, WAL and model residency.

The final walkthrough must execute this story through both the headless core
and authenticated WebUI:

1. Preserve Lena/sister/veterinarian/Lviv from ordinary dialogue; restart and
   clear working history. Ask a paraphrased English/Ukrainian question whose
   answer needs the relationship/occupation chain and show its sources.
2. Add an explicit goal, reason and blocker. Continue a different session and
   demonstrate the assistant knows the current unfinished situation.
3. Correct the city while a job/answer is in flight. Reject obsolete work and
   answer with the corrected evidence; distinguish historical questions.
4. Edit personality and an expiring disposition. Keep facts fixed while the
   next operation adopts the selected voice; replacement preserves memory.
5. Resolve the goal's blocker and schedule a concrete one-shot commitment.
   Produce inbox notices, exercise quiet/activity policy and dismiss/snooze;
   restart without duplicate causes or free quota.
6. Forget while a stream/export/job is blocked. Confirm the journal/publication
   barrier, drain purge, restore a pre-forget backup and prove no resurrection.
7. Lose acceptance/terminal replies and kill the process around a tool effect.
   Reconcile the same key, preserve known receipts and leave uncertainty honest.
8. Run memory disabled and web disabled independently; verify regressions and
   zero idle model inference when no eligible work exists.

Run workspace Rust gates, Rust 1.88 storage/integration tests, relevant binding
native tests, frontend check/test/build and browser tests. Update architecture,
LLM/memory/web contracts, model preparation instructions, owner startup/controls,
recovery examples and the documentation map. Report existing lint debt separately
from new warnings; do not edit generated bindings or weaken safety gates.

## Completion and timed validation

Implementation completion means every selected capability above exists, is wired
through the actual application/adapters and has executable acceptance coverage.
No required path is a stub, an uncalled service, mock-only inference or another
unimplemented milestone. Deliver the run commands, artifact identities, measured
reports and known limits with the change.

Complete v1 release qualification still requires Q1-Q8, including real model
results and the previously selected opt-in week. Calendar time and human
usefulness review cannot be replaced by passing code tests. The final task must
supply the working reviewed-inbox/live delivery paths and their instrumentation;
live remains disabled until its promotion evidence exists. This is a validation
boundary, not permission to defer implementing initiative.

If an empirical gate fails, fix what can be fixed within this same delivery and
report the exact remaining blocker. Do not declare complete v1 by silently
lowering thresholds, dropping semantic memory/personality/initiative, or presenting
another storage-only result. No further architecture-research phase is planned.

## Primary research checked for this revision

- E5's [configuration](https://huggingface.co/intfloat/multilingual-e5-small/raw/main/config.json)
  and [model card](https://huggingface.co/intfloat/multilingual-e5-small) establish
  the mixed architecture/tokenizer and interface contract. Native parity remains
  unmeasured here; converter risk is an inference from those and the pinned code.
- [LongMemEval](https://arxiv.org/abs/2410.10813) separates extraction,
  multi-session/temporal reasoning, updates and abstention. These motivate
  separate evaluation tracks, not a claim that its benchmark was run here.
- [Tokio JoinHandle](https://docs.rs/tokio/latest/tokio/task/struct.JoinHandle.html)
  documents detached tasks on drop and blocking-task cancellation limits;
  [Notify](https://docs.rs/tokio/latest/tokio/sync/struct.Notify.html) carries wake
  signals rather than work data. Owned completion and SQL cursors follow from
  these contracts.
- [SQLite isolation](https://sqlite.org/isolation.html) permits old reader
  snapshots after a writer commits; application publication revalidation is an
  additional requirement, not a guarantee supplied by WAL.
- [OWASP session guidance](https://cheatsheetseries.owasp.org/cheatsheets/Session_Management_Cheat_Sheet.html)
  supports careful credential exchange/lifecycle handling. The selected local
  pairing/bearer design and exact time limits are project choices.

This revision inspected source/artifact availability and primary documentation.
It did not change production code, run a new model benchmark or repeat the
previous milestone's Rust tests.
