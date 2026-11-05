# Durable memory storage

Status: Linux storage readiness milestone, 2026-11-05. This is the storage
foundation of [the implementation specification](memory-implementation-spec.md).
The [storage decision](decisions/0006-durable-memory-storage.md) records the
implemented boundary and compatibility choice.
The daemon and WebUI do not enable it yet. Automatic extraction, semantic
embeddings, personality, initiative and product output barriers remain separate
integration work; this module does not claim complete v1 delivery.
See the [final integration plan](memory-integration-plan.md) for the complete
remaining runtime, model, personality, attention and product delivery.

The first integration package now supplies durable **library commands** for
product admission, tool intent/result and finalization. The application still
does not call them. A successful library receipt establishes a committed storage
boundary; it does not imply authenticated HTTP, a publication barrier or a
qualified memory model.

## Ownership and use

[`assistant::memory`](../crates/assistant/src/memory/mod.rs) exposes owned typed
commands through `MemoryHandle`. `MemoryStore` owns the exclusive directory
lease, one writer and two query-only readers. The host opening the store receives
an `OwnerCapability`; it cannot be deserialized from model output. No raw SQL or
database connection is public. Native models are not required to use or test it.

```rust,no_run
use assistant::memory::{MemoryConfig, MemoryStore, MemoryText, OperationKey};

# async fn example(directory: std::path::PathBuf) -> Result<(), Box<dyn std::error::Error>> {
let store = MemoryStore::open(MemoryConfig::new(directory)).await?;
let memory = store.handle();
let owner = store.owner();
let saved = memory.remember(
    &owner,
    OperationKey::new("host", "remember-1")?,
    MemoryText::new("My sister Lena is a veterinarian in Lviv.")?,
).await?;
let recalled = memory.search(
    &owner, MemoryText::new("Lena")?, 10, Some(saved.sequence),
).await?;
assert_eq!(recalled.sources.len(), 1);
store.shutdown().await?;
# Ok(())
# }
```

Callers must await shutdown. Drop requests worker termination; each database
retains the directory lease until its worker actually exits. Closing the store
invalidates retained handles. Losing a reply after queueing does not cancel the
mutation; reconcile with the original operation key.

## Durable operation commands

`admit_operation` accepts `OperationAdmission`: a validated `ClientRequestKey`
(namespace/client/request), exact user input, host-observed UTC milliseconds and
validated `OperationLimits`. The canonical tuple is hashed unambiguously into
the existing command namespace. Owner authority is always a separate capability.
The admission digest excludes host observation time: a retry can arrive later
without conflicting, and the first committed source keeps its original timestamp.
Admission commits the source, attribution, lexical index, queued/deferred
formation, operation, receipt, causal event and settlement reservation together.
Only one product operation can be active per owner.

Use `operation_by_key` after a lost response to inspect current state; replay
returns the original receipt, which can describe an earlier state. Durable IDs
are independent of runtime chat IDs. `start_operation`, `tool_intent`,
`tool_result` and `finalize_operation` each require their own stable command key.
Tool ordinals are unique within the operation, independent of model call IDs.
Finalization releases the reservation in the terminal transaction. A completed
outcome is rejected while tool results are unknown or intents remain unmatched.

`ToolCapture::Ephemeral` accepts no raw arguments and rejects raw result data
before command serialization/digesting. Minimal handler/effect/ordinal/state
metadata remains. `Retained` accepts arguments and optional result prose within
the admitted bounds. Retained result sources have tool attribution. Visible
terminal output has assistant attribution; it does not corroborate a user fact.
No hidden-reasoning input is provided by these commands.

Recovery changes unfinished accepted/running operations to `interrupted`, marks
unmatched intents `unknown`, preserves confirmed receipts, releases reservations
and records a recovery event. It never invokes a tool or restarts inference.
`source_record` exposes literal text with host role, operation, observed time
and formation state. Unknown observation time stays null for legacy sources.

`register_consumer` / `consumer_events` / `acknowledge_events` provide durable
event cursors. Acknowledgment validates the expected cursor and commits dependent
formation requests in the same transaction. Consumer bookkeeping does not emit
new events. Retention requirements are stored; event garbage collection and
actual runtime consumers are not enabled yet.

The pending-job limit defaults to 10,000 and can be reduced with
`MemoryConfig::with_pending_job_limit`. Literal capture succeeds with a durable
`deferred` formation marker when optional work is full. A consumer can schedule
that source after capacity becomes available. `StorageHealth` reports used/quota
bytes, outstanding reservations, pending jobs and deferred sources separately.

## Implemented guarantees

- Literal remember commits its source, lexical index, queued/deferred formation, change
  event and replay receipt in one FULL/WAL transaction. Identical keys return
  the original receipt across restart; changed input conflicts.
- Exact quotation evidence is resolved to unique UTF-8 byte spans. Ambiguous
  quotes, missing sources, foreign IDs, invalid object types and stale target
  revisions are rejected. These checks establish provenance and shape, not
  natural-language entailment.
- Typed personal relationships include sibling, parent and partner links,
  occupations, locations, ownership and use. Entity kinds include places and
  occupations. A matching name never merges people automatically.
- Corrections retain historical versions and change the current version
  atomically. Status/due-time scopes reject competing overlapping current
  assertions; an identified correction is required instead of last-write-wins.
- `claim_history_page` returns a bounded page with an explicit continuation.
  The older `claim_history` convenience API returns `Capacity` for more than
  256 eligible versions instead of silently omitting history.
- Graph traversal returns qualified edges, not inferred conclusions. It honors
  known half-open validity intervals, preserves negative assertions, and excludes
  hypothetical/fictional and associative edges from factual traversal. Current
  traversal is outgoing only. Work is bounded by depth, nodes, edges and results.
- Literal search quotes bounded terms instead of executing caller-supplied FTS
  syntax. Results identify their committed snapshot and truncation.
- Extraction attempts have persistent fences, source/deletion checks and compute
  reservations. Validated graph changes and completion commit together. Late or
  cancelled attempts cannot publish; repeated identical completion returns its
  original result. No model worker is started by claiming a job.
- Source suppression revokes raw reads, graph evidence and replay receipts before
  physical purge. Derived entity wording and job results are purge targets too.
  Minimal request keys/tombstones and charged compute remain to prevent replay
  or free retries. A new deliberate source is independent evidence.
- Product outputs/arguments can repeat their own exchange. Suppression of any
  product source conservatively suppresses the operation's other sources,
  revokes all its receipts and rejects late settlement. Each affected source is
  journaled. `purge_next_batch` drains this work without requiring callers to
  enumerate forgotten content. This currently covers at most 34 sources and
  32 tool argument payloads per operation; independent exchanges remain separate.
- Online backups use SQLite's backup API, private destinations and checksummed
  manifests. Restore candidates verify the backup and apply the current store's
  newer deletion journal before exposing reads. They never replace a live store.

`suppress_source` is a **storage barrier**, not a completed product-level forget
operation. A future application integration must stop stale streams, in-flight
responses, cached prompts/history and exports before acknowledging logical
forgetting. Likewise, activating a restore candidate requires quiescence and
session/output reconciliation by the host. These requirements are not optional
hooks and are not implemented by the existing chat path.

## Filesystem and resource contracts

The directory must be private and owned by the OS user. Files are owner-only,
regular and not symlinks or hard links. Ancestors must be owned by the user or
the filesystem-root administrator; writable shared ancestors require sticky
semantics. An exclusive lock prevents two stores opening the same directory.
These restrictions address different-UID clients, not root or a hostile process
with the same OS credentials.

Each worker has 56 ordinary permits, seven critical permits and one dedicated
product-finalization permit. The single active operation owns the durable
finalization entitlement. Finalization precedes queued critical work, which
precedes ordinary work, at complete command boundaries. Reader workers use the
ordinary lane. Closing the queue drains accepted work; unexpected worker exit
disconnects pending replies. Payload bounds apply before queueing. SQL has a
progress-handler deadline of 250 ms for readers and one second for the writer,
with a 100 ms busy timeout. Backup page copying has a separate 30-second
allowance; large backup checksum work runs on a reader.

The default quota is 2 GiB with 64 MiB emergency headroom. Ordinary mutations
account for outstanding operation reservations as well as that margin. Admission
adds a conservative envelope derived from its input, message/tool limits,
serialization expansion and per-command page allowance. The bounded test checks
WAL growth against that envelope; it is not a device-independent allocation
proof. Critical settlement and suppression can use reserved/emergency capacity
after ordinary admission stops. This fixes the original near-quota suppression
failure and is not a guarantee against external disk exhaustion.
User sources are at most 16 KiB, assistant output at most 128 KiB and retained
tool payloads at most 16 KiB. Source results remain owned and bounded, graph depth
is at most 12, and history inspection pages contain at most 256 versions.
Purge batches share a 256-logical-record / 1-MiB accounted-payload allowance
across tool arguments, claims and receipts, including conservative final
source/index maintenance. This does not bound SQLite's physical I/O. No automatic
retention is enabled. Reservations exist in the library; live application
admission does not use them yet.

Soft quota exhaustion returns `Capacity` and keeps bounded cleanup possible.
`StorageFull` distinguishes actual SQLite/filesystem allocation failure (map it
to the public capacity error class in future adapters). Physical write failure,
corruption or I/O failure latches ordinary writer admission blocked; inspection
and critical recovery attempts remain available. Reopen performs recovery before
admission can resume. No failed terminal write is reported as completion.

The original `store/schema.sql` is immutable migration 1. Ordered migration 2
adds operations, attribution and consumer cursors. Checksummed migration history
is verified before recovery/readers; each upgrade and its history/meta update
commit together. Unknown/modified history fails closed. Existing version-1 stores
upgrade without erasing sources, receipts or FTS. Restore candidates preserve
the current host's configured quota and pending-job bound.

Extraction reservations use the selected v1 maxima and remain conservatively
charged after an incomplete attempt/restart. Backward wall time does not reset
the allowance. An actual inference integration must supply measured usage, token
chunking, hard deadlines and qualification; claiming/finishing leases is not an
automatic semantic validator.

## Dependencies and verification

The attempted rusqlite 0.40.1 build failed on Rust 1.88 because it uses the newer
`cfg_select!` macro. The implementation pins **rusqlite 0.39.0**, with bundled
SQLite 3.51.3, backup, hooks and limits. This preserves the workspace MSRV and
contains the WAL-reset fix. SHA-256 uses sha2 0.10.9. No upstream submodule or
native binding changed. [Selected wrapper](https://github.com/rusqlite/rusqlite/tree/v0.39.0),
[SQLite WAL fix](https://sqlite.org/wal.html#walresetbug).

The memory suite includes source/replay persistence, capability and lock checks,
unique Unicode evidence, failed-transaction rollback, temporal and negative
claims, personal knowledge/corrections, eight-edge restart traversal, attempt
fencing/accounting, symlink/insecure-parent rejection, abrupt process kill,
suppression/purge and backup/deletion reconciliation. It uses temporary stores
and no language models. Process termination testing does not simulate all
power-loss, filesystem or device failures.

The readiness tests additionally cover version-1 upgrade/rollback/tampered
history, lost admission replies, durable operation/capture/replay, foreign
authority, all writer lanes saturated, near-quota suppression, actual SQLite page
exhaustion, deferred formation with atomic consumer cursors, paginated history,
exchange-wide receipt revocation, byte-bounded large-payload purge, configured
restore bounds and a killed product operation with one confirmed and one
unknown tool outcome. A rejected proposal constraint rolls back without
misclassifying healthy storage as an I/O failure. Literal record IDs remain data;
they cannot select causal authority or create accidental source dependencies.

Storage-readiness verification on 2026-11-05: `cargo fmt --all --check`,
`cargo check --workspace --offline` and `cargo test --workspace --offline`
passed (645 tests passed, 14 ignored). The Rust 1.88 storage run
`cargo +1.88.0 test -p assistant --no-default-features --offline memory:: --lib`
passed all 37 tests. Clippy across the assistant's targets completed with
existing repository warnings and no warnings in the new files. These results
qualify this library milestone, not the unimplemented runtime/WebUI integration.

The research schema/examples remain design material. Their validator checks
structure and the SQL sketch; it does not exercise this Rust implementation's
semantic extraction or qualify a model.

## Native/application integration progress

Migrations 3/4 add source-bound E5 vectors/byte coverage and bounded product
terminal metadata. Source suppression excludes vector reads immediately; bounded
purge accounts for derived vectors before FK source cleanup. The opt-in Linux
runtime now owns E5/indexing and durable headless admission/tool settlement/recall.
This is not the complete final memory system; automatic graph formation,
authenticated web/owner controls and other planned contracts remain open. See
[the executable integration record](memory-native-integration.md) for artifact
selection, usage, failed semantic qualification and exact limitations.
