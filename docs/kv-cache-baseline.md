# KV cache configuration and baseline

Implementation and validation record, 2026-11-04. This prepares the existing
llama.cpp cache formats for comparison with a future TurboQuant codec. It adds
no TurboQuant type IDs, codebooks, QJL correction, or Vulkan codec kernels.

The native revision remains b12015,
`a34de8618b25dfc32ee43749db47d767a9f0a25d`. Existing native patches remain in
place. New native inspection and synchronization entry points live in the
maintained `ext_shim`, outside the submodule; no additional upstream patch or
submodule update is needed for these existing cache options.

## Configuration and compatibility

`LLMConfig::cache` uses the binding's `KvCacheConfig`. Supported initial cache
types are `F16`, `Q8_0`, and `Q4_0`, independently for K and V. The general
binding `GgmlType` setters remain available for advanced users. Configuration
is immutable after context creation, and every context on a model worker gets
the same policy. Changing layout requires a new context and prompt replay.

Defaults remain **FP16 K/V, full SWA, automatic flash attention and rotation**.
Compact SWA and every quantized profile remain explicit choices. There is no
automatic precision reduction, context expansion, or GPU fallback promise.

```sh
cargo run --release -- --model "$MODEL" --no-web --prompt 'Hello' \
  --cache-type-k q8_0 --cache-type-v q8_0 \
  --swa-cache compact --flash-attn auto --attn-rotation auto \
  --batch-size 512 --ubatch-size 512 --diagnostics
```

`--flash-attn` accepts `auto|on|off`; `--attn-rotation` accepts
`auto|disabled`. Quantized V with explicitly disabled flash attention fails
before model loading. After loading, validation checks attention head block
alignment and architectures requiring identical K/V types. Zero-KV recurrent
layers are excluded. Native validation still applies to backend/model details.

`microbatch_size: Option<NonZeroU32>` defaults to `min(batch_size, 512)`.
An explicit size cannot exceed the logical batch. Native padding and resolved
settings are reported rather than inferred. `ExecutionMode::Cpu` and CLI
`--cpu` disable KQV and operator offloading and use zero GPU weight layers.
`gpu_layers=0` alone only requests CPU weights.

New `LLMConfig` fields affect callers constructing exhaustive struct literals;
prefer `LLMConfig::new` followed by explicit overrides. Binding diagnostics and
typed cache configuration are additive APIs. No generated binding or upstream
source is hand-edited.

## What diagnostics mean

`LlamaModel::cache_topology()` returns owned per-layer geometry and weight
device names. `LlamaContext::cache_info()` inspects actual full/SWA arenas,
including hybrid SWA, and records capacities, formats, streams, and rotation
dimensions. Unknown memory implementations and compressed/MLA layouts explicitly report
unavailable inspection. MLA may omit V storage; its unchecked native type
getter must never be called by this inspector. A selected flash algorithm or a GPU-resident weight is not proof
of attention execution on the GPU.

`try_memory_breakdown()` uses a guarded query/fill ABI, rejects incomplete or
oversized output, validates complete UTF-8 names, and propagates errors. The
legacy `memory_breakdown()` remains source-compatible and best-effort; it is
unsuitable for memory admission decisions.

`LLMContext::cache_diagnostics()` collects requested/effective policy, complete
buffer accounting, and a formula-derived K/V payload estimate. Context bytes
include recurrent state and other native context storage. The estimate excludes
recurrent state, rotation buffers and allocator padding; unsupported layouts
have no estimate. Buffer accounting is not driver-wide VRAM or peak telemetry.

`RunningAssistant::cache_diagnostics()` asks owning worker threads for owned
snapshots. Native handles never cross into async code. Aggregate reports count
resident weights once per buffer/model and sum context/compute allocations with
checked arithmetic. Before the first context, the list is empty; it is not a
model-only memory census. CLI `--diagnostics` emits JSON on stderr after each
successful interaction and keeps streamed model text on stdout. Routine reports
contain no model paths or prompt content.

Contexts still reuse only an exact evaluated-token prefix. Divergence and
trimming reset and replay the prompt. No arbitrary rewind, shifting, or compact
cache checkpoint restoration has been introduced. The default per-model context
count remains four. There is no aggregate byte ceiling or hard preallocation
VRAM guarantee. A long-context deployment promising such a limit still needs
the admission/accounting extension described in the
[preparation plan](kv-cache-preparation-plan.md#change-7-select-defaults-and-bound-resident-memory).

## Test categories and reproduction

Default gates need no downloaded inference model:

```sh
cargo fmt --all --check
cargo check --workspace
cargo test --workspace
```

Vocabulary fixtures are used only for tokenizer-compatible assertions. Full
model property tests skip without a full checkpoint. `LLAMA_TEST_MODEL` selects
an explicit binding fixture; a bad path or failed load fails. Set
`LLAMA_TEST_REQUIRE_MODEL=1` to disallow fallback to a vocabulary fixture.

Cache correctness and numerical characterization use an explicit hybrid-SWA
LFM3 model. Use an **absolute** path: Cargo runs tests from each crate directory.

```sh
export ASSISTANT_CACHE_TEST_MODEL="$MODEL"
export ASSISTANT_CACHE_REQUIRE_MODEL=1
export ASSISTANT_CACHE_REQUIRE_VULKAN=1
cargo test -p assistant --lib required_ -- --nocapture --test-threads=1 2>cache-128.log
ASSISTANT_CACHE_UBATCH=512 cargo test -p assistant --lib \
  required_lfm3_cache_profiles -- --nocapture 2>cache-512.log
python3 scripts/cache-results.py quality cache-128.log
```

These cover window crossings, compact capacity crossings, repeated wrapping,
near-capacity prompts, exact append reuse, fresh replay, divergent/truncated
history, cancellation before work/during prefill/during decode, stale cursors,
failed output recovery, independent contexts, worker policy propagation,
context release and reallocation, and shared-weight accounting. Native decode
failure injection beyond the existing binding tests is not added here.

Teacher-forced comparisons use identical token streams and matching prefill
schedules. LFM3 has recurrent state: a single-batch replay can differ numerically
from an incremental append even without changing cache policy. The full-SWA
reference therefore stores separate incremental and fresh-replay logits.

Compact FP16 also develops small differences after wrapping on this GPU. The
initial 128-token microbatch characterization observed an incremental maximum
logit difference of 0.072 and RMS of 0.015; fresh-replay maximum was 0.091.
The regression guard is maximum 0.20 and incremental RMS 0.03. These are narrow
fixture/device regression tolerances, not a promise of bitwise equality or
task-quality equivalence on other models. Quantized profiles record finite
logits, RMS/max differences, KL divergence, top-1 agreement, and teacher-forced
next-token loss; they have no default-adoption quality threshold yet.

Live tool-choice evaluations remain separate from deterministic protocol tests:

```sh
ASSISTANT_TEST_MODEL="$MODEL" ASSISTANT_TEST_REQUIRE_MODEL=1 \
ASSISTANT_TEST_GPU_LAYERS=1000 ASSISTANT_TEST_SWA=compact \
ASSISTANT_TEST_CACHE_PROFILE=q8 \
cargo test -p assistant-daemon --test inference model_ -- --nocapture
```

Profiles are `f16|q8|q8-q4|q4`. Without an explicit model these evaluations skip;
required mode fails. Their expectations are unchanged. The existing LFM3
two-tool failure must remain visible and must be compared with the FP16/full
reference before attributing it to a cache policy.

## Benchmark workflow

Build once, then run without concurrent compilation or other GPU jobs:

```sh
cargo build --release -p assistant --example cache_bench
target/release/examples/cache_bench --model "$MODEL" --require-vulkan \
  --contexts 4096,32768 --batch 512 --ubatch 512 \
  --prompt-tokens 1024 --decode-tokens 64 --repeats 3 >cache.jsonl
python3 scripts/cache-results.py benchmark cache.jsonl
```

Each matrix cell runs in a child process. A child retains one model and context
for one cold and several warm trials. The matrix varies SWA and precision
independently. Use `--contexts 4096 --prompt-tokens 4000 --decode-tokens 32` for
a nearly full 4K context. Longer capacities are explicit experiments; allocation
failures remain failed cells, never successful CPU substitutions or missing
rows. The parent completes remaining cells and returns failure if any failed.

Microbatch sweeps are separate invocations with the same batch, corpus and
profile. `--rotation-disabled` is context-local. `--cpu` requests a CPU reference.
An individual profile can be selected with `--profile compact-q8`; other names
are `full-f16`, `compact-f16`, `full-q8`, `full-q8-q4`, `compact-q8-q4`,
`full-q4`, and `compact-q4`.

The JSONL schema records revisions, working-tree diff and source-file hashes
(including untracked implementation files), ordered patch hashes, model SHA256,
build profile, available driver identity, relevant environment overrides,
requested/effective settings, and allocation/prefill/decode snapshots. It uses
system `git`, `sha256sum`, and optionally `nvidia-smi` on the developer machine;
these are benchmark tools, not daemon runtime dependencies.

`LLMContext::prefill` evaluates through the normal generation machinery without
sampling and synchronizes before returning. The benchmark then generates from
that evaluated prefix and synchronizes again. Prefill throughput and decode
throughput have explicit completed-work boundaries. Time to first **text**
includes prefill and the first emitted UTF-8 chunk; tokenizer/holdback behavior
means it is not an exact first-token timestamp. Setup, tokenization, metadata
hashing, snapshots, and scheduler traces are outside timed intervals.

Host peak RSS uses Linux `/proc/self/status` `VmHWM`. Device peak memory is
explicitly unavailable; native buffer snapshots must not be labeled peak VRAM.
Use a separately recorded device profiler when driver-wide peak evidence is
needed. `--quality-artifact` links an external quality record to the run.
Local models and unredacted result artifacts stay outside Git.

Placement is a separate trace-enabled run:

```sh
target/release/examples/cache_bench --model "$MODEL" --require-vulkan \
  --profile compact-q8 --prompt-tokens 320 --decode-tokens 4 --repeats 1 \
  --audit >placement.jsonl 2>placement.log
python3 scripts/cache-results.py placement placement.log --require-backend Vulkan
```

The child sets `GGML_SCHED_DEBUG=2` through process creation and uses the safe
native logger to capture node/split assignments. Tensor transaction callbacks
are untouched. The parser requires cache-write (`SET_ROWS`) and flash-attention
assignments and rejects assignments outside the requested backend. Scheduler
traces include reservation graphs as well as inference graphs; this establishes
scheduled placement, not a profiler-level execution timeline. Audit timings are
marked unusable for throughput comparisons. For CPU, repeat with `--cpu` and
`--require-backend CPU`.

## Measured results and default decision

The [versioned evidence artifact](benchmarks/kv-cache-2026-11-04.json) retains
sanitized results, warm min/median/max spreads, model/native/patch identities,
quality metrics, placement counts, and SHA256s of the local raw artifacts.
Measurements used the local LFM3 Q6_K checkpoint (SHA256
`616775686c9abcbc1a0d302f75dc0a1f12498f272170bd7cda556a706c5606e1`),
an NVIDIA RTX A500 Laptop GPU with 4 GiB VRAM, driver 615.78.08, four CPU
threads, release builds, batch/microbatch 512, and one cold plus three warm trials.

All 16 capacity-matrix cells (4K/32K capacities, 1,024-token prompts), eight
nearly-full 4K cells (4,000-token prompts), and four scalar-path cells completed.
These are per-context native allocations; model weights are additional and
shared. No number in this table is peak VRAM.

| 4K profile | Context MiB | Compute MiB | KV payload estimate MiB | Warm prefill tokens/s | Warm decode tokens/s |
| --- | ---: | ---: | ---: | ---: | ---: |
| Full FP16 | 88.609 | 298.051 | 88.000 | 2350.8 | 55.6 |
| Compact FP16 | 56.109 | 291.551 | 55.500 | 2351.4 | 55.6 |
| Full Q8/Q8 | 48.219 | 298.051 | 46.750 | 2278.3 | 55.2 |
| Compact Q8/Q8 | 30.953 | 291.551 | 29.484 | 2271.3 | 55.3 |
| Full Q8/Q4 | 37.219 | 298.051 | 35.750 | 2282.5 | 55.3 |
| Compact Q8/Q4 | 24.016 | 291.551 | 22.547 | 2275.4 | 53.9 |
| Full Q4/Q4 | 26.219 | 298.051 | 24.750 | 2271.0 | 54.9 |
| Compact Q4/Q4 | 17.078 | 291.551 | 15.609 | 2276.0 | 55.3 |

Compact FP16 saves 39 MiB of accounted context plus compute allocations at 4K.
At 32K it saves 375 MiB: context 704.609 → 392.109 MiB, compute 410.051 →
347.551 MiB. The 32K compact Q8/Q8, Q8/Q4 and Q4/Q4 context allocations were
209.453, 160.516 and 111.578 MiB. Context allocation remains significant because
full-attention layers retain a full sequence even when SWA is compact.

Near-full 4K prefill medians were 2348.5 tokens/s for full FP16, 2345.4 for
compact FP16, and 2247.4–2256.4 for the quantized profiles. Decode medians were
53.6–54.3 tokens/s. On this device compact allocation has no meaningful observed
throughput cost; existing cache quantization costs roughly 3–4% of prefill
throughput in these workloads. This is a small paired baseline, not a universal
performance guarantee.

With cooperative-matrix paths explicitly disabled, compact FP16/Q8/Q8-Q4/Q4
prefill medians were 1052.0/983.2/1012.0/1025.5 tokens/s; decode remained about
55–56 tokens/s. Preserve optimized weight-matmul dispatch when experimenting
with codecs. These scalar runs establish real generation and timings, not the
same exhaustive wrap/recovery coverage as the default dispatch suite.

The required Vulkan correctness suite passed at microbatch 128 and 512. The
final 128 run also passed simultaneous rotation isolation and worker
inspection/release/reallocation checks: three tests, no skips. Independent
contexts using the same weights keep their rotation preferences. Scheduler
traces placed cache writes and flash attention on `Vulkan0` for all four compact
profiles, and on `CPU` for the CPU reference. Trace counts include reservation
and inference graphs and are not kernel execution counts.

At both microbatch sizes, all seven teacher-forced checkpoints retained the
reference top-1 token for all profiles. Maximum incremental RMS at microbatch
128 was 0.0146 for compact FP16, 0.0728 for compact Q8/Q8, 0.2005 for compact
Q8/Q4, and 0.2973 for compact Q4/Q4. Maximum KL divergence was approximately
0.000003/0.000134/0.003906/0.008381 respectively. This short repetitive,
high-confidence corpus is a regression fixture, not sufficient task-quality
coverage for adopting a lossy default.

Live tool-choice evaluation preserved its assertions:

| Profile | Arithmetic | Exact clock output | Both tools in one conversation |
| --- | --- | --- | --- |
| FP16/full reference | Pass | Pass | Fail: omitted calculator |
| FP16/compact | Pass | Pass | Fail: omitted calculator |
| Q8/Q8 compact | Pass | Pass | Fail: omitted calculator |
| Q8/Q4 compact | Pass | Pass | Fail: omitted calculator |
| Q4/Q4 compact | Pass | Pass | Pass in this run |

The two-tool failure predates these changes and remains visible. These runs
include a live changing clock value; the single Q4 success is not evidence of
superior precision or a causal quality improvement. Deterministic protocol tests
remain separate. Fixed-output task evaluations and broader language/tool data
are still required before selecting a lossy default.

Workspace formatting/check/tests pass (565 tests, 14 existing ignores; optional
model cases skip by design). Seven required full-model binding property tests also pass; an invalid explicit
fixture fails rather than becoming a skip. Clippy completes with existing
repository warnings.
The CLI generation smoke verified requested/effective settings, microbatch 128,
separate text/diagnostic streams, and absence of prompt/model-path logging.
The pinned submodule and patch series remain unchanged.

**Decision:** retain FP16/full-SWA defaults and expose the measured alternatives
explicitly. A repeatable median throughput regression over 5% requires
investigation before adopting a new default; quantized adoption also needs
explicit task/model quality thresholds. No automatic fallback is introduced.

Remaining coverage is explicit: AMD/Intel hardware, different model head
geometries, MLA inspection, nearly-full 32K quality/recovery, contexts above the
model's trained capacity, and driver-wide peak telemetry. The current bounds
support a controlled TurboQuant prototype; a production long-context rollout
promising a byte ceiling still needs admission/accounting work. No automatic
policy selection or TurboQuant integration is enabled by this baseline.
