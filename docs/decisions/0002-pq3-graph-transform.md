# PQ3 graph transform and prototype restrictions

Status: accepted for the experimental implementation, 2026-11-04. CPU and Vulkan operation/model tests pass on the available NVIDIA GPU;
AMD/Intel validation and promotion remain separate. It refines the [delivery plan](../turboquant-implementation-plan.md)
and preserves the [row-format ABI](../pq3-reference-contract.md).

## Context

The 128-coordinate PQ3 row codec applies no transform. The donor's graph and
standalone helpers disagree about signs. Existing llama.cpp cache rotation
normally uses a 64-coordinate value transform, and applies the same matrix
forward and backward. CPU/Vulkan optimized kernels can replace a matrix marked
as Hadamard with a fast Walsh-Hadamard operation. Supplying a signed matrix under
that hint would therefore be incorrect.

## Decision

Use column-vector convention `T = H D`, with normalized 128-dimensional
Sylvester H: `H[i,j] = (-1)^popcount(i & j)/sqrt(128)`.
D uses fixed words `40f54e8c, 6587b7b0, c31220ea, 32f6449b`, least-significant
bit first within each word; set bit means -1, clear bit means +1. This is
transform identity 1, separate from PQ3 row ABI 1. No mutable RNG or environment
policy determines signs.

Apply D then the existing plain-H operation to Q/K/V. Apply H then D to the
attention output before the output projection. Keep the Hadamard optimization
hint only on H. Test actual graph orientation with all basis vectors and the
inverse composition. Quantization remains between forward and inverse, so an
inference round trip is intentionally lossy.

Initially allow CPU-reference PQ3/PQ3 for single-sequence causal ordinary Llama
and the inspected LFM3 full/SWA/hybrid topology, with 128-dimensional K and V
attention heads. Skip recurrent layers. Reject mixed pairs, other architectures,
rotation/flash opt-outs and shared-context cache ownership. Patch 0010 supplies GPU support with
its own actual-device operation validation.

Disable external state I/O, including sequence/device state and file APIs,
before parsing, writing or mutating either hybrid memory component. Disable
position rebasing and speculative/training use for this prototype. Keep append,
compact-SWA cell reuse without rebasing, and full reset/replay. Existing-format
state and transform behavior remain unchanged.

## Consequences

The scalar CPU row ABI stays stable. Internal memory objects propagate a codec
capability so hybrid operations can reject before partial mutation. A distinct
graph ABI identifies this stronger contract. Context-parameter layout is unchanged.

Sign/intermediate storage and 128-coordinate value rotation add overhead that
must be included in the total memory and latency comparison. Later fusion is
allowed only when it preserves this transform identity and placement evidence.
Persistent state requires a separately versioned codec/transform contract and
compatibility tests before these restrictions can be removed.
