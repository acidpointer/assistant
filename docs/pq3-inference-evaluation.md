# Experimental PQ3 inference: implementation and measured decision

Status: implemented, measured, **not accepted for promotion**, 2026-11-04.
Keep FP16/full-SWA defaults. Codebook-only PQ3 reduces cache storage, but this
LFM3 fixture loses retrieval accuracy, exceeds the declared KL limit, and is
slower than Q4 on the available GPU. QJL-corrected keys remain a separate,
undelivered experiment.

## What is implemented

The native pin stays b12015, `a34de8618b25dfc32ee43749db47d767a9f0a25d`.
No submodule files were edited. The `turboquant` feature composes existing
patch 0008 with these ordered, applicable patches:

| Patch | Native behavior |
| --- | --- |
| [0009: graph/state](../crates/llama_cpp_sys/patches/0009-pq3-kv-graph-state.patch) | Restricted CPU live-cache inference, mandatory signed H128 transforms, full/SWA graph integration, early persistence/shift rejection, graph ABI and numerical/model tests. |
| [0010: Vulkan](../crates/llama_cpp_sys/patches/0010-pq3-vulkan-cache-attention.patch) | Packed indexed writes/copies, direct compressed-cache scalar/coopmat1/coopmat2 attention, actual-device operation preflight, completed-operation observation ABI and GPU tests. |

PQ3 row ABI 1 stays 128 coordinates / 50 bytes, or 3.125 effective bits/value.
Graph identity 1 uses D then H128 for Q/K/V, H128 then D on output. The
[ADR](decisions/0002-pq3-graph-transform.md) fixes the signs and orientation.
Only plain H retains GGML's Hadamard hint. Attention consumes compressed blocks
without a persistent full-cache FP16 shadow.

Safe Rust exposes matched `KvCacheType::PQ3_0`, native format/transform identity,
owned execution observations and typed rejection of cache-only weight types.
State/shift guards cover native entry points and legacy Rust aliases. Guarded
project shims retain the shared TLS exception/error boundary. Internal C++
memory vtables change; rebuild the whole composed native library. Public Rust
API changes, including fallible `set_causal_attn`, are listed in the
[inference contract](pq3-reference-contract.md).

The application change is configuration, CLI, diagnostics and evaluation
plumbing. The ordinary worker creates and uses the PQ3 context. No session,
tool, prompt or orchestration policy was rewritten. This CLI smoke completed
normally and returned `ready`:

```sh
cargo run --features turboquant -- --model ./model.gguf \
  --cache-type-k pq3_0 --cache-type-v pq3_0 --swa-cache compact \
  --require-vulkan --diagnostics --max-tokens 32 \
  --prompt 'Reply with just the word ready.'
```

Its native snapshot reported row/graph/transform versions 1, 128-coordinate
rotations, 44 completed Vulkan PQ3 indexed writes, 22 attention operations and
`CooperativeMatrix2`. Required-Vulkan mode validates PQ3 at every context
creation. Other formats retain their existing automatic-placement behavior.

## Quality: failed unchanged gates

The [complete quality artifact](benchmarks/pq3-quality-2026-11-04.json)
preserves 102 tasks and 119 teacher-forced numerical checkpoints, fixture/model/
source/patch/executable/log identities, and unchanged source inputs for capture.
FP16/full is repeated. The authored corpus and exact-answer scoring were not
changed to accommodate PQ3.

| Profile | Passed tasks | Mean target NLL change | Worst checkpoint KL |
| --- | ---: | ---: | ---: |
| FP16 full/compact | 12/17 | 0 | 0 |
| Q8 compact | 12/17 | -0.003896 | 0.000398 |
| Q4 compact | 10/17 | +0.044142 | 0.075786 |
| PQ3 full/compact | 11/17 | +0.007044 | 0.125069 |

PQ3 loses `retrieve-river-gate-3500-middle`, which both FP16 and Q4 pass.
Retrieval drops from 7/12 to 6/12, violating the no-decrease gate. Worst KL
exceeds 0.05, although mean NLL passes its 0.05-nat gate. Repeated FP16 has zero
KL variation here. Q4's separate failed quality verdict remains visible too;
beating its aggregate task count does not qualify PQ3 for promotion.

These small fixtures establish a development rejection, not broad model-quality
estimates. They do not establish scalar whole-model quality or 32K retrieval.

## Memory: cache compression has a small whole-process effect at 4K

The [release evidence artifact](benchmarks/pq3-inference-2026-11-04.json)
retains allocation snapshots, cold/warm trials, startup timings, medians/ranges,
placement and raw artifact hashes. This is an RTX A500 Laptop GPU (4 GiB),
driver 615.78.08, i7-1370P, and the local LFM3-1.7B Q6_K GGUF identified by
SHA256 `616775686c9abcbc1a0d302f75dc0a1f12498f272170bd7cda556a706c5606e1`.

All values below are MiB. Native totals sum model, context and compute buffers
across host and device; they are **not peak VRAM**. Shared model allocations
are counted once. Driver overhead and general process allocations are separate.

| Capacity / compact profile | KV payload estimate | Context + compute | Native total |
| --- | ---: | ---: | ---: |
| 4K FP16 | 55.500 | 347.660 | 1906.375 |
| 4K Q8 | 29.484 | 322.504 | 1881.219 |
| 4K Q4 | 15.609 | 308.629 | 1867.344 |
| 4K PQ3 | 10.840 | 304.380 | 1863.095 |
| 32K FP16 | 391.500 | 739.660 | 2298.375 |
| 32K Q8 | 207.984 | 557.004 | 2115.719 |
| 32K Q4 | 110.109 | 459.129 | 2017.844 |
| 32K PQ3 | 76.465 | 426.005 | 1984.720 |

At 4K, PQ3 saves 4.249 MiB of actual native allocations versus compact Q4,
about 0.23% of the total. Its payload saves 4.770 MiB; larger V rotation and
sign buffers consume approximately 0.521 MiB of that advantage. At 32K it saves
33.124 MiB versus compact Q4, or 1.64% of total native allocations. Against
compact FP16, the 32K saving is 313.655 MiB (13.65%).

The 32K cells allocate 32K capacity and exercise 1024-token prefixes; they are
not nearly-full 32K inference. Allocation snapshots stay constant from initial
reservation through prefill/decode in these runs. No device-peak measurement or
hard daemon memory ceiling is claimed. Each process has one resident context.

## Speed: the near-full 4K regression repeats

Measurements use release builds, four CPU threads, batch/ubatch 512, identical
checksummed representative prompt prefixes, 64 generated tokens in every trial,
a fresh process per profile, one cold trial and three warm trials. Timed runs
have no scheduler tracing or concurrent build/model job. Process-cold trials
retain normal OS/driver caches; this is not a cold-boot benchmark.

| Capacity / prompt | Compact profile | Warm prefill tokens/s | Warm decode tokens/s |
| --- | --- | ---: | ---: |
| 4K / 1024 | FP16 | 2353.60 | 54.96 |
| 4K / 1024 | Q8 | 2284.31 | 54.81 |
| 4K / 1024 | Q4 | 2289.49 | 54.98 |
| 4K / 1024 | PQ3 | 2170.25 | 52.53 |
| 4K / 3500 | Q4 | 2265.41 | 53.81 |
| 4K / 3500 | PQ3 | 2007.54 | 50.24 |
| 32K / 1024 | Q4 | 2248.26 | 54.45 |
| 32K / 1024 | PQ3 | 2150.81 | 52.04 |

A separate five-warm-trial reverse-order repeat confirms the near-full 4K
regression: PQ3 2023.98 versus Q4 2269.83 prefill tokens/s (**10.83% slower**),
and 50.09 versus 53.76 decode tokens/s (**6.82% slower**). Median first-text
latency is 1.733 versus 1.546 seconds. Prefill ranges are 2017.48–2033.75
versus 2264.07–2274.55; decode ranges are 47.94–50.44 versus 53.00–53.94.
This exceeds the preregistered 5% investigation threshold without overlapping
warm ranges.

On the forced scalar path, 4K/1024 Q4/PQ3 prefill is 997.52/964.58 tokens/s,
and decode 54.56/52.14 tokens/s. These are NVIDIA scalar-path measurements,
not AMD/Intel performance predictions. Full-SWA rows and cold/startup samples
are retained in the artifact too.

The prototype uses separate sign multiplications, 128-coordinate V transforms,
and ordered scalar block-norm reductions during Vulkan writes. Those are
plausible costs to profile; these measurements do not identify their individual
contributions. Any fusion must preserve the frozen transform and numerical
oracles. A cache-payload improvement alone cannot justify this latency cost.

## Correctness and placement evidence

- CPU graph tests pass basis/inverse/head-boundary, indexed-write/copy and
  independent decoded-attention checks. CPU flash attention uses its own
  Q8-query oracle; Vulkan uses an F32-query/F64-accumulation oracle.
- Required-device scalar, coopmat1 and coopmat2 synthetic Vulkan tests pass.
  The largest observed attention error is 0.000220765483 under the fixed
  `abs(error) <= 3e-4 + 3e-4*abs(reference)` bound. Each path observes eight
  completed indexed writes and three attention operations with its expected bit.
- Real LFM3 CPU/Vulkan full/compact inference, 900-token repeated window reuse,
  strict same-layout reset/replay, and early state/shift rejection pass.
- Separate full/compact scheduler traces assign all 552 `SET_ROWS` and 264
  `FLASH_ATTN` nodes to Vulkan0. All multiplication/matrix nodes, including
  transforms, are on Vulkan; the only observed CPU nodes are 48 `GET_ROWS`.
  Trace counts include other model operations. Actual PQ3 completion counters
  independently confirm the codec operations and selected attention path.
- Default workspace: 572 passed, 14 ignored. Experimental workspace: 580 passed,
  14 ignored. Both have zero failures. Format and workspace/all-target checks
  pass, as do five Python regressions and five staging/composition tests.
- CPU binding units: 127 passed. Eight required integration tests pass with the
  real model on CPU and separately on Vulkan. The actual CLI generates and exits
  normally. Packaging contains patches 0009/0010 and native test inputs.

Workspace tests were serialized after a parallel binding unit run hit the
existing global-logger callback-count race (an extra native log). No assertions
were weakened. Required model/device tests are explicit; default model-free
skips are not counted as actual inference coverage.

A full-versus-compact maximum-logit bound was invalidated by an FP16 control:
long-trajectory drift was 0.509114504 even without PQ3. CPU PQ3 drift was
0.807980776 and Vulkan PQ3 0.217959166. These remain characterization values,
not passed tolerances. Physical cache ordering changes floating-point reductions,
which recurrent trajectories amplify. Independent row-permutation attention
checks and same-layout replay bounded to 2e-5 replace that invalid assertion;
the model-quality gates above remain unchanged.

## Restrictions and remaining work

See the [support contract](pq3-reference-contract.md) for the creation allowlist
and APIs. External full/sequence/device/file state I/O, positional rebasing,
shared cache, speculative decoding and training are unavailable. Live kernels
retain finite-activation/representable-norm preconditions rather than a new
per-write recoverable numeric-error channel. Native constructor-only rejections
retain the existing context-creation error; precise reasons are in native logs.

AMD and Intel remain unvalidated. Ordinary 128-head Llama is in the experimental
architecture allowlist, but this run supplies model-backed evidence only for
LFM3. Near-full 32K quality/recovery, simultaneous resident-context capacity,
and an isolated split-K regression case remain open. Embedding-output/training
modes are outside the tested runtime contract.

The next decision should address the failed retrieval/KL cases and profile the
unfused costs before further expansion. QJL, alternative scale policies or
fusion need separate contracts and the same unchanged gates. Do not assume
QJL will fix these measurements. Production defaults, broader model/vendor
support, persistent state and hard memory admission remain separate work.
