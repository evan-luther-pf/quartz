# Durable reviewed edit

## Problem

Quartz can durably produce a model response and can publish one pre-admitted
workspace, but sandboxed code cannot read a source path or reconstruct a
repository edit without host admission. A coding harness needs to preserve
model-authored intent across restart, keep the repository unchanged during
review, and apply only an exact digest-anchored ranged edit the host has verified.

## Observable behavior

A production response carries a source digest, half-open UTF-8 byte range, exact
replacement bytes, and expected result digest. The host applies the range to the
exact admitted source in memory and verifies the result digest before admitting
the materialized bytes to the existing sandboxed proposal editor. The process
may stop with no repository mutation. A fresh runtime reconstructs the response,
repeats materialization from durable source evidence, and exposes only the
verified result through one host-admitted snapshot and workspace. Approval binds
that result SHA-256 before the editor activates; denial, range failure, digest
drift, stale source state, or missing authority leaves the source unchanged.

## Non-goals

- Ambient path strings, directory traversal, directory mutation, or multi-file transactions.
- Automatic approval, semantic review, diff parsing, merge conflict resolution, formatting, Git staging, or commits.
- Treating model output as executable code or granting a model ambient filesystem authority.
- Streaming responses, automatic model retries, or retry after ambiguous external emission.
- TUI or interactive approval presentation.

## Invariants

- Durable event payload bytes remain host-owned and bounded by the existing event-stream record, per-payload, total-payload, and read-index limits.
- Payload reads require explicit manifest capabilities and a committed event-stream provider view. They are available only during activation or callable dispatch, matching scalar event reads.
- A product proposal is one strict ranged edit. The range is half-open, bounded
  by the exact source bytes, and begins and ends on UTF-8 boundaries.
- Host materialization must produce the declared result digest, valid non-empty
  UTF-8, changed bytes, and a result within the workspace bound before sandboxed
  code receives it.
- The proposal editor remains admitted for one canonical source and exact
  materialized result digest; it cannot widen the workspace byte bound.
- Copying materialized bytes mutates only the editor fiber's private workspace
  buffer. Publication still requires the existing committed callable authority,
  exact operation/index approval, source-before digest, candidate-result digest,
  and durable mutation identity.
- Candidate production and candidate application are separate runtime
  generations. Restart cannot change range intent, materialized bytes, or
  provenance and cannot duplicate an already committed model exchange.
- Without a separate promotion commit, editor replacement follows ordinary
  lifecycle order: the old restoration effect restores the source before the
  next generation rematerializes and admits the same ranged edit.
- Event facts, candidate payloads, exchange records, and mutation-ledger records remain honestly external. Live event access, workspace buffers, approvals, and workspace recovery effects are recovered.

## Superseded implementation decision

Slice 8 used a complete-file model payload. Slice C replaced that product
representation with a digest-anchored ranged edit and deleted the complete-file
parser. The existing ABI still carries verified full workspace bytes because
the host materializes and checks the range before admission; this is mechanism,
not a second candidate representation.

Host-only path selection remains until Slice E admits a canonical path/digest
manifest selected only by numeric index. Model-authored path strings and ambient
filesystem discovery remain prohibited. That change will also be a clean
cutover with no compatibility path.

## Public contract

ABI 9 introduced two read-only imports retained by the current ABI:

- `event-payload-len(index) -> s64` returns the exact committed payload length, `STATUS_UNSATISFIED` when the selected fact has no payload, or a negative Quartz status;
- `event-payload-byte(index, offset) -> s32` returns one payload byte or a negative Quartz status.

Both imports require their own declared host capabilities plus the same committed `quartz.events/stream@1` view used by `event-count` and `read-event`. They expose bytes only, not host paths, credentials, adapter handles, or mutable event storage.

`quartz.slice8.proposal-editor-a` and
`quartz.slice8.proposal-editor-b` inject `quartz.events/stream@1` and
`quartz.repository/mutation-authority@1`. Their `u64` config is the selected
turn identity. During activation each scans committed facts, requires one
kind-5 response for that turn, copies the payload to workspace index zero
within the admitted bound, invokes authority operation 1 with its stable
mutation identity and index, and publishes only after approval. The two
generations differ only in module identity and admitted operation identity.

## Acceptance scenario

1. Create one real source file and durable composition, event, exchange, and mutation ledgers.
2. Run the production-compatible component path against a deterministic host adapter whose exact response is a candidate full-file replacement.
3. Stop after the response and verify the source is unchanged while the candidate and exchange success are durable.
4. Start a fresh runtime, reconstruct the event payload, and activate the proposal editor against a denying authority. The editor fails and the source remains unchanged.
5. Start another fresh runtime with a workspace grant that binds the same source and exact candidate digest. Activate the proposal editor against an approving authority and publish once.
6. Remove the editor and verify the source returns to its original bytes.
7. Replace editor A with editor B using a new operation identity. The old inverse restores the original before B copies the same durable candidate and publishes through the same public capability.
8. Exercise payload-free facts, out-of-range payload reads, undeclared payload imports, wrong turn selection, result-digest mismatch, and source drift during recovery. Every case fails closed.
9. Remove all roots. Live event access, workspace buffers, approvals, inverses, fibers, bindings, and artifacts recover; durable candidate, exchange, and mutation records remain external.

## Completion gate

Slice 8 closes only when focused contracts cover payload authority and bounds, restart-stable candidate reconstruction, approval denial, exact candidate publication, editor replacement ordering, source-drift ambiguity, and clean authority withdrawal; all existing contracts and Clippy remain clean; and the release executable runs candidate production and reviewed application as separate runtime generations against a real temporary file.
