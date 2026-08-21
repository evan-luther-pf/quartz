# Quartz

A small native coding harness designed for runtime self-modification through reversible effects and reactive coeffects.

Slices 0 through 2 implement the spatiotemporal composition kernel, governed
component-authored patches, and crash-safe desired-composition recovery in
Rust. All acceptance components run as sandboxed Wasmtime components through
`wit/quartz-component.wit`. Product knowledge and measured tradeoffs live in
`lode/`; the primary architecture paper is vendored at
`research/spatiotemporal-composability.pdf`.

Smoke: `cargo run --release -p quartz`

Focused contracts: `cargo test -p quartz --test slice0 --test slice1 --test slice2`
