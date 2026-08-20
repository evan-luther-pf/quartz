# Quartz

A small native coding harness designed for runtime self-modification through reversible effects and reactive coeffects.

Slices 0 and 1 implement the spatiotemporal composition kernel and governed
component-authored patches in Rust. All acceptance components run as sandboxed
Wasmtime components through `wit/quartz-component.wit`. Product knowledge and
measured tradeoffs live in `lode/`; the primary architecture paper is vendored
at `research/spatiotemporal-composability.pdf`.

Smoke: `cargo run --release -p quartz`

Focused contracts: `cargo test -p quartz --test slice0 --test slice1`
