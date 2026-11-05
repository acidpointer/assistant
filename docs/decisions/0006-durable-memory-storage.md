# Durable memory storage foundation

Status: implemented baseline for review, 2026-11-05.

## Context

The assistant needs knowledge that survives daemon restarts and replacement of
its bounded working context. Source text, qualified graph changes, background
work and replay receipts must not disagree after a failed transaction or lost
reply. Future personal recall also requires owner-scoped access and revocation.

## Decision

Implement a Linux library memory store within `assistant::memory`, with one
SQLite writer and two query-only readers behind bounded command queues.
Use bundled rusqlite 0.39.0 and SQLite 3.51.3, FULL/WAL commits, private local
files and an exclusive directory lock. This wrapper passes the actual Rust 1.88
test build; the initially proposed 0.40.1 wrapper did not compile on that compiler.

Use stable host-generated identities, validated versions and opaque owner
capabilities. Literal remember commits its source, FTS index, queued/deferred formation,
event and replay receipt together. Claims have exact source evidence and
immutable correction versions. Graph navigation does not itself infer truth.
Represent personal relationships, occupations and locations explicitly.

Fence extraction attempts and reserve compute durably before dispatch. Apply
validated graph changes and attempt completion in one transaction. Suppression
also revokes receipt replay; physical purge is bounded. Retain minimal tombstones
and charged usage. Seal backups with an external deletion journal; restored
candidates reconcile newer deletions before reading personal data.

## Consequences

Memory is independent of HTTP, native inference and chat history. The first
library implementation can be tested with temporary databases and no models.
Host-issued graph mutations still require semantic validation before accepting
model proposals. FTS provides literal retrieval; semantic embeddings and a
qualified extractor remain later integrations.

The daemon does not configure this library yet. Required product persistence,
authenticated transports, stream/history revocation and safe restore activation
must precede enabling personal recall. No claim of complete memory v1, model
quality or production scale follows from these storage tests.

The storage readiness extension adds ordered checksummed upgrades from the
immutable initial schema, attributed sources, durable product/tool state and
restart interruption, settlement byte reservations, independent priority lanes,
bounded pending formation and atomic consumer cursors. Physical purge accounts
for retained payload bytes as well as logical records. These remain library
contracts until the application participates in admission and publication.

See [the implemented API and limits](../memory-storage.md) and
[the complete delivery specification](../memory-implementation-spec.md).
