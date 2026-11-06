# Universal memory and a small assistant loop

Accepted implementation direction, 2026-11-06. This supersedes the domain
extraction, fixed personality traits/moods, and authored-corpus release gates in
`memory-implementation-spec.md` and `memory-integration-plan.md`. Existing typed
claims remain readable; they are not a prerequisite for remembering anything.

## Contract

Keep literal user/assistant/tool experiences with host attribution. Connect
conversation turns automatically. The assistant may save freeform notes and
labeled links to existing sources. A note is an assistant interpretation, never
a newly fabricated user assertion. No list of supported subjects, relationships,
personas, interests, or preferences defines what the assistant can remember.

Use three existing boundaries: `MemoryAccess` for durable search/notes/graph,
`OwnerHandle` for local client commands, and the typed `Tool` interface
for model actions. Add run-scoped memory authority to host-created tool context;
model arguments contain references, never capabilities. Semantic search finds
entry points; bounded bidirectional graph traversal retrieves connected sources.
Adjacency pages include continuation cursors so dense nodes remain traversable
without enlarging model context. Each result preserves its original role and
link direction. Links are navigation,
not proof of entailment. Read-time suppression also hides dependent notes.

Store personality as editable freeform instructions, independently of chat
history. They are pinned in foreground and initiative prompts and survive
context rotation and restart. A small optional initiative interval enables a
background decision with the same resident model, personality, and retrieved
memory. The model can remain silent. Scheduling and delivery are durable and
bounded; foreground work takes priority. Outreach is an assistant message with
its own source, not a stored fictitious user turn. Clients can read and dismiss
the same inbox through the core API; WebUI is the first adapter.

## Why

[Generative Agents](https://arxiv.org/abs/2304.03442) supports an experience stream,
retrieval and an action loop. [Zep](https://arxiv.org/abs/2501.13956) supports
retaining temporal/source context while navigating related memory. These are
architectural references, not evidence that their model results transfer to our
local model. The failed family/occupation development experiment is retained as
evidence against making specialized extraction a product dependency.

## Verification

Test transactions, authority, suppression, bounds, restart, profile changes and
initiative scheduling with deterministic model doubles. Test real journeys:
interleaved topics, arbitrary preferences, correction, context rotation, graph
follow-up, personality and assistant-initiated dialogue. A few local-model runs
exercise the whole path; default tests do not require weights. Do not define
success by exact JSON facts across a growing set of isolated synthetic sentences.
A model failure still requires fixing or documenting the approach; another model
is not a prerequisite.

Native compilation is a separate concern: preserve unchanged staged-source
mtimes; cache verified immutable native libraries across equivalent Cargo build
configurations; keep mutable CMake trees and Rust-profile shims private. Cargo's
[build-script change detection](https://doc.rust-lang.org/cargo/reference/build-scripts.html#change-detection)
does not itself provide native artifact reuse. No upstream pin or inference
numerics change is part of this work.


The owner requested direct local WebUI access. The HTTP adapter issues its core
handle once, binds memory access to loopback, and connects the browser without
pairing, credentials or expiring sessions. Other adapters choose their own access
policy; memory and initiative remain independent of HTTP.
