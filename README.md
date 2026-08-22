# Quartz

A small native coding harness designed for runtime self-modification through reversible effects and reactive coeffects.

Quartz implements the spatiotemporal composition kernel, governed
component-authored patches, crash-safe composition and event recovery,
host-admitted repository mutation and promotion, bounded exchange adapters, and
an external WASM repository-task orchestrator. Components run through
`wit/quartz-component.wit`; Rust-produced WASI Preview 2 components receive no
ambient filesystem, environment, network, terminal, or credential authority.
Product contracts and measured tradeoffs live in `lode/`; the primary
architecture paper is vendored at
`research/spatiotemporal-composability.pdf`.

Release CLI help: `cargo run --release -p quartz -- --help`

Release version: `cargo run --release -p quartz -- --version`

Complete acceptance smoke: `cargo run --release -p quartz -- --acceptance`

Focused contracts: `cargo test -p quartz --test slice0 --test slice1 --test slice2 --test slice3 --test slice4 --test slice5 --test slice6 --test slice7 --test slice8 --test slice9`

Repository-editing smoke:

```sh
cargo run --release -p quartz -- \
  --repository-edit "$(mktemp -d /tmp/quartz-slice7.XXXXXX)"
```

The command admits one real file as a bounded workspace, runs sandboxed editor
A through a callable mutation authority, replaces it with editor B through the
same public component contract, restores the original bytes after both
generations, validates the checksummed mutation ledger, and removes its
temporary artifacts.

Reviewed-edit smoke:

```sh
cargo run --release -p quartz -- \
  --reviewed-edit "$(mktemp -d /tmp/quartz-slice8.XXXXXX)"
```

The command commits one deterministic production-compatible response as a
durable candidate while leaving the source untouched. Fresh runtimes prove
denial, exact approval, governed editor replacement, guarded recovery, and a
clean final context against the real temporary file.

Promoted-edit smoke:

```sh
cargo run --release -p quartz -- \
  --promote-edit "$(mktemp -d /tmp/quartz-slice9.XXXXXX)"
```

The command durably publishes one exact reviewed candidate, obtains retention
approval from a separate callable authority, transfers recovery from source
restoration to promoted-state verification, reconstructs that commit in a
fresh runtime without republishing, and removes all live authority while
retaining the approved bytes.

Credentialed OpenAI smoke:

```sh
printf 'Reply with exactly: Quartz production path works.' > /tmp/quartz-prompt.txt
OPENAI_API_KEY="$OPENAI_API_KEY" cargo run --release -p quartz -- \
  --production-model gpt-5.4 /tmp/quartz-prompt.txt /tmp/quartz-production.qj
```

The credential stays in the host adapter. The command starts one bounded
background Responses API request with `store: false`, polls it to a terminal
state under the host deadline, reconstructs the exact assistant payload from
fresh runtime state, recovers live authority, and leaves the `.qj`, `.qe`, and
`.qx` durable records for inspection. OpenAI temporarily stores background
response data for asynchronous execution and polling.

External repository task:

```sh
OPENAI_API_KEY="$OPENAI_API_KEY" cargo run --release -p quartz -- \
  task gpt-5.4 /path/to/task.txt /path/to/session \
  path/to/source-a path/to/source-b -- executable exact args
```

Quartz admits 2 through 64 canonical UTF-8 source files and runs the task state
machine in a sandboxed WASM component. The initial model response is strict
JSON containing at least two digest-bound UTF-8 ranged edits. Each generation
is reviewed with `approve`, `reject <feedback>`, or `stop`; rejection requests
one corrected revision of that same proposal. Every accepted generation is
published before Quartz requests separate approval for the exact CLI-supplied
command vector.

Quartz durably records command start and the bounded terminal result, including
the exact argv and before/after source identities. The continuation model must
return either `PROPOSE <source-index>` followed by one strict ranged-edit JSON
object, or `COMPLETE` followed by a summary. Failure always continues through
`PROPOSE`; `COMPLETE` is accepted only after command success.

Only explicit `COMPLETE` returns exit 0. Stop or a terminal model, terminal, or
command exchange failure recovers live state and returns exit 2 with one
non-secret category such as `authentication`, `request-rejected`,
`remote-failed`, `empty-response`, `response-limit`, `protocol`, or
`ambiguous`. Reopening a failed session reconstructs that category without
repeating the external operation.

The authoritative `session/task.qe` event stream reconstructs every transition.
Re-running a completed session performs no model, terminal, command, mutation,
or promotion again. Exchange, mutation, promotion, and composition journals are
bounded idempotency and recovery evidence, not parallel task state.

Release builds place loadable modules in `components/` beside the `quartz`
executable. The executable and that directory may be relocated together.
Development launches may set `QUARTZ_COMPONENT_DIR`; production does not use a
compile-time Cargo output path.
