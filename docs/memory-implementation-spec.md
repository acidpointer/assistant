# Memory, personality and assistant loop: implementation specification

> Current authority: [ADR 0007](decisions/0007-universal-assistant-memory.md) and
> [the user guide](assistant-memory.md). Universal source graphs, freeform
> personality and one initiative loop are implemented. The specialized extraction,
> fixed personality/mood and synthetic-corpus requirements below are historical
> design proposals and are superseded. Remaining restore/forgetting UI limits are
> listed explicitly in the user guide.

## Historical specification


The [final integration plan](memory-integration-plan.md) audits the committed
storage foundation and specifies one complete remaining delivery, including
model qualification and end-to-end acceptance. This specification remains the
authority for selected v1 behavior.

Status: implementation design baseline, transport/UI refinement 2026-11-06.
Application code remains
human-owned; this document does not authorize implementing it. It selects the
first complete system from the alternatives in [the research](memory-system-plan.md)
and [protocol experiments](compact-model-protocol.md). Where they differ, this
specification takes precedence for v1. It is not an accepted ADR or a claim of
production qualification.

Implementation-ready means each subsystem has a selected contract, owner,
failure behavior and acceptance test. It does not mean unbuilt code, an untested
model or a dependency has passed its release gate. The qualification register
below identifies those remaining empirical requirements without leaving the
implementation dependent on an unspecified architecture decision.

The [storage implementation](memory-storage.md) supplies Linux library primitives,
ordered migrations, operation/tool settlement commands, reservations and
deterministic failure tests. The opt-in Linux headless path now participates in
durable admission, tool settlement, finalization and E5 semantic recall.
Automatic graph formation remains inactive after a failed extraction preflight;
personality, initiative and authenticated memory HTTP integration remain unbuilt.
See the [executable checkpoint](memory-native-integration.md) for evidence and
the [current delivery refinement](memory-integration-plan.md#core-boundary-and-webui-delivery-refinement--2026-11-06)
for shared application contracts, first-client UI journeys and implementation
order. The selected wrapper is rusqlite 0.39.0 after the 0.40.1 candidate failed
the actual Rust 1.88 build; the workspace MSRV is unchanged.

## Scope and closed decisions

The complete v1 includes durable capture and explicit memory, extraction,
temporal claims and evidence, lexical/semantic/graph recall, bounded internal
model jobs, situation tracking, one-shot commitments, an initiative inbox and
optional live notices, editable personality and deterministic temporary
disposition, inspection/correction/forgetting, backup/recovery and compiled
extension points, with an optional authenticated HTTP/WebUI adapter.
One Linux OS owner, one assistant identity,
one active product interaction and one local data directory are supported.

HTTP is not a memory dependency. The owner's 2026-11-06 delivery requires the
authenticated WebUI as its first real client, while the core stays independently
usable and testable. `--no-web` omits the web plugin, listener,
browser sessions, SSE and web-pairing endpoint; memory/personality services,
background jobs and the durable initiative inbox continue running. No transport
is required for background progress. Without a delivery adapter, notices remain
in the inbox subject to their expiry policy; they are not falsely marked delivered.

The core exposes typed Rust handles and transport-neutral application events.
An authorized in-process host can submit inputs, inspect/correct/forget memory,
edit personality and consume notices without serializing HTTP or calling a local
HTTP server. CLI and future Unix-socket/D-Bus adapters use these same commands;
each authenticates its caller and obtains a host-issued capability. Existing
low-level diagnostic `Chat` calls remain ephemeral unless explicitly integrated
with the durable product path. Headless mode does not bypass memory authorization
or mandatory persistence.

Adapters call an owner-authorized application facade, not a collection of raw
store mutations. The core owns command orchestration, revision/intent checks,
durable lookup/history and publication generations; adapters own authentication
and encoding. A revocable core access grant represents an authenticated owner
without embedding bearer/session/D-Bus/socket types in memory. Its identity does
not enter durable client-command digests. Disconnect closes connection-owned
readers; revoke also invalidates the grant and blocks new commands. Accepted
operations retain owned settlement. Cancellation remains an explicit command.

The web plugin depends on core capabilities and readiness; memory never depends
on web types or activation. Its pairing mechanism is installed only with the web
adapter. A future general Unix-socket transport is a distinct optional adapter,
not a requirement introduced by browser pairing.

Alternative databases, ANN, vocabulary training, a symbolic protocol, third-party
dynamic code loading, multi-owner service, recurring calendars, remote listeners,
external autonomous actions and learned psychological adaptation are outside v1.
These exclusions do not remove semantic recall, personality or initiative from
the complete delivery. Each is implemented and qualified before v1 is declared
complete; degraded modes are recovery behavior, not substitutes for acceptance.

| Gap in the research | Selected v1 decision |
| --- | --- |
| Multiple storage/codec options | Bundled SQLite, exact vector search, named JSON proposals; compact codecs remain an independent experiment |
| Lifecycle presence versus readiness | One composite memory/personality factory; explicit readiness latch gates personal admission and web activation |
| Optional observer versus durability | Exactly one required persistence participant outside middleware, with an explicit disabled variant |
| Unknown schema/command shape | Typed contracts below plus checked-in extraction schema, example and SQL constraint sketch |
| Authentication bootstrap unspecified | Local Unix-socket pairing; short-lived one-time code exchanged for an in-memory bearer session |
| Graph/temporal ambiguity | Immutable record versions, directed qualified claims, finite predicate registry and scoped revision checks |
| Correction/deletion fan-out | Immediate principal epoch/suppression barrier; resumable physical repair/purge |
| Personality behavior unspecified | Integer trait/disposition values, deterministic compiler and reducer rules below |
| Unbounded queues/storage/retries | Concrete default budgets, separate critical reservations and failure states |
| Recovery/restore unclear | Ordered startup recovery, persistent fences, external deletion-journal reconciliation and quarantined restore |
| Unmeasured model behavior | Fail closed at feature promotion; exact artifacts and pass/fail experiments in the qualification register |

## Ownership and readiness

`MemoryPlugin` installs a restricted `MemoryHandle`, internal
`AgentStateRepository`, the persistence participant, memory tools and a composite
runtime factory. `PersonalityPlugin` contributes configuration and its validated
handle to this factory without starting a second writer. A private builder
collects these contributions before `build`; registration performs no I/O.
Memory can run with a fixed neutral profile if no personality plugin is installed.

`RuntimeContext` gains a restricted internal-inference handle. It accepts owned
jobs and does not expose `Chat`, product submission or ordinary tools. Handles
must not capture an owning runtime/pipeline reference that forms a cycle.
The factory owns tasks and storage, while jobs hold command senders and borrowed
capabilities. Native models/contexts remain confined to existing owner threads.

Startup order:

1. Validate configuration, dependency registrations, paths and resource sums.
2. Acquire exclusive data-directory lock; open/migrate storage and verify pragmas.
3. Reconcile deletion journal, reclaim previous-daemon leases, interrupt unfinished
   operations, resume suppression barriers, load profile and dispositions.
4. Load native models; validate manifest and worker capabilities. Required-model
   failure fails startup, optional semantic model failure sets degraded health.
   Validate profile token packing against the now-loaded chat tokenizer before
   activating its behavior snapshot; earlier profile loading checks only schema.
5. Activate storage consumers, personality snapshots and the internal scheduler.
6. Release readiness, then enable authenticated product admission and transports.

On failure unwind every successfully prepared resource in reverse order. Recovery
does not replay tools or publish old notices before current policy is loaded.
Shutdown closes admission, quiesces transports/job claiming, cancels active
generation, records outcomes, joins service tasks and native users, then closes
the writer. The writer must remain available until all mandatory finalization
commands have settled. Do not implement shutdown by dropping a timed-out future
that owns unfinished finalization. An uninterruptible native call can extend
shutdown; never free its referenced owners early.

The current runtime closes services before joining model workers; this needs an
explicit completion barrier for memory jobs before closing storage. Existing
`require<T>` checks presence only. Neither behavior establishes the ordering
above without implementation changes.

## Typed interfaces and error semantics

All public handles use owned request/response structs, typed IDs, validated
budgets and `MemoryError`; no database handles or arbitrary query strings escape.
Use private fields and constructors for scope, version and limits. Host-issued
`OwnerCapability` and `OperationContext` are not deserializable model arguments.
Debug output redacts private text, credentials, paths and full model responses.

| Operation | Required input | Successful result |
| --- | --- | --- |
| Admit | Owner capability, client namespace/key, literal input, behavior selection, reservation | Operation ID, commit sequence, durable acceptance |
| Finalize | Operation ID, terminal status, visible messages, bounded receipts | Idempotent terminal receipt |
| Lookup/status | Original client namespace/ID/request key or stable operation ID | Current durable operation/receipt, without resubmitting input |
| Snapshot/history/subscribe | Access grant, bounded page/cursor | Visible state/history and atomically registered subsequent events |
| Remember | Current authorized source selection, preserve flag | Source receipt with stored/structured/indexed states |
| Search | Query, typed filters, minimum commit sequence, budget | Evidence bundle, coverage by channel, snapshot sequence |
| Follow | Offered reference/continuation, finite pattern, remaining budget | Qualified path bundle and single-use continuation |
| Correct | Target IDs/revisions, replacement/source, intent capability | New versions, invalidation epoch and commit receipt |
| Forget | Reviewed selection token, expected revisions, intent capability | Suppression/purge operation ID and barrier state |
| Patch/replace personality | Expected revision, typed profile edit, activation mode | New profile generation/revision and effective boundary |
| Set disposition | Expected revision, override, expiry | State revision and expiry |
| Inbox action | Notice revision, seen/dismiss/snooze action | Durable action receipt |
| Inspect/export | Typed selection, cursor, byte limit | Bounded records with provenance, or resumable export operation |
| Backup/restore | Host-managed backup ID/selection, expected generation and reviewed activation intent | Tracked candidate/activation operation and recovery state |

Each mutation carries a unique client operation key and a versioned canonical
input digest. Canonicalize by serializing the validated typed request in a fixed
field order; do not hash arbitrary JSON key order. Duplicate key/equal digest
returns the original receipt; changed digest yields `Conflict`. Store receipts
and deduplication state for the lifetime of retained operations. Forgetting may
erase payload/digest but keeps a minimal spent-key record; reuse then yields
`Gone`, never a fresh execution. Namespace reset is a separate owner action.

Errors: `InvalidInput`, `Unauthorized`, `Forbidden`, `NotFound`, `Gone`,
`Conflict`, `Busy`, `Capacity`, `Unavailable`, `Deadline`, `Cancelled`,
`Indeterminate`, `UnsupportedVersion`, `CorruptStore`. Distinguish valid empty
results from failure and from partial coverage. `Indeterminate` includes the
operation key for status reconciliation, not a suggestion to invent a new key.
After admission, cancellation is an outcome to persist, not rollback of input.
User-visible failures contain stable codes and retry eligibility, never native
diagnostics or source text. Clients can query operation status after timeout.

Configuration uses `MemoryMode::Disabled` or `Required(MemoryConfig)`; required
means accepted personal operations require persistence, not that every index is
healthy. Configuration includes data directory, model manifest, budgets, capture
policy, profile, attention policy and timezone. Keep model artifact/path and
storage settings startup-only. Profile, disposition, quiet hours, initiative and
quota settings update through revisioned owner commands; quota reductions cannot
invalidate outstanding reservations. Disabled mode does not mount personal v2
routes. Config errors fail before opening traffic.

Health is component-based: storage `ready/blocked/corrupt`, semantic index
`ready/building/unavailable`, extraction `ready/backlogged/unqualified`, attention
`inbox_only/live/disabled`, plus oldest pending age and bounded counters. Storage
blocked stops admissions; optional degradation remains visible on every affected
query. No single “healthy” boolean hides missing channels. Read-only owner recovery
remains available where storage integrity permits it; corrupt stores are never
automatically overwritten.

Direct `Chat` remains ephemeral without owner capability. Required memory mode
cannot be bypassed by calling a permissive hook. Host tools validate capabilities
again at dispatch; middleware selection does not create permission.

## Storage and transaction specification

Use one writer thread and two blocking reader connections. Begin authoritative
writes explicitly, read back `journal_mode=WAL`, `synchronous=FULL`,
`foreign_keys=ON`; reader snapshots end before inference. Reject unsupported
schema versions rather than creating a fresh empty store over them. Record
checksummed ordered migrations. SQLite documents connection-local settings and
WAL durability tradeoffs; verify the actual runtime rather than trusting a
successful pragma call. [SQLite pragmas](https://www.sqlite.org/pragma.html#pragma_synchronous).

Default data location is the owner's XDG data directory under `assistant/`, or
an explicit owner-supplied directory. Create it with mode 0700 and files with
0600, validate owner UID, reject symlinked lock/database/journal entries and
acquire the process lock before opening storage. Use descriptor-relative file
operations where possible to avoid check/open races. Backup/export destinations
are host-selected, never model arguments. Unsupported filesystems or permission
errors fail readiness with sanitized codes.

Stable IDs are 128 random bits encoded as 32 lowercase hexadecimal characters,
generated with the existing `getrandom`; checked entropy failure propagates.
Store revisions/sequences as positive signed-64-bit integers with overflow checks;
serialize them as decimal strings in HTTP to avoid JavaScript precision loss.
Time is UTC integer milliseconds plus optional IANA timezone/local-time intent.
Unknown valid time is null; known intervals are half-open. No implicit sentinel
date, implicit ingestion-time event date or universal last-write-wins.

The [SQL constraint sketch](memory-schema.sql) is executable design material,
not a production migration. It freezes principal-consistent identity/version,
evidence, job fence and outbox constraints. Its deliberately narrow columns are
complemented by this table of required production storage responsibilities:

| Records | Additional production fields/indexes and rules |
| --- | --- |
| Operations/tool invocations | Client namespace/key/digest, terminal receipt, cancellation/unknown-effect state, reservation; unique invocation ordinal per operation |
| Episodes/source revisions | Role/attribution, exact bounded bytes, observed/valid time, extraction cursor, preservation policy; unique operation/ordinal/revision |
| Nodes/claim versions | Entity kind, typed object, polarity, modality, lifecycle, valid and recorded intervals; subject/predicate and both adjacency directions indexed |
| Aliases/equivalences | Normalized lookup plus original spelling and supporting source; ambiguity allowed, no destructive entity merge |
| Evidence/dependencies | Exact source revision/span or structured receipt field; reverse dependency indexes; transitive derivation requires checked premises |
| Predicate scopes | Revision per principal/subject/predicate scope; changed on relevant inserts as well as updates, preventing phantom conflicts |
| Jobs/attempts | Kind/source/version key, fence, lease instance/expiry, next due, persisted budget reservation, retry count and output reason code |
| Events/consumers | Per-principal monotonic sequence, root cause, changed IDs; cursor advances atomically with dependent job creation |
| Search documents/FTS | Source/version, normalized text separate from immutable evidence; FTS updates in the same transaction |
| Embeddings/index generations | Manifest digest, source revision/hash, finite little-endian f32 vector, dimension/normalization; active-generation pointer and coverage watermark |
| Profiles/dispositions/appraisals | Agent identity, immutable versions, current pointer, reducer/compiler version, override expiry, supporting revisions and generation |
| Situation/subscriptions/notices | Typed goal/commitment/question state, predicate subscriptions, bounded fan-out cursor, cause identity, occurrence, policy/profile revision, delivery/user-action state |
| Deletion/retention/export | Suppression selection, deletion generation, journal acknowledgment, resumable purge/export cursor; no forgotten prose in tombstones |

Atomic writer commands own the following transactions: admission; tool intent;
tool receipt; finalization; remember; graph proposal application; job claim;
job completion; profile/disposition update; event consumption; notice publish;
notice action; suppression; purge batch; index-generation switch. No extension
owns an independent commit for part of these commands.

Job claim increments its fence and charges reserved work. Completion validates
fence, daemon instance, source/deletion generation, read-set revisions and
predicate-scope revisions before committing effects plus job completion.
Stale results become obsolete/reconsidered; they never overwrite newer state.
Source-backed duplicates merge evidence identities without counting repeated
copies as independent support. Derived records use the eligibility epoch.

Advance that epoch on every source-backed factual graph mutation, including
insertion of a new blocker or contradictory assertion, not just corrections and
deletion. Otherwise an old conclusion based on absence could remain eligible.
Derived-only recomputation does not advance it recursively. Proposals validated
in a transaction use its resulting epoch; unchanged direct facts remain readable.

For FTS v1 use ordinary contentful FTS5 with `unicode61`, updated centrally with
search documents. This deliberately duplicates text, simplifying initial update
and delete logic. Purge both copies and test rebuild parity. Do not introduce
external-content triggers until measured storage cost warrants the added coupling.
FTS has its own query syntax even with SQL parameters: construct bounded literal
terms and quote them. [FTS5 documentation](https://www.sqlite.org/fts5.html).

## Extraction, predicates and evidence

Use [extraction schema v1](memory-extraction.schema.json) for named JSON output.
The host provides source handles, known entity/claim handles and expected
revisions; the model returns new entity candidates and add/correct proposals.
The [contract examples](memory-contract-examples.json) include a complete happy
path and adversarial semantic cases. JSON validity is only the first check.

Jobs initially contain one source excerpt, at most 1,024 chat tokens. Split long
episodes on paragraph/sentence boundaries with up to 128 tokens of overlap and
retain absolute source-span mapping. An oversized sentence splits on tokenizer
boundaries that also preserve valid UTF-8. Mark partial source coverage; overlapping
chunks do not create independent evidence. At most four claims per pass, 512
output tokens and one targeted repair/reconciliation pass. A truncated response
commits no proposal. The next source cursor advances only over examined content;
retry exhaustion leaves that span unresolved and available to literal recall.

The schema's `coverage=partial` does not locate an examined prefix. Therefore it
never advances the source cursor: retain validated proposals idempotently and
split/retry the same excerpt with smaller host-defined bounds. Only a complete
pass advances across its entire offered excerpt. This avoids relying on model
character arithmetic to locate an unprocessed tail.

Host validation order: complete parse -> schema/size -> handles/kinds -> unique
exact quotation spans -> speaker/modality -> predicate types -> temporal/cardinality
rules -> revision/read set -> transactional apply. Duplicate quotations require
a more specific supplied excerpt; the host never guesses a matching occurrence.
Source attribution is supplied by host metadata, not a trust flag from the model.
Semantic entailment is evaluated and can remain unresolved; a schema cannot prove it.

Initial predicates, with inverse traversal generated by host code:

| Predicate | Subject/object | Change rule |
| --- | --- | --- |
| `prefers`, `interested_in` | Person -> entity/string | Additive; a correction targets a specific claim/dimension |
| `works_on`, `member_of` | Person -> project/organization | Multiple allowed; qualify valid time |
| `sibling_of`, `parent_of`, `partner_of` | Person -> person | Preserve relationship direction; sibling/partner symmetry is navigation, not independent evidence |
| `has_occupation`, `lives_in` | Person -> occupation/place | Qualified personal facts; a changed value requires explicit temporal/correction evidence |
| `owns`, `uses` | Person/agent/organization -> entity | Multiple allowed, source-backed and qualified |
| `has_goal` | Person/agent -> goal | Multiple, explicit actor |
| `blocked_by`, `depends_on`, `reason_for` | Goal/project/commitment -> entity/goal/claim | Directed and evidence-backed; relation alone never proves causality or transitivity |
| `status` | Goal/commitment/project -> finite status value | Exclusive per scope/time only when explicit replacement/correction is established |
| `due_at` | Commitment -> timestamp | Validated explicit instant; unresolved timezone/date is not scheduled |
| `asks` | Person/agent -> question | Question is not a factual assertion |
| `associated_with` | Any node -> node | Navigation only, excluded from factual derivation |

Supported status domains: goal `active/completed/abandoned`; commitment
`pending/fulfilled/cancelled`; project `planned/active/blocked/completed`.
Negative claims retain polarity; questions/hypotheticals/quoted reports remain
distinct modalities and cannot silently promote to positive current facts.
Free-form new predicates remain unresolved, not dynamically executable ontology.
New predicate kinds require a registered versioned host definition and fixtures.

Correction of known targets is synchronous. An ambiguous natural-language
correction cannot have a guaranteed immediate graph update: retain its literal
words and expose a pending conflict; ask for a target instead of claiming success.
This limits the earlier “immediate correction” promise to identified targets.

## Retrieval and embedding contract

Ordinary recall uses current input plus bounded recent referents; it does not
make a planning LLM call. Run exact alias, FTS and semantic channels; fetch up to
32 candidates each and apply reciprocal-rank fusion with constant 60. Stable ID
breaks ties. Deduplicate source families, reserve up to four slots for relevant
pending/correcting evidence, then traverse typed patterns under the research
budgets. An unresolved correction wins over a confident stale presentation.

Each bundle contains typed claims, literal excerpts, source kind/revision,
qualification, valid time, retrieval snapshot, index manifest/generation,
truncation and per-channel coverage. Preserve complete chains/qualifications
when packing. Base limit: 1,024 chat tokens; count through the loaded tokenizer.
Empty, unavailable, pending and budget-exhausted are distinct states.
Search honors `min_commit_seq`. Copy evidence in a short snapshot; before model
dispatch validate selected revisions and epochs. No inference holds a SQL reader.

A correction affecting evidence already used by an active run cancels further
publication from that run and finalizes it as interrupted. A bounded restart may
reprepare with current evidence only when no tool effect would be replayed;
otherwise the next interaction continues from known receipts. Unrelated factual
inserts do not restart a direct-fact answer. A derived-evidence epoch change
requires revalidation or interruption. Bytes delivered before the correction
cannot be recalled.

Packing order reserves current input, active tool protocol and minimum output
first, then the configured persona and evidence allowances. Drop optional old
exchanges and low-priority evidence before reducing those mandatory sections.
If a necessary correction/qualification cannot fit, return an explicit budget
failure or qualified incomplete coverage; never keep the stale fact alone.
The model's full template/tool-schema token count is authoritative, not UTF-8
length or the nominal 1,024-token evidence allowance.

Select multilingual-e5-small as the first embedder family, FP32 vectors with mean
pooling, L2 normalization, 384 dimensions and its query/passage prefixes. Keep
total tokenizer input including prefix/special tokens within 512. Document
chunks target 384 tokens with 64-token overlap, split at valid UTF-8 boundaries;
keep immutable source mappings. Claims may use shorter standalone documents.
These model interface requirements come from the
[E5 model card](https://huggingface.co/intfloat/multilingual-e5-small).

A required model manifest fixes source revision, license reference, tokenizer,
GGUF SHA-256, converter revision/options, dimension, pooling, precision, prefixes
and reference-vector fixture hash. No auto-download or model-path selection by
the LLM. Reject zero norm, NaN/Inf, wrong shape or incompatible manifests.
Model family selection is closed; artifact promotion remains a qualification gate.

Embedding input uses a dedicated owner worker and the existing backend lease.
The binding output-length/index/lifetime audit in the research is a hard
prerequisite. CPU embedding is the initial placement; compare GPU only after
measuring interference. Exact similarity scans owned blocks under byte/time
budgets and filters authoritative eligibility. Timeout returns partial coverage,
not “no match.” Index builds copy new-generation vectors in bounded pages;
switch only after coverage and source/deletion catch-up checks. Old generation
stays available until the switch; never compare across manifests.

## Personality compiler and temporary state

Profile v1 fields: bounded name (128 UTF-8 bytes), voice/backstory/values prose
(combined 8 KiB), trait integers `warmth`, `directness`, `playfulness`, `curiosity`,
`initiative`, `skepticism` in 0..100, optional named descriptive facets (maximum
16, 128-byte keys and 512-byte values). Unknown facets have no operational effect.
Defaults are 50; the user may replace the whole profile. Profile prose is counted
and packed to at most 512 chat tokens; reject an authored profile that cannot fit
its declared character budget rather than silently discard selected instructions.

Disposition v1 uses integers `energy`, `social_appetite`, `curiosity_delta` and
`irritability` in -100..100, baseline zero. Automatic transitions are opt-in;
the default is user overrides plus decay. Overrides last one hour by default,
maximum 24 hours, and revert to baseline unless a new owner edit changes them.
Expiry survives restart. Reducer v1 decays each absolute dimension by ten points
per complete elapsed hour, toward zero, retaining the fractional-hour remainder;
backward wall time performs no reverse decay. Zero state schedules no work.

When enabled, explicit positive feedback adds 5 social-appetite points; an
explicit request for less engagement subtracts 10. A completed user-authorized
task adds 5 energy points. Clamp to bounds, apply once per event ID, and retain
source cause. Do not infer anger from punctuation or model-generated emotional
language. No mood event by itself creates a notice. More sophisticated adaptation
is a versioned extension, not an implicit training loop.

Compiler v1 resolves `curiosity_effective = clamp(curiosity + curiosity_delta/4,
0,100)` and `initiative_effective = clamp(initiative + social_appetite/4,0,100)`;
integer division truncates toward zero. Curiosity bands 0..33/34..66/67..100 allow
0/1/2 optional explicit investigation continuations, still under cumulative host
limits. Skepticism >=67 reserves one optional counterevidence search if budget
allows; it cannot downgrade direct corrections or fabricate contradiction.
Other traits/dispositions affect bounded voice directives only in v1.

Initiative bands choose at most 0/1/2 live notices per rolling 24 hours, capped
by owner preference, default zero until explicitly enabled. All eligible notices
can enter the bounded inbox; reserved personalities still retain due commitments.
Profile replacement resets disposition and advances generation; preserved memory
and commitments remain intact. One behavior snapshot is pinned at admission.
`next_operation` is default activation; `interrupt_active` uses cancellation,
mandatory finalization and the output barrier without replaying tool effects.

The personality compiler never changes evidence eligibility, retention, authority
or factual extraction. Character-dependent appraisals/drafts are separately
versioned and invalidated after rebuild. Concurrent edits require expected
revision; no field-wise merge of conflicting edits. Snapshots identify compiler,
profile, disposition and host-policy revisions for reproducible diagnostics.

## Assistant loop, resource policy and attention

Consumers persist cursors and create jobs in the same transaction. A wake signal
is only a hint. Register wake observation before checking the next due item, then
sleep until notification/due/shutdown. Drain 32 events per iteration; use persisted
fan-out cursors for hubs. Generated changes retain root cause and do not grant
new compute. Required consumers determine the event-retention low watermark.

Initial defaults, configurable within validated hard limits:

| Resource | Default and response at exhaustion |
| --- | --- |
| Writer queue | 64 bounded commands; 8 slots reserved for admitted-run finalization/receipts; new admission returns Busy when ordinary capacity is unavailable |
| Background scheduler | 128 in-memory descriptors, 10,000 durable pending jobs; coalesce version-identical work and suspend new optional jobs at quota |
| Model concurrency | One background generation; one reserved interactive context and run permit; insufficient context capacity disables background with explicit health |
| Job lease/deadline | 60-second lease, heartbeat at 20 seconds, 120-second hard job lifetime; fence required for every completion |
| Retries | Two transient retries after the first attempt, 5/30-second delays; one malformed-output repair within the same root allowance |
| Root-cause compute | At most four model passes, 4,096 evaluated input tokens and 1,024 output tokens; work beyond allowance stays unresolved/deferred |
| Background daily compute | 60,000 evaluated input tokens, 10,000 output tokens and 300 seconds native execution per rolling 24 hours; all three limits enforced |
| DB/query | 100 ms busy allowance, 250 ms normal query target/deadline, 1-second writer-operation deadline; timeout reports actual commit uncertainty |
| Source/command | 16 KiB input source, 128 KiB visible assistant message, 16 KiB tool receipt; existing run-wide limits also apply |
| Store budget | 2 GiB combined DB/WAL/index files, 64 MiB emergency margin plus outstanding reservation accounting; never evict explicit memory silently |
| Maintenance | 256 records or 1 MiB per batch, whichever first; yield to critical/interactive commands between batches |
| Notice storage | 1,000 retained notices; expire/purge eligible dismissed/expired rows before refusing optional candidates |
| Disposition/notice checks | No periodic inference; at most 60-second clock recheck while a timer is armed, plus resume notifications |

Defaults are experiment settings, not measured capacity claims. Charge input
prefill on each retry, output and native time; crashes retain conservative
reservations. A pass cannot start if its reserved maximum exceeds remaining
allowance. Background limits do not consume interactive allowance. Admission
reserves a bounded worst-case finalization envelope derived from run limits,
including tool receipts, plus measured SQLite/WAL overhead. Disk margin cannot
guarantee writes against external disk exhaustion; required storage failure
stops new durable admissions and preserves indeterminate state.

Foreground pressure pauses background only at native owner boundaries. Paused
context retention counts toward memory; after hard lifetime expiry cancel/requeue
with a new fence and charged prefill. Do not claim preemption during synchronous
allocation, rendering or one native decode call. Query embedding has its own
queue, shared compute accounting and a foreground slot.

Attention v1 rule eligibility is deterministic: due pending commitment, or a
newly resolved blocker of an active goal. Confirm all required premises; an
unresolved additional blocker changes the suggestion, never claims full readiness.
No generic model score decides eligibility. Draft only after eligibility and
budget checks. A template-backed factual inbox card is available when drafting
fails; live prose delivery waits for its qualified model path.

Default notice expiry is 24 hours for blocker suggestions and seven days for
overdue commitment cards, with commitment state retained independently. Minimum
live gap is two hours; defer while a run is active or for five minutes after
the last user activity. Owner-configured quiet hours override personality.
Dismissal suppresses the exact cause permanently while that cause exists;
snooze defaults to one hour within expiry. New relevant external evidence or a
new schedule revision is required to create a different cause.

One-shot due times require an explicit offset or resolved IANA zone. Ambiguous
or nonexistent local time returns a clarification requirement, never a guessed
instant. Store the selected occurrence plus original local intent and timezone
database version; timezone-rule upgrades do not silently move existing instants.
Recurring commitments are explicitly rejected in v1. Use `chrono`/`chrono-tz`
for civil-time mapping instead of hand-written DST rules; they expose ambiguous
local-time cases. [Chrono](https://docs.rs/chrono/latest/chrono/),
[Chrono-TZ](https://docs.rs/chrono-tz/latest/chrono_tz/).

## Authentication and transport contract

When the optional HTTP adapter is installed, memory-enabled v1 binds only loopback
and uses the existing authority/origin/CSP
defenses. Local TCP is not authentication. Root or a hostile process running as
the same OS owner can access that owner's files/process; this design does not
claim isolation from them. Different-UID local clients and browser origins
receive no personal access without a session.

Provision through a 0600 Unix socket under the owner's 0700 runtime directory,
with peer UID checked. A local `assistant pair` command requests a 256-bit random,
single-use code valid for five minutes, displayed only interactively, not in
daemon logs. The browser submits it in a JSON body to `/api/v2/session`; never put
secrets in URLs. Limit exchange attempts to five per minute globally and allow
only one outstanding pairing code, replaced only by an owner socket request.

Successful exchange consumes the code and returns a random 256-bit bearer token
with eight-hour absolute and 30-minute idle expiry. Keep session token digests
in daemon memory; restart requires pairing again. Browser stores its token only
in tab memory, not persistent storage. Reload pairs again in v1. Authenticated
HTTP and fetch-based SSE send Authorization headers; EventSource URL tokens and
cookies are not used. This selects one concrete flow over the earlier cookie
proposal. Browser reload convenience can be added after this contract passes.
Session generation, expiry and revocation follow the principles of
[OWASP session guidance](https://cheatsheetseries.owasp.org/cheatsheets/Session_Management_Cheat_Sheet.html).

Require exact same-origin headers on browser mutations in addition to JSON and
the existing custom header. No credentialed cross-origin CORS. Logout or owner
revocation closes the session's subscriptions and removes queued personal
responses. All personal routes, including old v1 chat/bootstrap routes, require
authentication in enabled mode; a legacy endpoint cannot become a bypass.
Only static UI assets, minimal session exchange and `GET /api/discovery` are
public. Discovery returns only API version and memory-disabled versus
pairing-required mode. No raw diagnostics
endpoint exposes memories, model paths or credentials.

Use `/api/v2` for the complete durable contract; v1 remains only for memory-disabled
compatibility. Commands return 200/201 once committed, or 202 with a status URI
only after a durable operation record exists. Never call an in-memory reservation
durably accepted. Use 400 invalid, 401 unauthenticated, 403 forbidden, 404 unknown,
409 revision/key conflict, 410 spent/deleted, 413 too large, 429 capacity and 503
unavailable/indeterminate (with operation key). An indeterminate response is not
a definite rejection.

Required endpoint families: session create/delete; submit/status/cancel; snapshot
and events; memory search/evidence; remember/correction; forget preview/execute/
status; personality get/patch/replace; disposition override; inbox list/actions;
memory health; export request/status. Routes use typed bodies corresponding to
the command table; do not expose raw SQL or model schemas as public authority.
Mutating bodies require `request_id` and expected revisions when applicable.
Pagination is keyset-based with principal-bound expiring opaque cursors.

Concrete v2 route mapping (all IDs are opaque; scope comes from the session):

| Method and path below `/api/v2` | Command/result |
| --- | --- |
| `POST /session`, `DELETE /session` | Pairing exchange; logout/revoke current session |
| `POST /runs`, `GET /operations/{id}`, `POST /runs/{id}/cancel` | Durable submit; status/receipt reconciliation; cancel |
| `POST /operations/lookup` | Authorized lookup by original client namespace/ID/request key when an admission reply was lost; no new execution |
| `GET /snapshot`, `GET /events` | Atomic display snapshot; authenticated fetch-SSE |
| `GET /history` | Bounded keyset-paginated visible history from eligible durable sources |
| `POST /memory/search`, `GET /memory/evidence/{id}` | Query/filter/budget; bounded source inspection |
| `POST /memory/remember`, `POST /memory/corrections` | Literal source capture; identified correction |
| `POST /memory/forget-preview`, `POST /memory/forget` | Selection/revision preview token; execute suppression workflow |
| `GET /memory/forget/{id}` | Durable suppression/journal/publication-barrier/purge progress |
| `GET /personality`, `PATCH /personality`, `PUT /personality` | Inspect; patch expected revision; full replacement |
| `PUT /disposition` | Expiring owner override with expected state revision |
| `GET /inbox`, `POST /inbox/{id}/actions` | Paginated notices; seen/dismiss/snooze with expected revision |
| `GET /memory/health` | Sanitized component states, coverage and backlog counts |
| `POST /memory/exports`, `GET /memory/exports/{id}` | Start export; status or authorized bounded download |
| `POST /memory/backups`, `GET /memory/backups`, `GET /memory/backups/{id}` | Start backup; list host-managed backups; inspect bounded status/manifest |
| `POST /memory/restore-preview`, `POST /memory/restores`, `GET /memory/restores/{id}` | Review a host-managed backup and deletion coverage; activate; inspect durable recovery state |

Mutation receipt envelope: `operation_id`, `commit_seq`, `state`, nullable
`result` and `error`, `status_url`. State identifies the typed operation's durable
boundary; do not confuse a committed command with completion of its downstream
jobs. `status_url` belongs to the HTTP envelope, not the core Rust receipt.
Expected revisions and sequences use decimal strings. HTTP handlers
deserialize bounded typed bodies; schema generation and matching TypeScript
validators are required in P2, before opening these routes.

SSE carries process-instance stream cursors plus stable operation/notice IDs.
After restart, unknown cursor or lag, clients resnapshot. Snapshot and stream
registration are serialized by the coordinator. Durable inbox/operation tables
are authoritative; an event ring is never the only copy. UI must distinguish
provisional streamed text, committed outcomes, interrupted runs, unknown effects,
partial recall, memory health and stored/structured/indexed receipts.

Stable logical history is separate from runtime `chat_id` and stream cursor.
Serve a bounded recent window after pairing and page older eligible messages
through `/history`. Retain pending namespace/client/request IDs in tab
`sessionStorage` before submit, but no token or conversation text. On reload,
pair again and lookup that key; an absent record cannot reconstruct an unsaved
draft and must not trigger automatic resubmission. Logout/revoke clears all
personal UI stores/queues; opaque pending keys may remain for later authorized
reconciliation. The public app entry exposes only mode/API discovery, not the
personal bootstrap. The first-client journeys and fetch-parser cases are
specified in the [delivery refinement](memory-integration-plan.md#webui-is-the-first-required-product-client).

## Forgetting, export, retention and disaster recovery

Default capture retains accepted user input and visible assistant output;
hidden reasoning is never persisted. Tool receipts follow declared per-tool
capture policy: process inspection is ephemeral by default, calculator/time
receipts may be retained with the operation; future tools must declare it.

Even ephemeral-result tools retain a minimal invocation identity, handler,
start/terminal state and effect classification for recovery. Their raw arguments
and result text do not enter durable memory. The declared capture policy is
checked before serialization; an after-the-fact redaction hook is insufficient.
Only retained evidence can support durable facts. Authorization does not imply
permission to retain every field an external tool returns.
Memory is opt-in at configuration. A required-store failure cannot silently
switch an accepted interaction to volatile mode. No automatic transcript aging
is enabled in v1; users inspect storage and choose retention/forgetting.

Forget preview resolves bounded source/claim IDs, dependents and active-run
impact to a short-lived selection token. Execute checks current revisions and
owner intent, commits suppression plus deletion generation and a purge job,
then establishes the application output/cache barrier. Raw sources in scope are
suppressed immediately. Because derived text might embed them, conservatively
suppress all derived state until bounded revalidation proves independent support.
Reading historical versions is also subject to deletion, not an exemption.

Write the minimal deletion selection to an append-only, fsynced journal outside
the SQLite backup snapshot before declaring the forget operation fully durable.
The SQL transaction stores a pending journal entry; recovery completes it
idempotently. Keep owner-only permissions and no source prose in that journal.
The final logical-forget receipt requires SQL suppression, journal acknowledgment
and output barrier. Intermediate status is inspectable; a crash does not lose
which stage remains. Journal append failure blocks successful acknowledgment.

The barrier covers all personal publication paths: queued SSE, model deltas,
in-flight evidence/search responses, snapshots and export completion. Track their
deletion generation through coordinator publication; suppress stale responses
instead of merely clearing the model context. Already sent bytes remain outside
the barrier. Data readers never publish directly around this coordinator check.

Purge source/version/FTS/vector/profile/receipt/notice/export copies in bounded
batches; cancel affected jobs and reject late fences/generations. Existing clients
cannot be forced to erase previously received data, and physical SSD/WAL erasure
is not promised. The barrier prevents new publication after acknowledgment.
Deletion journal is part of the private data/backup policy, not anonymous telemetry.

Back up with SQLite's backup API, a manifest of schema/model/index generations,
and journal high watermark. Backups are owner-requested, kept seven days by
default, and purged on expiry by metadata-driven maintenance; report failures.
Journal retention must cover every retained backup. If all newer journals are
lost, an old backup cannot prove that no later deletions occurred: restore remains
quarantined until explicit owner reconciliation. Never silently restore it live.

Restore stops personal admission, verifies manifest/checksums, restores to a new
directory, applies all newer tombstones, runs integrity/foreign-key checks,
invalidates sessions/leases and rebuilds indexes before atomically selecting the
new store. Failed restore preserves the old directory. Export is bounded JSONL
with schema version, stable IDs, typed records and evidence; no hidden reasoning
or credentials. Recheck deletion generation before publishing a completed export.
External copies already downloaded are outside the daemon's forgetting guarantee.

## Dependency and qualification register

Dependency choices are concrete but their lockfile is an implementation artifact:
`rusqlite = 0.39.0` with bundled/backup/hooks/limits; `sha2` for stable artifact,
input and token digests; `chrono` and `chrono-tz` for explicit civil time. Reuse
existing serde/schemars/getrandom/tokio/thiserror. No ANN, database server, ORM,
dynamic extension loader or tokenizer dependency is required initially.

The original rusqlite 0.40.1 candidate failed an actual Rust 1.88 compile because
it uses `cfg_select!`. The selected 0.39.0 wrapper and bundled SQLite 3.51.3 compile
and run the initial memory tests on Rust 1.88. The locked dependency tree must
continue passing that gate. Do not silently raise MSRV. Packaging, cancellation
under load and the remaining Q1 platform requirements still require qualification.
[Tagged manifest](https://raw.githubusercontent.com/rusqlite/rusqlite/v0.39.0/Cargo.toml),
[SQLite WAL fix](https://sqlite.org/wal.html#walresetbug).

| Gate | Required experiment and passing evidence | Failure behavior |
| --- | --- | --- |
| Q1 Dependency/platform | Rust 1.88 build of locked selected features; runtime version/FTS5/backup/cancellation smoke; distributable Linux build | No persistence rollout; resolve dependency compatibility first |
| Q2 Storage correctness | Temporary DB, two principals, all command atomicity/fence/revision tests, process-kill matrix, quota and restore journal tests | Required mode unavailable; no silent volatile acceptance |
| Q3 Embedding safety/parity | Binding audit; checked shape/index/rank-pooling tests; checksummed E5 GGUF vs reference, cosine >=0.999 for FP32 reference-mode comparison, batch isolation and corpus recall | Semantic channel disabled with explicit health; complete v1 gate remains open |
| Q4 Extraction/reasoning | At least 120 held-out cases and separate development set; original research precision/recall/abstention gates, category counts and critical failures | Literal capture remains; automatic promotion disabled |
| Q5 Scheduling/resources | Paired cold/warm foreground trials under queued background load, p95/p99, memory peaks and 10K/100K/1M stores; original 250 ms/10% targets | Publish lower supported capacity or fix bottleneck; no unmeasured scale claim |
| Q6 Personality | Paired fixed evidence across profiles, edit/rebuild/race fixtures, deterministic compiler/reducer replay, model tone adherence reviewed | Host policy still enforced; do not advertise unqualified character fidelity |
| Q7 Initiative | Fake-clock due/DST/quiet/dismissal/restart tests; >=90% useful reviewed candidates plus missed opportunities, then opt-in week | Inbox review only; live initiation not promoted |
| Q8 Security/recovery/UI | Unauthorized route/SSE matrix, session revoke, injection/late-output deletion, browser resnapshot and indeterminate receipt recovery | Personal traffic remains disabled |
| Q9 Compact protocol (optional) | Paired native/JSON/tuple/DSL quality and total latency, including legends/retries | Keep named JSON; never blocks complete v1 |

Q3's numeric parity applies to a matched precision/reference path, not to an
arbitrary quantized model. Any precision change needs its own preregistered
tolerance and retrieval gate. Model manifests are promoted only after their
checks pass; inventing an artifact hash now would not close this gap.

## Delivery work packages and completion criteria

| Package | Files/ownership | Deliverable and dependency |
| --- | --- | --- |
| P0 Contract fixtures | `docs/`, test fixtures | Freeze schemas, predicates, defaults, error/status mapping and held-out corpus split; validate Q1 before production dependency use |
| P1 Storage core | `assistant/src/memory/{types,store,ingest}` | Migrations and every atomic command; execute Q2 failure matrix before capture integration |
| P2 Lifecycle/auth/durability | `runtime`, `application`, `chat`, web/daemon | Composite readiness, mandatory sink, owner capability, pairing and v2 receipts; Q8 initial auth/recovery tests |
| P3 Internal jobs | `runtime/jobs`, worker and preparation | Typed JSON contract, fenced scheduler, priority/admission, owned finalization; no primary-chat mutation |
| P4 Retrieval | Binding maintenance then `memory/retrieve` | Q3 safety first, manifest embedder, exact/FTS/graph recall, continuation and token packing |
| P5 Formation/situation | `memory/{extract,situation}` | Q4 extraction, corrections/dependency invalidation, goals/reasons/commitments and replay |
| P6 Personality | `personality/`, plugin glue, v2 UI | Compiler, versions, edit/rebuild/overrides/reducer, Q6 tests; depends on P2/P3 |
| P7 Attention | `memory/attention`, application/web UI | Cause rules, inbox, timers, profile-aware drafting, Q7; depends on P4-P6 |
| P8 Whole system | tests, docs, packaging | All required Q gates, full restart/forget/restore story, load tests and opt-in local review |

Each package updates implemented architecture/API docs only when behavior exists.
Run formatting, workspace check/tests and web checks when affected; model-required
jobs must fail when their declared artifact is missing. Default tests use fake
models/clocks and skip optional local-model jobs cleanly. Real generation and
embedding smoke tests are mandatory for promotion, not replaceable by compilation.

The acceptance story includes three/six/eight-edge recall after restart and cleared
working history, an immediate identified correction, personality replacement
during queued work, one useful initiative and suppression after dismissal,
forgetting during generation, and recovery from an older backup without deleted
data resurfacing. Retain supported facts and uncertainty throughout. Complete
core v1 is declared only after the transport-independent parts of P0-P8 and
Q1-Q8 pass, including a complete headless acceptance run. HTTP route/session/SSE
and browser checks apply to the optional web adapter and must pass before that
adapter is promoted. The owner's current delivery explicitly includes that
adapter, so these checks are required for delivery completion. Test both
memory-enabled/headless and memory-enabled/web
configurations, plus existing memory-disabled behavior. Q9 and other excluded
experiments do not expand that definition.

## Research verification performed

This specification was checked against current plugin registration, runtime
shutdown, tool context, application status and web policy code. External source
checks cover SQLite/FTS, dependency features, embedding interface, civil time and
session principles. The companion schema/fixtures and SQL sketch receive local
structural/constraint checks; they are not production migrations or end-to-end
tests. Remaining empirical gates are deliberately visible rather than described
as already solved by documentation.

Executed `python3 scripts/check-memory-contracts.py` with SQLite 3.54.1: all 16
authored schema examples, 12 expected SQL constraint rejections, stale/current
fence completion, profile transaction rollback, FTS deletion and integrity checks
passed. The validator supports exactly the schema vocabulary used by this file;
it is not a general JSON Schema engine and does not claim to check semantic
entailment. The 16 cases are development contract examples, not the required
120-case held-out model corpus. No production migration, Rust build, MSRV build,
embedding inference, model quality, crash-power-loss or load test ran here.
