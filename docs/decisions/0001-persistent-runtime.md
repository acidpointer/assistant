# 0001: Persistent runtime with a bounded interaction pipeline

Status: accepted and implemented, 2026-11-03.

## Context

The assistant must remain available between interactions, support many host tools
and future voice/memory services, and keep a bounded native context. A single
endless generation or an ever-growing transcript would consume resources without
providing durable memory. Native contexts cannot cross Rust threads safely.

## Decision

Keep the explicit `ModelEngine` singleton and engine/model/context lifetime chain.
Load each configured model inside a dedicated worker with a bounded command
channel. Advance native work in bounded cooperative steps and block while idle.

Expose `Chat::generate` plus a per-operation request builder. One bounded runner
handles plain responses and tool continuations. Plugins configure typed tools,
services and named hooks; small middleware traits wrap run/model/tool boundaries.
Use explicit stages and a stable topological sort rather than an ECS or a generic
workflow graph. Use Tokio for asynchronous services and I/O.

Treat recent messages and KV caches as disposable working state. Evict oldest
complete exchanges when budgets require it; preserve the current tool protocol.
Permanent semantic/graph memory is a later service, integrated through retrieval
and terminal-observer hooks. Summaries are optional application policy.

## Consequences

The simple path remains `chat.generate("...").await`. Advanced integrations can
select tools, inject literal context, stream events and observe completed/failed
interactions without owning native handles. Tests can script model turns without
model files. Native owners are destroyed on their own thread.

Cooperative cancellation cannot interrupt one native call or undo tool effects.
Explicit shutdown must be awaited. Greedy sampling, fixed resident models and a
small bounded context set are sufficient for the foundation; resource-aware
loading, durable memory, voice and transports remain separate changes.
