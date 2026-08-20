# Quartz

Quartz is a small native coding harness that can revise its own composition while it is running.

## Goal

Keep the idle path and interaction model as small and direct as Pi and fx while making DSH/Cordis-style spatiotemporal composability a hard invariant. Replaceability is a consequence; the product requirement is self-modification without stale state or broken dependents.

## Boundary

The host kernel is not the agent. It owns a unified context, tracked reversible effects, reactive dependencies, component lifecycles, declarative reconciliation, and code loading. Agent loops, providers, tools, persistence, policy, context maintenance, and interfaces are components.

A component may change the desired component tree through the same context it uses for every other effect. The loader reconciles that change, activates newly supported components, deactivates unsupported dependents before providers, and recovers removed effects in LIFO order.

## Implemented foundation

Slices 0 and 1 are complete. Quartz has a Rust context kernel and loads every
acceptance component as a Wasmtime component through the public WIT contract.
The runtime tracks structural inverses, resolves scalar and callable
dependencies by provider fiber identity, orders dependent recovery before
provider recovery, owns child registrations in parent accumulators, and
reconciles declared trees to quiescence.

A sandboxed controller can now invoke a callable governor and select an explicit
host-admitted add, remove, or replacement grant against a composition revision.
Successful patches belong to the controller accumulator; denied, stale,
malformed, cancelled, and failed requests leave or restore the prior
composition. Removing the declared roots leaves no fibers, bindings, state
cells, registrations, pending patches, composition effects, desired roots, or
live module artifacts.

## Non-goals

- Production model access.
- Repository mutation.
- Persistent sessions.
- TUI.
- Package installation or remote transport.
- Compatibility with the existing Quartz repository.
