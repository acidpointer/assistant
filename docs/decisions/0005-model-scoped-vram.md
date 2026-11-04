# 0005: Scope VRAM to the model's participating GPUs

Status: accepted and implemented, 2026-11-04.

VRAM means process residency on devices actually holding non-host buffers for the
primary model or its contexts. It does not mean every GPU installed, every device
registered by ggml, or whichever card has nonzero utilization at the sample time.
Idle resident model memory still counts. Split models include all participants;
a CPU-only model reports zero despite unrelated backend/graphics allocations.

A guarded native snapshot identifies buffer devices before the first context and
after context allocation changes. The native owner publishes owned PCI identities
through a bounded watch snapshot. No native handles enter async telemetry. One
sampler queries only these devices, matching NVML by PCI address and reading AMD
DRM resident VRAM by PCI/client identity. Duplicate DRM fds are one client; unknown
shared attribution across resident clients cannot become a complete total.

Driver counters retain their own scopes: selected-device process residency can
include other daemon work on that device. Native buffer sizes establish placement
and never masquerade as physical resident VRAM. Unknown placement or any missing
participant counter makes the aggregate unavailable. Per-device values remain
visible in details so partial coverage is inspectable.

Sources and bounds: [web contract](../web-ui.md#output-limits-and-measurements),
[binding ABI](../bindings-fork.md#allocation-backed-gpu-identities), and
[Linux DRM counter semantics](https://docs.kernel.org/gpu/drm-usage-stats.html#memory).
