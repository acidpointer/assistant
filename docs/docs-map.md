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
| [`bindings-fork.md`](bindings-fork.md) | Maintenance guide for the `llama_cpp` / `llama_cpp_sys` fork: build pipeline, ordered patches, shims, Vulkan shutdown and cache ownership, concurrent native logging, retained backend/handle ownership, API migration notes and guarded sampler constructors (2026-11-03), and upstream update procedure. | Current |
| [`turboquant-research.md`](turboquant-research.md) | TurboQuant and Vulkan KV-cache research: current Rust/native capabilities, inspected forks and source evidence, LFM3 memory calculations, patch strategy, compatibility risks, and validation criteria. | Research proposal, 2026-11-04 |
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
