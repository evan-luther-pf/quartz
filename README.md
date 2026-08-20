# Quartz

A small native coding harness designed for runtime self-modification through reversible effects and reactive coeffects.

Slice 0 implements the spatiotemporal composition kernel in Rust and loads all
acceptance components as sandboxed Wasmtime components through
`wit/quartz-component.wit`. Product knowledge and measured tradeoffs live in
`lode/`; the primary architecture paper is vendored at
`research/spatiotemporal-composability.pdf`.

Smoke: `cargo run --release -p quartz`

Focused contracts: `cargo test -p quartz --test slice0`
