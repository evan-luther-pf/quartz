# Kernel architecture

## Problem

Quartz must be able to change its own running composition. Swapping code is insufficient: removing or replacing a component must also withdraw everything it installed, preserve dependency ordering, and leave unrelated state intact.

## Decision

Quartz implements the paper's context paradigm as its irreducible kernel:

1. one unified context carrying effects and coeffects;
2. a fiber for each component instance;
3. an effect accumulator that stores inverses and recovers them in LIFO order;
4. a reactive dependency resolver keyed by provider identity;
5. an inertial component lifecycle that completes each load/unload transition before following a newer target;
6. a declarative component tree reconciled to a quiescent runtime state;
7. a module loader that can introduce and retract component code.

The kernel contains no agent loop, provider, tool, prompt assembly, storage policy, approval policy, or interface implementation.

## Lifecycle

A component declares `inject`, `provide`, and `apply`.

- It activates only when every injected key resolves to an active provider.
- Activation commits the resolved provider view and runs `apply` through the effect tracker.
- Every context mutation returns or registers an inverse.
- A provider enters unloading before any inverse runs; dependents therefore become unsupported and unload first.
- The provider recovers its effects only after affected dependents are inactive.
- A changed provider identity reloads the consumer even when the provided value compares equal.
- Parent-owned component registrations are effects, so unloading a parent recursively withdraws its subtree.

## Composition kernel public contract

The runtime owns a declarative, keyed component tree. `declare_tree` admits a
tree without driving it, `step` advances one lifecycle action, and `apply_tree`
declares then reconciles to quiescence. Admission checks artifact ABI, interface
compatibility, provider collisions, dependency cycles, depth, and component
count before mutating the current declaration. Runtime generation changes use
`replace_entry`, which performs the staged replacement transaction below.
Components may realize only child entries present in their declared child list.

The unified context contains:

- fiber-owned state cells;
- active coeffect bindings keyed by namespaced interface and revision;
- parent-owned component registrations;
- the fiber registry and desired root registrations.

Every successful mutation appends one structural inverse with a unique effect
identity to the acting fiber. Recovery takes and executes at most one inverse
per lifecycle step, so order is LIFO and an inverse cannot execute twice.
Lifecycle tracing is diagnostic output, not context state and not part of
observational equivalence.

A fiber records its immutable identity and parent, artifact generation,
declaration, desired/retired state, lifecycle state, committed provider view,
transition instance, effect accumulator, and terminal outcome. Its current
target is recomputed from active provider identities. Lifecycle states are
inactive, activating, active, unloading, and failed.
Activation and unloading are inertial: a target change is observed only between
atomic component steps or inverse steps, and the current step lands before the
transition diverts or chains.

Only active fibers provide. Changing an active provider to unloading therefore
makes it unavailable before its first inverse runs. An unloading provider may
recover only when no installed fiber's committed view names it. Consumers keep
their committed view until their own recovery completes. Provider selection and
target comparison use fiber identity, never provided-value equality.

A child registration belongs to the parent accumulator. Its inverse retires the
logical child registration; retirement cascades through each child's own
accumulator. Inactive retired fibers are removed only after their children are
gone. A context is observationally clean when its state cells, bindings,
registrations, desired roots, fibers, composition effects, pending patches,
pending events, event outbox, journal registration, event-stream registration,
and live artifacts are all empty. Monotonic allocators, composition revision
history, and durable external records are not observable context state.

## Replacement and failure

Replacement first loads, validates, links, instantiates, and starts the
candidate generation while the current generation remains active. Commit then
marks the old provider unavailable, drains dependents, and recovers the old
generation before activating the staged candidate. A successful candidate gets
a fresh fiber identity.

If candidate activation fails, its partial accumulator recovers completely and
its store is dropped. The runtime then reinstates the prior artifact under its
prior fiber identity and reconciles affected dependents back to quiescence.
Admission, instantiation, or start failure occurs before commit and changes
nothing. A failed ordinary activation remains failed and inert until its
declaration changes; there is no implicit retry.

Component iteration, tree depth, fiber count, and total reconciliation steps are
bounded. A limit breach is an activation or admission failure and follows the
same recovery rules. Cancellation is retirement during activation: the landed
partial iteration recovers before removal.

## Governed composition effects

Slice 1 introduces a monotonically increasing in-memory composition revision.
A component specification may carry host-admitted patch grants; guest code sees
only their numeric indices. A grant is exactly one of:

- add a top-level root;
- remove a top-level root;
- replace an existing keyed entry.

The patch authority is a normal callable provider. A requester must declare and
commit that dependency, receive approval from its `invoke` export, request the
patch host capability, present the current base revision, and select one of its
admitted grants. Unknown indices, stale revisions, undeclared authority,
out-of-scope paths, self/ancestor replacement, and replacement of the committed
authority provider are rejected before mutation.

An approved request is queued until the requester activation step lands. A
failed or cancelled activation discards its queued request before mutation.
Successful reconciliation appends a composition inverse to the requester
accumulator and claims its target path against conflicting patches and direct
replacement. Candidate failure restores the prior composition before the
requester fails. Recovery performs the inverse before releasing the target
claim. If an enclosing declaration has already removed both requester and
target, recovery releases the claim without recreating state that the enclosing
declaration removed.

## Durable composition boundary

Slice 2 persists the kernel-owned desired declaration without moving storage
policy into the kernel. A bootstrap persistence component registers one
host-admitted journal path as a context effect. The host framing mechanism
appends checksummed desired-tree snapshots and replays only the latest committed
snapshot; the component owns availability and capability lifetime.

Persistent declarations exclude the bootstrap journal root. Restart activates
that root first, verifies the recovered tree and artifact digests, assigns the
recorded composition revision, then creates fresh application fibers. Historical
fiber identities, accumulators, inverses, and lifecycle callbacks are never
replayed.

Durable append is withheld until a candidate declaration is admitted and, for a
replacement, proven activatable. An append failure restores the prior
in-memory composition. Recovering a committed composition effect appends the
inverse declaration before releasing its target claim.

## Durable event boundary

Slice 3 adds a second durable stream without adding a second framing convention.
The persistence bootstrap component opens both host-admitted files. Composition
snapshots retain the complete event outbox and next durable event identity;
event frames use the same bounded sequence, length, checksum, torn-tail, and
fail-closed rules as composition records.

An appender selects one exact host-admitted event type and supplies a `u64`
value during activation. The request becomes eligible only after that fiber is
active and has committed the event-stream provider. Persistent reconciliation
first synchronizes the outbox, then appends each event idempotently by durable
ID, then synchronizes an empty outbox. Startup drains a recovered outbox before
application fibers activate. Historical activation replay cannot enqueue an
event.

ABI 5 separates ordinary event production from replay-aware resumption.
`append-event` remains unavailable while historical composition is activating.
A component admitted for `resume-event` first projects committed facts, then
may queue one missing fact through the same event grant and transactional
outbox. A manifest cannot request both authorities. This preserves replay
silence for ordinary components without making a durable workflow inert after
restart.

Event facts are irreversible external emissions, not context effects. Opening
the stream is a reversible capability registration; recovery closes it but does
not claim to erase committed facts. A projection component reads the bounded
stream through its committed provider view and publishes ordinary coeffects.

## Immutable snapshot and payload boundary

Slice 5 admits immutable repository evidence without granting guest filesystem
authority. A snapshot grant binds one canonical regular file, provenance, byte
length, and SHA-256 identity. Admission reads and verifies the file before
activation, stores the bytes in the prepared fiber, and repeats verification
when restoring a persisted declaration. Guest code sees only numeric grant
indices and may read bytes only during activation.

An event may retain its ordinary scalar projection value while carrying one
bounded durable payload. `resume-snapshot` selects one of the emitting fiber's
admitted snapshots and queues its exact bytes, provenance, and digest through
the existing transactional outbox. Event framing, record checksums, append
ordering, replay deduplication, torn-tail repair, and fail-closed interior
corruption behavior remain unchanged. Payload count, per-record bytes, total
payload bytes, and snapshot grants are bounded independently.

Snapshot access and event-stream registration are reversible capabilities.
Committed payload facts are withheld external emissions: recovery closes live
authority and clears pending work but does not claim to erase durable evidence.

## Self-modification

The authoritative state is a declarative component tree. A running component—including the agent—may propose a tree patch through a governed composition capability. Reconciliation turns the accepted patch into fiber insertions, updates, disablement, and removal. Source changes become new module artifacts; the loader stages the artifact, reconstructs affected fibers, and rolls back to the previous artifact if activation fails.

This is broader than hot module replacement. HMR is one loader operation built on reversible effects and reactive coeffects.

## System boundary

Only context-mediated mutations are automatically recoverable. External emissions cannot be honestly undone. Each capability must classify operations as:

- inside the boundary and invertible;
- withheld until commit;
- compensated under an explicit weaker equivalence; or
- irreversible and therefore gated before execution.

Untrusted component code requires WASM or process isolation; dependency declarations alone are not a sandbox.

## Kernel replacement

The currently executing replacement mechanism cannot replace itself in-place. Kernel source changes use a supervised process handover with explicit state transfer and re-exec. This is the only non-component lifecycle boundary.
