# PQ3 row and inference contract

Status: experimental row, CPU graph and Vulkan graph implemented, 2026-11-04.
Patch [0008](../crates/llama_cpp_sys/patches/0008-pq3-cpu-format.patch) freezes
the row codec; [0009](../crates/llama_cpp_sys/patches/0009-pq3-kv-graph-state.patch)
and [0010](../crates/llama_cpp_sys/patches/0010-pq3-vulkan-cache-attention.patch)
add restricted live-cache inference. The [transform ADR](decisions/0002-pq3-graph-transform.md)
fixes graph identity separately from the row ABI. Quality/performance acceptance
is a separate measured decision; defaults remain FP16/full SWA.

## Source and attribution

The local native base is unchanged: llama.cpp b12015,
`a34de8618b25dfc32ee43749db47d767a9f0a25d`, with mandatory patches 0003–0007.
The donor is [QVAC PR 115](https://github.com/tetherto/qvac-fabric-llm.cpp/pull/115),
head `362003a25376555e5c01939ed82710bbb2c3bcf8`, base
`4c993bdde3e90d1c6a6977e21734a05a0b5f64dc`. The downloaded PR diff matched
the checksum preserved in the research. Source and license files were retrieved
from that immutable head; the patch retains attribution and the existing MIT
license. Donor license: copyright 2023–2024 The ggml authors.

| Audited input | SHA256 |
| --- | --- |
| PR diff | `a16a01ae5cf73acc5a8d8f4ddeecca8ca0ff3a4a8a97b1d84be294a45ba0026b` |
| `LICENSE` | `e562a2ddfaf8280537795ac5ecd34e3012b6582a147ef69ba6a6a5c08c84757d` |
| `ggml/src/ggml-common.h` | `e2ce3af8f440d8dc75e09942ba1d8c02de758cdccc4c949e39c4549fe23f35ea` |
| `ggml/src/ggml-quants.c` | `762bb79ce68cc8ee200a917abd2620fcebcee05073556bc837c4beb48c68bb7d` |
| `ggml/src/ggml-quants.h` | `11409c17620fcc6a9042ba1095a869f3eb42240b46660ffb99b8a561f6153d96` |
| `ggml/src/ggml-cpu/quants.c` | `d6fdd798db3c4866a878b39ec542fb41cbd1c6df47283a0e731e383a18e228d0` |
| `ggml/src/ggml-cpu/quants.h` | `602cbf753474c7a24c384810cadabf4587265e6781b8957616563146b033ea9d` |

For example, the immutable [block declarations](https://github.com/tetherto/qvac-fabric-llm.cpp/blob/362003a25376555e5c01939ed82710bbb2c3bcf8/ggml/src/ggml-common.h)
and [row implementation](https://github.com/tetherto/qvac-fabric-llm.cpp/blob/362003a25376555e5c01939ed82710bbb2c3bcf8/ggml/src/ggml-quants.c)
are the packing/codebook sources. The local scalar implementation is adapted,
not a wholesale donor merge. No donor performance result establishes local
performance.

## Frozen row format, reference ABI v1

Existing native type IDs are preserved, including F8_E4M3 = 43. PQ3_0 is
appended at 44, and type count becomes 45 only with the experimental patch.
The safe general `GgmlType` preserves F8 and exposes PQ3 only with `turboquant`.
PQ3 is cache-only: safe quantization entry points and native model/weight paths
reject its use as a weight type.

| Property | Contract |
| --- | --- |
| Logical block | 128 F32 coordinates, no partial block/tail |
| `qs` | 48 bytes at offset 0 |
| `d` | GGML FP16 L2 norm at offset 48 |
| Block size/alignment | 50 bytes / native half alignment (2 bytes) |
| Row size | `50 * (coordinates / 128)` bytes; contiguous complete blocks |
| Payload | 3.125 effective bits/value including norm |
| Reconstruction | `float(d) * codebook[index]` |

The ordered codebook constants, rounded by the C compiler to F32, are:

```text
-0.18839718597003241, -0.11813976699668613,
-0.06658560804735174, -0.02160431064212660,
 0.02160431064212660,  0.06658560804735174,
 0.11813976699668613,  0.18839718597003241
```

Compute the block L2 norm from an ordered F32 sum of squares and `sqrtf`.
Normalize coordinates by this original norm, choose the nearest codebook
coefficient using F32 midpoint boundaries, and encode the original norm in
FP16. An exact midpoint selects the higher index. For each eight consecutive
indices, form `sum(index[j] << (3*j))` and store its low, middle and high byte.
This repeats 16 times. The FP16 norm uses native GGML byte order; these bytes
are not a portable interchange or inference-checkpoint format.

The row codec applies **no rotation**. Inputs and queries must already use the
same coordinate system. No hidden sign generation, environment lookup or
context state participates in encoding. The scalar CPU dot decodes PQ3 and
four Q8_0 query blocks, accumulating their products in F32. It includes Q8
query error and is an oracle for that operation, not an exact F32-query dot.
The attention test compares these scores and a separately accumulated
softmax/value result with independent F64 calculations on decoded operands.

## Numerical and FFI behavior

The guarded public block interface accepts exactly 128 finite coordinates with
an L2 norm no greater than 65504, the largest finite FP16 value. A zero norm,
or a norm rounding/underflowing to FP16 zero, produces 50 zero bytes.
Decoding a canonical zero produces positive F32 zero in every lane. A raw
block with a negative, negative-zero, infinite or NaN norm is rejected. A zero
norm requires all index bytes to be zero; other encodings are noncanonical.
Every index bit pattern is valid for a positive finite norm.

Project shims validate lengths/pointers/numerics, use aligned local storage
and `memcpy`, and write outputs only after successful validation/computation.
C++ exceptions become the existing shared thread-local shim error. Rust owns
its 50 bytes, retains no native pointer, and can use the stateless codec
concurrently without a backend lease. Separate base and CPU ABI guard symbols
reject stale/mixed native libraries. The raw internal GGML row functions keep
upstream-style valid-pointer, complete-block and valid-numeric preconditions;
they are not the checked public entry point.

Golden tests cover canonical zeros, a constant block, independently packed
index ramps, midpoint ties, repeated encoding, tiny inputs, row sizes, scalar
dots, reference attention and invalid inputs. Scalar reference code is not an
optimized inference benchmark. GPU reductions may need declared tolerances
rather than byte equality when their reduction order differs.

## Deliberate differences and graph transform

The donor has an optional `GGML_TQ_NORM_CORRECTION` environment policy that
changes the stored scale to an MSE optimum, with mutable static initialization.
It is omitted: ABI v1 always uses the original L2 scale. Canonical zero bytes
and positive-zero reconstruction are explicit local adaptations. Future scale
policies require a separate format/ABI decision, not an environment switch.

There is a material donor discrepancy: its standalone transform helpers use
`H D / sqrt(128)`, with seed-42 xoshiro sign masks
`40f54e8c, 6587b7b0, c31220ea, 32f6449b`, while the inspected graph's
`set_input_hadamard` builds the plain normalized Hadamard matrix. The donor
row helpers obtain signs but do not apply them. Copying those helpers would
not establish which transform actual inference uses.

The row contract therefore applies no implicit transform. Graph identity 1
explicitly uses `T = H128 D` for Q/K/V and `T^-1 = D H128` for output. The
[ADR](decisions/0002-pq3-graph-transform.md) freezes sign words, ordering and
orientation. Separate sign multiplication preserves the fast-Hadamard hint only
on plain H. Both K and V use 128 coordinates. Rotation opt-outs are rejected.

## Build, support and state boundaries

`turboquant` is off by default, independent of `vulkan`, and forwarded through
assistant → llama_cpp → llama_cpp_sys. It adds ordered patches 0008–0010 to the mandatory
0003–0007 series, including in CPU-only builds. Selection and contents participate in the source-copy cache
key; absent required files fail the build. Generated FFI uses selected headers.
The package includes native test sources so patched CMake/test inputs exist
outside a checkout too.

```sh
cargo test -p llama_cpp --no-default-features --features turboquant \
  --lib --test test_pq3_reference
cargo test -p llama_cpp_sys --features turboquant \
  --test patch_staging --test patch_verification
```

The native `test-pq3-reference` target is available in a disposable patched
source copy configured with `LLAMA_BUILD_TESTS=ON`. Patch verifier tests compare
the real ordered series against CLI `patch`, including the new test file.

`KvCacheType::PQ3_0` requires matched K/V, exact 128-dimensional attention
heads, causal single-sequence Llama or LFM3, required rotation and flash
attention. Recurrent zero-KV layers are excluded from geometry checks. CPU
execution requires explicit CPU placement. Vulkan execution checks the actual
attention-layer devices and required operations before allocating cache buffers;
other backends and unsupported combinations fail. GPU F16 indexed writes are
supported; a contiguous F16 encode uses F16→F32→PQ3 rather than an advertised
direct F16 copy. Production attention decodes compressed blocks directly.

Full/sequence/extended/device/file state I/O and positional rebasing are
unsupported. They fail before parsing, writing, allocation from caller-supplied
capacity, or hybrid-state mutation. Append, compact-window cell reuse without
rebasing, and full reset/replay are supported. Speculative decoding, training,
shared-cache ownership and dynamic noncausal attention are rejected.

The checked `Pq3Block` API rejects nonfinite/oversized inputs. Live GGML kernels
retain valid-finite-activation and representable-norm preconditions; they do not
provide a new per-write host error/readback mechanism. NaN/infinite activations
can propagate invalid norms and attention results. They are not a supported
input or silently repaired cache data. Norm underflow deliberately yields the
canonical zero. This is an experimental numerical limitation, not a guarantee
of recoverable model arithmetic errors.

`ContextCacheInfo::pq3` owns row/graph/transform identities, native block size,
execution mode and completed Vulkan indexed-write/attention counts and path
mask. Counts advance after successful normal synchronization, with no extra
production readback. They prove those operations occurred, not that every graph
node stayed on GPU; separate scheduler audits provide that evidence.

ABI v1 keeps context-parameter layout and the 50-byte row unchanged. Patch 0009
changes the internal C++ memory interface: rebuild the complete composed native
source; never mix old native objects. New guarded project C ABI records are
versioned. `LlamaContext::set_causal_attn` now returns `Result<(), ShimError>`;
callers must handle it. `try_model_quantize` adds typed validation errors;
legacy `model_quantize` retains its numeric signature and returns error 1 for
invalid strings/cache-only selections instead of panicking. Legacy unsafe state
restore now passes the slice length to the bounded native API. New feature-gated
cache-info fields/error variants may require downstream struct/match updates.
QJL-corrected TBQ3 and persistent PQ3 state remain separate future work.
