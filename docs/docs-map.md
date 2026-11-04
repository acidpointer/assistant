# Documentation map

Canonical index of project documentation. Start here when you need context.
Each row points at a real file and says what it is for.

| Document | Description | Status |
| --- | --- | --- |
| [`../AGENTS.md`](../AGENTS.md) | Operating manual for coding agents: ownership model, task modes, Rust standards, review protocol, binding-fork rules, quality gates. | Current |
| [`../README.md`](../README.md) | Workspace entry point: crates, build prerequisites, run commands, upstream revisions, and test notes. | Current |
| [`architecture.md`](architecture.md) | Current system architecture, crate boundaries, ownership split, resident workers, bounded working context, extension boundaries, and deliberate limits. | Current |
| [`llm-api.md`](llm-api.md) | High-level chat/runtime API, typed tools, plugins/hooks/middleware, context-window policy, cancellation, failure and lifecycle contracts. | Current |
| [`decisions/0001-persistent-runtime.md`](decisions/0001-persistent-runtime.md) | Accepted decision: singleton backend, thread-confined workers, bounded interaction pipeline and separate future permanent memory. | Accepted |
| [`decisions/0002-pq3-graph-transform.md`](decisions/0002-pq3-graph-transform.md) | Experimental signed-Hadamard graph convention, row/graph ABI separation and state/shift/support restrictions. | Accepted prototype contract; CPU/NVIDIA verified |
| [`bindings-fork.md`](bindings-fork.md) | Binding fork maintenance: ordered patches, guarded shims, Vulkan/logging ownership, fallible APIs, cache policy/inspection ABI, and upstream update procedure. | Current |
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
