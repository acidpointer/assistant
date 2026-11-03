# AGENTS.md

Operating manual for coding agents working in this repository. Read it before
changing code. It is deliberately specific: generic agent advice is not enough
for a project whose native dependency is a maintained fork.

## 1. What this project is

`assistant` is an on-device, long-running user daemon. The target shape is a
small model kept resident in a daemon that serves local clients over HTTP,
D-Bus, Unix sockets, and similar transports, with streaming chat, tools, and
session state.

The workspace is in an early stage. The llama.cpp binding fork is usable; the
application and high-level LLM API are still being designed and are not
trustworthy as a specification. Verify the code instead of trusting summaries,
including this file. `docs/architecture.md` records the current baseline.

## 2. Ownership model

There are two kinds of work here, with different rules.

### 2.1 Human-owned: application logic

The human owner writes the application and high-level assistant logic. Unless
explicitly instructed otherwise, agents **must not implement or rewrite
business logic** in:

- `crates/assistant/` (daemon, transports, sessions, tools, prompt policy,
  orchestration, high-level LLM API)
- future application crates

For these areas, act as a senior engineer and reviewer:

- establish what the code actually does
- propose designs, APIs, invariants, tradeoffs, and migration steps
- review for correctness, security, performance, and maintainability
- point at concrete files/lines and explain failure modes
- write mechanical fixes or tests only when the human asks for code

Do not "helpfully" implement a feature the human is designing. Offer a plan, a
patch sketch, or a review instead. If the human explicitly asks for an
implementation, keep it minimal and call out that it touches human-owned code.

**Proactive review is welcome; proactive edits are not.**

### 2.2 Agent-owned: the llama.cpp binding fork

Agents own maintenance of:

- `crates/llama_cpp/` — safe Rust API over llama.cpp
- `crates/llama_cpp_sys/` — FFI, build script, C++ shims, build-time patches
- the pinned `crates/llama_cpp_sys/llama.cpp` submodule relationship
- the build and test glue that keeps the fork healthy

In these areas agents may implement, fix, refactor, and update code. The human
still reviews every change. Keep changes narrow, explain upstream compatibility
consequences, and never weaken a safety check to make a test pass.

### 2.3 Shared: documentation, tests, repository hygiene

Agents may update `docs/`, `README.md`, and repository tooling. Keep them
accurate. Update `docs/docs-map.md` whenever a document is added, renamed, or
removed.

## 3. Task modes

Classify each task before acting.

### Research / review mode (default)

Output findings, not edits. Cover:

- current behaviour and the intended invariant
- correctness and soundness issues
- security and resource-exhaustion risks
- API and design problems
- performance risks
- the smallest credible fix or next step

Separate must-fix findings from suggestions. Do not bury important findings in
a wall of speculation.

### Binding-maintenance mode

Implement in `llama_cpp` / `llama_cpp_sys`, following §7. Treat upstream
compatibility, FFI safety, and error containment as first-class requirements.

### Requested implementation mode

Only when the human explicitly asks for code. Restate the intended contract,
keep the diff focused, and avoid inventing policy that belongs to the human.

## 4. Working protocol

Before changing code:

1. Read `docs/docs-map.md` and the documents relevant to the task.
2. Inspect the actual code; do not rely on README claims or stale summaries.
3. Identify the ownership boundary from §2.
4. State the current behaviour and the invariant the change must preserve.
5. For application logic, stop at analysis/plan unless asked to implement.

While working:

- Prefer small, reviewable changes over broad rewrites.
- Preserve existing user work. If a file looks mid-edit, ask before replacing it.
- Do not add dependencies without a concrete reason; prefer the existing stack.
- Do not change a public binding API without documenting the impact.
- Do not edit generated files or files inside the `llama.cpp` submodule.
- Never commit `*.gguf`, `target/`, secrets, or machine-local model paths.

After working:

- run the relevant quality gates (§9)
- update docs and invariants when behaviour changes
- report what changed, what was verified, and what remains uncertain

## 5. Rust standards

### 5.1 Error handling

- Production code **must not** use `.unwrap()` or `.expect()`.
- Propagate fallible operations with `?`. Add context at the boundary where the
  caller can act on it.
- Use `thiserror` typed errors in libraries. Use `anyhow` only at binary/CLI
  edges and avoid leaking `anyhow` into public library APIs.
- Do not use `panic!`, `unreachable!`, `todo!`, or `unimplemented!` in
  production paths. Encode impossible states in the type system or return a
  descriptive error.
- Do not silently swallow errors, fabricate fallback values, or log-and-continue
  when the caller needs to know.
- Existing violations are debt, not a license. Do not add new ones; fix
  in-scope occurrences when you touch that code, and do not start a repo-wide
  cleanup unasked.
- Tests may use `.expect("why this should hold")` to fail with context. Avoid
  bare `.unwrap()` even in tests where a message would help.

### 5.2 API design

- Make invalid states unrepresentable where practical: newtypes, enums,
  `NonZero*`, private fields with validating constructors.
- Avoid boolean parameters; use enums or small config structs.
- Prefer owned types at boundaries and borrows internally.
- Implement `Debug` for public types and `#[must_use]` where ignoring a result
  is a bug.
- Mark public enums `#[non_exhaustive]` when future variants are likely.
- Document `# Errors`, `# Panics`, and `# Safety` sections on public items.
- `unsafe` is only for FFI or a documented invariant. Every block needs a
  `SAFETY:` comment explaining why it is sound. `Send`/`Sync` impls need the
  same treatment plus a test or compile-time assertion.

### 5.3 Documentation and comments

- Every module starts with `//!` technical documentation: purpose, ownership,
  invariants, threading model, error model, and non-obvious lifecycle rules.
- Public APIs and important private members have `///` docs. Explain contracts,
  not just names.
- Complex in-code decisions and non-obvious steps get clear comments that
  explain **why**, including upstream commit/PR or issue links where relevant.
- Comments must stay true; update them with the code.
- Do not add noise comments that merely restate the next line.
- Prefer links between rustdoc items so the API forms a navigable whole.

### 5.4 Style

- Follow `cargo fmt` and workspace lints (`clippy::pedantic` is enabled).
- Prefer explicit imports over globs in new code.
- Keep functions focused; extract a helper when a block has a distinct
  invariant or error policy.
- Avoid premature abstraction; this project is early-stage.
- No commented-out code. Delete it or turn it into a tracked issue/doc.

## 6. Documentation system

All project documentation lives in `docs/` except:

- `README.md` — workspace entry point
- `AGENTS.md` — this file
- crate-local READMEs

`docs/docs-map.md` is the canonical index: a table mapping each document to what
it covers and its status. An agent looking for context **must start there**.

Rules:

- Add every new document to `docs/docs-map.md` in the same change.
- Use lowercase-kebab-case filenames.
- Keep docs factual and current. Mark stale sections explicitly instead of
  leaving contradictory claims.
- Record significant architectural decisions as short ADRs in
  `docs/decisions/NNNN-title.md` and add them to the map.
- Use relative links so the tree works locally and on a forge.

## 7. Binding fork maintenance

Read `docs/bindings-fork.md` before touching native code. Core rules:

- `crates/llama_cpp_sys/llama.cpp` is a pinned upstream submodule. **Do not edit
  files inside it.** Local changes belong in:
  - `crates/llama_cpp_sys/patches/*.patch` — applied to a build-directory copy
  - `crates/llama_cpp_sys/{chat,common,ext,mtp}_shim/`
  - `crates/llama_cpp_sys/shim_support/`
  - `crates/llama_cpp_sys/build.rs`
  - `crates/llama_cpp/src/`
- Keep the patch set minimal and ordered. Each patch needs a short header
  explaining why it exists and which upstream API forced it.
- Shims must expose a narrow C ABI, fail closed, and keep errors in the shared
  thread-local buffer. Do not let C++ exceptions cross the FFI boundary.
- Preserve the pinned submodule revision unless the task is an explicit upstream
  update.
- An upstream update is a deliberate change: update the submodule, re-apply or
  rebase patches, adapt shims, run the full binding tests, update the pin in
  `README.md`, and report any API breakage.
- Never weaken or remove a safety check to make a test pass.
- Keep `TENSOR_TRANSACTIONS_SAFETY.md` and the safety comments in sync with the
  code. If a patch changes callback lifetime rules, update that contract first.

## 8. Architecture and security review

The human will often ask for review rather than implementation. In review mode:

- Cite concrete files and lines.
- Group findings by severity: correctness/soundness, security, API/design,
  performance, polish.
- Explain the failure mode, the affected users or states, and a minimal fix.
- State assumptions and what would falsify them.
- Explicitly flag:
  - FFI lifetime, aliasing, `Send`/`Sync`, and unwind hazards
  - unbounded token, context, queue, or memory growth
  - prompt-injection paths and trust boundaries between model output, tool
    results, and host policy
  - tool execution boundaries and argument validation
  - secrets or model paths leaking into logs/errors
- Prioritize. A short list of real issues is more valuable than an exhaustive
  list of hypothetical ones.

Threat model: this is a local user daemon. Assume untrusted local clients,
untrusted model output, and untrusted tool results. Model output must never
select executable code or shell text. Token, context, queue, and memory limits
are security controls, not only UX settings.

## 9. Quality gates

Run from the workspace root:

```sh
cargo fmt --all --check
cargo check --workspace
cargo test --workspace
```

Practical notes:

- Model files are local, ignored by Git, and must not be required for the
  default test path. Tests should skip cleanly when a model is absent.
- Binding tests use `target/test-models/stories260K.gguf` when present.
- Assistant model-backed tests use `LFM3-*.gguf` or `ASSISTANT_TEST_MODEL`.
- If a full run is too expensive, run the narrowest package/test and state
  exactly what was and was not verified.
- Compiling is not verification. For inference, shim, or patch changes, run the
  relevant tests and, where possible, a real generation smoke test.

### Known baseline

The application layer is mid-refactor. At the last audit, `cargo run` was a
no-op, `crates/assistant/src/example.rs` was orphaned, and several assistant
integration tests failed because the binary ignored CLI arguments. Do not
assume a red assistant test means a binding regression; check
`docs/architecture.md` for the current known state and verify the cause.

## 10. Change management

- Do not commit unless asked. When asked, use a focused message explaining the
  why, not a list of files.
- Do not amend or rewrite the human's commits.
- Keep the working tree free of generated artifacts and local models.
- If a task conflicts with these rules, surface the conflict before acting.
