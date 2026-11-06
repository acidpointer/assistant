# Native build performance

2026-11-06 audit: the helper files and measurements below existed in the working
tree, but the active `build.rs` did not import them. They are now connected:
verified timestamp-preserving source publication, tracked CMake definitions,
optional compiler launchers, immutable library bundles, library-only multimodal
targets, and patch 0012's bounded/incremental shader build. Direct build/dev
dependencies and package includes now cover these modules. Focused helper tests
pass (12 cases). Actual subsequent builds reported verified bundle reuse for
keys `71ce7c3985b0` and `a0f31b63c80e`. A daemon build took 11.67 seconds after the
cache integration; a check that waited on another Cargo build took 56.06 seconds.
These are observations, not isolated cold/warm benchmarks. Earlier measurements
below are retained as prior-session evidence, not measurements from this rework.

Status: implemented for the audited native Linux GNU static-library subset,
2026-11-05. The research and original sequencing are retained below; the current
build contract is documented in [bindings-fork.md](bindings-fork.md).

## Implemented results

The ordinary build now uses library-only CMake targets, timestamp-preserving
verified snapshots, optional compiler launchers, verified native bundle reuse
between Cargo outputs, and incremental/bounded shader generation. Native
inference stays optimized in Rust debug and release builds. No flags or manual
cache preparation are needed for supported native bundle reuse.

Measured normal commands from this implementation session:

| Scenario | Elapsed |
| --- | --- |
| First daemon debug build, including native compilation | 242.05s |
| Warm daemon debug build after build-helper edits | 6.99s |
| Switch to daemon release, importing native libraries | 16.02s |
| Immediate release repeat | 0.11s |
| Warm workspace check | 5.65s |
| Full workspace tests with one test thread | 32.80s |

These are development observations, not a controlled cold-build comparison.
The warm debug build changed none of 294 observed native object timestamps.
The release output imported verified archives and has no native CMake build
graph. All 155 generated shader C++ files matched the previous build byte for
byte. Shared library-only configuration succeeded with multimodal support and
without CLI targets; shared linking was not exercised.

Formatting and workspace check passed. The final serialized workspace suite
reported 617 passed, 14 ignored. An earlier parallel suite passed, but a later
parallel rerun exposed an existing test-isolation race: the global logging test
captured an invalid-model error from another test alongside its own message.
No logging assertion was weakened; the final suite used `--test-threads=1`.
GPU/full-model optional cases skipped in this environment. A final focused run
also passed all 10 cache/configuration/staging/shim tests, including a real C++
compile through a delegating launcher with spaces and apostrophes in paths.

Versioned [measurements](benchmarks/native-build-2026-11-05.json) include the
commands, timings and coverage limits. Compiler-launcher detection and forwarding
are implemented, but sccache/ccache path normalization and hit rates remain
unverified because neither tool is installed here. The native bundle path works
without them. Unsupported platforms/configurations retain normal CMake builds;
remote/prebuilt bundle distribution remains future work.

## Findings

The expensive package is `llama_cpp_sys`, which builds llama.cpp, Vulkan
shaders, common helpers, optional multimodal code, and the C++ shims.
`llama_cpp` itself is the Rust wrapper. Keep native inference optimized in both
Rust debug and release builds; avoid repeating equivalent native compilation.

Evidence comes from the preceding fence-wait implementation's build logs,
Cargo fingerprints, generated CMake caches and Ninja logs. These are local
observations, not controlled performance benchmarks:

| Observation | Workspace check | Workspace tests |
| --- | --- | --- |
| Cargo build/compile stage | 5m 54s | 4m 33s |
| Native build type | Release, `-O3 -DNDEBUG` | Release, `-O3 -DNDEBUG` |
| Native object outputs recorded in Ninja | 381 | 381 |
| Generated shader `.comp.cpp` outputs | 155 | 155 |
| Upstream tool executables | 20 | 20 |
| `ggml-vulkan.cpp` object compilation | 209.1s | 150.2s |

The test compile stage excludes execution of the test suite. Per-object times
are elapsed task durations while other jobs ran, not process CPU time. The two
runs are not a speed comparison: system conditions and build configurations can
affect duration.

### Unnecessary executables

`crates/llama_cpp_sys/build.rs:2566` enables `LLAMA_BUILD_TOOLS` whenever the
`mtmd` feature is enabled. The pinned upstream
`llama.cpp/tools/CMakeLists.txt:17` then adds benchmarks, quantization,
perplexity, splitting, completion and other executable targets. The binding
needs libraries, not those executables.

The pin already offers `LLAMA_BUILD_MTMD` independently of tools at
`llama.cpp/CMakeLists.txt:267`, and gates multimodal CLI targets on
`LLAMA_BUILD_TOOLS` in `tools/mtmd/CMakeLists.txt`.

Both observed workspace configurations included default library features
`mtmd`, `openmp`, and `rpc`, plus Vulkan. Their Ninja logs contain 26 tool
objects outside `libmtmd`, in addition to the required 59 multimodal library
objects. Removing the tools does not eliminate the multimodal library's cost.
Ordinary daemon-only builds already disable default binding features; their
benefit from this particular fix will therefore differ.

### A patch change invalidates unchanged sources

`build.rs:412` includes the selected patch fingerprint in source identity.
At `build.rs:1986`, any source-identity change removes and replaces the staged
source tree. Unix staging uses `cp -rf` at `build.rs:480` without preserving
timestamps. For an unchanged shader inspected in both build directories, bytes
matched upstream but modification times differed.

This is correct about patch freshness but expensive for incremental compilation:
an edit to one patch refreshes unrelated C++ sources, headers and shaders.
The invariant to preserve is a complete, verified patched tree before CMake
runs, never a stale or partially patched input. Skipping verification or
hardlinking mutable staged files to the submodule is unacceptable.

### Cargo configurations duplicate native work

The observed check and test invocations had different `OUT_DIR` directories
despite matching sys features, native profile, compiler path, and inspected
native flags. Their build-script dependency fingerprints differed. This is
normal Cargo behavior; it is not evidence that Cargo ignored its cache.

Native outputs are intentionally isolated per `OUT_DIR` at `build.rs:1944`.
That prevents CMake from switching one mutable tree between incompatible source
paths and configurations. Both Rust profiles default to native Release at
`build.rs:1898`, so equivalent native inputs can potentially be reused without
making Rust release/debug code identical. Shims built through `cc` may have
different profile flags and must be treated separately.

[Cargo documents](https://doc.rust-lang.org/cargo/reference/build-scripts.html)
that build-script dependencies can trigger rebuilds, that `OUT_DIR` survives
reruns, and that generated files should live there. A native artifact cache
must account for these rules instead of forcing every build into one CMake
directory. Workspace feature selection also matters: a daemon-only Vulkan
build cannot reuse a whole bundle compiled with different backend or ABI flags
merely because both use native Release.

### Compiler caching is currently inactive here

Ninja is installed in this environment; `sccache`, `ccache`, and `mold` are not
on PATH. Both observed CMake caches report no sccache, with no configured
compiler launcher. `build.rs:2484` supports sccache only when found; its comment
about cheap feature switches is not an established performance result here.
This describes the agent build environment, not a separate audit of the user's
interactive shell.

An object cache can avoid recompilation after Cargo changes directories, but
path changes, flags, headers and preprocessed source affect cache hits.
[sccache's cache-key documentation](https://github.com/mozilla/sccache/blob/main/docs/Caching.md)
and [path normalization options](https://github.com/mozilla/sccache/blob/main/docs/Configuration.md)
must be checked against the installed version. Installing a cache alone does
not prove reuse across hashed source/build paths. A C++ object cache also does
not automatically cache subprocesses launched directly as `glslc`.

### Vulkan shader generation has its own costs

The generator launches up to 16 shader compilations per generator process,
independent of Cargo's job budget (`vulkan-shaders-gen.cpp:343`). Ninja can run
several generator processes concurrently. Increasing `-j` indiscriminately can
therefore increase contention and memory use.

Generated headers use `write_file_if_changed`, but generated shader C++ files
are always rewritten (`vulkan-shaders-gen.cpp:1392`). Identical regeneration
can unnecessarily trigger embedding-object recompilation.

Do not assume `ExternalProject`'s `BUILD_ALWAYS` recompiles all shaders on every
build. In the inspected Ninja graph its generator target is an order-only
dependency; the shader generation rules also track source files and depfiles.
Preserving correct inputs and timestamps is the first fix. Audit executable,
tool-version and configuration dependencies before changing these rules.

## Ordered implementation

### 1. Build libraries without upstream CLI tools

Small first patch in `build.rs`:

- Default `LLAMA_BUILD_TOOLS=OFF` and select `LLAMA_BUILD_MTMD` from the `mtmd`
  feature. Preserve `LLAMA_BUILD_COMMON=ON`, which supplies speculative-session
  shim implementations. Preserve the existing Rust feature/API defaults.
- Respect documented explicit CMake-option precedence; do not silently change
  a deliberate user request for upstream tools.
- Make configuration changes invalidate the CMake configuration correctly.
  Current stale-cache checks omit tools/MTMD/profile and several other inputs.
  Add an effective-configuration digest or reconfigure when these options
  change; do not delete the whole object tree as the default response.
- Validate static/shared linking and existing multimodal APIs. Inspect the
  generated target graph: required libraries present, unwanted CLI targets
  absent. Use a small configuration test before another expensive full build.

Expected result: remove the observed 20 executable targets from default full
workspace builds. No wall-time percentage is promised because jobs overlap.

### 2. Preserve unchanged staged source timestamps

Replace whole-tree timestamp invalidation without weakening staging safety:

1. Construct the desired patched snapshot in a temporary directory from the
   pristine pin and selected patches; verify the composed patch result.
2. Compare desired files with the currently staged tree. Preserve the existing
   modification time for byte-identical unchanged files; changed contents must
   invalidate their build dependencies. Do not publish via mutable hardlinks.
3. Publish a complete snapshot at the same source path using the existing
   temporary-directory/rename strategy with explicit recovery for interrupted
   replacement. Write the source-identity sentinel only after success.
4. Remove files omitted from the desired snapshot. Feature disablement, patch
   removal and submodule updates must not retain stale generated sources.

A new directory inode is fine: CMake/Ninja see the same source paths and
unchanged file timestamps. Simply copying upstream timestamps blindly is not
enough; decide preservation by comparing the desired patched contents.

Test unchanged-file timestamps, changed and removed files, patch removal,
codec toggles, interrupted publication and corrupted trees. Existing real
patch-series tests remain mandatory. Acceptance for a C++-only patch change:
no unrelated shader regeneration and no unchanged translation-unit compilation.
Changing `ggml-vulkan.cpp` itself still requires compiling that large file once.

### 3. Enable compiler caching with demonstrated hits

Support detected sccache or ccache as the C/C++ compiler launcher, with explicit
configuration when needed. No automatic system package installation. Verify
that the launcher reaches the main build, C++ shims and host shader-generator
sub-build; CMake launcher settings do not automatically propagate to every
independent ExternalProject.

Normalize staged source and generated build paths using the installed cache's
supported mechanisms, preserving distinctions that affect code generation.
Validate actual hits when moving between equivalent Cargo configurations.
Keep incompatible compiler, ISA, sanitizer, CRT, ABI and native profile entries
separate. Keep native `-O3` for normal debug and release inference.

This improves repeated compilations and overlapping sources between different
native feature configurations. It does not eliminate every generation/configure
step or the first compilation of genuinely changed C++ code.

### 4. Reuse verified native artifacts across Rust profiles

For predictable check/test/debug/release switching, add an immutable native
artifact bundle keyed by effective native inputs, independent of Cargo's
incidental `OUT_DIR` name and Rust-only build dependency graph.

The key must include pin, complete active patch identity, native build recipe,
target/host and cross toolchain, compiler identity, sysroot/SDK, native profile,
flags/ISA, static/shared mode, CRT/sanitizers, and effective backend/library
configuration. Include shader compiler/generator identities and inputs. Rust
features affecting native headers or shim ABI must remain represented.

Store verified libraries, required headers/assets and a manifest with artifact
checksums. Lock by key, build in a private directory, and publish atomically
after verification. Never point incompatible Cargo configurations at one
mutable CMake tree. Compile profile-dependent shims separately unless their
complete inputs also match.

The implemented integration publishes a bundle inside each writer's own
`OUT_DIR` and discovers matching bundles in neighboring Cargo profile outputs
read-only. Consumers copy verified archives into their own output. This makes
ordinary Cargo commands reuse native libraries automatically without a shared
mutable CMake tree or writes outside `OUT_DIR`. No separate preparation command
or global cache path is required.

Extend the existing prebuilt-loading path only with a verifiable fork-specific
identity envelope. It currently rejects unverifiable generic upstream archives
for good reason. Preserve ABI guards and fail closed on mismatches, corruption
or incomplete publication. Local cached builds must work offline.

Acceptance: after one build of a native configuration, equivalent debug,
release, check and test invocations execute zero native C++/shader compilation
commands. A native option change must select a distinct bundle. CI-produced
bundles can later reduce first-build cost using the same verification contract.

### 5. Improve shader incrementality and scheduling

As a separate build-time patch, write generated shader C++ only when contents
change, retaining valid depfiles and Ninja restat behavior. Add regeneration
tests for transitive includes, generator/configuration changes and missing
outputs. Do not remove kernels or GPU vendors to reduce build time.

Then use a coordinated job budget or Ninja pool for shader generation instead
of multiplying full per-process concurrency by Ninja's job count. Measure
elapsed time and peak memory; lower contention is useful only if the total
build improves. Keep this separate from C++ object caching and runtime driver
shader compilation, which are different processes.

## Validation budget and success criteria

Do not perform another full build for every exploratory edit. Start with pure
configuration/staging/cache-key tests and inspection of generated target graphs.
After each coherent implementation phase, perform the required normal builds
and quality gates, retaining their artifacts for the next phase.

Use ordinary debug/release builds; no diagnostic-only binaries. Preserve the
current Cargo target tree rather than running `cargo clean`. Measure:

| Scenario | Required behavior |
| --- | --- |
| Immediate repeat, same command | Zero native compile/generation commands |
| Rust application-only edit | Zero native compile/generation commands |
| C++-only patch edit | Rebuild changed native dependencies; preserve unrelated shaders/objects |
| Return to a previously built equivalent native configuration | Artifact/object cache reuse |
| Switch Rust debug/release with matching native inputs | Reuse native Release artifacts after phase 4 |
| Change backend, ABI/compiler flags, native profile, or patch identity | Correct invalidation; no unsafe reuse |
| Incomplete/corrupt cache or interrupted staging | Recover or fail closed; no stale linkage |

Record total wall time, native compilation/generation command counts, cache
hits/misses and peak memory. Separate cold from warm results. First-build
latency remains bounded by real compilation: the observed large Vulkan C++
translation unit alone took 150–209 seconds under those conditions. Avoid
promising seconds for a cold build until verified native bundles are available.

Faster linkers and Rust profile tuning can be assessed later. They do not
address the dominant repeated native compilation observed here. A genuine
native Debug build remains available for native debugging; making it the
ordinary Rust debug default could slow inference and is outside this plan.
