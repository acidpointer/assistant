# Native memory integration progress

Status: implemented opt-in E5 indexing/recall and durable headless request path;
**the final memory-system delivery is incomplete and unqualified**, 2026-11-05.
The complete contract remains [the final integration plan](memory-integration-plan.md).
This document records executable progress and failures, not a replacement scope.

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

A real daemon run exposed an additional failure: missing byte-fallback vocabulary
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

Create a private local configuration (paths resolve relative to that file):

```json
{
  "data_directory": "./memory-data",
  "embedding_model": "./multilingual-e5-small-q8_0.gguf"
}
```

```sh
cargo run -p assistant-daemon --offline -- \
  --model ./LFM3-1.7B-Heretic-Abliterated-Q6_K.gguf \
  --memory-config ./memory-config.json --no-web \
  --prompt 'Remember this fact: my bicycle is blue.' \
  --request-id bicycle-1 --cpu --no-tools
```

Run a fresh process with a different request key and ask about the bicycle. A
retry must reuse the original key **and exact input**; it reconciles the original
operation without generating or executing tools again. `--interactive` uses the
same durable path. `--raw` conflicts with required memory. The memory configuration
is bounded to 4096 bytes and rejects unknown fields. Explicit `--require-tool`
is not yet propagated to durable application defaults and should not be used
with this experimental path.

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
