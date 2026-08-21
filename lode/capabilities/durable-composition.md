# Durable composition

## Problem

Governed composition changes are safe in memory, but a process exit loses the selected desired tree and artifact generations. Quartz must reconstruct committed composition without replaying historical lifecycle callbacks or treating an incomplete filesystem write as a committed fact.

## Observable behavior

A persistence component opens one host-admitted journal path through an explicit capability. Quartz loads the last committed desired-tree snapshot, verifies every artifact digest, and reconciles fresh fibers from that declaration. A committed patch and its later inverse each append the resulting declaration. Denied, stale, cancelled, malformed, and rolled-back patch attempts append nothing.

## Non-goals

- Conversation, model, tool, or repository-event persistence.
- A database, package resolver, remote artifact store, or artifact copying.
- Replaying fiber identities, accumulators, host effects, or lifecycle callbacks.
- Kernel process handover.

## Invariants

- Persistence policy is represented by a component using the public component contract. The kernel supplies only an explicitly admitted journal-file capability and the framing/replay mechanism required for its own authoritative composition state.
- Guest code receives a journal grant index, never an ambient path or filesystem handle.
- Registering journal access is a reversible context effect. Its inverse closes the capability. Durable records are external emissions withheld until composition commit; they are not described as reversible effects.
- A persistent runtime has exactly one journal provider. Application roots cannot replace or remove its bootstrap root.
- A governed patch requester must have committed the journal provider when persistence is active.
- Replay reconstructs only the latest committed desired tree and composition revision. It creates fresh fibers and reruns current lifecycle activation.
- Every persisted component specification contains the canonical artifact path and SHA-256 digest. Admission fails if current bytes do not match the recorded digest.
- Journal sequence numbers increase by one. Composition revisions remain monotonic across restart.
- A synchronized journal failure restores or retains the prior in-memory declaration before the public mutation returns.
- Provider withdrawal ordering remains ordinary coeffect ordering: application dependents recover while the journal capability remains registered, then the journal component recovers and closes it.

## Public contract

`Runtime::open_persistent` receives a host-built journal component specification containing one admitted journal path. It activates that component, reads the journal, verifies the recovered tree, and reconciles it. `declare_tree` and `apply_tree` treat their arguments as application roots and keep the bootstrap journal root outside persisted application state. `shutdown_persistent` commits an empty application tree, unloads the application, then unloads the journal component so observational cleanliness remains testable.

The journal starts with the eight-byte magic `QUARTZJ1`. Each record contains a monotonically increasing `u64` sequence, a bounded `u32` payload length, UTF-8 JSON for schema version 1, and a SHA-256 checksum over the sequence, length, and payload. The payload contains the composition revision and complete application `ComponentTree`.

Opening scans every frame. A truncated final header or payload is an uncommitted torn tail and is removed back to the last complete frame. Bad magic, sequence discontinuity, invalid JSON, an invalid checksum, or corruption before the final incomplete frame fails closed. Appends use one encoded frame followed by `sync_data` before the commit is reported.

## Acceptance scenario

1. Open an empty journal through a sandboxed journal component.
2. Declare provider A, a consumer, a governor, and a journal-dependent controller.
3. The controller commits provider B; the journal records the resulting desired tree.
4. Drop the process without lifecycle recovery and reopen the same journal.
5. Quartz verifies artifact digests, reconstructs provider B with fresh fiber identities, and reactivates the consumer against it.
6. Recovering the controller inverts its patch and records provider A; another restart reconstructs provider A.
7. Committing an empty application tree and shutting down leaves a clean context; reopening observes an empty application composition.

Contract tests cover cold reconstruction, failed-candidate omission, persisted inverse recovery, torn-tail repair, interior corruption rejection, artifact digest mismatch, journal write failure before mutation, and clean empty restart.
