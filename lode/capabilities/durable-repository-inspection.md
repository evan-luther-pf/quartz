# Durable repository inspection

## Problem

Slice 4 proves exact restart-safe orchestration over closed scalar fixtures. It cannot inspect a real repository because components receive neither byte payloads nor filesystem authority. Slice 5 must ground one durable agent turn in host-admitted repository bytes without opening mutation, shell, ambient filesystem, or production-provider access.

## Observable behavior

The host admits exact immutable file snapshots to an inspector and the agent loop. Each grant binds a canonical source path, model-visible provenance, byte length, and SHA-256 digest. A sandboxed inspector reads only its indexed grants during activation, derives a deterministic inspection result from the actual bytes, and publishes the existing callable tool shape at revision 2. The agent loop attaches the selected snapshot bytes and provenance to the durable tool-result fact before requesting provider continuation. A fresh process verifies every admitted digest, projects committed scalar facts and payload metadata, and derives at most one owed fact.

Inspector A reads `README.md`; a governed controller replaces it with inspector B, which reads `lode/summary.md`. A second turn uses B while the first turn and payload remain unchanged. The deterministic provider emits closed answer identities rendered by the executable as answers citing the inspected path. This proves grounded inspection, not arbitrary model text.

## Non-goals

- Repository writes, shell execution, package operations, or mutable workspace authority.
- Production HTTP, credentials, model selection, streaming, or cancellation.
- Directory traversal, globbing, symlink-following guest APIs, or an ambient filesystem import.
- An unbounded blob store, generalized attachment system, prompt framework, or tool registry.
- Treating durable external payload records as reversible context effects.

## Invariants

- A snapshot grant names one canonical regular file and one non-empty provenance string. Guest code receives only a numeric index.
- Admission reads each file once, enforces per-component grant count and byte limits, and verifies its declared SHA-256 digest. Prepared fibers share those immutable admitted bytes.
- Restart re-reads the exact canonical path and rejects any digest drift before application activation. It never silently substitutes current content.
- Snapshot reads are available only during the granted component's activation. Callable invocation still cannot reenter host context operations.
- A payload event carries the ordinary scalar turn value plus exactly one payload containing provenance, SHA-256, and bytes. Existing scalar event readers continue to read the scalar value.
- `resume-snapshot` requires a committed event-stream provider, one admitted event grant, one admitted snapshot grant, activation state, and replay-aware authority. It uses the existing transactional event outbox and append ordering.
- Payload count, per-payload bytes, total durable payload bytes, event record bytes, and total event records are bounded independently.
- A tool-result payload commits before provider request 2. The following request refers to the scalar inspection result from that committed tool-result fact.
- Durable payloads are withheld external emissions. Recovery withdraws snapshot access and event capability but does not claim to erase committed records.
- Agent gateway, loop, provider, inspector, controller, and renderer behavior remain components or executable acceptance code. The kernel knows only admitted immutable snapshots and optional durable event payloads.

## Public contract

ABI 6 adds three generic host imports:

- `snapshot-len(index) -> s64` returns the admitted immutable byte length or a negative Quartz status;
- `snapshot-byte(index, offset) -> s32` returns one byte or a negative Quartz status;
- `resume-snapshot(event-index, snapshot-index, value) -> s32` queues one replay-aware payload event.

`ComponentSpec::with_snapshot_grants` supplies exact `SnapshotGrant` values. `SnapshotGrant::from_file(path, provenance)` canonicalizes a regular file and records its SHA-256 identity. Persistence serializes that identity, not the bytes; every process re-admits and verifies the file before creating application fibers.

`EventRecord::payload` is optional. A `DurablePayload` contains `provenance`, lowercase SHA-256, and the exact bytes. The event framing schema advances cleanly; old event files are not silently accepted as the new schema.

The Slice 5 callable identities are `quartz.agent/deterministic-provider@1` and `quartz.agent/repository-inspector@1`. The public `quartz.agent/submit@1` entry remains unchanged. Turn facts retain the Slice 4 bit layout and use event type `quartz.agent/repository-turn@2`; the tool-result scalar identity is the deterministic inspection result while the attached durable payload contains its evidence.

## Acceptance scenario

1. Admit immutable `README.md` and `lode/summary.md` snapshots with exact readable paths.
2. Submit repository-inspection prompt 1 through the public agent gateway.
3. Across fresh process generations, commit provider request 1 and a typed read-only inspection call.
4. Inspector A derives its result from admitted `README.md` bytes.
5. Commit one tool-result fact carrying bounded bytes, SHA-256, and `README.md` provenance before provider continuation.
6. Commit provider request 2, a deterministic answer citing `README.md`, usage, and exactly one stop.
7. Governedly replace inspector A with inspector B and submit prompt 2.
8. Complete turn 2 from `lode/summary.md`; turn 1 records remain byte-for-byte unchanged.
9. Reject unauthorized snapshot indices, changed source bytes, oversized grants or payload totals, malformed payload records, and interior corruption. Repair only an incomplete final frame.
10. Remove the application and persistence roots. All fibers, bindings, snapshot authority, pending work, outbox entries, and artifacts recover while durable records remain external.

## Production-track decision

Slice 5 opens real read-only repository inspection for explicitly admitted immutable files. It does not open production providers or repository mutation. Real edits still require an isolated mutable workspace, approval, validation, and an explicit ambiguous-mutation recovery contract. In-flight provider cancellation remains absent.

## Verification

- `cargo test -p quartz --test slice5` passes four contracts covering two complete turns with governed inspector replacement, missing/changed/undeclared snapshot rejection, payload count and byte limits, payload corruption, snapshot drift, exact history preservation, and clean recovery.
- `cargo test -p quartz` passes 39 contracts across seven suites.
- `cargo run --release -p quartz` runs 17 fresh Slice 5 process generations. It admits real `README.md` and `lode/summary.md` bytes, commits 16 facts across two turns, replaces inspector A with B, reconstructs both transcripts, cites both canonical repository paths, and shuts down cleanly.
