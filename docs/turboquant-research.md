# TurboQuant and KV cache compression for Vulkan

Status: research and implementation proposal, 2026-11-04.

This preserves the inspected pre-implementation source baseline. Existing-cache
configuration, diagnostics, compact SWA and Q8/Q4 evaluation have since been
implemented; use the [measured baseline](kv-cache-baseline.md) for current APIs,
runtime behavior and results. The experimental
[PQ3 CPU reference](pq3-reference-contract.md) is now implemented as patch
0008; live TurboQuant cache inference and Vulkan kernels remain proposed.

The [TurboQuant implementation plan](turboquant-implementation-plan.md) defines
the next native patch series and its runtime/validation gates.

The [KV cache preparation plan](kv-cache-preparation-plan.md) sequences the
existing-engine work to complete before the native codec experiment.

The first useful memory reduction is already available in the pinned engine:
configure quantized KV caches and evaluate compact sliding-window allocation.
Both are exposed by the Rust bindings. Full TurboQuant support requires native
cache formats and attention kernels; it cannot be added by extending Rust enum
values alone.

For this Vulkan-focused daemon, the preferred sequence is to establish the
existing FP16, Q8 and Q4 baselines, then evaluate a selective port of QVAC's
CPU/Vulkan implementation. Keep native changes as ordered, applicable patches
over the existing llama.cpp pin. Replacing the engine with a complete fork would
also require reconciling model support and the maintained safety patches.

This assessment targets ordinary consumer PCs, including machines with little
VRAM. Apple-specific optimizations are outside the target. The memory figures
below are calculations from model metadata and source layouts, not measurements
of a TurboQuant-enabled application. No codec implementation or inference
configuration was changed during the research.

## Contents

- [Current project behavior](#current-project-behavior)
- [What TurboQuant adds](#what-turboquant-adds)
- [Implementation options and donor code](#implementation-options-and-donor-code)
- [Cache formats and actual storage costs](#cache-formats-and-actual-storage-costs)
- [LFM3 memory estimates](#lfm3-memory-estimates)
- [Performance implications](#performance-implications)
- [Proposed integration](#proposed-integration)
- [Validation and adoption criteria](#validation-and-adoption-criteria)
- [Evidence and reproduction](#evidence-and-reproduction)

## Current project behavior

The inspected workspace revision is
`edfde54af88993baa3c5ad4545d9d75dd52d1672`. Its native submodule is pinned to
`a34de8618b25dfc32ee43749db47d767a9f0a25d` (`b12015`). Repository-relative
links in this section identify the implementation at that baseline. The
[binding maintenance guide](bindings-fork.md) defines the patch and ownership
rules; the [architecture](architecture.md) describes context lifetime and reuse.

### Application defaults

[`LLMModel::new_context`](../crates/assistant/src/engine/llm.rs), lines 114–125,
sets context size, batch size and thread counts. It does not override K/V cache
types, flash-attention policy or sliding-window allocation.

[`LlamaContextParams::default`](../crates/llama_cpp/src/context/params/mod.rs),
lines 696–712, forwards native defaults. At the pin,
[`llama_context_default_params`](../crates/llama_cpp_sys/llama.cpp/src/llama-context.cpp)
uses `type_k=F16`, `type_v=F16` and `swa_full=true` around lines 4070–4079.
Consequently, the application's default 4,096-token context uses FP16 caches
and allocates the full context capacity for its sliding-window layers too.

### Capabilities already exposed by Rust

| Capability | Existing API | Source |
| --- | --- | --- |
| Key-cache storage | `with_cache_type_k(GgmlType)` | [Context parameters](../crates/llama_cpp/src/context/params/mod.rs), line 566 |
| Value-cache storage | `with_cache_type_v(GgmlType)` | [Context parameters](../crates/llama_cpp/src/context/params/mod.rs), line 590 |
| Attention-rotation opt-out | `with_attn_rot_disabled(bool)` | [Context parameters](../crates/llama_cpp/src/context/params/mod.rs), line 621 |
| Flash-attention policy | `with_flash_attn_type(LlamaFlashAttnType)` | [Advanced parameters](../crates/llama_cpp/src/context/params/advanced.rs), line 19 |
| Full or compact SWA allocation | `with_swa_full(bool)` | [Advanced parameters](../crates/llama_cpp/src/context/params/advanced.rs), line 195 |
| Native allocation accounting | `LlamaContext::memory_breakdown` | [Memory accounting](../crates/llama_cpp/src/context/memory_breakdown.rs) |

For example, the existing binding API can express this experimental profile:

```rust
use llama_cpp::context::params::{LlamaContextParams, LlamaFlashAttnType};
use llama_cpp::quantize::GgmlType;

let params = LlamaContextParams::default()
    .with_cache_type_k(GgmlType::Q8_0)
    .with_cache_type_v(GgmlType::Q4_0)
    .with_flash_attn_type(LlamaFlashAttnType::Enabled)
    .with_swa_full(false);
```

This is a configuration example, not a validated default. Application exposure
through `LLMConfig` or CLI arguments is still needed. Enabling quantized V
requires flash attention: native context initialization enables it for the
automatic policy and rejects an explicitly disabled policy. It also checks
cache-block alignment against head dimensions. See
[native initialization](../crates/llama_cpp_sys/llama.cpp/src/llama-context.cpp),
lines 4131–4163.

### Existing attention rotation

Upstream [PR #21038][rotation-pr] added backend-agnostic Hadamard rotation for
existing quantization formats. Our pin already contains it. The
[KV constructor](../crates/llama_cpp_sys/llama.cpp/src/llama-kv-cache.cpp),
lines 310–328, enables rotation for quantized K/V when the relevant head
dimensions are compatible, unless model, context or environment policy disables
it. The key alignment test is divisibility by 64, rather than the simplified
power-of-two description in some Rust comments.

The [attention graph](../crates/llama_cpp_sys/llama.cpp/src/llama-graph.cpp)
rotates Q and K consistently, rotates V before storage, and applies the inverse
V transform to the attention output. It has separate rotation inputs for full
and sliding-window caches, including the hybrid path used by LFM3.

The Rust comments around `with_cache_type_k` and `with_attn_rot_disabled` call
this feature TurboQuant. They should eventually describe it as attention
rotation: the pin has no `TURBO*`, `TBQ*` or `PQ*` cache formats. Existing
`TQ1_0` and `TQ2_0` names designate ternary formats and do not establish
TurboQuant cache support.

### Existing Vulkan cache support

The Vulkan `FLASH_ATTN_EXT` capability check accepts F32, F16, BF16, Q8_0,
Q5_0, Q5_1, Q4_0, Q4_1 and IQ4_NL, subject to its additional layout and device
checks. See [Vulkan dispatch](../crates/llama_cpp_sys/llama.cpp/ggml/src/ggml-vulkan/ggml-vulkan.cpp),
lines 15655–15708. A type appearing elsewhere in ggml is insufficient evidence
that Vulkan flash attention supports it: Q2_0, for example, exists in the pin
but is absent from this allowlist.

## What TurboQuant adds

The [TurboQuant paper][paper] distinguishes reconstruction error from error in
inner products. Its MSE-oriented method rotates a vector so coordinate
distributions become easier to quantize, then uses optimized scalar codebooks.
For inner products, it adds a Quantized Johnson–Lindenstrauss (QJL) sketch
with one sign bit per residual projection to correct estimation bias. Keys
participate in query–key inner products, while values are combined into the
attention output; their quality and implementation tradeoffs need separate
evaluation.

Rotation alone preserves attention algebra before quantization. If `R` is
orthogonal, `(Rq)^T(Rk) = q^T k`. Consistently rotating V and applying the
inverse to the resulting weighted sum also preserves that sum. Quantizing the
rotated vectors introduces error; rotation is not a guarantee of identical
model output.

The extra native work beyond our existing rotation is therefore the storage
codec, its codebooks and metadata, and kernels that consume that representation.
QJL correction must participate in the inner-product calculation. Treating a
QJL format as an ordinary dequantized vector can omit part of the estimator.

KV compression leaves the GGUF model weights unchanged and requires no new
weight download. It does not compress LFM3's short-convolution state, ordinary
compute buffers or other allocations merely because they belong to the same
context.

Google reports substantial compression and an attention-logit speedup of up to
8× for four-bit TurboQuant against FP32 keys on H100. That comparison is not
whole-model generation throughput, and our current cache baseline is FP16.
Consumer Vulkan performance must be evaluated at the application level.
[Google Research announcement][google-blog]

## Implementation options and donor code

| Option | Rust work | Native work | Assessment |
| --- | --- | --- | --- |
| Existing rotated Q8/Q4 caches and compact SWA | Expose configuration, validate combinations and report actual settings | No new codec | Preferred first experiment; uses the maintained engine directly. |
| Selective port of a TurboQuant implementation | Add type mappings and cache-specific validation | Formats, CPU reference, graph integration and Vulkan kernels | Preferred route to full support if measurements justify it. |
| Adopt a complete fork | Reconcile binding APIs and application configuration | Reapply our maintained patches and reconcile upstream divergence | Greater maintenance and compatibility burden; not needed to obtain the codecs. |
| Implement an independent Rust attention backend | Build an equivalent native-backend interface and ownership model | GPU kernels are still required, even if authored through Rust tooling | An engine project, not a small binding extension. |

### QVAC CPU and Vulkan implementation

[QVAC PR #115][qvac-pr] was merged on 2026-04-28 into the QVAC fork, not
ggml-org mainline. Its API record reports 50 changed files, 12,424 additions
and 302 deletions. The captured diff separates into approximately 4,957 added
lines across 35 native/tooling files and 7,467 across 15 test/script/CI files.
Those counts describe the donor change, not an estimate of the final port.
[PR metadata][qvac-api], [complete diff][qvac-diff]

The donor contains codebook-only `PQ3/PQ4` formats and QJL-corrected `TBQ3/TBQ4`
formats, with 64- and 128-value blocks. Its changes include CPU operations,
cache writes, Vulkan dequantization, and scalar/CoopMat1/CoopMat2 attention
paths. Tests cover quantization functions, backend operations and subgroup
copies; evaluation scripts cover throughput, perplexity and RULER retrieval.
[Native Vulkan changes][qvac-vulkan], [CPU and layout changes][qvac-common],
[backend tests][qvac-tests]

This is the strongest donor for evaluating QJL-corrected keys on Vulkan. It
also exposes a useful lower-cost codebook-only path. Its support matrix is
not universal: some mixed K/V combinations lack matching GPU pipelines, and
the non-flash path has additional restrictions. A port must define supported
pairs explicitly instead of assuming that every pair of enum values works.
[Donor support matrix][qvac-pr]

### TheTom and related forks

The inspected TheTom revision is
`643956891dfdf51aecdbd05ca508dd21e21e8195`, committed on 2026-10-22. Its
`turbo3` storage is a codebook-only representation without QJL. The actual
block declaration uses 128 values and 50 bytes, despite nearby comments still
describing an older 32-value layout. The default `turbo4` declaration similarly
uses a codebook-only layout. Future ports should derive sizes from definitions
and assertions, not comments or format names. [Block definitions][thetom-common]

The Vulkan implementation has fused TurboQuant dequantization for scalar and
CoopMat1 attention. When a TurboQuant cache would select CoopMat2, dispatch
explicitly chooses CoopMat1 or scalar instead. That fallback remains on the
GPU, but can change performance substantially on NVIDIA hardware.
[Dispatch policy][thetom-vulkan]

The fork also changes ordinary attention rotation to default off and carries
automatic asymmetric/layer-adaptive policies. These change the comparison
baseline as well as the codec. It also forces FP32 accumulation for quantized
KV in the inspected Vulkan attention path. Whole-fork speed or quality results
cannot therefore be attributed to the storage format alone.
[KV policies][thetom-kv], [attention implementation][thetom-vulkan]

The inspected architecture enum contains LFM2 and LFM2MoE but no LFM3. This
revision cannot replace the pinned engine for the current model directly.
[Architecture enum][thetom-arch]

The [gl33mer derivative][gl33mer] and [AtomicBot derivative][atomic] advertise
Vulkan support and packaged builds. Their descriptions help locate related
work; they do not establish measured LFM3 parity or performance on our target
devices. The separate [llama-tq Vulkan documentation][llama-tq] explicitly
distinguishes its ordinary Vulkan backend from CUDA-only TurboQuant cache
types. A Vulkan build option or binary is not proof of Vulkan codec coverage.

### Upstream status at the inspected pin

The pin contains attention rotation, not the donor cache types. The separate
upstream [CPU-only TurboQuant PR #21089][cpu-pr] is closed; it should not be
treated as a merged Vulkan implementation. These observations describe the
inspected sources and dated proposals, not a promise about future upstream
releases.

## Cache formats and actual storage costs

Effective bits per value include block metadata. At the pin, Q8_0 stores
32 values in 34 bytes, or 8.5 bits/value; Q4_0 stores 32 in 18 bytes, or
4.5 bits/value. Their symmetric K/V storage reductions against FP16 are
approximately 1.88× and 3.56× respectively.
[Baseline block definitions](../crates/llama_cpp_sys/llama.cpp/ggml/src/ggml-common.h)

The QVAC donor defines the following layouts. The 128-value variants match the
attention head dimensions of the inspected LFM3 model.
[Donor block definitions][qvac-common]

| Format | Bytes per 128 values | Bits/value | Bytes per 64 values | Bits/value | QJL residual sketch |
| --- | ---: | ---: | ---: | ---: | --- |
| PQ3 | 50 | 3.125 | 26 | 3.25 | No |
| PQ4 | 66 | 4.125 | 34 | 4.25 | No |
| TBQ3 | 68 | 4.25 | 36 | 4.5 | Yes |
| TBQ4 | 84 | 5.25 | 44 | 5.5 | Yes |

For example, PQ3 at dimension 128 consists of 48 bytes of indices and a
two-byte norm. TBQ3 adds 16 bytes of sketch signs and a two-byte residual
norm. Thus a format described as three-bit may consume 3.125 or 4.25 bits per
value depending on whether correction is included. Some numbers in the donor
PR prose reflect different block sizes; the layout definitions settle the
storage arithmetic.

K and V precision also changes the total. With equal K/V element counts,
Q8_0 keys plus 3.125-bit values average 5.8125 bits/value: approximately 2.75×
smaller than FP16, but larger than symmetric Q4_0. Conversely, TBQ3 keys and
PQ3 values average 3.6875 bits/value. Comparisons must hold quality and the
actual K/V pair in view, rather than comparing format labels alone.

## LFM3 memory estimates

### Model identity and cache structure

The local model examined was LFM3 1.7B Heretic Abliterated Q6_K. Its GGUF
identity and relevant metadata were read directly; the file itself remains
outside version control.

| Field | Value |
| --- | --- |
| GGUF SHA-256 | `616775686c9abcbc1a0d302f75dc0a1f12498f272170bd7cda556a706c5606e1` |
| File size | 1,427,637,536 bytes, approximately 1,361.5 MiB |
| GGUF version | 3 |
| Tensor count | 287 |
| Metadata entries | 35 |
| Architecture | `lfm3` |
| Blocks | 24 |
| Declared GGUF context length | 262,144 |
| Embedding length | 2,048 |
| Query heads | 16 |
| KV heads in attention layers | 4 |
| Key and value head dimensions | 128 each |
| Sliding window | 256 tokens |
| Short-convolution cache length | 7 |

The per-layer metadata gives these zero-based layer indices:

- Full attention: `2, 6, 10, 14, 18, 22` — six layers.
- Sliding-window attention: `4, 8, 12, 16, 20` — five layers.
- Recurrent/short-convolution layers: the remaining thirteen layers, with zero
  KV heads.

The native [LFM3 loader](../crates/llama_cpp_sys/llama.cpp/src/models/lfm3.cpp)
identifies recurrent layers from zero KV-head counts and reads the SWA pattern.
The [hybrid memory implementation](../crates/llama_cpp_sys/llama.cpp/src/llama-memory-hybrid-iswa.cpp)
allocates attention and recurrent state separately. Compressing K/V therefore
affects eleven layers, not all twenty-four.

### Assumptions and formulas

The estimates assume one sequence per context, no cross-context cache sharing,
512-token microbatches, and context sizes already divisible by allocation
padding. Each cached K or V vector contains `4 × 128 = 512` elements.

The [SWA allocator](../crates/llama_cpp_sys/llama.cpp/src/llama-kv-cache-iswa.cpp),
lines 69–80, sizes a compact cache as the window plus microbatch allowance,
bounded by the full capacity and padded to 256 cells. Under these assumptions:

```text
C = full context capacity in tokens
W = sliding window = 256
U = microbatch allowance = 512

S_full    = C
S_compact = round_up(min(C, W + U), 256) = 768 for the capacities below

attention cells = 6 × C + 5 × S
KV bytes        = attention cells × 512 × (bits_K + bits_V) / 8
MiB             = KV bytes / 1,048,576
```

### Estimated cache storage

| Configuration | 4,096 tokens | 32,768 tokens | 131,072 tokens |
| --- | ---: | ---: | ---: |
| Current FP16 K/V and full SWA | 88.0 MiB | 704.0 MiB | 2,816.0 MiB |
| FP16 K/V and compact SWA | 55.5 MiB | 391.5 MiB | 1,543.5 MiB |
| Q8_0 K/V and compact SWA | 29.5 MiB | 208.0 MiB | 820.0 MiB |
| Q8_0 K and Q4_0 V with compact SWA | 22.5 MiB | 159.0 MiB | 627.0 MiB |
| Q4_0 K/V and compact SWA | 15.6 MiB | 110.1 MiB | 434.1 MiB |
| Proposed PQ3 K/V and compact SWA | 10.8 MiB | 76.5 MiB | 301.5 MiB |
| Proposed TBQ3 K and PQ3 V with compact SWA | 12.8 MiB | 90.2 MiB | 355.7 MiB |

These are K/V tensor payload sizes, including quantization-block metadata.
They exclude tensor/buffer alignment, rotation matrices, recurrent state,
compute scratch, model weights, host allocations and driver overhead. The
proposed rows require native work, and the rows do not imply equal quality.

At the current 4K capacity, existing compact Q4 would reduce this storage from
88.0 to about 15.6 MiB. A further move from Q4 to codebook-only PQ3 saves about
4.8 MiB per context. At 128K that additional saving is about 132.6 MiB. This
makes long contexts and multiple resident contexts more compelling targets for
the extra codec work than the current short context alone.

The model file is roughly 1.33 GiB; its size indicates the scale of the weight
storage, not an exact VRAM measurement. Cache compression does not divide total
process or GPU memory by the cache compression ratio.

### Compact SWA and context reuse

Compact SWA is a separate allocation optimization from quantization. It
retains the model's attention window, but stores less historical window state.
That can restrict rollback or checkpoint reuse after old cells have been
overwritten.

The current [application cache policy](../crates/assistant/src/engine/context.rs),
around lines 390–410, reuses an exact evaluated-token prefix and resets the
native cache when the new input diverges. This favors testing compact SWA, but
does not by itself prove cancellation, replay or future checkpoint behavior.
Those paths belong in the validation matrix before a default changes.

## Performance implications

At short context lengths, weight reads and other model work can dominate
decode. Reducing an already small cache may provide little speed benefit, and
unpacking, rotation or QJL work can offset lower memory traffic. At long
contexts, fewer cache bytes can reduce bandwidth pressure and avoid CPU
offload. These are workload-dependent expectations, not measured LFM3 results.

Prefill must be assessed separately. The pinned Vulkan implementation can
dequantize quantized K/V into FP16 scratch for sufficiently large query batches
on selected paths. The conditions include query batch size, layout, available
pipelines and vendor/path restrictions; CoopMat2 is excluded from that
particular optimization. See [Vulkan attention](../crates/llama_cpp_sys/llama.cpp/ggml/src/ggml-vulkan/ggml-vulkan.cpp),
around lines 8180–8213 and 8277. A small stored cache can coexist with a large
workspace, so both peak allocation and retained allocation matter.

Vulkan portability also requires a scalar path that works when cooperative
matrix features are absent. NVIDIA, AMD and Intel differ in subgroup behavior,
available matrix paths and driver performance. Enabling the Vulkan backend
does not establish that cache writes, prefill and decode all remain on that GPU.

The earlier Vulkan lifecycle patch and its generation benchmarks, recorded in
[binding maintenance](bindings-fork.md#vulkan-resource-lifetime), validate a
different change. They provide no TurboQuant quality or throughput result.

## Proposed integration

### First evaluate existing settings

Expose a host-selected cache configuration through the application layer and
compare FP16/FP16, Q8_0/Q8_0, Q8_0/Q4_0 and Q4_0/Q4_0. Evaluate compact SWA
independently so its effects are distinguishable from quantization. Keep
flash attention explicit for profiles with quantized V and record whether
rotation actually takes effect.

This uses existing binding APIs. It does not require a new native patch.
Application configuration and default policy remain human-owned under
[AGENTS.md](../AGENTS.md); this research is not an implementation of them.

### Port a bounded set of native functionality

If the remaining memory or quality tradeoff justifies a new codec, begin with
the QVAC PQ3/TBQ3 family as an experimental port. Codebook-only storage and
QJL-corrected keys should remain separately selectable. Do not automatically
take weight formats, speculative-decoding changes, sparse-attention policies,
layer heuristics or unrelated fork defaults with the codec.

The native work divides into reviewable patch responsibilities:

1. **Formats and CPU reference.** Define block layouts and codebooks, add
   quantization/type traits and reference operations, and pin actual storage
   sizes with tests. Use CPU behavior to validate the GPU implementation.
2. **KV graph and state integration.** Apply transforms consistently to Q, K,
   V and output; integrate full/SWA caches and LFM3's hybrid memory path;
   validate head dimensions; preserve cache-sharing and state contracts.
3. **Vulkan execution.** Add cache-write/copy operations, dequantization,
   attention consumption and QJL correction where applicable. Maintain scalar,
   CoopMat1 and CoopMat2 coverage or explicitly reject unsupported paths.
4. **Binding and build integration.** Generate sys declarations from patched
   headers, extend safe type mappings and validation, stage every required
   patch, and run the complete native/binding checks.

All upstream source changes must be applicable files in
`crates/llama_cpp_sys/patches/`, applied to the build-directory copy. The
submodule remains read-only. Follow the existing composed patch verification
and preserve the decode hooks, logging safety, context-local rotation control
and Vulkan lifecycle fix. See [binding maintenance](bindings-fork.md).

### Required compatibility adaptations

**Type identifiers cannot be copied from the donors.** QVAC assigns TBQ types
to IDs 40–43, occupied in our pin by NVFP4, Q1_0, Q2_0 and F8_E4M3. The
inspected TheTom fork assigns `TURBO2_0` to 43, also conflicting with F8_E4M3.
Append new IDs after the current native enum, update the Rust mappings and
shader constants consistently, and rebuild every native component. Existing
GGUF/type IDs must retain their meanings.
[Pinned enum](../crates/llama_cpp_sys/llama.cpp/ggml/include/ggml.h),
[QVAC enum][qvac-types], [TheTom enum][thetom-types]

**Rotation must follow the cache representation.** The QVAC diff targets older
head-dimension and graph APIs. Its guards and graph additions require adaptation
to the current per-layer and full/SWA rotation inputs. A blind transplant can
omit a required transform or apply a second transform around the existing
rotation. LFM3's zero-KV recurrent layers must not invalidate the compatible
attention-layer dimensions. Preserve the context-local opt-out contract rather
than introducing process-environment mutation.
[QVAC graph][qvac-graph], [current graph](../crates/llama_cpp_sys/llama.cpp/src/llama-graph.cpp)

**Storage layout is a state compatibility contract.** Saved cache state must
identify a compatible type/layout and rotation/codebook convention. Reject
incompatible state instead of accepting bytes under an old interpretation.
Changing a live context's cache type needs context reconstruction and token
replay, not an in-place parameter flip. Donor changes such as 32-to-128-value
blocks show why a stable format name alone is insufficient.

**Native errors must remain contained.** Unsupported dimensions and K/V pairs
should produce actionable errors before allocation or dispatch. New C ABI
boundaries must catch C++ exceptions. The Rust API must not infer `Send`/`Sync`
from a backend's existence or relax the native context's thread confinement.
No callback lifetime rule should change incidentally to add a codec.

### Rust API direction

The first native selection can use the existing `type_k` and `type_v` fields;
an additional public context field is not inherently required. Extend
`GgmlType` and its fallible raw conversion for the new formats. A cache-specific
validated configuration would be preferable to exposing every ggml storage
type as a valid cache choice.

Keep key precision, value precision, rotation policy, flash-attention policy
and SWA allocation explicit. Validate the model/backend combination and report
resolved formats when a dimension-specific layout is selected. Any CPU fallback
policy should be deliberate and observable because performance is a core
requirement. Preserve token, context-count and memory limits: compression does
not make unbounded contexts safe.

## Validation and adoption criteria

| Area | Required evidence |
| --- | --- |
| Numerical correctness | CPU/GPU agreement for quantization, copies and attention; norms, packing boundaries, partial rows, zero/extreme inputs and QJL correction where selected. |
| Model coverage | LFM3 hybrid full/SWA attention, recurrent layers excluded from KV compression, and reference models with 64- and 128-dimensional heads. |
| Application behavior | Normal generation, tool JSON/arguments, multi-turn continuation, cancellation, exact-prefix reuse, reset/replay and output limits. |
| Long-context quality | Perplexity or logit divergence plus retrieval and tool-task accuracy at representative context lengths; compare against the same weights and FP16 baseline. |
| Memory | Stored K/V bytes, total device/host allocations, peak prefill memory and retained workspace, including multiple resident contexts. |
| Latency and throughput | Cold/warm setup, prefill tokens/s, time to first token and decode tokens/s at short and near-full contexts. |
| Vulkan coverage | NVIDIA, AMD and Intel; scalar and available matrix paths; actual dispatch and unexpected CPU execution recorded. |
| Persistence and safety | Compatible state round trips, rejection of incompatible layouts, backend/model/context lifetimes, error containment and existing callback invariants. |

Pair benchmark runs under the same model, weights, prompt, token counts,
batching, thread settings and generation policy. Record the exact commit,
patch set, build options, GPU, driver and selected kernels. Keep unrelated
loads out of timing comparisons and report a distribution across repeated
runs rather than a single best result.

Use exact checks for storage layout and bit packing, and declared numerical
tolerances for floating-point outputs. An independent FP16 attention reference
is needed alongside CPU/GPU codec comparisons: two implementations agreeing
with each other can still share the same transform or correction error.

The existing test suite has independent fixture and model-behavior failures
documented in [binding maintenance](bindings-fork.md#vulkan-resource-lifetime).
Record those separately; neither a pre-existing failure nor a skipped model
test establishes codec correctness. Default tests should continue to skip
cleanly when required models or GPUs are absent, with an explicit mode that
requires real GPU coverage.

Adopt a new default only after it improves the relevant total-memory budget
or usable context length at acceptable task quality and latency. A lower
payload size alone is insufficient. Small instruction models and the
abliterated/quantized checkpoint in use need their own quality results; the
paper's model results do not transfer automatically.

## Evidence and reproduction

### Source revisions

| Source | Inspected revision or dated record |
| --- | --- |
| Workspace | `edfde54af88993baa3c5ad4545d9d75dd52d1672` |
| Pinned llama.cpp | `a34de8618b25dfc32ee43749db47d767a9f0a25d` |
| QVAC PR base | `4c993bdde3e90d1c6a6977e21734a05a0b5f64dc` |
| QVAC PR head | `362003a25376555e5c01939ed82710bbb2c3bcf8` |
| QVAC merge | `01ac2a5ca2049d595041f6528dd0b3fd35d1f8f2`, 2026-04-28 |
| TheTom feature branch | `643956891dfdf51aecdbd05ca508dd21e21e8195`, 2026-10-22 |
| TurboQuant paper | arXiv `2504.19874`, version 1, 2025-04-28 |
| Google announcement | 2026-03-24 |

The donor implementation findings come from source and diff inspection. The
TheTom Vulkan, common-layout and KV source captures were compared byte for
byte with the immutable commit URLs below. Fork README claims are identified
separately from code findings. No donor was built or benchmarked as part of
this assessment, and no LFM3 TurboQuant quality result is asserted.

### Source checksums

These SHA-256 values preserve the identity of the downloaded evidence. They
are integrity checks for the recorded bytes, not substitutes for the source
revisions or for runtime validation. The report does not depend on temporary
files remaining on the research machine.

```text
QVAC PR 115 complete diff
  a16a01ae5cf73acc5a8d8f4ddeecca8ca0ff3a4a8a97b1d84be294a45ba0026b
TheTom ggml/src/ggml-vulkan/ggml-vulkan.cpp
  3d388b9d0ca84fb3e8e05693d54f373935a21cff01963afb432f982fd8039084
TheTom ggml/src/ggml-common.h
  5f694da247c440b73c2fc5f04f143033326f2bd5b9434ff9083820dbf37d7aea
TheTom src/llama-kv-cache.cpp
  a3ea5a87b34b78ee4a38088fd034b2a5bb8d6a83c369cdfb75be2c501e665cdf
TheTom src/llama-arch.h
  eaa20689e4bde30451628b7319cb30616535830088d50e488b7c7b694dc01146
TheTom ggml/include/ggml.h
  9786e92643b1e8db2e4569827ed7a737bc5a2d277445a7836e1ca53cf9b53d0c
```

The complete QVAC diff is 663,685 bytes and 14,293 lines. Retrieve it through
the [diff endpoint][qvac-diff]; use the [PR API record][qvac-api] to check its
base, head, merge revision and change counts. Immutable file links below point
to the PR head rather than a moving branch.

### Reproduce the storage calculations

This standard-library Python calculation regenerates the unrounded values
behind the table. Its constants describe the recorded LFM3 metadata and the
stated single-sequence/microbatch assumptions; it is not a generic estimator
for every model architecture.

```python
def kv_mib(capacity, key_bits, value_bits, full_swa=False):
    window = 256
    microbatch = 512
    swa = capacity if full_swa else (
        (min(capacity, window + microbatch) + 255) // 256 * 256
    )
    cells = 6 * capacity + 5 * swa
    return cells * 512 * (key_bits + value_bits) / 8 / (1024 ** 2)

profiles = [
    ("current f16 full SWA", 16, 16, True),
    ("compact f16", 16, 16, False),
    ("compact q8_0", 8.5, 8.5, False),
    ("compact q8_0 / q4_0", 8.5, 4.5, False),
    ("compact q4_0", 4.5, 4.5, False),
    ("proposed compact pq3", 3.125, 3.125, False),
    ("proposed compact tbq3 / pq3", 4.25, 3.125, False),
]
for name, key_bits, value_bits, full_swa in profiles:
    sizes = [
        round(kv_mib(c, key_bits, value_bits, full_swa), 3)
        for c in (4096, 32768, 131072)
    ]
    print(name, sizes)
```

To repeat the model inspection, use a GGUF reader on the same model SHA-256
and check `general.architecture`, `lfm3.block_count`,
`lfm3.attention.head_count_kv`, the key/value lengths, the SWA pattern/window
and `lfm3.shortconv.l_cache`. A different checkpoint, allocation policy,
microbatch allowance or sequence count requires new estimates.

[paper]: https://arxiv.org/html/2504.19874v1
[google-blog]: https://research.google/blog/turboquant-redefining-ai-efficiency-with-extreme-compression/
[rotation-pr]: https://github.com/ggml-org/llama.cpp/pull/21038
[cpu-pr]: https://github.com/ggml-org/llama.cpp/pull/21089
[qvac-pr]: https://github.com/tetherto/qvac-fabric-llm.cpp/pull/115
[qvac-api]: https://api.github.com/repos/tetherto/qvac-fabric-llm.cpp/pulls/115
[qvac-diff]: https://github.com/tetherto/qvac-fabric-llm.cpp/pull/115.diff
[qvac-common]: https://github.com/tetherto/qvac-fabric-llm.cpp/blob/362003a25376555e5c01939ed82710bbb2c3bcf8/ggml/src/ggml-common.h
[qvac-types]: https://github.com/tetherto/qvac-fabric-llm.cpp/blob/362003a25376555e5c01939ed82710bbb2c3bcf8/ggml/include/ggml.h
[qvac-vulkan]: https://github.com/tetherto/qvac-fabric-llm.cpp/blob/362003a25376555e5c01939ed82710bbb2c3bcf8/ggml/src/ggml-vulkan/ggml-vulkan.cpp
[qvac-graph]: https://github.com/tetherto/qvac-fabric-llm.cpp/blob/362003a25376555e5c01939ed82710bbb2c3bcf8/src/llama-graph.cpp
[qvac-tests]: https://github.com/tetherto/qvac-fabric-llm.cpp/blob/362003a25376555e5c01939ed82710bbb2c3bcf8/tests/test-backend-ops.cpp
[thetom-common]: https://github.com/TheTom/llama-cpp-turboquant/blob/643956891dfdf51aecdbd05ca508dd21e21e8195/ggml/src/ggml-common.h
[thetom-vulkan]: https://github.com/TheTom/llama-cpp-turboquant/blob/643956891dfdf51aecdbd05ca508dd21e21e8195/ggml/src/ggml-vulkan/ggml-vulkan.cpp#L3853
[thetom-kv]: https://github.com/TheTom/llama-cpp-turboquant/blob/643956891dfdf51aecdbd05ca508dd21e21e8195/src/llama-kv-cache.cpp
[thetom-arch]: https://github.com/TheTom/llama-cpp-turboquant/blob/643956891dfdf51aecdbd05ca508dd21e21e8195/src/llama-arch.h
[thetom-types]: https://github.com/TheTom/llama-cpp-turboquant/blob/643956891dfdf51aecdbd05ca508dd21e21e8195/ggml/include/ggml.h
[gl33mer]: https://github.com/gl33mer/llama-cpp-turboquant
[atomic]: https://github.com/AtomicBot-ai/atomic-llama-cpp-turboquant
[llama-tq]: https://llama-tq.com/docs/vulkan
