# 0003: Runtime services, shared application state and the default web plugin

Status: accepted and implemented, 2026-11-04.

## Context

The product is one resident `assistant` executable. WebUI is its default local
interface, and future transports need the same admission, run ownership,
cancellation and conversation updates. Existing dependency services start before
models and cannot receive the ready primary chat. HTTP must not become a core
dependency or control model startup from its request pipeline.

## Decision

- Keep `assistant` as the core library. `assistant-daemon` produces the single
  product binary named `assistant`; the workspace defaults to that package.
- `assistant-web` implements a compiled Rust `WebPlugin`, registered by default.
  `--no-web` skips registration: no web bind, requests, streams or service tasks.
- `--prompt` and `--interactive` require `--no-web`. One-shot generation is a
  diagnostic/testing operation, not a mode combined with a running web server.
- Add runtime service factories with preparation, activation, quiescence,
  termination observation and explicit shutdown. Prepare before models; activate
  after the shared application exists. Keep these resources outside `Pipeline`.
- The core application task serializes product admission and owns accepted runs.
  `AssistantHandle` is the transport boundary; it grants no shutdown authority.
- Maintain bounded volatile display state independently of native working context.
  Subscriptions start with an atomic snapshot. Lagging subscribers resnapshot;
  browser connection loss does not cancel inference.
- Registration performs no I/O. Startup rolls back on failure. Required service
  termination shuts down the daemon. SIGINT and SIGTERM request awaited cleanup.

## Consequences

Public library imports and native ownership stay in the existing core package.
Package-qualified executable commands and binary integration tests move to
`assistant-daemon`. Low-level `Chat` remains available for library clients and
explicit CLI diagnostics; it does not publish application snapshots. Product
transports must use `AssistantHandle` for shared state to remain coherent.

Plugins are trusted Rust code linked into the executable. This establishes no
dynamic loading, hot reload, process sandbox or durable operation recovery.
Native calls remain non-preemptible. A finite systemd stop allowance can still
expire during a long native call.

See [the web contract](../web-ui.md), [architecture](../architecture.md) and
[runtime API](../llm-api.md).
