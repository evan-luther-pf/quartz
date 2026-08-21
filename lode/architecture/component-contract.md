# Component contract

## Purpose

A component is a unit of composition, not necessarily a process. The contract must support trusted in-process modules, sandboxed WASM modules, and out-of-process adapters without changing lifecycle semantics.

## Declaration

Each component artifact declares:

- stable implementation identity and version;
- required bindings (`inject`) with slot, kind, namespaced interface, and revision range;
- provided bindings (`provide`) with slot, kind, namespaced interface, and revision;
- the `u64` configuration schema;
- execution mode and requested host capabilities;
- `start`, bounded `step`, synchronous `invoke`, and `drop` lifecycle exports.

Admission validates identity, compatibility, declared authority, and configuration before code becomes active. A component receives only its derived context and declared dependencies.

## Native host residency

Outside the irreducible context and lifecycle kernel, native resident behavior
is admissible only when at least one of these conditions is true:

1. credentials or operating-system handles cannot cross the sandbox boundary;
2. the operation is privileged system I/O and therefore must cross an explicit
   host capability;
3. terminal I/O owns the human interaction boundary.

Every resident must have one recorded justification naming the applicable
condition. Being first-party, convenient, performance-sensitive without a
measurement, or already implemented in Rust is not a justification. Product
state machines, response validation, candidate lifecycle, retry or continuation
policy, and review sequencing are components and use the same public contract
as third-party modules.

Current justified residents are the production adapter that retains credentials
and network handles (condition 1); journal, event, exchange, mutation, and
promotion storage, canonical filesystem admission and publication, and exact
child-process spawning (condition 2); and CLI argument, rendering, and approval
I/O (condition 3). The repository-task orchestrator currently resident in
`crates/quartz/src/main.rs` has no valid condition; its correction boundary is
recorded in `capabilities/dogfood-repository-task.md`.

## Effect contract

`start` and `step` perform mutations through admitted context operations. Each
atomic mutation supplies an inverse or uses a host operation whose inverse is
structural. The runtime accumulates inverses in application order and runs them
in reverse order during unload. `invoke` is synchronous and cannot mutate the
host context; `drop` runs only after recovery.

Component registration is itself an effect. A parent may realize a declared
child; withdrawing that registration retires the child and recursively recovers
its subtree.

## Coeffect contract

A provided scalar value or callable interface is stored under its declared
interface identity. A consumer commits the provider fiber identities resolved
for its injected slots when activation begins. Scalar reads and callable
invocations use that committed view, including during teardown. Undeclared
access fails.

Changing provider identity changes the consumer target and causes reactivation.
A published binding is immutable for that activation; a second installation
with the same interface identity is a collision.

## Initial module format

Quartz loads WebAssembly components through Wasmtime. A component artifact is one
`.wasm` file containing a `quartz:manifest` custom section and implementing the
versioned `quartz:component/module` WIT world. The manifest declares component
identity, version, injected and provided interface revisions, configuration
schema, and requested host imports. Admission parses and validates the manifest,
compiles the component, links only admitted imports, and instantiates its store
before the active generation is disturbed.

Each fiber owns its Wasmtime store and instance. Retraction first recovers every
host-tracked effect, then drops that store. Artifact caches hold weak references,
so compiled code is retained only while a desired, staged, or live generation
owns it. A process remains an optional isolation mode; it is not the composition
model.

The ABI 10 WIT contract exposes four lifecycle calls and twenty-seven capability
imports:

- `start(config)`, `step(instance)`, `invoke(instance, operation, arg0, arg1)`,
  and `drop(instance)` implement bounded activation, synchronous callable
  dispatch, and disposal;
- `set-state` performs an invertible fiber-owned context mutation;
- `publish` installs a declared scalar coeffect with a structural inverse;
- `resolve` reads a scalar slot only through the fiber's committed provider view;
- `publish-callable` installs a declared callable coeffect;
- `call-provider` invokes a callable slot only through the committed view;
- `apply-patch` queues one authority-approved, host-admitted composition patch;
- `register-child` realizes a declared child entry as a parent-owned effect;
- `open-journal` registers one host-admitted composition journal path;
- `open-event-stream` registers one host-admitted durable event path;
- `append-event` queues one granted typed fact until activation and composition
  commit;
- `event-count` and `read-event` expose bounded committed scalar facts to an
  authorized projection component;
- `event-payload-len` and `event-payload-byte` expose bounded committed payload
  bytes through the same provider view but separate manifest capabilities;
- `resume-event` has the same grant, outbox, and commit enforcement as
  `append-event`, but may append the unique owed fact after historical event
  projection during restart.
- `snapshot-len` and `snapshot-byte` expose only immutable bytes admitted to
  that fiber by indexed canonical-file grant during activation;
- `resume-snapshot` has replay-aware append enforcement and attaches one
  admitted snapshot as a bounded durable event payload.
- `open-exchange` registers one host-admitted adapter and durable ledger as a
  component-owned effect;
- `exchange` consumes one committed event payload during the invoked
  provider's callable dispatch and stages a bounded response under a stable invocation identity;
- `resume-exchange` attaches the staged callable response to one replay-aware
  durable event request.
- `workspace-len` and `workspace-byte` expose only a fiber's indexed bounded
  host-owned mutable buffer;
- `workspace-set-len` and `workspace-write-byte` mutate that buffer without
  touching its admitted source file;
- `publish-workspace` requires exact committed callable mutation approval,
  admitted before/result digests, and durable mutation identity before a
  digest-guarded same-directory atomic publication;
- `promote-workspace` requires a separate committed callable approval bound to
  the exact published mutation and transfers its recovery effect from source
  restoration to promoted-state verification only after durable commit.

Every binding declares `value` or `callable` kind as part of its versioned
identity. Context-changing imports remain tracked by structural inverses.
Journal, event, exchange, and mutation-ledger records are external emissions and
survive component recovery. Registration, publication, and promotion effects
close live authority and restore or verify only context-owned or
still-identifiable source state; they do not claim to erase committed facts,
network requests, billing, approved promoted bytes, or unrecognized external
edits. Exchange requests remain irreversible emissions
guarded by a durable started/terminal ledger. The world exposes no ambient
filesystem, network, clock, randomness, process, environment, or credential
import.

### Slice 1 verification

`cargo test -p quartz --test slice1` exercises seven contracts: authorized
replacement, authority denial, stale revision, malformed-grant admission,
candidate rollback, pending-request cancellation, committed-patch recovery, and
reversible top-level add/remove operations. `cargo run --release -p quartz`
executes the governed controller path, inverts its accepted provider patch when
the controller unloads, then asserts a clean context.

### Slice 2 verification

`cargo test -p quartz --test slice2` exercises eight durable-composition
contracts: cold reconstruction, failed-candidate omission, persisted patch
inverse recovery, torn-tail repair, interior-corruption rejection, artifact
digest mismatch, journal-write failure before mutation, and clean empty restart.
`cargo run --release -p quartz` starts three real executable generations against
one journal: the first commits provider B and exits without recovery, the second
reconstructs B and persists the controller inverse to provider A, and the third
reconstructs A and performs a clean persistent shutdown.

### Slice 3 verification

`cargo test -p quartz --test slice3` exercises eight durable-event contracts:
cold projection reconstruction, failed and denied append omission, event ID and
sequence continuity, torn-tail repair, interior-corruption rejection, record
count and payload bounds, idempotent recovered-outbox delivery, and clean
capability recovery. `cargo run --release -p quartz` starts three executable
generations against one composition journal and event stream: the first commits
one typed fact and exits without recovery, the first restart installs a
projection from that fact, the second reconstructs the persisted projection
without duplication, and the final generation performs a clean persistent
shutdown.

### Slice 4 verification

`cargo test -p quartz --test slice4` exercises three deterministic-turn
contracts: exact two-turn restart reconstruction with governed tool replacement,
provider failure followed by stable-request retry, and explicit interruption of
an ambiguous non-idempotent tool call. `cargo run --release -p quartz` starts a
fresh executable generation after every turn fact, reconstructs the unique owed
action from the committed event stream, switches fixture tool generations
between turns, verifies both exact transcripts, and performs a clean persistent
shutdown.

### Slice 5 verification

`cargo test -p quartz --test slice5` exercises four durable repository contracts:
exact two-turn reconstruction with governed inspector replacement, snapshot
admission and authority failures, independent payload limits and corruption
rejection, and restart-time snapshot drift rejection. `cargo run --release -p
quartz` admits real `README.md` and `lode/summary.md` snapshots, starts a fresh
process after every durable boundary, preserves the first transcript across
inspector replacement, cites both canonical paths, and recovers all live
authority on clean shutdown.

### Slice 6 verification

`cargo test -p quartz --test slice6` exercises six production-exchange
contracts: durable response reconstruction and governed provider replacement,
missing grant or adapter denial, independent request and response bounds with a
closed ambiguous turn after external emission, host-enforced timeout without
retry plus worker joining before clean recovery, started-only crash ambiguity
without retry, and cached-success replay with invocation-digest collision
rejection. `cargo test -p quartz --bin quartz`
covers bounded normalization of a completed background Responses API envelope.
`cargo run --release -p quartz` runs the same sandboxed production provider and
loop against a deterministic host adapter, starts a fresh runtime generation
for each fact, reconstructs exact response bytes, and recovers every live
authority. The credentialed OpenAI command is documented in `README.md`; it
requires `OPENAI_API_KEY`.

### Slice 7 verification

`cargo test -p quartz --test slice7` exercises six isolated-editing contracts:
bounded workspace isolation and canonical admission, approval denial and stale
source rejection, exact publication with replacement and subtree recovery,
restart reconstruction without duplicate publication, mutation identity
collision rejection, and source-drift ambiguity without overwrite. `cargo run
--release -p quartz -- --repository-edit /tmp/quartz-slice7-smoke` runs the real
file path through sandboxed editor and callable authority components, replaces
the editor through the same public contract, restores the original bytes after
each generation, and asserts a clean context.

### Slice 8 verification

`cargo test -p quartz --test slice8` exercises three durable-reviewed-edit
contracts: payload authority, missing-payload and offset bounds; explicit
approval, wrong-turn and result-digest rejection plus source-drift ambiguity;
and restart-stable candidate reconstruction with governed editor replacement
ordering. `cargo run --release -p quartz -- --reviewed-edit
/tmp/quartz-slice8-smoke` commits a production-compatible response without
touching the source, reconstructs it in fresh runtimes, proves denial and exact
approval, replaces the sandboxed editor through a governed patch, restores the
original bytes, and asserts a clean context.

### Slice 9 verification

`cargo test -p quartz --test slice9` exercises five durable-promotion contracts:
separate exact approver binding; denial and cancellation restoration; restart
before commit restoring the original; restart after commit preserving and
verifying the candidate; and third-party drift ambiguity without overwrite.
`cargo run --release -p quartz -- --promote-edit
/tmp/quartz-slice9-smoke` publishes one reviewed candidate, promotes it through
a separate sandboxed callable authority, reconstructs the durable commit in a
fresh runtime without republishing, preserves the candidate during final
shutdown, and asserts a clean live context.

### Decision evidence

Three release probes used the same `value + 1` operation on arm64 macOS: a
direct stable-C-ABI dynamic-library call, a Wasmtime 48 component canonical-ABI
call, and a vendored Lua 5.4 function call. Cold start is process entry through
loader readiness; idle RSS is sampled after readiness; activation is load,
resolve, and instantiate; unload is handle/store/state release; call overhead is
the steady-state median over ten million calls. Seven operation runs and fifteen
startup/RSS runs produced:

| Format | Cold start p50 | Idle RSS p50 | Host binary | Activation p50 | Call p50 | Unload p50 |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Native C ABI | 2.812 ms | 2.031 MiB | 0.452 MiB | 119.334 us | 1.600 ns | 47.292 us |
| Wasmtime component | 2.855 ms | 2.859 MiB | 14.613 MiB | 661.292 us | 57.221 ns | 37.459 us |
| Embedded Lua 5.4 | 2.681 ms | 2.375 MiB | 0.874 MiB | 16.833 us | 20.850 ns | 21.458 us |

These numbers are separate costs, not substitutes for one another. The native
probe's idle path initializes no VM; the Wasmtime path initializes an engine;
the Lua path initializes a state. The call probe measures only the format
boundary, not reconciliation or application work.

| Criterion | Native C ABI | Wasmtime component | Embedded Lua 5.4 |
| --- | --- | --- | --- |
| Retraction | Symbols become unusable after handle close, but POSIX does not require physical unmapping | Dropping the fiber store deterministically releases the instance; host effects still recover explicitly | Closing a per-fiber state releases it; clearing `package.loaded` alone does not prove collection |
| Recovery safety | Manual ownership and callback lifetime rules | Typed WIT boundary and store-owned guest state | GC and dynamic typing require host-side lifetime and interface checks |
| Sandboxing | None in-process | Import-only access, bounds-checked memory, typed control transfers | Restricted libraries reduce authority, but the VM is not a memory-isolation boundary |
| Cross-platform | Per-target libraries and loader APIs | Native backends on major 64-bit targets and portable Pulley fallback | Broad clean-C portability |
| Implementation | Small loader; highest ABI and lifetime burden | Larger dependency and canonical-ABI work; declarations map directly to WIT | Small embedder; bespoke typing, module eviction, and sandbox policy |

Native wins the measured small path, but fails the unload and untrusted-code
requirements: POSIX defines `dlclose` as intent and does not require removal
from the address space. Lua is compact, but module eviction is reachability-based
and secure authority requires a bespoke VM policy. Wasmtime costs about 14 MiB
of release binary and 56 ns per scalar call relative to native in this probe,
while providing the only candidate that directly satisfies deterministic
instance ownership, capability-only host access, typed cross-language
interfaces, and portable sandboxing. Those properties decide the format.

Sources:

- The Open Group `dlclose` specification:
  https://pubs.opengroup.org/onlinepubs/9699919799/functions/dlclose.html
- Wasmtime component embedding API:
  https://docs.wasmtime.dev/api/wasmtime/component/index.html
- Wasmtime platform support:
  https://docs.wasmtime.dev/stability-platform-support.html
- Wasmtime security model:
  https://docs.wasmtime.dev/security.html
- Lua 5.4 reference manual, state closure, module registry, and library loading:
  https://www.lua.org/manual/5.4/manual.html

## Slice 0 verification

Exact smoke command:

```sh
cargo run --release -p quartz
```

Focused contract command:

```sh
cargo test -p quartz --test slice0
```

The smoke executable loads the generated `.wasm` artifacts from disk, activates
root/provider/consumer fibers, attempts and rolls back a failing provider,
replaces an equal-valued provider with a new identity, removes the root, and
asserts a clean observation. The same executable measures committed-view
coeffect reads by running one million `resolve` imports from a consumer
component.

Release measurements on the same arm64 macOS host as the format probes use
twenty independent process runs. Phase timers are emitted separately:

| Measurement | p50 |
| --- | ---: |
| Cold process start through idle kernel readiness | 3.115 ms |
| Idle RSS after readiness | 2.906 MiB |
| Release executable size | 15.117 MiB |
| Initial artifact load, activation, and reconciliation | 2.152 ms |
| Failed replacement staging, recovery, and rollback | 0.448 ms |
| Valid replacement and dependent reactivation | 0.578 ms |
| Complete root-subtree removal | 0.026 ms |
| Full acceptance scenario | 3.227 ms |
| Cross-component committed coeffect read | 78.788 ns/read |

The earlier 57.221 ns figure measures only a scalar Wasmtime canonical-ABI call.
The 78.788 ns figure is the implemented Quartz path through a consumer import,
committed provider identity lookup, and provider value return. Neither is
reported as reconciliation latency.

## Callable coeffects

Slice 1 adds a callable binding kind beside scalar values. A callable provider
publishes its declared interface and implements the revisioned `invoke`
export. A consumer may invoke only a callable slot in its committed provider
view and only while its own activation step is running. Calls are synchronous
and inertial: provider identity cannot change during the call. Ordinary
providers cannot reenter context operations. ABI 7 permits the one provider
currently marked as invoked to use only its declared event-read and exchange
imports after callable dispatch releases the core borrow; concurrent and nested
provider calls fail closed.

The composition authority is the callable
`quartz.composition/patch-authority@1` interface. Authorization is a pure call;
the kernel separately validates and applies the selected host-admitted patch.
This keeps policy in a replaceable component while the kernel retains structural
authority over the component tree.

## Guest-authored durable payloads

ABI 11 adds one missing primitive required by component-owned orchestration: a
component may build a bounded payload in a host-admitted, fiber-private event
output buffer and queue that exact buffer as a resumable event payload. The
public imports set the buffer length, replace one byte, and consume the buffer
into one declared event grant. The buffer is unavailable without an explicit
grant, cannot name a path or external adapter, is discarded on failed
activation, and becomes externally durable only through the existing withheld
event outbox after activation commits.

ABI 10 could persist only host-authored snapshot payloads or adapter-authored
exchange payloads. It therefore forced the native host to author prompts and
session facts, which is product orchestration rather than privileged I/O.
Indexed workspace grants and callable coeffects already express multiple
workspaces and exact command approval; they are not ABI gaps. Host-only source
selection remains until the separate admitted-manifest slice.

The first ABI 11 consumer is `repository-task-orchestrator`, built from the
reviewable Rust source under `components/` and loaded from its committed WASM
artifact. It imports only declared Quartz event and snapshot capabilities and no
WASI filesystem, environment, network, clock, random, or polling interface.
Before a session fact becomes durable, the component reads the prior fact
stream, validates the transition and bounded payload grammar, copies the exact
accepted bytes into its output, and queues the append. The native session
reducer is a projection for privileged boundary work and display, not an
alternate append authority.

## Versioning

Key names alone are insufficient. Dependency declarations include a namespaced interface identity and compatible revision range. The runtime rejects collisions and incompatible providers before activation.
