# Quartz

Quartz is a small native coding harness that can revise its own composition while it is running.

## Goal

Keep the idle path and interaction model as small and direct as Pi and fx while making DSH/Cordis-style spatiotemporal composability a hard invariant. Replaceability is a consequence; the product requirement is self-modification without stale state or broken dependents.

## Boundary

The host kernel is not the agent. It owns a unified context, tracked reversible effects, reactive dependencies, component lifecycles, declarative reconciliation, and code loading. Agent loops, providers, tools, persistence, policy, context maintenance, and interfaces are components.

A component may change the desired component tree through the same context it uses for every other effect. The loader reconciles that change, activates newly supported components, deactivates unsupported dependents before providers, and recovers removed effects in LIFO order.

## Implemented foundation

Slices 0 through 5 are complete. Quartz has a Rust context kernel and loads
every acceptance component as a Wasmtime component through the public WIT
contract. The runtime tracks structural inverses, resolves scalar and callable
dependencies by provider fiber identity, orders dependent recovery before
provider recovery, owns child registrations in parent accumulators, and
reconciles declared trees to quiescence.

A sandboxed controller can invoke a callable governor and select an explicit
host-admitted add, remove, or replacement grant against a composition revision.
Successful patches belong to the controller accumulator; denied, stale,
malformed, cancelled, and failed requests leave or restore the prior
composition.

A sandboxed persistence component can register one host-admitted composition
journal and one host-admitted event stream. Committed desired-tree snapshots
carry canonical artifact paths, SHA-256 digests, composition revisions,
committed patch inverses, next event identity, and the transactional event
outbox. Restart verifies the latest complete records, drains recovered event
requests idempotently, creates fresh fibers from the declaration, and preserves
committed patch inverses without replaying historical lifecycle emissions.
Authorized appenders emit typed facts only after activation commit. Facts retain
their scalar projection value and may carry one bounded, checksummed durable
payload. Projections reconstruct model-visible state through their committed
storage-provider view. Torn final writes are removed and interior corruption
fails closed.

A replaceable agent gateway, loop, deterministic provider, and read-only fixture
tool now complete a closed turn protocol from committed facts. Each restart
projects the exact transcript, derives one owed action, and commits at most one
new fact with a stable invocation identity. Provider failure preserves the
request for retry; an ambiguous non-idempotent call becomes
`interrupted/unknown`. A governed tool replacement changes the second turn
without rewriting the first.

Host-admitted snapshot grants bind a canonical regular-file path, provenance,
byte length, and SHA-256 identity. Sandboxed inspectors can read only their
immutable admitted bytes during activation. The replay-aware agent loop may
attach one admitted snapshot to a tool-result fact through the transactional
outbox. Every restart re-verifies source identity before activation. The release
scenario inspects real `README.md` and `lode/summary.md` bytes across two turns,
governedly replaces the inspector, preserves the first transcript exactly, and
recovers all live inspection authority on shutdown.

Removing the application and persistence roots leaves no fibers, bindings,
state cells, child registrations, pending patches or events, composition
effects, desired roots, journal or event registrations, outbox entries, or live
module artifacts. Durable journal and event records remain honestly external.

## Non-goals

- Production model access.
- Repository mutation.
- Production or unbounded conversation/session payloads.
- TUI.
- Package installation or remote transport.
- Compatibility with the existing Quartz repository.
