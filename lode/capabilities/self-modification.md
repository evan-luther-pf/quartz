# Self-modification

## Implemented behavior

Slice 0 separates safe composition change from authorship or policy. A Wasm
component may register only entries in its host-admitted child list. The host
may replace a registered artifact through `replace_entry`; the runtime stages
the candidate, recovers affected fibers in dependency order, commits a working
candidate, or restores the prior artifact and provider identity. There is no
agent, policy engine, source mutation, durable event stream, or kernel handover
in this slice.

## Product control path

The later governed self-modification capability must use this kernel path:

1. A component proposes a typed composition patch.
2. Policy validates authority, scope, compatibility, and irreversible effects.
3. The loader stages new artifacts and configuration without disturbing active fibers.
4. Reconciliation updates the desired tree.
5. Reactive coeffects deactivate unsupported dependents before providers.
6. Revertible effects recover removed contributions in LIFO order.
7. New components activate when their declared dependencies are satisfied.
8. The accepted declaration and lifecycle events become durable facts.

No future agent receives a hidden first-party mutation route.

## Failure rules

- A rejected proposal changes nothing.
- Staging or activation failure restores the prior artifact and desired tree.
- External emissions are never described as reverted unless the owning capability provides a real inverse.
- A component cannot replace the lifecycle mechanism currently executing its transition; kernel changes use supervised process handover.
- A component replacing its own implementation completes or checkpoints its current turn before the old fiber unloads.

## Slice 0 acceptance

The executable acceptance path proves:

1. A parent component registers a provider component as a tracked effect.
2. A consumer activates only after that provider becomes active.
3. Replacing the provider changes its identity and reactivates the consumer even
   though both generations provide the same value.
4. A failing candidate recovers its partial effects and restores the prior
   artifact, provider identity, and consumer.
5. Removing the parent deactivates the consumer first, then recovers the
   provider and the whole registered subtree.
6. The final context equals a clean context under the runtime's observational
   equivalence.
