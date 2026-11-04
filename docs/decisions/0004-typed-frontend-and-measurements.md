# 0004: Typed frontend and measured chat performance

Status: accepted and implemented, 2026-11-04.

## Decision

Replace the handwritten single HTML asset with a React/TypeScript application
built by Vite 8, Rolldown and Oxc. Keep one deployed `assistant` executable and
the compiled `assistant-web` plugin. Node is a development/build prerequisite,
not a runtime dependency. Cargo embeds a verified build without starting npm or
accessing the network. Missing, stale or corrupt assets fail the build.

Use Oxlint (including native type-aware rules), Oxfmt, canonical `tsc` and
Steiger. Pin dependency versions and the npm lockfile. React's official Vite
plugin uses Oxc; neither Babel nor React Compiler is enabled. Explicit targets
are Chromium 100, Firefox 102 and Safari 15.4-era WebKit, rather than Vite's
newer defaults. WebKitGTK 2.36 is a compatibility target requiring an actual
deployment-device check; a current Playwright WebKit pass is not certification.

Apply FSD's pages-first rule. The chat page owns its presently unique composer,
recall, stop and performance inspection logic. Shared chat/telemetry entities
expose public APIs; their one explicit `@x/chat` relationship validates nested
measurements. App owns connection lifecycle and global design tokens/styles.
Shared contains generic HTTP, subscriptions, buttons and safe Markdown. Introduce
features/widgets only when meaningful reuse appears. Steiger checks downward
imports, public APIs and insignificant slices.

Retain POST commands and multiplexed SSE. A connection neither owns inference
nor automatically retries a command. Reconnection replaces state atomically.
This separates transport from state/rendering without introducing another wire
protocol before a measured need exists.

## Rendering and resources

Parse Markdown in a dedicated worker with one in-flight job and one latest
pending version per mounted message. Normalize GFM to a closed node vocabulary;
never interpret raw HTML or model-selected attributes. Explicit HTTP(S)/mailto
links are checked and images remain alt text. Syntax grammars load on demand
only for completed messages. Bound input, nodes, depth and worker execution.
Completed block signatures preserve DOM when later references revise a message.

The normal React render path never parses Markdown or loads syntax grammars.
Keyed subscriptions isolate a changing message from unrelated transcript rows.
The display transcript remains bounded and volatile. Fonts and dependency
licenses are embedded local assets; there are no CDNs or persistent browser data.

Measure sampled non-EOG tokens and active owner-thread prompt/decode time.
Do not substitute visible text, SSE chunks or end-to-end run duration. Preserve
partial measurement provenance on cancellation; unreported native work is not
invented. CPU is all-thread daemon CPU time over a monotonic phase window,
with 100% representing one core. Linux RAM uses smaps_rollup RSS/PSS. Dedicated VRAM uses process residency on allocation-backed primary-model GPU
devices, via PCI-matched NVML or Linux DRM resident counters. CPU-only scopes are
zero; unknown placement or incomplete counter coverage is unavailable. See
[ADR 0005](0005-model-scoped-vram.md). Observed memory maxima include resident/shared
state and are neither exclusive per-message costs nor continuous allocation peaks.

One sampler serves every transport/tab: 250 ms while running, one second with
visible subscribers, no idle sampling without demand. Only bounded aggregates
are retained. Other platforms/drivers can extend this transport-neutral provider
module without changing the frontend or HTTP event contract.

## Generation budgets

Daemon defaults become 8192 context tokens, 4096 output tokens per model turn,
8192 total run tokens and 600 seconds. These are ceilings, not promised response
lengths. Tools, reasoning and protocol consume sampled tokens. The runner clamps
each requested turn to remaining run tokens; the native chat preparation path
can reduce the reserve to actual space after complete old exchanges are evicted.
Protected current input/instructions still fail when they cannot fit. Report
output, run and context cutoff reasons; never silently start another interaction.

## Consequences

Frontend builds precede full Rust builds and CI. Binding-only development remains
independent of Node. Generated dist/assets stay ignored; an immutable copy in
Cargo OUT_DIR prevents later frontend rebuilds from deleting compiler inputs.
The initial JS/CSS gzip budget is 120 KiB; workers, fonts and grammars load
separately. Minimum-engine and reference-hardware latency certification remain
explicit deployment gates, not claims inferred from current-engine tests.
