# Quartz

A small native coding harness designed for runtime self-modification through reversible effects and reactive coeffects.

Slices 0 through 6 implement the spatiotemporal composition kernel, governed
component-authored patches, crash-safe desired-composition recovery, a
transactional durable event stream, restart-safe deterministic agent turns,
host-admitted immutable repository inspection, and credential-safe production
model exchange in Rust. All acceptance components run as sandboxed Wasmtime
components through `wit/quartz-component.wit`. Product knowledge and measured
tradeoffs live in `lode/`; the primary architecture paper is vendored at
`research/spatiotemporal-composability.pdf`.

Release smoke: `cargo run --release -p quartz`

Focused contracts: `cargo test -p quartz --test slice0 --test slice1 --test slice2 --test slice3 --test slice4 --test slice5 --test slice6`

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
