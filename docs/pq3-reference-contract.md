# PQ3 CPU reference contract

Status: experimental CPU format delivered, 2026-11-04. This is the contract for
[`0008-pq3-cpu-format.patch`](../crates/llama_cpp_sys/patches/0008-pq3-cpu-format.patch)
and [`Pq3Block`](../crates/llama_cpp/src/pq3_reference.rs), not a claim of
compressed-cache inference. See the [implementation plan](turboquant-implementation-plan.md)
for the remaining graph, state and Vulkan work.

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
The safe general `GgmlType` gains the missing F8 mapping, but deliberately has
no PQ3 cache/weight variant in this phase.

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

## Deliberate differences and remaining transform decision

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

Therefore the row contract freezes *no implicit transform*. The future graph
patch must explicitly choose and test the Q/K transform, V transform and output
inverse before implementing kernels or state compatibility. A seeded transform
needs identical signs on CPU/GPU and the correct inverse order; plain Hadamard
needs its own quality evidence. The current K rotation uses 128 dimensions,
while V commonly uses 64; this 128-value format cannot inherit V's existing
64-dimensional rotation without a deliberate graph change. Context-local
rotation opt-outs must not silently disable a required transform.

## Build, support and state boundaries

`turboquant` is off by default, independent of `vulkan`, and forwarded through
assistant → llama_cpp → llama_cpp_sys. It adds only patch 0008 to the mandatory
0003–0007 series. Selection and contents participate in the source-copy cache
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

PQ3 K/V selection is rejected at native context creation before allocation.
PQ3 model tensors and weight-quantization selections are rejected too. Existing
FP16/Q8/Q4 inference remains available with the feature enabled. There is no
PQ3 CLI option, Vulkan kernel, persistent cache state or supported inference
pair yet. No new-format state can be produced by a live context in this phase.
Graph/state patch 0009 and Vulkan patch 0010 are the next deliverables; only
then should the safe cache API/runtime expose PQ3/PQ3. QJL-corrected TBQ3
remains separate later work.
