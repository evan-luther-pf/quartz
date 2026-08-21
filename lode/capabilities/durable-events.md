# Durable events

## Problem

Slice 2 reconstructs the desired component tree, but model-visible session state still disappears with the process. Quartz needs durable facts that components can append and replay without ambient filesystem access, duplicate delivery after a crash, or historical lifecycle replay.

## Observable behavior

A sandboxed storage component opens one host-admitted event-stream path beside
the composition journal. An authorized component selects one host-admitted
event grant and supplies a non-negative value no greater than `i64::MAX` during
activation. Quartz withholds the request until that activation commits, records
it in the composition journal as a transactional outbox item, appends it once
to the event stream, and then durably clears the outbox. On restart, Quartz
drains any recovered outbox before application components activate. A
projection component then reads the bounded committed stream through its
committed storage-provider view and reconstructs the same scalar state.

## Non-goals

- An agent loop, model/provider calls, tools, prompts, or a TUI.
- Arbitrary byte payloads, attachments, event compaction, snapshots, or schema migration.
- Remote replication, multi-process writers, or distributed transactions.
- Treating event facts as reversible context effects.
- Kernel handover.

## Invariants

- Storage policy and projection logic remain components. The kernel supplies only admitted file operations, shared framing, transactional outbox delivery, and capability checks.
- Event types are versioned identities: namespace, name, and revision. An event grant binds one identity; guest code receives only its numeric index and supplies one non-negative `s64`-representable value through the `u64` append ABI.
- An appender must have committed the provider fiber that owns the event stream. A reader must have committed that same provider and request read authority.
- Event requests made by failed or cancelled activations are discarded. Composition replay never re-emits historical requests.
- Each staged event receives a monotonic durable event ID. Event-stream append is idempotent by ID and rejects the same ID with different content.
- The composition journal outbox is synchronized before event append. It is cleared only after the event frame is synchronized. A crash at either boundary therefore retries without loss or duplication.
- Event records are irreversible external facts. Component recovery closes access but does not remove committed records.
- Replay count and record size are bounded before application activation. Torn final frames are removed; interior corruption, sequence discontinuity, invalid schema, and inconsistent duplicate IDs fail closed.
- A projection reads only committed records and publishes ordinary immutable coeffects. Provider identity still governs dependent reactivation.

## Public contract

`ComponentSpec::with_event_stream_paths` admits the storage component's one
path. `ComponentSpec::with_event_grants` admits exact `EventGrant` identities
for appenders. The ABI exposes `open-event-stream`, `append-event`,
`event-count`, and `read-event`. Open access belongs only to the persistence
bootstrap root; append and read access require a committed dependency on its
provider fiber. Count and read return non-negative values or a negative Quartz
status.

The event stream uses the same framed-log implementation as composition
persistence with eight-byte magic `QUARTZE2`. Each frame contains a monotonic
sequence, bounded JSON payload length, schema-versioned payload, and SHA-256
checksum. An `EventRecord` contains durable ID, actor path, type identity,
scalar value, and an optional exact durable payload.

The composition journal snapshot carries the next event ID and staged outbox.
Fresh startup or recovery drains the outbox before declaring application roots.
`Runtime::events` is an observation API for committed records; components use
the capability imports.

`DurableEventLog` exposes the same event-stream implementation to justified
native storage boundaries without requiring a composition runtime. Opening
binds one path and explicit `Limits`; `records` returns committed facts; and
`append` assigns the next monotonic ID, validates all bounds and payload
identity, synchronizes one frame, and returns only after that fact is durable.
It adds no new framing, task schema, ambient path authority, or component ABI.

## Acceptance scenario

1. Start with empty composition and event files through a sandboxed storage component.
2. Activate an appender with one admitted `quartz.session/value@1` grant.
3. The appender commits value 37; Quartz synchronizes the outbox, appends event ID 1, and synchronizes the cleared outbox.
4. Terminate without lifecycle recovery.
5. Restart from both files, add a projection component, and reconstruct value 37 from the committed fact.
6. Restart again; the persisted projection reconstructs 37 without duplicate append.
7. Commit an empty application tree and recover the storage component, leaving a clean context.

Contract tests cover cold projection reconstruction, failed and denied append
omission, sequence continuity, outbox retry idempotence, torn-tail repair,
interior-corruption rejection, record bounds, clean shutdown, and child-process
abort after returned native-log appends.
