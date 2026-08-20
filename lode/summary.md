# Quartz

Quartz is a small native coding harness that can revise its own composition while it is running.

## Goal

Keep the idle path and interaction model as small and direct as Pi and fx while making DSH/Cordis-style spatiotemporal composability a hard invariant. Replaceability is a consequence; the product requirement is self-modification without stale state or broken dependents.

## Boundary

The host kernel is not the agent. It owns a unified context, tracked reversible effects, reactive dependencies, component lifecycles, declarative reconciliation, and code loading. Agent loops, providers, tools, persistence, policy, context maintenance, and interfaces are components.

A component may change the desired component tree through the same context it uses for every other effect. The loader reconciles that change, activates newly supported components, deactivates unsupported dependents before providers, and recovers removed effects in LIFO order.

## Implemented foundation

Slice 0 is complete. Quartz has a Rust context kernel and loads every acceptance
component as a Wasmtime component through the public WIT contract. The runtime
tracks structural inverses, resolves dependencies by provider fiber identity,
orders dependent recovery before provider recovery, owns child registrations in
parent accumulators, reconciles declared trees to quiescence, and restores the
prior generation after a failed replacement. Removing the declared root leaves
no fibers, bindings, state cells, registrations, desired roots, or live module
artifacts.

## Non-goals

- Production model access.
- Repository mutation.
- Persistent sessions.
- TUI.
- Package installation or remote transport.
- Compatibility with the existing Quartz repository.
