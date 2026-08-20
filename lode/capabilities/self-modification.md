# Self-modification

## Implemented behavior

Slices 0 and 1 separate safe composition change from agent authorship. A Wasm
component may register declared children, inject a callable composition
authority, and select an explicit patch grant embedded in its host-admitted
specification. The governor's `invoke` result supplies policy approval; the
kernel independently enforces binding kind, committed provider identity, exact
scope, composition revision, lifecycle safety, target ownership, admission, and
rollback.

Accepted add, remove, and replacement patches are effects owned by the
requesting fiber. Unloading that fiber recovers the patch unless an enclosing
declaration already removed both requester and target. There is no ambient path
or artifact access: guest code sees only grant indices. There is no agent,
source mutation, durable event stream, package resolver, or kernel handover.

## Remaining product integration

An agent-authored change must later use this same path:

1. A component authors a typed composition proposal.
2. Production policy validates authority, scope, compatibility, and irreversible effects.
3. The host converts accepted artifact references into explicit grants.
4. The existing governor call and kernel patch transaction apply the grant.
5. The accepted declaration and lifecycle events become durable facts.

No future agent receives a hidden first-party mutation route.

## Failure rules

- Authority denial, a stale revision, or malformed scope changes nothing.
- A requester activation that fails after queueing a patch cancels the request.
- Staging or candidate activation failure restores the prior artifact and desired tree.
- Self, ancestor, authority-provider, and overlapping-target changes are rejected.
- External emissions are never described as reverted unless the owning capability provides a real inverse.
- Kernel changes remain a supervised process-handover boundary.

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

## Slice 1 acceptance

The executable and contract tests prove that a sandboxed controller can use an
explicit grant and callable authority to replace a provider through the Slice 0
dependency order. Authority denial and stale revisions change nothing;
malformed grants fail admission; queued requests disappear when requester
activation fails; candidate failure restores the prior generation; and
controller recovery inverts a committed patch. Add and remove grants are
limited to top-level roots in this slice; replacement may address any existing
keyed entry. Model calls, source editing, durable policy facts, package
resolution, and agent behavior remain out of scope.
