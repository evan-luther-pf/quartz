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

## Internal ownership

The kernel remains one crate and one public `Runtime` contract, but its source
ownership follows the runtime concepts rather than accumulating in the facade:

- `component.rs` owns component specifications, trees, limits, public fiber
  state, observations, and trace types;
- `fiber.rs` owns fibers, provider views and bindings, effect inverses, and
  recovery state;
- `composition.rs` owns prepared declarations, governed patches and undo,
  desired-tree operations, and dependency validation;
- `repository.rs` owns admitted immutable snapshots and snapshot host
  operations;
- `events.rs` owns event registrations, pending events, projection reads, and
  transactional-outbox mechanics;
- `exchange.rs` owns exchange grants and adapters, registration and worker
  lifetime, durable outcome handling, and exchange host operations;
- `wasm_host.rs` owns component instantiation support, WIT linking, host
  dispatch, and status translation;
- `runtime.rs` retains the public facade, persistent bootstrap, reconciliation,
  activation, replacement, and orchestration across those owned boundaries;
- `journal.rs` remains the single framing and durable-log implementation.

This decomposition changes no public item, re-export, serialized declaration,
ABI, WIT import, status code, journal frame, lifecycle ordering, or algorithm.
Cross-boundary implementation state is `pub(crate)` only. Extraction does not
introduce generalized traits or a second runtime convention.

The behavior-preserving extraction is validated by 48 unchanged workspace tests,
Clippy with warnings denied, and `cargo run --release -p quartz`. On the same
Apple M5 Max release build, the pre/post measurements were:

- release binary: 19,028,128 / 19,046,304 bytes (`+0.10%`);
- `quartz --idle 2000` readiness, 20-run median: 3.833 / 3.753 ms;
- executable `scenario_total_ns`, 20-run median: 4.036 / 3.804 ms.

The credentialed OpenAI smoke remains an explicit open gate when
`OPENAI_API_KEY` is unavailable; it is not replaced by the deterministic
release reconstruction smoke.

## Lifecycle

A component declares `inject`, `provide`, and `apply`.

- It activates only when every injected key resolves to an active provider.
- Activation commits the resolved provider view and runs `apply` through the effect tracker.
- Every context mutation returns or registers an inverse.
- A provider enters unloading before any inverse runs; dependents therefore become unsupported and unload first.
- The provider recovers its effects only after affected dependents are inactive.
- A changed provider identity reloads the consumer even when the provided value compares equal.
- Parent-owned component registrations are effects, so unloading a parent recursively withdraws its subtree.
- A `continue-buffered-event` or `continue-exchange` request becomes eligible
  only after its acting activation commits. It then resumes that same fiber by
  returning `Active` to `Activating` with a reset step counter, the same
  committed provider view, and the same effect accumulator. Failure,
  cancellation, target change, and replacement still enter ordinary unloading
  and recover the accumulator.

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

The same `EventStream` framing is available to justified native storage
boundaries through `DurableEventLog`. This wrapper opens one explicitly supplied
path under caller-supplied limits, exposes committed records, and synchronizes
one validated event at a time with an automatically assigned monotonic ID. It
does not know repository-task schemas, infer state, dispatch work, or grant
ambient filesystem authority. The component event imports and ABI are
unchanged.

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

## Durable payload read boundary

Slice 8 makes committed payload bytes available to sandboxed consumers without
granting event-stream mutation or ambient storage access. Payload length and
byte reads use the same fiber-state and committed-provider checks as scalar
event projection, but require separate manifest capabilities. Missing payloads,
invalid indices, and out-of-range offsets return explicit status codes.

A reviewed-edit component may copy one durable model response into its private
workspace only after a fresh runtime reconstructs the event stream. The host
then admits the exact candidate digest and fixed source independently.
Publication remains the Slice 7 operation: callable approval, source identity,
mutation identity, durable intent, atomic replacement, and guarded inverse.
Reading a durable candidate creates no context effect; the live authority to
read it disappears with the consumer fiber, while the committed fact remains
external.

## Bounded exchange boundary

Slice 6 admits one explicit host exchange adapter without exposing ambient
network, credentials, endpoints, or environment access to guest modules. An
exchange grant binds the adapter identity, durable ledger path, request and
response byte limits, and timeout. The provider opens that grant as a
component-owned activation effect and can invoke it only while its callable
export is running against a committed event payload.

The exchange ledger uses Quartz's bounded, checksummed framing. It synchronizes
the stable invocation and request digest before external emission. A synchronized
success retains normalized response bytes, provenance, usage, and digest and can
be replayed without another emission. A started-only record, timeout, or
transport-ambiguous terminal is never retried. Invocation reuse with different
request bytes fails closed.

Provider callable dispatch remains synchronous. The dispatcher releases its core
borrow, marks exactly one provider in flight, and permits only that provider's
declared event-read and exchange imports; concurrent or nested invocation fails
closed. Exchange output is staged on the active provider fiber, transferred to a
consumer only through its committed callable view, and attached to a durable
event through the existing transactional outbox. A host deadline records an
ambiguous terminal result without waiting indefinitely; if the adapter worker
is still finishing, exchange recovery joins it before reporting a clean
context. Recovery closes the ledger and clears staged authority. Ledger records,
transmitted input, remote compute, and billing remain honestly external.

## Bounded mutable workspace boundary

Slice 7 admits one canonical regular source file into a host-owned, bounded
mutable buffer without exposing a path or ambient filesystem authority to guest
code. A workspace grant binds provenance, a durable operation identity, exact
before/result digests, a canonical mutation ledger, and a byte limit.
Activation reads and verifies mutable source bytes only after any replaced fiber
has recovered, so a new generation never stages stale bytes from before its
predecessor's inverse.

An editor may publish only while activating and only after a committed callable
mutation-authority provider approves the same operation and workspace index.
The host independently verifies the staged result and current source, records
started intent, synchronizes a same-directory temporary file, atomically renames
it, and records the terminal outcome. A successful publication installs a fiber
inverse. Recovery restores the exact admitted bytes only from the published
digest; any third digest records ambiguity and is not overwritten.

Applied operations reconstruct from the checksummed mutation ledger without a
second replacement or duplicate terminal record. Workspace buffers, staged
approvals, callable views, and workspace recovery effects are reversible
context authority. Source bytes and mutation-ledger records cross the system
boundary: recovery is exact only while Quartz retains the expected source
identity.
Concurrent non-Quartz writers are excluded during the bounded replacement
critical section.

## Durable edit promotion boundary

Slice 9 separates permission to retain published bytes from permission to
publish them. A promotion grant binds one admitted workspace operation to the
same canonical source, provenance, before/result digests, exact bytes and byte
limit, plus a callable approver identity. The editor may request promotion only
while activating, after exact mutation approval and successful publication, and
through its committed promotion-provider view.

The host verifies the live source and durable mutation identity, synchronizes a
promotion-intent record, then synchronizes the terminal promotion before
changing the fiber's recovery effect from restoration ownership to
promoted-state verification. A process loss before terminal commit reconstructs
restoration ownership; a process loss after commit reconstructs verification
ownership without republishing. Removal then preserves the exact promoted
candidate. Any different approver, identity field, or current source digest
fails closed; a third source digest is durably ambiguous and never overwritten.
All live workspace and callable authority still recovers in LIFO order.

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

Quartz does not implement kernel handover. Today, changing the executing kernel
requires stopping the process and starting a new binary; no in-flight fiber,
component, or external-operation state is transferred. A supervised process
handover with explicit state transfer and re-exec is the only admissible future
kernel-replacement boundary, because the executing replacement mechanism cannot
replace itself in place. Implement it only when a real kernel change must
preserve active runtime state across process generations. Until that gate is
met, Quartz must not claim live kernel self-replacement or uninterrupted
handover.
