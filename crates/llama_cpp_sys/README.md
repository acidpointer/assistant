# llama_cpp_sys

Local raw FFI bindings and native build for llama.cpp. Prefer the safe
[`llama_cpp`](../llama_cpp/) API in application code.

Ported from [eugenehp/llama-cpp-rs](https://github.com/eugenehp/llama-cpp-rs)
revision `1d62841020af8a309ab749b64ac3688919f383df` (0.9.0).
The [llama.cpp submodule](llama.cpp/) is pinned to upstream
`a34de8618b25dfc32ee43749db47d767a9f0a25d` (`b12015`), including LFM3.
See the [workspace README](../../README.md) for build prerequisites and commands.

`build.rs` copies the pinned source into the build directory, applies the three
ported patches in `patches/`, generates Rust FFI with bindgen, builds native
libraries with CMake, and compiles the C++ shims. The patches provide the
speculative-state and decode lifecycle APIs used by the safe crate. Upstream's
chat capabilities JSON API is handled directly in `chat_shim`.

The sys crate has no default features. GPU/backend flags and `mtmd` select the
native components; `dynamic-link` selects shared libraries. Source builds link
statically otherwise. Cargo's job budget (`-j N`) or an explicit
`CMAKE_BUILD_PARALLEL_LEVEL` controls native build parallelism.

The build script also retains upstream's `LLAMA_*` CMake configuration support
through environment variables, `llama-cpp.toml`, and package metadata. Required
patches prevent unverified prebuilt libraries from being mixed with local
headers: `prebuilt` falls back to a source build, and `LLAMA_PREBUILT_DIR` is
rejected while those patches are active.

The Rust/build/shim port retains its MIT and Apache-2.0 license texts here.
The native source retains its own [MIT license](llama.cpp/LICENSE).
