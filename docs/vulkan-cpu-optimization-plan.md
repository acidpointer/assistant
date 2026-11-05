# Reduce Vulkan inference CPU usage

Status: blocking waits implemented as the default, 2026-11-05. Real-GPU
correctness and performance acceptance remain open; see validation below.

## Recommendation

Replace the unbounded final-fence polling loop with a blocking Vulkan fence
wait. Make this the normal behavior in the ordinary build. This is a small,
vendor-independent binding-fork change with a measured CPU target. It requires
neither a diagnostic build nor changes to application policy.

The contract remains: synchronization returns successfully only after the same
final GPU fence signals; pending intermediate fences are consumed and reset;
command buffers and output storage remain live until completion; native failures
retain device-loss diagnostics and existing error propagation.

## Evidence and expected benefit

The user supplied two captures of the same release daemon with compact SWA and
Q8 K/V caches. Percent process CPU uses one logical core as 100%.

| Observation | Earlier capture | Later capture |
| --- | --- | --- |
| Process CPU | 47.5% | 151.3% |
| Main model thread, TID 29649, user CPU | 36.4% | 38.6% |
| Additional thread, TID 29652, user CPU | approximately 0% | 97.2% |

In the later capture, flat perf samples assigned 16.96% of sampled CPU time to
`ggml_vk_wait_for_fence` itself. A library grouping assigned 64.11% to NVIDIA's
`libnvidia-gpucomp`. These are separate costs; the compiler share must not be
attributed to fence polling. Root-owned capture location supplied by the user:
`/tmp/assistant-profile-dhxjbdis`. The findings use the pasted reports, not an
independent reanalysis of the raw capture.

Scaling the fence sample share by recorded CPU time suggests approximately
24–26 percentage points of one core spent in that function during this capture.
This estimates the size of the target, not a promised reduction: profiling
windows differ slightly, blocking has its own costs, and driver wait behavior
can vary. Removing our loop does not guarantee zero CPU use inside the driver.
The earlier capture had no function profile, so its potential saving is unknown.

NVIDIA documents background optimization of Vulkan shaders in its
[Nsight Graphics release notes](https://developer.nvidia.com/nsight-graphics-2023_4).
That is consistent with the additional compiler worker, but the reports do not
prove what triggered it or whether it will recur. This first change does not
claim to eliminate that worker.

## Source findings

Paths below refer to the pinned, pristine upstream source under
`crates/llama_cpp_sys/llama.cpp/`.

- `ggml/src/ggml-vulkan/ggml-vulkan.cpp:339`: `ggml_vk_wait_for_fence`
  first waits for an optional almost-ready fence, then repeatedly calls
  `getFenceStatus` on the final fence without a time bound. Each unsuccessful
  iteration issues 1,000 `YIELD()` operations.
- `ggml/src/ggml-vulkan/ggml-vulkan-types.h:78`: on x86, `YIELD()` is
  `_mm_pause()`. It does not put the thread to sleep.
- `ggml/src/ggml-vulkan/ggml-vulkan.cpp:14769`: the intermediate fence is
  selected using remaining graph node count, not measured GPU execution time.
  It cannot bound how long the final polling loop consumes CPU.
- `ggml/src/ggml-vulkan/ggml-vulkan.cpp:13549`: synchronization already
  uses blocking fence waits in the serialized path. In the ordinary path it
  submits the final completion fence before calling `ggml_vk_wait_for_fence`.
  Subsequent buffer reset and output copies depend on successful completion.
- `ggml/src/ggml-vulkan/ggml-vulkan-types.h:195`: `VK_CHECK` preserves
  device-loss reporting and rejects unsuccessful results.

The polling policy was introduced by
[upstream PR #12630](https://github.com/ggml-org/llama.cpp/pull/12630) to reduce
wake-up latency. Its author reported up to 5% higher throughput on
NVIDIA/Windows. That establishes a real tradeoff; it does not predict or bound
the throughput change on this Linux system.

[Vulkan specifies](https://docs.vulkan.org/refpages/latest/refpages/source/vkWaitForFences.html)
that a wait returns immediately when its condition is already satisfied and
otherwise blocks until completion or timeout. Device loss must return in finite
time. We need no preliminary status query or arbitrary sleep interval.

## Implementation

The following change is implemented. It is a production change in the ordinary
build; the remaining performance targets are validation criteria.

1. Add `crates/llama_cpp_sys/patches/0011-vulkan-blocking-fence-wait.patch`.
   Include the upstream PR and the daemon CPU rationale in its header. Modify
   only `ggml_vk_wait_for_fence` in the build-directory source copy.
2. Keep the existing almost-ready fence wait, reset, and pending-state update.
   Replace the entire final status/pause loop with:

   ```cpp
   VK_CHECK(ctx->device->device.waitForFences({ ctx->fence }, true, UINT64_MAX),
            "final fence waitForFences", ctx->device);
   ```

   Keep the final reset after successful completion. The existing macro keeps
   device-loss diagnostics and exception behavior; a failed wait must not
   advance to reset or resource reuse.
3. Add the patch to the mandatory list in
   `crates/llama_cpp_sys/patch_staging.rs`. It must apply with and without the
   experimental codec feature. Existing fingerprints invalidate stale native
   build copies. Update `docs/bindings-fork.md` with the behavior and tradeoff.

This has no public ABI change, Rust API change, model/cache format change, or
upstream pin update. Keep queue synchronization, graph scheduling, intermediate
submissions, and resource ownership intact. The intermediate fence could later
be simplified, but that is a separate change.

Do not add a CLI switch, timer, spin budget, or per-token logging for this first
patch. Do not enable `GGML_VK_SERIALIZE_SUBMISSIONS`: that changes GPU pipelining
as well as waiting. A millisecond sleep also adds avoidable latency after GPU
completion; a fence wait wakes on the actual completion condition.

## Validation and adoption

Use normal release builds for before/after measurements. No diagnostic-only
build or extra instrumentation is required.

- Run patch staging and patch verification tests, including the real patch
  series with both codec selections, then workspace formatting/check/tests.
- Exercise actual Vulkan computation/readback with repeated synchronization and
  buffer reuse. Cover a small graph without an intermediate fence and a longer
  graph that signals one; validate numerical outputs. Extend the existing
  Vulkan integration tests only where these cases are missing.
- Run strict Vulkan lifecycle tests, including forced staging transfers with
  `GGML_VK_DISABLE_HOST_VISIBLE_VIDMEM=1`. Confirm shutdown and device retirement
  still work. GPU-skipped tests do not establish this coverage.
- Run existing required-model LFM3 inference/cache checks with compact Q8 K/V,
  plus generation cancellation and subsequent reuse. Fence scheduling must not
  change token arithmetic or remove existing failure checks.
- Compare warmed long answers using the same model, prompt, context, generated
  token count, and cache settings. Alternate baseline/candidate runs, with at
  least three runs of each. Record tok/s and process CPU seconds per generated
  token. The existing profiler and WebUI suffice; check that compiler activity
  does not differ substantially between versions before attributing a result.

Proposed adoption gate: all correctness checks pass, warmed median CPU seconds
per generated token improves by at least 20%, and warmed median decode
throughput loses no more than 5%. These are product acceptance targets, not
measured outcomes. Also compare time to first token and cancellation behavior;
investigate any regression outside baseline variation. If the gate fails,
revisit the waiting policy rather than declaring the optimization successful.

Persistent pipeline caching remains a separate candidate: the current code
reuses compiled pipeline objects but passes `VK_NULL_HANDLE` as the explicit
pipeline cache at creation. It may reduce compilation costs, particularly across
restarts, but it is larger in scope and does not remove the measured polling.
