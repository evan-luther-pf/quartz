# Lode map

## Product

- `summary.md` — product goal, boundaries, implemented foundation, and non-goals.
- `terminology.md` — canonical names used in code and protocol.
- `practices.md` — development and validation discipline.

## Architecture

- `architecture/kernel.md` - effect/coeffect context, fibers, lifecycle,
  bounded host capabilities, and the self-replacement boundary.
- `architecture/component-contract.md` — component declarations, authority, effects, code loading, and versioning.

## Capabilities

- `capabilities/self-modification.md` — governed composition changes and recovery rules.
- `capabilities/durable-composition.md` — append-only desired-tree facts, artifact identity, and restart recovery.
- `capabilities/durable-events.md` — typed event facts, transactional outbox delivery, and bounded projection replay.
- `capabilities/deterministic-agent-turn.md` — closed turn protocol, restart-safe owed work, deterministic provider/tool calls, and production-track readiness.
- `capabilities/durable-repository-inspection.md` — immutable snapshot grants, durable byte evidence, and restart-safe real repository inspection.
- `capabilities/production-model-call.md` — credential-safe bounded exchange, durable ambiguity, and production Responses API calls.
- `capabilities/isolated-repository-editing.md` — bounded mutable workspaces,
  callable approval, durable publication identity, and guarded recovery.
- `capabilities/durable-reviewed-edit.md` — restart-stable model-authored
  candidates, explicit exact-byte approval, and bounded application to one
  host-selected source.
- `capabilities/durable-edit-promotion.md` — separate durable retention
  authority, restoration ownership transfer, and restart-safe promoted bytes.
- `capabilities/dogfood-repository-task.md` — end-to-end credentialed edits,
  resumable proposal correction, explicit review and promotion, exact
  host-approved command evidence, and bounded correction or completion.

## Research

- `research/spatiotemporal-composability.pdf` — primary architecture paper; hot module replacement is one application, not the core model.

## Implementation

Slices 0 through 9 and the bounded production repository loop are implemented
as one Rust workspace:

- `crates/quartz-kernel` — unified context, fibers, callable coeffects,
  composition, durable journal/event/exchange/mutation records, immutable
  snapshots, bounded payload reads and workspaces, host exchange, publication,
  and promotion authority, Wasmtime loading, replacement, and rollback.
- `crates/quartz` — Slice 0 through Slice 9 executable scenarios, contract tests,
  and bounded proposal, correction, approved-command, and continuation
  orchestration.
- `wit/quartz-component.wit` — public component/host boundary.
- `modules` — real WebAssembly component sources and embedded manifests used by
  the executable acceptance paths.
