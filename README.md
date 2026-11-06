# assistant

A local AI assistant with streaming chat, tools, and an extensible Rust core.
The model stays ready between messages. The runtime and the entire web UI ship
in one executable.

[Quickstart](#quickstart) · [Build from source](#build-from-source) ·
[Development](#development) · [Documentation](docs/docs-map.md)

## Why assistant?

- **Run your own model.** Local GGUF inference through a maintained llama.cpp
  binding. No cloud account required.
- **Deploy one executable.** HTML, JavaScript, CSS, fonts, and workers are embedded.
  Add your model file and the system runtime libraries.
- **Stay ready between messages.** Model weights remain loaded while the daemon
  waits for the next request.
- **Chat comfortably.** Streaming Markdown, syntax highlighting, keyboard shortcuts,
  prompt recall, and a responsive UI.
- **See what it costs.** Native token throughput and VRAM on the model's GPUs,
  with CPU, RAM, and per-message measurements in the details.
- **Build on the core.** Typed tools, plugins, hooks, middleware, and a shared
  application API. HTTP/WebUI is the first transport plugin.

Built-in [tools](docs/builtin-tools.md) cover arithmetic, current time, and Linux
process inspection. Ask which processes use the most RAM, or “tell me about the
htop process.”

> **Early development.** Today: one ongoing chat, local inference, and tools.
> Chat is temporary by default. Linux supports opt-in durable semantic memory,
> personality and initiative; additional transports remain future work.

## Quickstart

With a prebuilt `assistant` for your OS, CPU architecture, and GPU backend, plus a
supported local GGUF model:

```sh
chmod +x ./assistant
./assistant --model /absolute/path/to/model.gguf
```

Open **http://127.0.0.1:8080**. The daemon runs in the foreground; Ctrl+C or SIGTERM
shuts it down cleanly. The model stays external; the frontend is already inside
the executable. No Node.js, npm, Rust, or frontend directory is needed to run it.

### Runtime libraries and GPU drivers

The default build uses Vulkan. Linux needs the Vulkan loader, a working GPU Vulkan
driver, and the executable's system libraries (usually glibc, libstdc++, and
libgcc; OpenMP if enabled when building). A prebuilt binary must be compatible
with the destination's system-library versions. Inspect a trusted binary with:

```sh
ldd ./assistant
```

Follow your distribution's GPU driver instructions. On Arch Linux, userspace
packages include `vulkan-radeon` for AMD, `vulkan-intel` for Intel, or `nvidia-utils`
with the matching NVIDIA kernel driver; see the
[Arch Vulkan guide](https://wiki.archlinux.org/title/Vulkan#Installation).
GPU placement may span multiple visible GPUs. `--cpu` disables GPU inference;
a Vulkan-linked binary still needs its loader.

### Run it your way

```sh
# Choose a listen address and port.
./assistant --model /absolute/path/to/model.gguf --host 127.0.0.1 --port 9000

# CPU inference with the web UI.
./assistant --model /absolute/path/to/model.gguf --cpu

# Headless resident service.
./assistant --model /absolute/path/to/model.gguf --no-web
```

### Enable memory and WebUI

Pass both memory paths directly; no configuration file is needed:

```sh
./assistant --model /absolute/path/to/model.gguf \
  --memory-dir "$HOME/.local/share/assistant/memory" \
  --embedding-model /absolute/path/to/multilingual-e5-small-q8_0.gguf \
  --port 3333
```

Open **http://127.0.0.1:3333** to use the assistant directly. The private memory
directory is created automatically. See [using memory](docs/assistant-memory.md).

### One-shot and terminal diagnostics

```sh
# Print an answer and exit.
./assistant --model /absolute/path/to/model.gguf --no-web --prompt 'Hello'

# Interactive terminal session.
./assistant --model /absolute/path/to/model.gguf --no-web --interactive
```

`--prompt` and `--interactive` require `--no-web`: they are testing modes, while
the web server is a resident service. Explicit `--host`/`--port` conflict with
`--no-web`. Run `./assistant --help` for generation, context, and cache settings.

### Run as a systemd user service

Adapt the executable and model paths in the example
[user service](packaging/systemd/assistant.service), install it as
`~/.config/systemd/user/assistant.service`, then run:

```sh
systemctl --user daemon-reload
systemctl --user enable --now assistant.service
journalctl --user -u assistant.service -f
```

## Build from source

The commands below cover Linux with Vulkan, the default backend.

### 1. Install prerequisites

| Component | Requirement |
| --- | --- |
| Rust | Rust/Cargo 1.88+; current stable recommended. Install with [rustup](https://rustup.rs/). |
| Frontend | Node.js 22.x from 22.18.0, or Node.js 24.12.0+; npm. |
| Native toolchain | Git, a C compiler and C++17 compiler, CMake (3.24+ recommended), libclang for bindgen; Ninja recommended. |
| Vulkan | Vulkan headers and loader development files, `glslc`, SPIR-V headers with the `SPIRV-Headers` CMake package, and a GPU driver to run inference. |
| Workspace checks | rustfmt, Clippy, Bash, and OpenMP support for the binding crate's default features. Python 3 for smoke/evaluation scripts. |

#### Arch Linux

```sh
sudo pacman -Syu --needed base-devel git cmake ninja clang openmp \
  vulkan-headers vulkan-icd-loader spirv-headers shaderc
```

Here, [`shaderc`](https://archlinux.org/packages/extra/x86_64/shaderc/files/)
provides `glslc`; [`spirv-headers`](https://archlinux.org/packages/extra/any/spirv-headers/)
is a separate requirement.

#### Debian 13

```sh
sudo apt update
sudo apt install build-essential git cmake ninja-build clang libclang-dev \
  libomp-dev libvulkan-dev glslc spirv-headers
```

Debian's [`spirv-headers`](https://packages.debian.org/trixie/all/spirv-headers/filelist)
includes the required CMake configuration.

On other distributions, install equivalent development packages. A Vulkan driver
is separate from the headers and loader. Install Rust and a supported Node.js
version separately; distribution packages may provide an older Node.js. If bindgen
cannot locate libclang, set `LIBCLANG_PATH` to its library directory.

### 2. Initialize the repository

Replace `REPOSITORY_URL` with this repository's clone URL:

```sh
git clone --recurse-submodules REPOSITORY_URL assistant
cd assistant
```

For an existing checkout, initialize or synchronize the pinned native source:

```sh
git submodule sync --recursive
git submodule update --init --recursive
```

Keep the recorded llama.cpp revision. Cargo builds a patched copy under
`target/`; it does not modify or fetch the native submodule.

### 3. Build the frontend, then the executable

Run from the workspace root:

```sh
npm --prefix crates/assistant-web/frontend ci
npm --prefix crates/assistant-web/frontend run build
cargo build --locked --release -p assistant-daemon
```

The output is **`target/release/assistant`**. Run it directly:

```sh
./target/release/assistant --model /absolute/path/to/model.gguf
```

`npm run build` type-checks and bundles the frontend into `frontend/dist/`, then
generates compressed assets and a checksum manifest. Cargo verifies and embeds
those assets. Missing, stale or corrupt assets stop the build with instructions;
Cargo never invokes npm automatically. Assets are required even when you intend
to run with `--no-web`.

Re-run `npm ci` when dependencies change, and the frontend build whenever its
sources change. Rust-only changes need only Cargo. For distribution, copy the
executable; `dist/` and `node_modules/` stay on the build machine. Supply the model
separately and retain the relevant project/native license notices. The UI embeds
its frontend dependency notices. System/GPU runtime libraries remain external.

### Other backends

Other backends replace the default Vulkan feature and require their own native
toolkits. The frontend build above is still required:

```sh
# CPU-only build: no Vulkan build dependencies.
cargo build --locked --release -p assistant-daemon --no-default-features

# NVIDIA CUDA: requires the CUDA toolkit.
cargo build --locked --release -p assistant-daemon --no-default-features --features cuda

# macOS Metal: requires Apple's command-line developer tools/SDK and libclang.
cargo build --locked --release -p assistant-daemon --no-default-features --features metal
```

## Development

After preparing the frontend, `cargo run -- --model /absolute/path/to/model.gguf`
starts the daemon. In another terminal, enable frontend hot reload:

```sh
npm --prefix crates/assistant-web/frontend run dev
```

Open the URL printed by Vite. Its development server proxies `/api` to the
running daemon at `127.0.0.1:8080`. Rebuild the production frontend before the next
Cargo build after frontend changes.

After `npm ci`, run the frontend checks/build and workspace Rust gates together:

```sh
bash scripts/check-web.sh
```

Browser integration tests are separate:

```sh
cd crates/assistant-web/frontend
npx playwright install chromium firefox webkit
npm run test:browser
```

Default tests do not download or require model weights; model-dependent cases
skip when weights are absent. See the [web guide](docs/web-ui.md#typed-frontend-and-verification)
for real-daemon smoke tests and the [cache guide](docs/kv-cache-baseline.md#test-categories-and-reproduction)
for required-model/GPU evaluations.

## Inside the project

| Crate | Responsibility |
| --- | --- |
| `assistant-daemon` | CLI and process lifecycle; produces the `assistant` executable |
| `assistant` | Resident runtime, chat, tools, sessions and plugin APIs |
| `assistant-web` | HTTP/WebUI plugin and embedded frontend |
| `llama_cpp` / `llama_cpp_sys` | Safe Rust API, FFI and maintained native fork |

| Explore | Start here |
| --- | --- |
| Understand the runtime and extension boundaries | [Architecture](docs/architecture.md) |
| Add tools, hooks, or plugins | [Assistant API](docs/llm-api.md) |
| Build a client or work on the UI | [Web guide](docs/web-ui.md) · [HTTP API](docs/web-api.yaml) |
| Find the rest of the documentation | [Documentation index](docs/docs-map.md) |

## Acknowledgments

Bindings derive from
[eugenehp/llama-cpp-rs](https://github.com/eugenehp/llama-cpp-rs/commit/1d62841020af8a309ab749b64ac3688919f383df)
(version 0.9.0, MIT/Apache-2.0). llama.cpp is pinned to
[`a34de8618b25dfc32ee43749db47d767a9f0a25d`](https://github.com/ggml-org/llama.cpp/commit/a34de8618b25dfc32ee43749db47d767a9f0a25d)
(`b12015`). Native linking is static by default. See the
[binding maintenance guide](docs/bindings-fork.md) for patches and upstream updates.

The experimental `turboquant` feature remains opt-in and
[has not passed quality gates](docs/pq3-inference-evaluation.md).
