# Documentation map

Canonical index of project documentation. Start here when you need context.
Each row points at a real file and says what it is for.

| Document | Description | Status |
| --- | --- | --- |
| [`../AGENTS.md`](../AGENTS.md) | Operating manual for coding agents: ownership model, task modes, Rust standards, review protocol, binding-fork rules, quality gates. | Current |
| [`../README.md`](../README.md) | Project overview, feature highlights, prebuilt quickstart/distribution, native dependencies, source builds, development checks, and upstream pin. | Current |
| [`architecture.md`](architecture.md) | Current system architecture, crate boundaries, ownership split, resident workers, bounded working context, adaptive output allowance, extension boundaries, and deliberate limits. | Current |
| [`builtin-tools.md`](builtin-tools.md) | Built-in calculator/time and Linux process listing/lookup tools: arguments, live measurement semantics, visibility, resource bounds and cancellation. | Implemented |
| [`llm-api.md`](llm-api.md) | High-level chat/runtime API, typed tools, plugins/hooks/middleware, context-window policy and adaptive output allowance, cancellation, failure and lifecycle contracts. | Current |
| [`lfm3-limits.md`](lfm3-limits.md) | LFM3-1.7B published context limits, runtime defaults, memory costs, quality evidence, unverified companion behavior and proposed operating-range evaluation. | Research reference, 2026-11-05 |
| [`decisions/0001-persistent-runtime.md`](decisions/0001-persistent-runtime.md) | Accepted decision: singleton backend, thread-confined workers, bounded interaction pipeline and separate future permanent memory. | Accepted |
| [`decisions/0002-pq3-graph-transform.md`](decisions/0002-pq3-graph-transform.md) | Experimental signed-Hadamard graph convention, row/graph ABI separation and state/shift/support restrictions. | Accepted prototype contract; CPU/NVIDIA verified |
| [`bindings-fork.md`](bindings-fork.md) | Binding fork maintenance: ordered patches, guarded shims, Vulkan/logging ownership, fallible APIs, cache/device inspection ABI, and upstream update procedure. | Current |
| [`vulkan-cpu-optimization-plan.md`](vulkan-cpu-optimization-plan.md) | Source evidence, implementation and validation plan for default blocking Vulkan fence waits, synchronization/error contracts, and CPU/throughput tradeoffs in normal builds. | Implemented; GPU acceptance open, 2026-11-05 |
| [`turboquant-research.md`](turboquant-research.md) | Preserved TurboQuant/Vulkan research baseline: donor forks, source evidence, model memory calculations, proposed native patch strategy, risks and adoption criteria; links to the implemented preparation baseline. | Historical research / codec proposal, 2026-11-04 |
| [`turboquant-implementation-plan.md`](turboquant-implementation-plan.md) | CPU-first PQ3/TBQ3 delivery plan and concrete next-stage design: signed-transform composition, patches 0009/0010, state/shift restrictions, strict Vulkan placement, runtime wiring and validation gates. | Experimental CPU/Vulkan/runtime measured; no promotion, 2026-11-04 |
| [`pq3-inference-evaluation.md`](pq3-inference-evaluation.md) | Implemented native/runtime PQ3, complete quality rejection, paired release memory/speed, reverse-order confirmation, placement evidence and remaining limits. | Measured experimental rejection; no promotion, 2026-11-04 |
| [`pq3-reference-contract.md`](pq3-reference-contract.md) | Audited donor identities, frozen PQ3 row/graph ABI, numerical contracts, guarded APIs, CPU/Vulkan support and inference restrictions. | Experimental CPU/Vulkan inference, 2026-11-04 |
| [`cache-quality-evaluation.md`](cache-quality-evaluation.md) | Checksummed authored fixtures, fixed-clock tools, retrieval/numerical jobs, artifact capture and preregistered candidate gates. | NVIDIA references and failed PQ3 candidate recorded, 2026-11-04 |
| [`benchmarks/pq3-inference-2026-11-04.json`](benchmarks/pq3-inference-2026-11-04.json) | Release paired 4K/32K and scalar results, raw trial spreads/allocation snapshots, reverse-order regression, placement, native tests and artifact identities. | Measured NVIDIA evidence; no promotion, 2026-11-04 |
| [`benchmarks/pq3-quality-2026-11-04.json`](benchmarks/pq3-quality-2026-11-04.json) | Complete six-profile candidate quality: 102 tasks, 119 numerical records, native/source/model/executable/log identities and unchanged failed PQ3 verdict. | Measured NVIDIA evidence; no promotion, 2026-11-04 |
| [`kv-cache-preparation-plan.md`](kv-cache-preparation-plan.md) | Ordered prerequisites before TurboQuant: trustworthy fixtures, fallible diagnostics, typed cache and microbatch configuration, compact SWA and Q8/Q4 validation, benchmarks, defaults, and memory limits. | Implemented for bounded experiments; rollout gates open, 2026-11-04 |
| [`kv-cache-baseline.md`](kv-cache-baseline.md) | Implemented cache APIs/CLI, diagnostics and accounting contracts, required-model/GPU tests, repeatable benchmarks, placement audits, measured results and coverage limits. | First-device baseline validated, 2026-11-04 |
| [`benchmarks/kv-cache-2026-11-04.json`](benchmarks/kv-cache-2026-11-04.json) | Sanitized release benchmark spreads, native memory, numerical/tool quality and placement results with artifact checksums for the initial NVIDIA baseline. | Measured evidence, 2026-11-04 |
| [`benchmarks/cache-quality-2026-11-04.json`](benchmarks/cache-quality-2026-11-04.json) | Feature-enabled Vulkan quality references: 68 task cases, 85 numerical checkpoints, model/source/patch/executable identities and unchanged experimental gates. | Measured NVIDIA evidence; no PQ3 inference, 2026-11-04 |
| [`../crates/llama_cpp/README.md`](../crates/llama_cpp/README.md) | Binding crate overview, feature flags, usage example, and fallible/lifetime-bound API migration. | Current |
| [`../crates/llama_cpp/TENSOR_TRANSACTIONS_SAFETY.md`](../crates/llama_cpp/TENSOR_TRANSACTIONS_SAFETY.md) | Safety contract for `TensorTransactions` and decode-lifecycle callbacks. Read before touching tensor capture or FFI callbacks. | Current |
| [`web-ui.md`](web-ui.md) | Default web plugin, typed FSD frontend/build, keyboard/Markdown behavior, generation limits, precise measurement scopes/providers and validation. | Implemented, 2026-11-04 |
| [`web-api.yaml`](web-api.yaml) | Versioned HTTP/SSE API: requests, run/snapshot/performance/resource schemas, cancellation, errors and bounded recovery. | Implemented v1 |
| [`decisions/0003-runtime-services-and-web.md`](decisions/0003-runtime-services-and-web.md) | One executable, compiled transport plugins, runtime-ready lifecycle and shared application ownership. | Accepted and implemented |
| [`decisions/0004-typed-frontend-and-measurements.md`](decisions/0004-typed-frontend-and-measurements.md) | Oxc/React/FSD build boundary, safe worker Markdown, honest native/process metrics and generation budgets. | Accepted and implemented |
| [`decisions/0005-model-scoped-vram.md`](decisions/0005-model-scoped-vram.md) | Allocation-backed primary-model GPU scope, PCI-matched residency counters and complete-coverage totals. | Accepted and implemented |
| [`../scripts/check-web.sh`](../scripts/check-web.sh) | Frontend checks/build followed by workspace Rust quality gates; suitable for local or CI use after npm ci. | Implemented |
| [`../scripts/profile-assistant.sh`](../scripts/profile-assistant.sh) | Read-only Linux CPU diagnostic: automatic daemon discovery/activity trigger, per-thread `/proc` CPU/scheduling counters, memory/I/O/pressure, optional perf stacks and NVIDIA device samples; prints a pasteable report. | Implemented |
| [`../packaging/systemd/assistant.service`](../packaging/systemd/assistant.service) | Example foreground user service; replace executable/model paths before installation. | Example |

## Planned documents

Add these to the table above when created; do not leave dangling links.

- `docs/roadmap.md` — milestones from baseline restoration through daemon
  transports.
- `docs/security.md` — threat model, IPC authorization, tool execution policy,
  resource limits, and review checklist.
- `docs/decisions/NNNN-title.md` — architecture decision records for choices
  that are expensive to reverse.

## Maintenance rule

Any change that adds, renames, removes, or materially changes a document must
update this table in the same change. A document that is not listed here is
effectively invisible to agents.
