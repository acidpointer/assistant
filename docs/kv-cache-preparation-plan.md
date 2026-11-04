# KV cache preparation before TurboQuant

Status: implemented for bounded experiments on the initial NVIDIA/Vulkan target,
2026-11-04. See the [implementation and measured results](kv-cache-baseline.md).
Default adoption, additional vendors, broad task-quality coverage and promised
long-context byte limits remain gated. This plan follows the
[TurboQuant research](turboquant-research.md) and targets the existing native pin.

The objective is a trustworthy baseline for compact sliding-window attention
(SWA) and the existing Q8/Q4 cache formats before adding another codec. The
daemon must expose its cache settings, preserve context correctness, show what
the engine actually allocated, and support repeatable memory, quality and
performance comparisons.

Implement the preparation as seven focused changes. The first six establish
the baseline needed for a TurboQuant experiment. The seventh governs changing
defaults and expanding long-context use. Broad GPU claims require results on
the corresponding vendors; missing hardware is a validation gap, not a reason
to label an untested path supported.

## Implementation order

| Change | Deliverable | Depends on | Completion condition |
| --- | --- | --- | --- |
| 1 | Reliable model fixtures and test categories | Existing source baseline | Default tests no longer confuse vocabulary fixtures with inference models or model behavior with deterministic protocol correctness. |
| 2 | Fallible memory reporting and cache diagnostics | Existing binding/shim contracts | Complete owned reports or typed errors; requested and resolved state are distinguishable. |
| 3 | Typed cache configuration and microbatch control | Model validation/reporting from 2 | Library and CLI reach the same validated native settings while initially preserving existing defaults. |
| 4 | Compact SWA correctness and allocation tests | 1–3 | FP16 compact SWA preserves supported cache/recovery behavior and reduces measured allocation. |
| 5 | Existing quantized KV profiles and rotation tests | 1–4 | Q8/Q8, Q8/Q4 and Q4/Q4 have explicit support and measured quality/performance baselines. |
| 6 | Repeatable benchmark and regression workflow | Instrumentation starts in 2–3; results cover 4–5 | Versioned result artifacts separate payload, allocator memory, peak memory, prefill, decode and quality. |
| 7 | Default selection and resource policy | Results from 4–6 | A supported default has documented evidence, an explicit fallback choice and bounded context allocation. |

The benchmark harness should grow with the changes, so each behavioral change
can be compared with its predecessor. Change 6 completes the workflow rather
than postponing all measurement until the end.

## Existing contracts to preserve

- Resident model weights remain shared by contexts on their model worker.
- A context remains confined to its native worker thread. Diagnostic data
  crossing into async code is an owned snapshot, not a borrowed native handle.
- Cache reuse requires an exact evaluated-token prefix. Divergence resets and
  replays; this work does not introduce arbitrary KV removal or shifting.
- Cancellation, partial failures and stale cursors retain their current
  bookkeeping and recovery semantics.
- Context configuration is fixed at allocation. A different cache layout
  requires a new context and replay, not mutation of an existing cache.
- Context count, token, payload and duration limits remain active.
- The Vulkan resource-lifecycle fix and native logging/callback safety patches
  remain in the build. No new instrumentation replaces callback state behind
  the existing safe owner.
- Model output and tools do not select native cache policies. These remain host
  configuration.

The relevant implementation is
[`engine/llm.rs`](../crates/assistant/src/engine/llm.rs),
[`engine/context.rs`](../crates/assistant/src/engine/context.rs),
[`runtime/worker.rs`](../crates/assistant/src/runtime/worker.rs), and the
[binding maintenance contract](bindings-fork.md).

## Change 1 Make the baseline tests trustworthy

### Separate fixture capabilities

Update [binding test support](../crates/llama_cpp/tests/support/model.rs) so
tests request a vocabulary fixture or a full inference fixture explicitly.
Absence of an optional model should produce a clear skip; an explicitly
configured file that fails to load should fail with its actual cause rather
than becoming a successful skip through `.ok()?`.

The known failures in [binding maintenance](bindings-fork.md#vulkan-resource-lifetime)
need focused corrections:

- Special-token tests in
  [`test_chat.rs`](../crates/llama_cpp/tests/test_chat.rs) must use control
  tokens actually supported by the fixture and a matching template. Preserve
  assertions that untrusted input cannot forge control tokens.
- Model property/display tests in
  [`test_model.rs`](../crates/llama_cpp/tests/test_model.rs) must require a
  full model when their assertions depend on loaded model hyperparameters.
  Do not enter native attention-head getters using the uninitialized
  vocab-only fallback. Keep the full-model assertions.
- Update the test-support comments that still describe the native logging race
  fixed by the maintained patches. Do not opportunistically remove test
  synchronization or change the library's threading contract.

### Separate deterministic correctness from model quality

Keep deterministic tool dispatch, argument handling and continuation behavior
in tests with controlled model responses. Keep the actual LFM3 tool-selection
test as an explicit model-behavior evaluation. The recorded two-tool failure
also occurs without the cache changes; weakening its expected result would
not make a valid baseline.

Provide an explicit way to run model-quality evaluations and require their
models. Their results must remain visible even when they are outside the
default deterministic gate. The existing arithmetic/date tests that pass are
still useful integration checks.

**Exit:** a clean default test run with absent optional fixtures, a required
fixture mode that cannot silently skip, and separate recorded model-quality
results. Fixing unrelated application features is outside this change.

## Change 2 Add reliable memory and cache diagnostics

### Make memory collection fallible

The current
[`llama_memory_breakdown_collect`](../crates/llama_cpp_sys/ext_shim/ext_shim.cpp)
returns the number of entries written, has no exception barrier, and cannot
directly distinguish a complete result from a truncated one. The
[Rust collector](../crates/llama_cpp/src/context/memory_breakdown.rs) retries
by growing a buffer, but eventually returns a capped result without a
truncation error. These are unsuitable contracts for enforcing budgets.

Add a versioned fallible shim entry point using the existing shared status and
thread-local error conventions. It must report the required entry count,
support the size-query/fill pattern, catch native exceptions, and distinguish
invalid input, incomplete capacity and native failure.

Expose a fallible Rust collection method, such as `try_memory_breakdown`, and
use it for all new diagnostics and benchmark code. Preserve the existing
public method for source compatibility, document its legacy limitations, and
avoid turning a collection error into an empty or apparently complete report.
Check allocation caps, byte arithmetic and string boundaries in the new path.

### Report requested and effective settings

Add owned diagnostic types with two distinct parts:

| Requested policy | Effective native information |
| --- | --- |
| K/V formats | Actual formats in the participating attention caches |
| Full or compact SWA | Actual full/SWA cache capacities, or not applicable |
| Context, batch and microbatch settings | Actual native `n_ctx`, `n_batch`, `n_ubatch` and sequence capacity |
| Flash-attention policy | Resolved native algorithm setting; GPU placement is a separate observation |
| Rotation policy | Actual per-cache rotation availability/dimensions where inspectable |
| GPU layer/offload request | Selected devices and memory buffer types |

The existing public Rust model getters are not sufficient to infer every
layer's KV geometry, particularly for LFM3's zero-KV recurrent layers. Add a
small guarded C++ shim to snapshot the needed per-layer topology and supported
cache information from the pinned internals. Cover ordinary full-attention,
SWA and hybrid-SWA memory used by the baseline models. Report an explicit
unsupported/unknown inspection result for other memory implementations rather
than manufacturing effective settings from the requested configuration.

Existing native context getters, `get_cparams`, and full/SWA cache accessors
provide starting points. Keep private C++ layouts inside the shim. Return
scalars and owned Rust data; do not expose raw C++ containers or references.

### Define the limits of the measurements

The current breakdown groups model, context and compute allocations by backend
buffer type. The context category includes KV and recurrent memory, so do not
label that entire value KV bytes. Label a formula-derived KV payload as an
estimate until native accounting can distinguish its components.

When aggregating contexts, count resident weights once per loaded model.
Adding every context report's model column would count the same shared weights
repeatedly. Preserve host/device categories and use checked byte sums.

Take snapshots after allocation, prefill and decode. A snapshot of native
buffer accounting is not a driver-wide peak-VRAM measurement. A benchmark may
add supported device/OS peak telemetry, but must identify its source and
report unavailable telemetry honestly.

**Exit:** tests for collection errors and capacity boundaries, correct shared
weight accounting, owned thread-safe report data, and a real LFM3 report whose
requested/effective fields can be compared. No upstream patch is expected for
this shim work.

## Change 3 Expose a typed cache configuration

### Configuration contract

Introduce a cache-specific storage enum, initially limited to `F16`, `Q8_0`
and `Q4_0`. Keep the general `GgmlType` API available for advanced binding use;
do not advertise integer or unsupported weight formats as cache options.

Add a model-level configuration containing:

- Key and value storage types as separate choices.
- `SwaCacheMode::{Full, Compact}`.
- The existing typed flash-attention policy.
- `AttentionRotation::{Auto, Disabled}`. `Auto` permits native rotation and
  respects model/environment opt-outs; it does not promise to override them.

Add explicit microbatch control to `LLMConfig`, separate from `batch_size`.
An optional positive size is sufficient: an unset value resolves to
`min(batch_size, 512)` at this pin; an explicit value must be positive and no
larger than the batch. Report the actual native result. This preserves small
batch tests and avoids a fixed 512 microbatch becoming invalid when callers
set a smaller batch.

Initially preserve the current FP16/full-SWA defaults so configuration plumbing
and default-policy changes can be reviewed separately. Do not automatically
expand context length or reduce precision to make a configuration fit.

### Validation and application wiring

Split validation into portable configuration checks before model loading and
model/backend checks once the model is available, before context allocation.
Reject quantized V with explicitly disabled flash attention. Check relevant
attention head/block alignment and restrictions such as architectures that
require equal K/V types. Recurrent layers with zero KV heads are not malformed
attention layers.

Use typed errors that identify the requested setting and reason. Keep native
validation as an additional check. A cache allowlist or an enabled flash flag
must not be reported as proof that attention executed on Vulkan.

Wire the configuration through `LLMConfig`, `LLMModel::new_context`, all
contexts created by the worker, and CLI parsing. Proposed CLI controls are:

```text
--cache-type-k f16|q8_0|q4_0
--cache-type-v f16|q8_0|q4_0
--swa-cache full|compact
--flash-attn auto|on|off
--attn-rotation auto|disabled
--ubatch-size <positive count>
```

Keep CLI parsing in the adapter and policy in the library. Add a diagnostics
output option that does not pollute streamed model text and does not expose
model paths or prompt content in routine logs. Benchmarks can record model
identity through an explicit local result artifact.

**Exit:** the same library configuration and CLI arguments produce the same
native settings, invalid combinations fail before mutation, and every worker
context receives the selected policy. Tests should verify behavior and errors,
not just setter round trips.

## Change 4 Prove compact SWA independently

Start with FP16 K and V. Compare full and compact SWA with all other inputs
and policies fixed. The research predicts approximately 88.0 to 55.5 MiB of
K/V payload at 4K capacity for the inspected LFM3 checkpoint and a 512-token
microbatch. Actual context allocations also include other state.

Add model-backed tests covering:

1. Prompts shorter than the 256-token SWA window, across the window boundary,
   across the compact allocation boundary, and across several wraps.
2. Multiple prefill batches, varied microbatch sizes, short decode and decode
   near the configured context limit.
3. Exact-prefix append reuse compared with a fresh full replay.
4. Divergent history and history trimming, which must trigger the existing
   reset/replay policy.
5. Cancellation during prefill and decode, continued tool interactions,
   pre-cancelled requests and stale-cursor rejection.
6. Native/streaming failure recovery, independent interleaved chats, and
   context release followed by reallocation.

Use the same teacher-forced token stream to compare full-SWA and compact-SWA
logits within established floating-point tolerances. This isolates cache
correctness from divergence caused by feeding different generated tokens back
into the model. Continue normal greedy-generation integration checks too.

Do not add arbitrary rewind/sequence-removal logic for the hybrid model. Do
not claim compact-cache checkpoint support beyond the tested replay contract.
For models without SWA, report the mode as not applicable.

**Exit:** allocation reduction is observed, correctness holds across wrapping
and recovery, and prefill/decode performance is recorded. Compact SWA remains
opt-in until the default-selection step.

## Change 5 Establish existing quantized KV baselines

With compact SWA validated, compare these explicit profiles:

| Profile | K | V | Purpose |
| --- | --- | --- | --- |
| Reference | F16 | F16 | Numerical and behavioral reference |
| Lower memory | Q8_0 | Q8_0 | Conservative quantized baseline |
| Mixed precision | Q8_0 | Q4_0 | Evaluate more aggressive V compression separately |
| Smallest initial cache | Q4_0 | Q4_0 | Lower-memory baseline whose quality needs independent evidence |

Use flash attention for quantized V and record effective rotation. Test the
context-local rotation opt-out without changing process environment state or
leaking settings between concurrent contexts. Correct the Rust documentation
that currently names Hadamard rotation TurboQuant and inaccurately describes
its environment handling or alignment conditions.

Repeat the compact-SWA lifecycle/recovery cases for quantized caches. Test
different head dimensions on suitable fixtures and reject unsupported
combinations. Compare quantized results with FP16 using fixed-token numerical
evaluation and task-level accuracy; do not require identical generated text.

Perform a separate placement audit for cache writes and attention. The native
scheduler's existing `GGML_SCHED_DEBUG` trace can be enabled in isolated test
processes. Use that before introducing new native tracing hooks. Preserve the
existing callback owner; a new observer must not overwrite tensor-transaction
callbacks. Keep trace-heavy placement checks out of timed benchmark runs.

A CPU reference run needs zero GPU weight layers and disabled KQV/operator
offload, or an otherwise verified CPU execution path. `gpu_layers=0` alone
means CPU weights and does not establish CPU-only attention.

**Exit:** supported profiles have real generation, quality and placement
results; invalid profiles return useful errors; Vulkan-required tests fail
when no usable GPU is present rather than silently skipping or using CPU.

## Change 6 Build repeatable memory and performance comparisons

Add a small benchmark/example under the assistant crate that exercises the
public engine API and emits structured results. Use a child process per
configuration for isolated startup and peak telemetry, and reuse the resident
model/context within a run for warm measurements. Do not create a second
inference implementation or benchmark only a dequantization kernel.

The matrix should vary SWA mode and cache precision independently, use 4K,
32K and longer contexts where the model/device budget permits, and include
short and nearly full contexts. Record skipped oversized cases explicitly.
Fix microbatch size when comparing formats; sweep it separately when tuning.

Each result needs:

- Workspace/native revisions, patch identity, model checksum and fixture/data
  identity, build profile, GPU, driver and relevant environment overrides.
- Requested and effective cache, context, batch, microbatch, rotation and
  attention settings.
- Model startup and context-creation times, prefill tokens/s, time to first
  token, and decode tokens/s with clear timing boundaries.
- Native model/context/compute snapshots, the KV payload estimate, and any
  independently measured peak/retained host or device memory.
- Cold and warm results, repeated paired runs and their spread. Verify or
  synchronize native work before closing timing intervals where needed.
- Placement-audit outcome and quality results, linked to the same configuration.

Use deterministic local test data and fixed tool outputs for correctness.
Keep live model tool-choice evaluations and long-context retrieval as quality
results with their own baselines. Existing failures must not be attributed to
a cache change without comparison.

Run model-free/default tests separately from required-model and required-GPU
jobs. Start with available NVIDIA hardware, then exercise AMD and Intel before
claiming their profiles validated. Include scalar and available cooperative
matrix paths; source inspection alone does not establish driver parity.

**Exit:** another developer can reproduce a configuration and understand both
its measured gains and its quality/cost tradeoff without reconstructing shell
history. Keep local models and unredacted machine-specific artifacts out of Git.

## Change 7 Select defaults and bound resident memory

First consider compact FP16 as a default change independent of precision.
Then consider compact Q8 if its quality/performance evidence supports the
target hardware. Keep mixed Q8/Q4 and symmetric Q4 explicit choices until
their model-specific tradeoffs are established. Preserve a full-SWA/FP16
reference configuration for compatibility and diagnosis.

Define acceptance tolerances before interpreting benchmark results. Functional
cache/protocol failures block adoption. A practical proposed default gate is
to investigate any repeatable median prefill or decode regression above 5%;
smaller differences still need comparison with run-to-run noise. Quality
thresholds must be explicit per evaluation task and model before a lossy format
becomes the default. Experimental profiles may expose a documented tradeoff
without becoming a universal default.

The runtime currently bounds native contexts by count, with four per model by
default. Four contexts can still exceed a small GPU's memory at a large token
capacity. Retain that count limit and add allocation diagnostics before any
automatic context-size expansion is considered.

For a deployment that enables long or numerous contexts, add an explicit
per-model ceiling on aggregate accounted context/compute bytes. The worker
should check a conservative estimate before creating another context where
available, reconcile the actual report before admitting it, and release its
accounting with the context. Count shared model weights once. A rejected
allocation must not insert a half-created context or disturb another chat.

An after-allocation check alone is not a hard preallocation VRAM guarantee.
Estimates must identify what they exclude; driver allocations and concurrent
external GPU use can still cause allocation failure. If a reliable estimate
or compute bound is unavailable, do not advertise strict preallocation budget
enforcement. Keep the context-count cap, handle allocation failure cleanly,
and make any stronger preflight planner a separate reviewed change.

Automatic precision downgrades, dynamic eviction, free-VRAM polling, arbitrary
KV rewinding, model unloading and a general GPU memory manager are not
prerequisites for the first TurboQuant prototype. They would introduce policy
and lifecycle work beyond this baseline.

**Exit for new defaults:** documented model/device coverage, measured gains,
explicit quality/latency tolerances, working compatibility settings and
bounded allocation behavior appropriate to the deployment. The byte-budget
extension is required for a long-context rollout that promises that limit;
it need not block an explicitly bounded experimental codec benchmark.

## Files and ownership

| Area | Planned changes | Ownership |
| --- | --- | --- |
| `crates/llama_cpp/tests/support/`, `test_chat.rs`, `test_model.rs` | Fixture capability contracts and meaningful default gates | Binding test maintenance |
| `crates/llama_cpp_sys/ext_shim/` and existing shim support | Fallible memory collection, topology/effective-state snapshots | Agent-owned binding/native glue |
| `crates/llama_cpp/src/context/`, `model.rs`, `quantize.rs` | Owned diagnostics, safe validation, cache type mapping, accurate docs | Agent-owned bindings |
| `crates/assistant/src/engine/` | Model-level cache configuration, microbatch resolution, diagnostics and typed errors | Human-owned application; implementation requires an explicit request |
| `crates/assistant-daemon/src/app.rs` | Thin CLI parsing and diagnostic output | Human-owned application |
| `crates/assistant/src/runtime/` | Context policy propagation; accounting/admission when selected | Human-owned application |
| Assistant tests and benchmark/example tooling | Cache lifecycle, quality categories and reproducible results | Shared tests; respect application ownership |
| `docs/` and crate READMEs | API contracts, results, supported profiles and instructions | Shared documentation |

No upstream source edit is expected to enable existing SWA or Q8/Q4 options.
New guarded shims belong outside the submodule. If a reproducible native
correctness defect is found, isolate its fix as an applicable patch and verify
it independently; do not bypass a check or edit the pinned submodule.

## Ready to begin TurboQuant

The native codec experiment can begin when:

- [x] Default and required-fixture tests have trustworthy failure/skip behavior.
- [x] Requested/effective settings and complete memory reports are available.
- [x] Library/CLI cache configuration and microbatch control are exercised.
- [x] Compact-SWA wrapping, prefix reuse, reset, cancellation and recovery pass
  on LFM3 with FP16 caches.
- [x] Existing Q8/Q4 profiles have usable correctness, quality, memory and
  throughput baselines on the first target GPU.
- [x] Placement auditing can distinguish Vulkan attention from CPU fallback.
- [x] A repeatable benchmark workflow records coverage gaps and artifacts.
- [x] Changes to defaults are separated from configuration plumbing and follow
  the documented evidence requirements.

The checked items describe the bounded first-device experiment. Numerical and
live-task baselines include the existing two-tool failure; they are not a
production quality approval. Defaults remain unchanged. Additional hardware
and long-context rollout gates remain open as recorded in the baseline guide.

Run the repository's formatting, check and test gates for implementation
changes, plus the required model/GPU cases for native inference work. A green
compilation or a skipped GPU test is not an inference validation result.
See [AGENTS.md](../AGENTS.md) and [binding maintenance](bindings-fork.md).

This preparation introduces no new TurboQuant type IDs, codebooks, QJL
correction or Vulkan codec kernels. Those remain the subsequent native patch
series described in the [research](turboquant-research.md#proposed-integration).
