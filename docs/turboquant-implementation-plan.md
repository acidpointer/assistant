# TurboQuant implementation plan

Status: experimental PQ3 row/CPU/Vulkan implementation delivered, 2026-11-04.
This follows the [preparation baseline](kv-cache-baseline.md). Patches 0008–0010
and ordinary worker/CLI selection now exist. The
[row and inference contract](pq3-reference-contract.md),
[transform ADR](decisions/0002-pq3-graph-transform.md) and
[fixed quality evaluation](cache-quality-evaluation.md) define the boundaries.
Defaults remain FP16 K/V with full SWA. The [measured evaluation](pq3-inference-evaluation.md)
rejects promotion: PQ3 fails retrieval/KL gates and repeats a near-full 4K
slowdown against Q4. QJL/TBQ3 remains deferred.

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

At the end of that earlier CPU slice, the submodule was unchanged and clean;
there was no PQ3 inference result, cache selector, Vulkan kernel or AMD/Intel
validation. Native context creation then rejected reference-only PQ3 before
allocation. The graph/Vulkan stage below supersedes those earlier inference
restrictions. Defaults stay FP16/full.

## Next implementation stage: PQ3 cache inference

Status: patches 0009 and 0010 and their guarded binding/runtime integration
are implemented. The following records the authorized design and exit gates
against which the implementation is evaluated. QJL/TBQ3 stays deferred
until this experiment demonstrates a useful tradeoff. Keep the native pin and
patch 0008's row layout/ABI unchanged.

### Inspected integration points

These paths are inside the pinned submodule and identify **patch targets**, not
files to edit directly. Line numbers refer to pristine b12015; earlier patches
move some lines.

| Current code | Consequence for this stage |
| --- | --- |
| `src/llama-kv-cache.cpp:310–344` chooses conditional K rotation and normally 64-dimensional V rotation. | PQ3 must require 128-dimensional transforms for both, independently of the old-format heuristic. |
| `src/llama-graph.cpp:3174–3219` and `3450–3514` rotate Q/K/V and apply the same V matrix again on output. | Plain Hadamard is self-inverse; seeded rotation needs an explicitly different inverse composition in ordinary and full/SWA graph paths. |
| `src/llama-impl.h:57` marks the matrix multiplication as `GGML_HINT_SRC0_IS_HADAMARD`; CPU and Vulkan consume that hint. | Do not substitute a signed dense matrix under this hint: optimized kernels can compute plain Hadamard instead of the supplied signed transform. |
| `src/llama-kv-cache.cpp:1379–1467` writes K/V with `SET_ROWS`. | Add actual indexed PQ3 writes; type registration and a contiguous quantization helper alone cannot populate the live cache. |
| `ggml/src/ggml-cpu/ops.cpp:8834` obtains K dot and V reconstruction through type traits. | Exercise the actual CPU flash-attention operation with PQ3 before adding a redundant attention implementation; the existing reference traits may supply much of it. |
| `ggml/src/ggml-vulkan/ggml-vulkan.cpp:15655–15790` explicitly allowlists flash-attention, indexed-write and copy types. | Each advertised operation needs a shader, pipeline registration, dispatch and matching capability check. |
| `src/llama-kv-cache.cpp:2253` and `2532` serialize/check rotation dimensions. | Dimensions cannot distinguish plain Hadamard from a seeded transform. Block prototype state I/O before any reads/writes until a versioned identity exists. |
| `crates/assistant/src/engine/llm.rs` currently offers execution `Auto` or `Cpu`; quality jobs inspect weight-device names. | Add a required-Vulkan contract and actual operator-placement evidence; weights on Vulkan alone are insufficient. |

### Planned decisions and invariants

The following prototype choices are implemented and frozen by the
[ADR](decisions/0002-pq3-graph-transform.md).

**Transform.** For column-vector notation, use `T = H D`, where H is the
normalized 128-dimensional Sylvester Hadamard matrix and D is a fixed diagonal
sign matrix. Define `H[i,j] = (-1)^popcount(i & j) / sqrt(128)`. Use the audited
sign words `40f54e8c, 6587b7b0, c31220ea, 32f6449b` in that order; coordinate i
uses bit `i % 32` of word `i / 32`, with a set bit meaning -1 and a clear bit +1.
This explicit local convention is the contract; verify donor equivalence rather
than assuming its seed name establishes bit order. No runtime PRNG or mutable
sign initialization is needed. This transform identity is separate from the
unchanged row-format ABI.

Apply D then H to Q/K and V, after the model's existing Q/K preparation and
before cache writes/attention. Apply `T^-1 = D H` to the attention result, meaning
H then D before the existing output projection. Initially use the same fixed
transform for K and V and all attention layers. Keep it out of recurrent layers.
The row codec continues to quantize already-transformed coordinates.

Implement the composition with a sign multiplication and the existing plain-H
operation, preserving its fast-transform hint only on H. Audit GGML tensor and
matrix orientation with basis-vector tests instead of equating memory order
with the mathematical notation. Avoid a new GGML operation in the first port.
Share immutable sign storage per compatible backend where straightforward;
account for actual matrix/sign buffers and intermediate tensors. Fuse signs
only after the unfused CPU/GPU graph is verified and profiling warrants it.

**Support.** First allow exactly PQ3/PQ3, causal inference, one sequence, and
128-dimensional K and V heads in each attention-bearing layer of the inspected
LFM3 topology or an explicitly validated ordinary Llama fixture. Validate
per-layer geometry; skip recurrent zero-KV layers rather than rejecting their
zero head dimensions. Cover GQA, full and compact SWA. Exclude MLA/DSA/special
attention, training, speculative decoding, shared-context cache ownership and
mixed PQ3/old-format pairs initially. Return clear unsupported errors for those
combinations; do not broaden support from a generic type-trait match.

Required rotation cannot be disabled by context, model or environment opt-outs.
Reject conflicts; never change environment variables or shared model settings.
Resolve automatic flash attention to enabled for this pair; explicitly disabled
flash attention is unsupported. Do not silently fall back to a non-flash graph.

**State and positions.** Disable external full/sequence state get/set/size and
file save/load for PQ3 in the prototype, including legacy aliases and extended
`ON_DEVICE`/`PARTIAL_ONLY` paths. Reject before buffer writes, file creation,
on-device handle interpretation, or mutation of KV/recurrent state. Preserve
old-format behavior and existing state layout. Guard new shim calls with the
shared TLS error mechanism; ensure existing numeric/bool native entry points
return their failure sentinel, with no new exception crossing C ABI boundaries.
Do not rely on a late exception in `memory->state_read/write`: hybrid memory
could already have consumed metadata or recurrent state by then.

Disable positional K shifting (`get_can_shift`, `seq_add`/`seq_div` paths) for
PQ3 initially. Its current implementation also assumes the same transform is
both forward and inverse and would requantize old keys. Compact SWA cell reuse
without position rebasing remains supported. Preserve prefix append and full
reset/replay; arbitrary hybrid rewind remains excluded. Verify that runtime
recovery uses those paths and does not require an external state snapshot.

**Placement.** CPU reference execution must be explicit. Runtime PQ3 GPU
experiments require Vulkan support for the selected cache writes, attention,
transforms and copies on their actual devices. Missing support is an error,
not an automatic CPU detour. Existing FP16/Q8/Q4 `Auto` behavior stays unchanged.
Distinguish supported operations, selected kernel path and observed placement.

### Ordered changes and exit gates

| Order | Deliverable | Exit gate |
| --- | --- | --- |
| A | Freeze graph transform/support contract and host oracles. | Basis vectors, norm/dot preservation, inverse order and K/V head boundaries pass independently of quantization; existing formats retain their transforms. |
| B | `0009-pq3-kv-graph-state.patch`: native validation, graph transforms, CPU live-cache path, state/shift rejection and graph ABI guard. | Real CPU `SET_ROWS` plus `FLASH_ATTN_EXT` tests pass; CPU model inference, cache wrap/reset/replay and negative support tests pass. GPU PQ3 remains rejected until 0010. |
| C | `0010-pq3-vulkan-cache-attention.patch`: portable GPU writes/copies and scalar flash attention. | Guarded buffer tests, CPU-oracle comparisons and real LFM3 full/compact inference pass; placement tracing shows required operations on Vulkan. |
| D | Complete validated cooperative-matrix paths within 0010. | Implemented scalar/coopmat1/coopmat2 routes agree within declared tolerances; missing routes are explicitly gated and reported. Benchmark prefill and decode before claiming performance support. |
| E | Safe bindings, diagnostics and runtime/CLI integration. | A typed `pq3_0` request reaches every ordinary worker context; unsupported builds/models/devices fail clearly; existing selectors/defaults remain stable. |
| F | Quality, memory, latency and vendor evaluation. | Preserved quality gates and paired compact Q8/Q4 comparisons produce a documented proceed/revise/stop decision. No default-policy change. |

Do not create placeholder patches. Stage 0009 only when B is usable and tested;
stage 0010 only with its implemented capability checks. Keep rejection of GPU
PQ3 in the 0009-only build explicit. The portable scalar route is the correctness
milestone, not the performance conclusion; unsupported matrix routes must never
reuse an old-format shader or silently select an unreported path.

### Patch 0009 details

Patch `src/llama-context.{h,cpp}`, `src/llama-kv-cache.{h,cpp}` and the graph
input/build code as needed. Enforce the allowlist before context cache/buffer
allocation and check the shared-cache source before the existing `other` fast
path can inherit its layout. Retain patch 0008's model-weight loading/quantization
rejections. Add a separate graph ABI/capability version; the base and CPU row ABI
symbols do not prove graph support. Avoid changing `llama_context_params` unless
an explicit policy cannot be represented by current parameters; any new field
would require an appended layout and its own compatibility guard.

Test generic CPU indexed writes, quantized copies/contiguity and flash attention
against small synthetic graphs. Audit the CPU operation support tables and
workspace sizing; add only missing integration. Separate three references:

- Transform-only F32 correctness, without quantization error.
- Frozen PQ3 bytes plus independent decoded-coordinate F64 attention, using an
  F32 query; this is the Vulkan attention oracle.
- CPU PQ3/Q8 dot/flash attention, which additionally quantizes the query to Q8.

Do not require CPU Q8-query scores to equal F32-query GPU scores bit-for-bit.
Add native tests through the patch and keep the packaged-source and CLI/Rust
patch-engine tests covering the composed 0003–0009 series.

### Patch 0010 details

Extend `types.glsl`, quantization/reconstruction helpers,
`copy_to_quant.comp`, `copy_from_quant.comp`, `flash_attn_dequant.glsl` and the
necessary flash-attention variants. Wire generated shaders and pipelines through
`vulkan-shaders-gen.cpp`, `ggml-vulkan.cpp` and related backend declarations.
Do not append PQ3 to a global type list that generates weight matmul kernels.
Register only the copy/write/attention paths that this cache actually needs.

Test the exact 50-byte block stride in arrays of multiple blocks: std430
alignment, the half at byte 48, packed index groups crossing bytes, nonzero
buffer offsets and row strides must match the host contract. Cover F32/F16
sources and I32/I64 indices only when advertised, repeated indexed writes,
untouched-row sentinels, zero/tiny values, masked/all-masked rows, GQA and split-K
reductions. A test-only host readback is appropriate here; production decode
must consume the compressed cache without expanding the entire cache into a
persistent F16/F32 shadow buffer.

Keep exact golden-byte tests for representable deterministic blocks. For
parallel norm reductions and score accumulation, declare input-scaled tolerances
in test fixtures before evaluating the GPU results; allow only justified
near-boundary index differences rather than relaxing arbitrary byte mismatches.
Specify live CPU/GPU handling of non-finite activations and norm overflow
separately from the checked `Pq3Block` interface: the raw row codec currently
requires valid finite input. Include numeric-negative cases in operation tests
and do not silently convert invalid norms to plausible zero blocks. Any GPU
error reporting must use existing synchronization boundaries rather than adding
a diagnostic host readback on every cache write.

Start the scalar oracle with F32 accumulators. Validate lower-precision/matrix
paths separately against decoded operands and the unchanged model-quality gates.

Use one capability predicate for pipeline availability and backend support
advertisement. Add an actual-device preflight using metadata-only representative
nodes before cache allocation, then retain post-scheduling placement checks.
Audit fused RMS/RoPE/write paths and alternative attention/decode dispatches so
none can bypass PQ3 handling. Returning `supports_op = true` without a complete
pipeline and correct stride logic is a correctness defect.

### Binding and runtime integration

After the native gates pass, add feature-gated `KvCacheType::PQ3_0` and its
required `GgmlType` conversion. Validate the pair and per-layer geometry in
`context/cache.rs`. Reject the cache-only type at safe weight-quantization
boundaries as well as in the native loader/quantizer. Keep `Pq3Block` as the
model-free oracle API; its existence does not establish context capabilities.

Add a guarded capability query carrying codec/layout/transform identity,
supported dimensions/pairs, state/shift restrictions and actual device/path
support. Evolve cache inspection with a versioned record or new symbol rather
than changing an existing C struct under an old ABI. Rust diagnostics must own
all results and remain confined to the existing context/worker lifetime rules.

The later application diff is explicitly limited to configuration plumbing in
`engine/llm.rs`, CLI parsing in `main.rs`, byte accounting in
`engine/diagnostics.rs`, and their tests/evaluation/benchmark harnesses. Add a
required-Vulkan execution mode, resolve PQ3's automatic request to an explicit
supported policy, and preserve the existing single worker/inference path.
Include capability checks at each context creation, not only initial model
registration. Session, tool, prompt, cancellation and orchestration policy are
outside the implementation. These are human-owned files; this plan proposes
the minimal diff for review and does not edit them.

Report the selected pair, layout/transform version, actual 128-value/50-byte
blocks, full/SWA capacities, effective flash mode, kernel path and placement
status. Derive payload bytes from native metadata and account separately for
rotation/sign/intermediate buffers, recurrent state, shared weights and compute.
A selected GPU device is not a measured peak-memory value or a placement trace.

### Required validation and decision

Run default and CPU-only experimental builds, clean and warm feature selection,
packaging and real-series patch verification at each increment. Run workspace
format/check/tests and native numerical tests. Add required-model CPU/Vulkan
jobs for short and nearly-full 4K, repeated compact-SWA wraps, append/divergence,
reset/replay, cancellation/recovery, stale generation cursors, independent
contexts and process exit. For persistence/shift failures, verify no partial
state/file/buffer mutation and that existing formats still work.

Extend the authored quality harness and parser to include the candidate while
retaining the current fixture/scoring version and FP16/Q8/Q4 references. Required
Vulkan jobs must use actual operation placement; replace the weight-name-only
assertion for this new claim. Keep correctness traces outside timed benchmarks.
Use paired isolated release runs with fixed batch/microbatch, resident warm
trials and synchronized timing boundaries. Measure prefill, first text, decode,
startup, total context/compute allocations and actual driver telemetry when
available. Start at 4K and then test 32K where the model/device budget permits;
32K is a separate required evaluation before claiming a long-context benefit.

Preserve the declared 0.05-nat mean NLL increase, 0.05 maximum checkpoint KL,
no loss on cases passed by both FP16/full and Q4, and no retrieval-accuracy drop
against the stronger reference. Compare case identities as well as aggregate
scores. Keep FP16/Q8's 12/17 and Q4's 10/17 exact-answer failures visible.
Investigate repeatable latency/throughput regressions over 5% with trial spread.
At 4K the estimated PQ3 payload saving against compact Q4 is only 4.77 MiB;
extra workspace can erase it, so require measured total-memory or quality value.

Start on the available NVIDIA GPU. AMD and Intel need the same operator,
model/lifecycle and performance evidence on real consumer devices before their
paths are called validated; compile-only or software-Vulkan checks do not
substitute for those runs. Publish unsupported paths and hardware gaps plainly.
If quality or performance fails, preserve the result and decide whether to
revise PQ3 or proceed to a separately scoped QJL experiment. Do not automatically
start patches 0011–0013 or change defaults to rescue an unsuccessful experiment.

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

Do not copy donor type IDs. The pristine native enum has F8_E4M3 at 43 and
`GGML_TYPE_COUNT = 44`. Delivered patch 0008 appends PQ3 at 44 and raises the
experimental count to 45; the safe Rust F8 mapping is implemented. Preserve
those assignments and existing IDs. TBQ3 at 45 remains a later proposal, subject
to confirmation before its patch is created.

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
- [x] Graph transform is fixed; external state is explicitly unavailable.
- [x] Deterministic quality references and acceptance tolerances are recorded.
- [x] PQ3 CPU patch and numerical tests pass.
- [x] Feature staging, composed patch verification and clean/warm reference builds pass.
- [x] PQ3 LFM3 full/compact inference and reset/replay pass on Vulkan.
- [x] Rust and ordinary worker/CLI paths use the selected codec and report it.
- [x] PQ3 has a measured comparison with compact Q8 and Q4; promotion is rejected.
- [ ] Corrected-key CPU/graph/Vulkan patches: deferred, not part of this measured PQ3 stage.
- [x] State I/O is explicitly unavailable and rejection is tested.
- [x] Hardware coverage and unsupported combinations are recorded; AMD/Intel remain unvalidated.
- [x] Workspace format/check/tests, required model/GPU tests and real CLI generation pass.
- [x] Defaults stay FP16/full; this candidate is not promoted. Production rollout remains separate.

The CPU slice is followed by implemented graph/state patch 0009 and Vulkan
patch 0010. Safe cache/CLI exposure uses these native operations in the existing
worker path. QJL, persistent state, vendor expansion and production-default
policy remain separate decisions.

## Implementation evidence and numerical controls

Native graph tests cover basis vectors and inverse ordering, head boundaries,
indexed writes/copies, independent decoded-operand attention, all-masked rows,
full/compact model inference, 900-token repeated-window reuse, and state/shift
rejection. Same-configuration full reset/replay is bounded to 2e-5 logit delta.

A proposed full-versus-compact whole-model 0.05 maximum-logit assertion was
invalidated by the existing FP16 control: it drifted 0.509114504 after the long
repeated-token trajectory. CPU PQ3 drift was 0.807980776 and Vulkan PQ3 drift
0.217959166. Physical row ordering changes floating-point attention reductions,
which recurrent trajectories amplify. These are characterization values, not
passed tolerances. The invalid cross-layout assertion was replaced with an
independent row-permutation operation oracle and strict same-layout replay;
the model-quality gates were not relaxed.

On RTX A500 Laptop, required-device Vulkan tests pass scalar, coopmat1 and
coopmat2 paths against the independent decoded F64/F32-query oracle with
preregistered `abs <= 3e-4 + 3e-4*abs(expected)`. Largest observed error was
0.000220765483. Each path observed eight completed indexed-write and three
attention operations with its expected path bit. CPU attention separately uses
a Q8-query oracle. GPU model full/compact wrap and reset/replay pass.

The implementation rejects unsupported state/shift/speculative/training and
shared-cache configurations; it does not add a persistent state format or QJL.
See the inference contract for API compatibility and live numeric preconditions.


## Measured stage decision

The [evaluation](pq3-inference-evaluation.md) and its versioned artifacts record
completed CPU/Vulkan/runtime implementation, 102 scored tasks and 119 numerical
records, paired release 4K/32K measurements, a reverse-order regression repeat,
scalar timings, placement audits and API/support limitations. Default workspace
572 tests and feature-enabled workspace 580 tests pass, with 14 ignored each.
Required real-model CPU and Vulkan binding tests and an actual CLI generation
pass. Patch staging/composition and source packaging include 0009/0010.

PQ3 full/compact score 11/17 versus FP16/Q8 12/17. Mean NLL delta +0.007044
passes, but worst KL 0.125069 and one lost jointly-passed retrieval case fail
the unchanged gates. Reversed near-full 4K trials confirm 10.83% lower prefill
and 6.82% lower decode throughput versus Q4. Actual total native allocation
savings versus compact Q4 are 4.249 MiB at 4K and 33.124 MiB at 32K capacity.

This completes the bounded PQ3 experiment with a **do not promote** decision.
AMD/Intel, nearly-full 32K quality/recovery, simultaneous resident contexts and
an isolated split-K regression case remain unvalidated. Do not treat optional
future milestones in this broader plan as completed or automatically authorize
QJL, new defaults, persistent state or relaxed quality thresholds.
