# Quartz

A small native coding harness designed for runtime self-modification through reversible effects and reactive coeffects.

Slices 0 through 4 implement the spatiotemporal composition kernel, governed
component-authored patches, crash-safe desired-composition recovery, a
transactional durable event stream, and one restart-safe deterministic agent
turn in Rust. All acceptance components run as sandboxed Wasmtime components
through
`wit/quartz-component.wit`. Product knowledge and measured tradeoffs live in
`lode/`; the primary architecture paper is vendored at
`research/spatiotemporal-composability.pdf`.

Smoke: `cargo run --release -p quartz`

Focused contracts: `cargo test -p quartz --test slice0 --test slice1 --test slice2 --test slice3 --test slice4`
