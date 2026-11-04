# TurboQuant implementation plan

Status: CPU/reference slice implemented, 2026-11-04. This is the implementation
sequence after the [cache preparation baseline](kv-cache-baseline.md). Patch
0008, experimental feature staging and deterministic quality tooling now exist.
The [format contract](pq3-reference-contract.md) records the donor audit and
remaining graph-transform decision; [quality evaluation](cache-quality-evaluation.md)
records fixtures and gates. Runtime defaults remain FP16 K/V with full SWA.
PQ3 cache inference and Vulkan kernels are not implemented.

Build an experimental native codec for consumer Vulkan hardware while retaining
our pinned llama.cpp and binding safety contracts. Deliver codebook-only
three-bit K/V first, then add QJL-corrected keys as a separate configuration.
The first implementation deliverable is an applicable CPU format/reference
patch with numerical tests. Vulkan support and runtime exposure follow it.

## Delivered CPU/reference slice

Patch 0008 and the `turboquant` feature are implemented. The donor audit,
row/ABI boundary and outstanding transform choice are recorded in the
[PQ3 contract](pq3-reference-contract.md). The
[quality reference artifact](benchmarks/cache-quality-2026-11-04.json) captures
68 task cases and 85 numerical records with the experimental feature enabled.
Exact-answer task scores are FP16 full/compact 12/17, Q8 compact 12/17 and Q4
compact 10/17. Model failures and Q4's failed experimental gate are preserved.

Validated for this slice:

- Workspace format/check/tests: 572 passed, 14 ignored, zero failures.
- CPU-only experimental binding units/reference tests: 132 passed, including
  five guarded codec/layout/rejection tests.
- Native CMake `test-pq3-reference`: passed without a model or GPU.
- Patch staging and real-series CLI/Rust verification: five tests passed;
  default and experimental series agree at each increment.
- Python artifact/gate regressions: five tests passed; missing/duplicate evidence,
  mismatched token streams and failed candidate quality are rejected.
- Source packaging contains patch 0008, staging support and native test inputs.
- Required model/Vulkan baseline completed with unchanged source inputs and
  preserved source/model/patch/executable/log checksums. This observes Vulkan
  weight allocation; it is not an operator-placement or performance measurement.

The pinned submodule is unchanged and clean. There is no PQ3 inference result,
new cache selector, Vulkan kernel or AMD/Intel validation in this slice. Native
context creation rejects reference-only PQ3 before allocation. Steps 4–8 below
remain pending; defaults stay FP16/full.

## Scope and success criteria

The first milestone supports a deliberately small combination:

- `PQ3/PQ3`: codebook-only keys and values, initially 128-value blocks.
- Causal text inference with the inspected LFM3 hybrid full/SWA topology and
  suitable ordinary attention reference fixtures with 128-dimensional heads.
- Compact and full SWA, fixed configuration at context creation, exact-prefix
  append reuse, and reset/replay recovery.
- CPU reference execution and an explicitly validated Vulkan execution path.

The second milestone adds `TBQ3/PQ3`: QJL-corrected keys and codebook-only values.
Here QJL means the additional residual sketch used to correct query–key scores;
it must participate in attention, not just ordinary vector reconstruction.

Keep the pair allowlist explicit. Arbitrary cross-products with FP16/Q8/Q4,
QJL values, four-bit donor variants, and 64-dimensional heads are later
extensions. Reject unsupported pairs, dimensions and architectures before
allocation or graph execution. Recurrent zero-KV layers are excluded from
attention geometry validation and compression.

Neither milestone changes model weights, increases context defaults, adopts a
complete donor fork, or introduces cache eviction or arbitrary hybrid rewind.
CUDA/Metal kernels, weight quantization and model-policy changes are outside
this port. AMD and Intel are target validation vendors; a working NVIDIA run
does not establish their support.

Success means a useful measured tradeoff against **compact Q8/Q8 and Q4/Q4**,
as well as FP16: lower total accounted memory at acceptable quality and latency,
or better quality at a comparable memory budget. A smaller codec label alone
does not qualify.

## Preserve the current baseline

The native base remains b12015,
`a34de8618b25dfc32ee43749db47d767a9f0a25d`. Preserve patches `0003`–`0007`,
including decode hooks, exception/error containment, logging ownership,
context-local rotation and Vulkan resource lifetime.

Use the existing [versioned results](benchmarks/kv-cache-2026-11-04.json) and
benchmark harness. The current preparation diff must remain separately
reviewable from codec changes. Creating a plan or implementing a patch does not
authorize a commit or a submodule update.

The runtime contract stays:

```text
CLI or library configuration
  → LLMConfig.cache and execution policy
  → worker-owned model
  → LLMModel::new_context
  → validated native context parameters
  → native cache writes and attention kernels during ordinary inference
```

Weights remain shared, native contexts stay on their worker thread, diagnostics
cross threads only as owned data, and cancellation/recovery retain their current
semantics. Applying a different codec requires a new context and replay.

## Delivery sequence

| Step | Deliverable | Completion condition |
| --- | --- | --- |
| 1 | Donor and format specification | Immutable source identities, attribution, block layout, transforms, supported pairs and error behavior are recorded. |
| 2 | Deterministic quality evaluation | Fixed data and tool outputs establish reproducible FP16/Q8/Q4 references and declared quality gates. |
| 3 | PQ3 CPU format and build integration | An applicable patch builds; packing, reconstruction and reference attention tests pass without a model or GPU. |
| 4 | PQ3 cache graph and Vulkan execution | Real LFM3 generation uses the codec, wraps/replays correctly, and executes cache writes and attention on Vulkan. |
| 5 | Rust and ordinary runtime exposure | Library and CLI select the same validated native policy; unsupported builds/backends fail clearly. |
| 6 | PQ3 benchmark decision | Memory, quality and latency justify proceeding, revising the codec, or stopping the port. |
| 7 | TBQ3 corrected-key extension | CPU and Vulkan attention agree on the additional score correction; the mixed pair has its own quality/performance results. |
| 8 | Vendor and rollout evaluation | Per-vendor coverage and default/long-context requirements are satisfied before promotion. |

Quality tooling can proceed alongside CPU work. Do not start optimizing Vulkan
against an undefined layout, or expose a CLI profile whose required kernels are
not implemented. PQ3 does not need to wait for the corrected-key extension to
be evaluated as an explicitly experimental profile.

## 1 Specify the donor and codec contract

Use QVAC as the initial donor candidate identified in the
[research](turboquant-research.md#qvac-cpu-and-vulkan-implementation). Its
[PR 115](https://github.com/tetherto/qvac-fabric-llm.cpp/pull/115) describes both
codebook-only K/V and corrected-key configurations with CPU/Vulkan execution.
Treat reported donor performance as a hypothesis for our hardware.

Before copying code, retrieve and audit the immutable PR head
`362003a25376555e5c01939ed82710bbb2c3bcf8` and base
`4c993bdde3e90d1c6a6977e21734a05a0b5f64dc` recorded in the research. Verify
checksums, source attribution, applicable license notices, exact block
declarations, shader dependencies and required operations. If a different donor
revision is chosen, record it and repeat this audit. Keep unrelated fork
features and defaults out of the port.

Write down the format contract before the CPU patch:

- Bit packing, alignment, norm/scaling representation and actual bytes per block.
- Codebook constants and version, transform ordering, inverse transform, fixed
  seeds/sign conventions, and rounding/tie behavior.
- Supported head geometry, tails, stride/row layout, K/V pairs and flash policy.
- Zero vectors, small/extreme inputs and non-finite-input behavior; numerical
  tests must distinguish supported inputs from rejected ones.
- State identity and compatibility rules, including transform/codebook identity.

Do not copy donor type IDs. The local native enum currently has F8_E4M3 at 43
and `GGML_TYPE_COUNT = 44`. Tentatively append PQ3 at 44 and TBQ3 at 45;
confirm these assignments against the unchanged pin when creating the patch.
Preserve every existing ID. Audit Rust mappings too: the current `GgmlType`
conversion stops at Q2_0/42 and must not mistake 43 for a free slot. Add the
missing F8 mapping when updating this conversion and cover existing IDs in tests.

### Expected memory benefit

The recorded donor 128-value layouts are PQ3 = 50 bytes and TBQ3 = 68 bytes;
the source declarations must be reconfirmed in step 1. Using those layouts and
the existing LFM3 compact-cache capacities gives these **payload estimates**:

| K/V pair | Average effective bits/value | 4K K/V payload MiB | 32K K/V payload MiB |
| --- | ---: | ---: | ---: |
| Existing Q4/Q4 | 4.5000 | 15.609 | 110.109 |
| Proposed PQ3/PQ3 | 3.1250 | 10.840 | 76.465 |
| Proposed TBQ3/PQ3 | 3.6875 | 12.791 | 90.229 |

These calculations exclude weights, recurrent state, compute, transforms,
alignment overhead and driver allocations. At 4K, PQ3 saves only about 4.77 MiB
of payload against existing compact Q4/Q4; the corrected-key pair saves about
2.82 MiB. Extra buffers can erase that gain. Long-context savings and quality
relative to Q4 therefore matter more than compression claims against FP16.

## 2 Establish stronger quality gates

Extend the existing evaluation tooling with versioned, local fixtures:

1. Representative prose/code with fixed token streams and stable checksums.
2. Tool tasks using fixed host outputs, including a fixed clock response. Keep
   live-clock integration separate so timestamps do not confound codec comparisons.
3. Long-context retrieval with fixed distractors and known answers, at 4K and
   longer capacities that fit the model/device budget.

Keep model-backed evaluation under
`crates/assistant/tests/cache_quality.rs`, with small authored fixtures and a
checksummed manifest under `crates/assistant/tests/fixtures/`. Record exact
tokenization/model identity and provide a small smoke subset plus a separate
full evaluation job. Reuse the public runtime and typed test tools, rather than
adding deterministic-output switches to the production CLI. Prefer the existing
test/JSON tooling over adding an evaluation framework dependency.

Record FP16/full, FP16/compact, compact Q8/Q8 and compact Q4/Q4 before testing a
new codec. Keep the existing live two-tool failure visible. A codec experiment
must not rewrite application tool policy or weaken assertions to hide it.

Pin task counts and expected answers, and declare acceptable changes in
perplexity/target loss, retrieval and tool accuracy before inspecting codec
results. Use FP16 repeated runs to establish numerical/backend variation.
The existing seven-checkpoint repetitive corpus remains a wrapping regression
fixture; it is not the adoption-quality dataset.

Keep deterministic tests in the default gate and model-quality evaluations in
explicit required-fixture jobs. No automatic model or dataset downloads.

## 3 Deliver applicable native patches

All upstream source modifications go into
`crates/llama_cpp_sys/patches/*.patch`, applied in order to a disposable
build-directory copy. Never edit the pinned submodule. Native tests and shader
build changes inside upstream are included in those patches too.

Patch 0008 is implemented. Filenames 0009–0013 remain planned; no placeholder
patches are staged:

| Patch | Responsibilities |
| --- | --- |
| `0008-pq3-cpu-format.patch` | Append the PQ3 type; define layout/codebooks and CPU reference operations; add numerical/packing tests and native type/trait integration. |
| `0009-pq3-kv-graph-state.patch` | Validate pairs and geometry; wire transforms, full/SWA/hybrid caches, required operations, state safety and capability reporting. |
| `0010-pq3-vulkan-cache-attention.patch` | Add shader definitions, cache writes/copies and fused attention consumption; register only implemented pipelines and supported device paths. |
| `0011-tbq3-cpu-reference.patch` | Append the corrected-key type, residual sketch representation, CPU score correction and algebra/numerical tests. |
| `0012-tbq3-kv-graph-state.patch` | Integrate query projection/correction inputs, mixed TBQ3/PQ3 validation and versioned state behavior. |
| `0013-tbq3-vulkan-attention.patch` | Implement corrected-key writes and score correction in each advertised Vulkan attention path, with CPU/GPU comparisons. |

Each patch header records its base, donor provenance, dependencies, purpose,
ABI/storage consequences and tests. Keep type IDs and public names provisional
until step 1 freezes the format. If a patch is split further, retain explicit
dependencies and update the staging table and maintenance guide together.

### Build integration accompanies the first patch

Add an experimental `turboquant` Cargo feature, forwarded from assistant to
bindings to sys, off by default. It is independent of `vulkan`: CPU reference
builds must work without a GPU backend. Stage the codec patches only when this
feature is enabled; existing safety patches remain mandatory in all builds.

`patch_staging.rs` copies an explicit five-patch mandatory allowlist plus 0008
when enabled. Merely adding a patch file does not activate it. Update that allowlist
deliberately as each patch is delivered; fail if a required file is absent.
Do not add placeholder patches or silently skip a missing codec patch.

The active feature/patch selection must participate in source-copy and build
cache identity. Generate FFI from the selected patched headers and require a
codec ABI/capability guard when Rust enables the feature. Feature-disabled,
CPU-only and Vulkan builds must agree internally on types and symbols; stale
prebuilt libraries must fail clearly rather than accept incompatible layouts.

Verify each patch increment and the composed series using the existing Rust
patch verifier and the CLI patch engine. Cover clean and warm builds, changing
feature selection, overlapping hunks, new files and packaged source inclusion.
Compile/run relevant native numerical tests from a patched copy; successful
patch application is not numerical verification.

## 4 Integrate cache graphs and Vulkan

Use the CPU implementation as the reference for operations actually required
by KV writes and attention. Do not add a generic weight-quantization port or
every donor backend operation merely because it exists in the donor diff.

Define one transform contract for Q/K, V and output. Adapt it to the current
per-layer and full/SWA rotation inputs, including LFM3's hybrid graph. Audit
the existing Hadamard rotation to avoid missing or doubling a transform.
Required codec transforms cannot silently disappear when a rotation opt-out is
set. Reject incompatible context/model/global opt-outs for the new codec while
preserving existing-format behavior. Never mutate process environment state.

For QJL keys, validate score correction against a direct CPU implementation.
An ordinary dequantized-key dot product that omits the correction is not a
valid TBQ3 attention path. The non-flash fallback stays unsupported initially;
resolve automatic flash policy explicitly and reject an explicitly disabled one.

Implement Vulkan scalar attention first as a correctness reference, then the
available cooperative-matrix paths. Preserve optimized weight-matmul dispatch.
Register only complete combinations of format, geometry and shader path. A
missing pipeline must lead to a clear unsupported result, never a null pipeline
or an unannounced dispatch change. Scalar GPU fallback may be an explicit
experimental choice and must be reported; it does not imply performance parity.

Use the existing scheduler tracing outside timed runs to check cache writes,
attention and corrected-key operations. Extend it only if it cannot reveal an
essential placement fact. Preserve tensor-transaction callback ownership.
Do not add per-token host readback to the production timing path for diagnostics.

### State and cache ownership

State must identify compatible codec type, block geometry, layout version and
transform/codebook convention. Preserve existing-format state behavior. Reject
incompatible data before consuming it as cache payload.

If versioned new-format state I/O is not ready in the first prototype, reject
it explicitly through all exposed full/sequence save/load paths. This temporary
restriction must be documented and tested; returning plausible bytes under an
incomplete state contract is unacceptable. Require compatible round trips before
claiming persistence support. Shared caches must validate identical layouts.

## 5 Wire Rust and the real runtime

Extend safe storage mappings and raw conversions under the experimental feature.
Audit all public APIs that accept `GgmlType`: a cache-only type must not become
an accidentally accepted model-weight quantization option. Reject it at the
relevant fallible execution boundary without silently changing the request.

Add typed cache choices only once their required execution path is available.
Proposed CLI spellings are `pq3_0` and `tbq3_0`; first allow PQ3/PQ3, then
TBQ3/PQ3. Keep these independent of existing ternary `TQ1_0`/`TQ2_0` types.
Do not expose 64-value variants as aliases for 128-value layouts.

Extend the existing path rather than adding a separate inference implementation:

- `context/cache.rs`: formats, supported pairs, geometry, transform requirements
  and capability errors.
- `quantize.rs`: type mappings and cache-only restrictions.
- `LLMConfig` / `LLMModel::new_context`: validate before context allocation and
  apply the chosen native K/V types for every worker context.
- CLI: parse only compiled-in choices; do not change defaults.
- Diagnostics: actual type IDs, effective block dimensions, layout/version,
  transform state, memory estimates and execution-path support.

Update the application payload estimator from actual native block bytes and
geometry, including QJL metadata, rather than nominal bit counts. Report added
transform/query workspaces in the allocation breakdown. Extend both
`engine/diagnostics.rs` and the worker/CLI diagnostics tests; an unsupported
layout must remain explicitly unknown rather than acquire a guessed estimate.

Add an explicit required-Vulkan execution policy to the library/CLI for codec
experiments, alongside CPU reference execution. Its preflight checks must cover
required codec operations, not just GPU weight/cache allocations. Retain
post-execution placement audits as independent evidence. Preserve the current
automatic policy for existing formats.

New native C ABI helpers belong in maintained shims and use the shared TLS
status/error contract. These project-owned C++ files are edited directly;
upstream format, graph and kernel changes are patch files. No generated Rust
bindings are edited manually. Update API/feature documentation with each step.

## 6 Test and measure each milestone

| Area | Required checks |
| --- | --- |
| CPU format | Size/alignment assertions, fixed packing vectors, norm/scaling behavior, codebook boundaries, ties, tails/strides and supported stress inputs. |
| GPU operations | Quantization, copying, reconstruction and attention against CPU references, including partial rows and buffer boundaries. Use declared numerical tolerances where reduction order prevents exact byte equality. |
| Transforms and QJL | Q/K and V/output consistency; context-local isolation; direct corrected-score reference; no accidental second rotation. |
| LFM3 lifecycle | Window and allocation crossings, repeated wraps, near capacity, prefix append, divergence/trimming reset, cancellation, stale cursors, failure recovery and independent worker contexts. |
| State and safety | Supported round trips or explicit rejection; incompatible layouts; shared-cache checks; existing callback/model/backend lifetimes and process exit. |
| Negative support | Wrong pair, head size, architecture, rotation/flash policy, absent feature, absent GPU, missing pipeline and forbidden CPU fallback. |
| Compatibility | Existing FP16/Q8/Q4 behavior, native type IDs, current patch composition, CPU-only/default builds and required Vulkan jobs. |

Record the format contract and exact test inputs with the numerical tolerances.
Do not use identical generated text as a lossy-codec acceptance rule. Compare
fixed-token loss/divergence and task accuracy as well as real generation.

Extend `cache_bench` and `scripts/cache-results.py` with the new pairs and a
selectable, checksummed representative corpus. Keep child-process isolation,
resident warm trials and synchronized timing boundaries. Report cold/warm
startup, prefill, first-text latency, decode, native allocation snapshots,
payload estimates and peak telemetry where actually available. Fix microbatch
size when comparing codecs and tune it in a separate sweep.

Use paired FP16, Q8/Q8, Q4/Q4 and PQ3/PQ3 runs. Add TBQ3/PQ3 only when its
correction is complete. Measure short and nearly-full 4K, then representative
32K contexts where affordable, including simultaneous resident contexts.
Keep estimates, context/compute buffers, shared weights and driver-wide peak
memory separate. Compression must not automatically expand daemon limits.

Before interpreting results, declare task-specific quality tolerances from step
2. Investigate repeatable median prefill/decode or first-text regressions over
5%, using paired repeats and their spread. Require a real total-memory or
quality benefit over the existing compact quantizers to justify maintenance.
A codec that loses this comparison can remain a measured rejected experiment.

## 7 Validate hardware and decide promotion

Start on the available NVIDIA device with scalar and available matrix paths.
Run the same required-model cases and placement audits on representative
consumer AMD and Intel devices before advertising those configurations as
validated. Record each GPU/driver, feature overrides, effective kernel path and
unsupported cases; a Vulkan build alone is insufficient.

Do not change defaults as part of the prototype. A later default proposal must
name supported models/devices, quality tolerances, latency results and explicit
compatibility settings. A long-context deployment promising a byte ceiling
also needs the separate admission/accounting extension from the
[preparation plan](kv-cache-preparation-plan.md#change-7-select-defaults-and-bound-resident-memory).
The existing context-count cap does not provide a hard preallocation VRAM limit.

## Completion checklist

- [x] Donor audit and CPU row/support contract are fixed and attributed.
- [ ] Graph transform and new-format state identity are fixed before patch 0009.
- [x] Deterministic quality references and acceptance tolerances are recorded.
- [x] PQ3 CPU patch and numerical tests pass.
- [x] Feature staging, composed patch verification and clean/warm reference builds pass.
- [ ] PQ3 LFM3 full/compact inference and recovery pass on Vulkan.
- [ ] Rust and ordinary worker/CLI paths use the selected codec and report it.
- [ ] PQ3 has a measured comparison with compact Q8 and Q4.
- [ ] Corrected-key CPU/graph/Vulkan patches pass their separate gates.
- [ ] State I/O is versioned and tested, or explicitly unavailable for the prototype.
- [ ] Hardware coverage and unsupported combinations are accurately recorded.
- [ ] Workspace format/check/tests, required model/GPU tests and real generation pass.
- [ ] Defaults and production rollout are decided separately from prototype delivery.

The first slice delivers the CPU row contract, quality fixtures/gates, patch
0008 and its experimental build selection. Next, settle the graph transform and
state contract in 0009, then implement and validate Vulkan operations in 0010.
Safe cache/CLI exposure waits for those operations; no PQ3 production selection
is available from the CPU reference alone.
