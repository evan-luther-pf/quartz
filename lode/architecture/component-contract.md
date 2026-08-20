# Component contract

## Purpose

A component is a unit of composition, not necessarily a process. The contract must support trusted in-process modules, sandboxed WASM modules, and out-of-process adapters without changing lifecycle semantics.

## Declaration

Each component artifact declares:

- stable implementation identity and version;
- required coeffect keys (`inject`);
- keys it may install (`provide`);
- configuration schema;
- execution mode and requested host capabilities;
- an `apply` entry point.

Admission validates identity, compatibility, declared authority, and configuration before code becomes active. A component receives only its derived context and declared dependencies.

## Effect contract

`apply` performs mutations through context operations. Each atomic mutation supplies an inverse or uses a host operation whose inverse is structural. The runtime accumulates inverses in application order and runs them in reverse order during unload.

Component registration is itself an effect. A parent may use another component; withdrawing that registration retires the child and recursively recovers its subtree.

## Coeffect contract

A provided value is stored under a key and realm. A consumer commits the provider identities resolved for its declared keys when activation begins. Context reads use that committed view, including during teardown. Undeclared access fails.

Changing provider identity changes the consumer target and causes reactivation. Updating a value in place does not imply provider replacement; capabilities that need value-level reactivity define it explicitly.

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

The Slice 0 WIT contract exposes scalar lifecycle calls and four capability
imports:

- `start(config)`, `step(instance)`, and `drop(instance)` implement an inertial,
  bounded activation iterator;
- `set-state` performs an invertible fiber-owned context mutation;
- `publish` installs a declared coeffect with a structural inverse;
- `resolve` reads only the fiber's committed provider view and rejects undeclared
  slots;
- `register-child` realizes a declared child entry as a parent-owned effect.

All Slice 0 imports remain inside the system boundary and are invertible. The
world exposes no filesystem, network, clock, randomness, process, environment,
or credential import. External emissions are therefore absent rather than
described as reverted.

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

## Versioning

Key names alone are insufficient. Dependency declarations include a namespaced interface identity and compatible revision range. The runtime rejects collisions and incompatible providers before activation.
