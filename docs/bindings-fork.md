# Binding fork maintenance

Status: current. Read this before changing anything under `crates/llama_cpp/` or
`crates/llama_cpp_sys/`.

The binding layer is a maintained fork, not an upstream mirror. It must stay
safe, buildable, and compatible with the pinned llama.cpp revision. Application
logic belongs elsewhere; see `../AGENTS.md` §2.

## 1. Scope and provenance

- Rust crates are ported from
  [eugenehp/llama-cpp-rs](https://github.com/eugenehp/llama-cpp-rs) revision
  `1d62841020af8a309ab749b64ac3688919f383df` (0.9.0).
- The native source is `crates/llama_cpp_sys/llama.cpp`, pinned to
  `a34de8618b25dfc32ee43749db47d767a9f0a25d` (`b12015`, upstream HEAD on
  2026-10-30), including LFM3 architecture support.
- The submodule is kept pristine. Local native changes are patches and shims
  applied by the build script to a copy of the source.
- Licenses are retained per crate. Do not remove or replace them.

## 2. Layout

| Path | Purpose |
| --- | --- |
| `crates/llama_cpp_sys/llama.cpp/` | Pinned upstream source. Treat as read-only. |
| `crates/llama_cpp_sys/patches/` | Build-time patches applied to an OUT_DIR copy. |
| `crates/llama_cpp_sys/chat_shim/` | C ABI over `common/chat.h` and JSON-schema-to-grammar. |
| `crates/llama_cpp_sys/common_shim/` | C ABI over common sampler, n-gram, and related helpers. |
| `crates/llama_cpp_sys/ext_shim/` | C ABI for C++-only helpers: memory breakdown, fit, vector-returning APIs. |
| `crates/llama_cpp_sys/mtp_shim/` | C ABI over `common/speculative.cpp` for MTP support. |
| `crates/llama_cpp_sys/shim_support/` | Shared status enum, thread-local error buffer, helper macros. |
| `crates/llama_cpp_sys/build.rs` | Copies source, applies patches, drives CMake, runs bindgen, builds shims. |
| `crates/llama_cpp_sys/wrapper.h` | Bindgen entry point. |
| `crates/llama_cpp/src/` | Safe Rust wrappers. |
| `crates/llama_cpp/tests/` | Behavior, ABI, and model-gated integration tests. |

## 3. Build pipeline

At a high level:

1. Stage the exact selected patch allowlist, fail if any required file is absent,
   and read the submodule revision plus a hash of active patch names/contents.
2. Copy `llama.cpp` into the cargo `OUT_DIR`.
3. Apply `patches/*.patch` to that copy, never to the submodule.
4. Configure and build llama/ggml with CMake.
5. Generate FFI with bindgen against the patched headers.
6. Compile the C++ shims and link the static libraries.
7. Expose the generated symbols through `llama_cpp_sys` and safe wrappers.

The build script stores a sentinel for the source revision plus patch hash and
re-copies when either changes. `patch_verification.rs` also reconstructs every
touched file from pristine source with the complete ordered patch set, then
compares it with the build copy. This verifies overlapping patches as one
composed result and rejects partial applications and unrelated edits. If you edit `build.rs`, test both a warm build and
a clean build; cache assumptions are easy to break.

### Existing debt

`build.rs` still contains `expect`/`panic` paths from the ported build script.
They are known debt. New code must not add more; prefer returning a diagnostic
`Result` from helper functions and exiting nonzero with a clear message. Do not
expand a panic path just because it is nearby.

## 4. Patches

Patches are minimal, ordered, and applied with `-p1`. Each must explain why it
exists and which upstream API or behavior forced it.

| Patch | Purpose |
| --- | --- |
| `0003-exact-speculative-state.patch` | Makes speculative state restore report success/failure instead of silently doing nothing, and makes EAGLE-3 boundary state handling exact and fail-closed. |
| `0004-exact-decode-lifecycle-hooks.patch` | Adds decode begin/end callbacks plus an ABI guard symbol, so safe callback owners (tensor transactions) can rely on exact lifecycle ordering and fail at link time against an unpatched library. |
| `0005-fail-closed-eagle3-process.patch` | Replaces process-aborting `GGML_ABORT`/`GGML_ASSERT` paths in EAGLE-3 with recoverable errors for bad model pairs. |
| `0006-runtime-state-safety.patch` | Appends a context-local attention-rotation opt-out with an ABI guard; synchronizes native logger pairs and confines fitting log scopes to their thread. |
| `0007-vulkan-resource-lifecycle.patch` | Retires idle Vulkan logical devices before NVIDIA driver finalization, preserves registry descriptors across cache recreation, and destroys the instance from an ordered process-exit handler. |
| `0008-pq3-cpu-format.patch` | Opt-in `turboquant` CPU/reference format (type 44, 128 values / 50 bytes), base/CPU ABI guards and numerical tests; rejects weight use. |

| `0009-pq3-kv-graph-state.patch` | Signed-Hadamard graph identity, restricted CPU cache inference, early state/shift rejection, graph ABI, indexed-write and attention tests. |
| `0010-pq3-vulkan-cache-attention.patch` | Packed Vulkan writes/copies and scalar/coopmat1/coopmat2 attention, actual-device preflight, completed-operation observation ABI and GPU tests. |
| `0011-vulkan-blocking-fence-wait.patch` | Uses a blocking final-fence wait for resident inference instead of host polling, preserving submission/reset order and device-loss reporting. |

Patches 0003–0007 and 0011 are mandatory. `patch_staging.rs` adds 0008–0010 only for the
`turboquant` feature; feature selection and patch contents change source-copy
identity. Generated FFI and shim defines follow that same selection. Native
`tests/` sources are packaged so the experimental test/CMake patch applies to
packaged builds. See the [PQ3 contract](pq3-reference-contract.md) and
[quality evaluation](cache-quality-evaluation.md). Real-series tests compare
CLI and Rust patch engines at each increment, including overlapping/new files.

Rules:

- Do not add a patch if a shim can solve the problem without touching upstream
  behavior.
- Keep hunks narrow. Do not reformat or opportunistically refactor upstream
  code in a patch.
- Prefer fail-closed behavior: report an error rather than continuing with
  partially valid state.
- Appending fields to `llama_context_params` is deliberate ABI work; document
  offset/ABI consequences in the patch header.
- If a patch changes public native signatures, update the safe wrapper, the
  shim, tests, and `README.md` in the same change.
- Re-run the relevant tests after any patch edit; a stale OUT_DIR copy can hide
  a broken patch until a clean build.

## 5. Shims

Shims exist because bindgen cannot express some C++ types (vectors, strings,
exceptions) or because a narrow, checked ABI is safer than exposing raw C++.

Requirements:

- Expose a narrow C ABI with explicit ownership and buffer lengths.
- Never let a C++ exception cross the FFI boundary. Catch at the boundary and
  convert to the shared status code plus error text.
- Use the shared thread-local error buffer from `shim_support/`. Do not invent a
  second error channel.
- For variable-length output, use the size-query/fill two-call pattern already
  used by `chat_shim` and related modules.
- Do not return pointers to temporaries or containers that die at function exit.
- Do not store a borrowed Rust pointer beyond the documented call.
- Fail closed on invalid enum values, malformed JSON, interior NULs, and
  out-of-range lengths.
- Keep shim structs layout-stable. If a field is appended, document it and bump
  an ABI guard when the Rust side depends on it.
- Prefer `std::string_view`/`const char* + length` over implicit NUL-terminated
  assumptions at the ABI.

The Rust side must validate shim output: offsets, lengths, UTF-8, enum
discriminants, and null handles. A shim returning success is not proof that the
payload is safe.

## 6. Safe wrapper rules

- Every `unsafe` block needs a `SAFETY:` comment that explains the exact native
  invariant it relies on.
- Every `Send`/`Sync` impl needs a proof comment and, where possible, a
  compile-time assertion or test. Native thread-safety is not implied by a
  handle being a pointer.
- Preserve drop order: contexts before models, models before the backend, and
  callback state after native `llama_free`. See
  `../crates/llama_cpp/TENSOR_TRANSACTIONS_SAFETY.md`.
- Do not expose raw pointers in public APIs unless there is no safe alternative;
  if you must, document lifetime and ownership precisely.
- A wrapper may add validation, but must not silently coerce invalid values into
  valid ones. Return a typed error.
- If upstream returns a non-zero status, map it to a specific error variant
  where possible; keep an `Unknown(code)` fallback for new codes.
- Keep feature gates consistent with Cargo features and CMake options. A Rust
  feature that does not actually reach the native build is a bug.

### Native log callback

`llama_cpp::logging` is the managed primitive; it is intentionally not a
logging framework:

- `logging::set_log_callback` installs a static C trampoline and stores the
  supplied callback in process-global state.
- Registration is persistent. Calling the setter again replaces the active
  callback. The module does not keep a stack and does not restore the callback
  that was active before the first Rust registration.
- The callback receives a borrowed `&str` plus a `LogLevel`. It may run on any
  native thread, so it must be `Send + Sync`.
- Panics are caught at the FFI boundary; recursive logging from inside the
  callback is dropped.
- The mandatory native patch synchronizes global callback/user-data pairs.
  Emission snapshots the pair, unlocks, and then calls it; first installation
  and replacement are safe during inference.
- Fitting helpers use scoped thread-local callbacks. Stack-local fit state is
  never published to logging threads. Raw callbacks still require their owners
  to retain user data until every in-flight invocation finishes.
- Closure captures are destroyed outside the Rust state lock. The complete
  trampoline, including in-flight capture destruction, contains unwinding.
  Panic payloads are intentionally forgotten to prevent a second destructor
  panic from crossing the C ABI.
- The deprecated `LlamaBackend::void_logs` compatibility path installs a no-op
  callback through the same setter.

Do not add filtering, formatting, buffering, or `tracing` dependencies to this
module. Bridging records into `tracing` belongs to the application.

Coverage is deliberately narrow: this callback covers llama/ggml core logs. The
`common/` logger (`runtime::log`) and the clip/mtmd logger (`mtmd_log_set`) are
separate native sinks and are not captured by `set_log_callback`.

### Vulkan fence waiting

Patch 0011 makes the normal Vulkan synchronization path block on its final
completion fence instead of repeatedly querying it with CPU pause instructions.
It retains the optional almost-ready fence's wait/reset, the final fence reset
after successful completion, and `VK_CHECK` error/device-loss diagnostics.
Submission batching, GPU work, and resource lifetimes are unchanged. There is no
new configuration or public ABI; the patch is active with or without the
experimental codec feature and leaves the upstream pin intact.

This trades potential CPU wake-up latency for lower host CPU consumption in a
resident daemon. Upstream's hybrid policy was introduced by
[PR #12630](https://github.com/ggml-org/llama.cpp/pull/12630), which reported up to
5% higher throughput on NVIDIA/Windows. The local CPU/throughput impact still
requires real-GPU before/after measurement; the user profile establishes the
polling cost, not the replacement's performance. See the
[optimization plan](vulkan-cpu-optimization-plan.md) for evidence and validation.

The isolated `test_vulkan_lifecycle` child executes small and longer graphs,
changes their inputs, and repeats asynchronous submission, synchronization, and
readback before exit. The longer graph also crosses the intermediate-fence
threshold. These tests require a usable Vulkan device to establish GPU coverage.

### Vulkan resource lifetime

The Vulkan patch addresses the process-exit failure class tracked in
[llama.cpp #10528](https://github.com/ggml-org/llama.cpp/issues/10528). The original
NVIDIA 615.78.08 core showed a background shader compiler fault while the main
thread finalized the driver, after generation and worker shutdown.

- Registry device and buffer-type descriptors remain valid for the process
  lifetime. They hold a device index rather than retaining a logical device.
- Backends, native Vulkan buffer contexts, events, and pinned host
  allocations retain their native storage owners. Releasing the last external
  owner attempts cache cleanup; resident model weights continue to pin devices.
- The additive native `ggml_backend_vk_cleanup()` returns false without retiring
  any device while external owners or pinned allocations remain. It synchronizes
  every eligible device before breaking staging-buffer cycles and destroying
  devices. Later operations can recreate them using the same descriptors.
- Initialization, device publication, staging-cache replacement and cleanup share
  a lifecycle mutex. Retirement checks run on native owner release; there is no
  polling or per-token cleanup hook.
- A handler registered after instance creation attempts final device cleanup and
  then destroys the instance ahead of the finalizers registered during instance
  creation. Normal owner release retires devices before process finalization,
  including compiler libraries loaded lazily after instance creation. It refuses to destroy
  live resources. Hosts must still stop and release native work before process
  exit; abandoning threads or live global inference owners is not supported.
- The public native struct ABI and safe Rust API are unchanged. The sys wrapper
  includes `ggml-vulkan.h` for the additive raw declarations; referencing Vulkan
  symbols still requires a native Vulkan build. The upstream pin is unchanged.

The isolated `test_vulkan_lifecycle` integration binary exercises live-owner
refusal, empty buffers, tensor transfers, events, pinned allocations, stable
descriptors, recreation, and concurrent cleanup/release without model weights.
Its child-process test executes a Vulkan graph and exits through the real driver
finalizers without calling explicit cleanup.
Set `LLAMA_TEST_REQUIRE_VULKAN=1` to turn a missing device into a failure. Setting
`GGML_VK_DISABLE_HOST_VISIBLE_VIDMEM=1` additionally exercises staging transfers
on discrete GPUs that normally map their memory directly. Run with a real GPU;
a sandbox that hides driver devices cannot establish this coverage.

Validation on 2026-11-03 used an NVIDIA RTX A500 Laptop GPU with driver
615.78.08 and LFM3-1.7B Q6_K. The strict lifecycle tests passed with staging
forced. GDB observed device destruction on the model worker before the exit
hook, followed by normal process exit. Ten repetitions of the reported CLI
prompt and three native generation runs exited cleanly without compiler errors.
Three interleaved native runs per version measured median decode throughput of
55.92 tokens/s before and 56.27 tokens/s after; this small sample establishes no
observed decode regression, not a speedup. AMD and Intel hardware were not
available for validation. The original driver fault was intermittent, so clean
smoke runs alone cannot prove its absence.

`cargo fmt --all --check` and `cargo check --workspace` passed. The unfiltered
workspace test gate is still red for independent baseline issues:

- `plaintext_tokenization_does_not_forge_special_tokens` and
  `tokenize_prompt_neutralizes_injected_markup` assume special-token markup
  absent from the fallback Llama 3 vocabulary.
- `test_model_desc`, `test_model_rope_properties`,
  `test_model_numeric_properties`, and `test_model_display` query model
  properties left uninitialized by vocab-only loading. The display test aborts
  in upstream `llama_hparams::n_head`, including without a usable GPU.
- `model_can_use_two_different_tools_in_one_conversation` sometimes answers
  arithmetic without calling `calculate`; the same failure reproduced on the
  preserved pre-patch executable.

The remaining tests passed with those cases excluded. The two other model tool
tests passed separately and were excluded from the final repeat to avoid
another lengthy CPU run. No existing test assertions were changed. The new
Vulkan tests also passed the missing-driver skip path and the strict real-GPU
path after that skip handling was verified.

### Ownership, allocation, and API compatibility (2026-11-03)

- `LlamaBackend` has private construction and cloneable retained leases. The
  initialization gate remains closed until final native shutdown completes.
  Loaded, raw, and synthetic models retain the lease and release it after
  `llama_model_free`; dropping the public handle cannot shut down a live model.
  `init_numa` initializes the backend before NUMA, rejects the `COUNT` sentinel,
  and reports a missing CPU backend instead of entering an asserting native API.
- `LlamaVocab<'model>`, `GgmlTensor<'ctx>`, and `GgmlGraph<'ctx>` borrow their
  native storage owners. ggml dependencies must belong to the same arena;
  copying shape/type into another arena does not retain a dependency.
- `LlamaBatch::new` and `get_one` now return `Result<_, BatchInitError>`.
  Token batches use stable Rust-owned arrays rather than unchecked native
  `malloc` results. Each add validates token and sequence-row capacities before
  writing. Never free these owned arrays with `llama_batch_free`.
- ggml constructors and metadata operations are fallible (`GgmlError`). Names
  are owned snapshots; renaming an aliased handle cannot invalidate a borrowed
  Rust string. Native type indices, dimensions, offsets and pool headroom are
  checked before allocation. `GgmlAllocr` borrows its backend.
- Raw ggml allocation and execution (`alloc_ctx_tensors`, `alloc_graph`,
  `graph_compute`) are explicitly unsafe: external tensor-buffer lifetime and
  initialization cannot be proven from a graph metadata handle. Keep every
  referenced allocator and buffer alive through all later uses of its addresses.
  Returning a graph/tensor metadata handle does not transfer buffer ownership.
- `QuantModelDesc::build`, unsafe `load_from_file_ptr`, and unsafe
  `init_from_user` now take a backend reference to retain initialization.
- `set_attn_rot_disabled` stores an atomic Rust default for subsequent contexts;
  it no longer writes the process environment. A context-local flag and an
  externally configured `LLAMA_ATTN_ROT_DISABLE` remain independent opt-outs.
  Native scoped state is used only during synchronous memory construction,
  including nested KV-cache constructors. Recheck this assumption on upstream
  updates; a shared KV cache continues to inherit its source's rotation layout.

The new context field is appended after the decode hooks. All earlier field
offsets are preserved, but struct size changes. Rust references
`llama_cpp_rs_runtime_safety_abi_v1` so a stale library cannot silently omit this
contract. No source inside the pinned submodule is edited. Regression coverage
includes compile-fail lifetime examples, invalid batch dimensions/memberships,
backend epochs/model retention, native logging races and reentrant capture
cleanup, context rotation isolation, and composed patch verification.

## 7. Features and linking

- `llama_cpp` defaults to `openmp`, `mtmd`, `ggml`, and `rpc`.
- `assistant` selects `default-features = false`; only the backend it needs
  should be enabled.
- Backends: `vulkan`, `cuda`, `metal`, `blas`, `opencl`, `hip`, `webgpu`.
- `dynamic-link` builds shared libraries; static linking is the workspace
  default. Dynamic linking needs a runtime library search path.
- `prebuilt` is not currently usable because the required patches must be
  applied to source. Do not present it as a supported shortcut.
- `q1` is a compatibility no-op; Q1_0 is upstream and unconditional.

When adding or changing a feature, verify:

1. `cargo check -p llama_cpp --no-default-features`
2. `cargo check -p llama_cpp` (default features)
3. the backend-specific combination you touched
4. that the CMake option is actually forwarded

## 8. Upstream update procedure

An update is deliberate and reviewable:

1. Fetch and check out the target revision in the submodule:
   ```sh
   git -C crates/llama_cpp_sys/llama.cpp fetch origin master
   git -C crates/llama_cpp_sys/llama.cpp checkout --detach <revision>
   ```
2. Run `cargo test -p llama_cpp` from a clean build. Note that a patch may fail
   to apply; do not "fix" it by editing the submodule.
3. Rebase or rewrite patches against the new source. Keep each patch's intent
   and header comment.
4. Adapt shims and safe wrappers to upstream API changes.
5. Run the binding test suite and at least one real generation smoke test.
6. Update the revision in `README.md` and any docs that quote it.
7. Stage the submodule gitlink and report breaking API changes to the human.

Do not update the native revision opportunistically while fixing an unrelated
bug. Keep the two changes separate so failures are attributable.

## 9. Testing

Baseline:

```sh
cargo test -p llama_cpp --lib
cargo test -p llama_cpp --test test_chat
cargo test -p llama_cpp --test test_context_params
cargo test -p llama_cpp --test test_sampler
cargo test -p llama_cpp --test test_batch_ext
```

Model-gated tests use `target/test-models/stories260K.gguf` or
`LLAMA_TEST_MODEL`. They must skip cleanly when the model is absent; do not
download models from tests.

Before changing a shim or patch, identify the narrowest test that exercises the
affected API. If none exists, add one before changing behavior. For lifetime or
callback changes, add a compile-time assertion or a deterministic test rather
than relying on a manual smoke test.

## 10. Common failure modes

- **Stale OUT_DIR copy.** A patch edit appears to have no effect. Clean the
  package build output or bump the patch hash path.
- **Patch context drift.** Upstream changed nearby code. Rebase the patch; never
  edit the submodule to make it apply.
- **ABI mismatch.** A prebuilt or previously built library lacks a guard symbol.
  Check the ABI guard and rebuild.
- **Exception across FFI.** Undefined behavior. Catch in the shim and return a
  status.
- **Dangling error string.** The thread-local buffer is valid only until the
  next shim call on that thread; copy immediately.
- **Wrong drop order.** Context or callback state outlives native storage. Check
  `Drop` implementations and the tensor transaction contract.
- **Feature not forwarded.** A Rust feature compiles but the native code was
  built without the corresponding CMake option. Inspect `build.rs` and the
  build log.
- **Weakened safety check.** A test starts passing for the wrong reason. Revert
  and fix the root cause.

## 11. Reporting binding changes

When handing work back, state:

- what changed and why
- the pinned revision and whether it moved
- patches added/edited and their upstream dependency
- shims touched and ABI impact
- tests run, including which were skipped and why
- any known compatibility or safety caveat

Keep this report short and factual. The human reviews the diff, not a narrative.

## Guarded sampler construction (2026-11-03)

The assistant foundation uses additive `LlamaSampler::try_greedy`,
`LlamaSampler::try_chain` and `ChatParams::try_grammar_sampler` methods. Existing
constructors remain available. `try_grammar` and `try_grammar_lazy_patterns` now
also route native construction through guarded C++ shims, containing allocation
and regex exceptions. `SamplerInitError` gains a `Native(String)` variant, so
exhaustive matches must handle it. No native struct layout or submodule revision
changes accompany these APIs.

The new shim chain-add entry point transfers child ownership only on success.
At pinned b12015, native add is a `vector::push_back` of a trivial entry; an
allocation exception preserves the vector and caller ownership. Rust forgets a
child only after successful transfer; earlier children are released by the chain
on a later failure. Review that native operation's exception guarantee when
updating the upstream pin. All guarded failures use the shared thread-local error
channel. Fallible render grammar construction preserves lazy triggers and
propagates generation-prefix tokenization/acceptance errors.

## Cache inspection and policy (2026-11-04)

`ext_shim` now exposes versioned fallible memory collection, owned scalar
model/cache inspection and guarded synchronization. Its inspection code depends
on the pinned model/context and full/SWA/hybrid cache classes; review those
accessors during an upstream update. No private C++ container or native pointer
is returned to Rust callers. All new entry points use the shared exception/TLS
error barrier. Names must fit the fixed ABI field completely; truncation fails.
The cache inspector skips empty arenas before calling native type getters,
which index their first layer. MLA/compressed layouts are rejected before
those getters: MLA can have a null V tensor even in a nonempty cache. Unknown
memory classes explicitly report that inspection is unavailable. The old best-effort collector remains compatible.

The additive `context::cache` API restricts the initial supported cache policies
to FP16/Q8_0/Q4_0 and validates model geometry independently of parameter
application. Native backend checks remain required. These APIs and all new
shim symbols require rebuilding the sys crate; the native pin, parameter layout,
existing ABI guard and patch series are unchanged. See the
[baseline record](kv-cache-baseline.md) for API usage and validation evidence.

## Allocation-backed GPU identities

`llama_gpu_allocations_collect_v1` is an additive guarded size-query/fill API in
`ext_shim`. It inspects allocated model buffers, optionally including a matching
context's cache/compute buffers. Host buffers and empty allocations are excluded;
non-host allocations without an attributable device remain unknown. No-allocation
fit models are rejected. Fixed strings fail instead of truncating identities.
`LlamaModel::gpu_allocations` and `LlamaContext::gpu_allocations` return owned
`GpuAllocation` records; no backend handles or borrowed pointers cross threads.
Context records include shared model weights, so their byte counts must not be
summed across contexts. The runtime unions PCI identities rather than allocation
bytes. This proves placement, not driver residency or actual operator execution.
The pinned submodule and existing memory-breakdown ABI are unchanged.

## Checked embedding and tokenizer access

Embedding slices use `n_embd_out` (or `n_cls_out` for rank pooling), a checked
allocation length and last-successful output eligibility. Failed evaluation clears
eligibility. Token rows require explicit nonnegative selected batch indices;
pooled sequences require valid nonnegative sequence IDs. `EmbeddingsError` is
non-exhaustive and reports invalid indices/shapes/native synchronization errors.
`get_embeddings` returns the first selected token row, not a guessed matrix.

The additive `llama_tokenize_guarded` shim contains C++ vocabulary exceptions
before returning to Rust. It preserves signed size-query/retry counts and keeps
Rust vector length zero on failure. Both tokenization and counting use it;
`StringToTokenError::Native` carries no input text. This was required by the
owner-supplied E5 vocabulary's missing byte-fallback rows, which caused a real
foreign-unwind abort during background indexing. No upstream submodule edit or
pin change is involved.

`LlamaModelParams::append_kv_override` now returns `Result<(), KvOverrideError>`.
It supports multiple distinct keys and rejects invalid keys/values and excessive
lists before mutation. Call sites must handle the result. See the
[memory native integration record](memory-native-integration.md) for the selected
artifact's two explicit overrides and qualification limits.
