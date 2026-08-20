# Lode map

## Product

- `summary.md` — product goal, boundaries, implemented foundation, and non-goals.
- `terminology.md` — canonical names used in code and protocol.
- `practices.md` — development and validation discipline.

## Architecture

- `architecture/kernel.md` — effect/coeffect context, fibers, lifecycle, and self-replacement boundary.
- `architecture/component-contract.md` — component declarations, authority, effects, code loading, and versioning.

## Capabilities

- `capabilities/self-modification.md` — governed composition changes and recovery rules.


## Research

- `research/spatiotemporal-composability.pdf` — primary architecture paper; hot module replacement is one application, not the core model.

## Implementation

- `crates/quartz-kernel` — unified context, fibers, reconciliation, Wasmtime
  loading, replacement, and rollback.
- `crates/quartz` — Slice 0 executable scenario and contract tests.
- `wit/quartz-component.wit` — public component/host boundary.
- `modules` — real WebAssembly component sources and embedded manifests used by
  the executable acceptance path.
