# Quartz engineering rules

Quartz uses Lode Coding. Persistent project knowledge lives in `lode/`; code remains the source of truth.

## Session start

1. Read `lode/lode-map.md`.
2. Read `lode/terminology.md`.
3. Read only the Lode files relevant to the requested capability.
4. Inspect code only after the Lode map identifies the owning boundary.

## Change discipline

- Frame each capability as: problem, observable behavior, non-goals, invariants, public contract, acceptance scenario.
- Record material architectural decisions in Lode before implementation.
- Implement one runnable vertical slice at a time.
- Exercise the real product path before closing a slice.
- Update Lode to describe current behavior, not implementation history.
- Delete obsolete plans and scaffolding when a slice closes.
- Do not create a second convention beside an existing one.

## Product invariants

- Everything above the context kernel is a component, including the agent loop, model adapters, tools, session storage, context maintenance, policy, and UI.
- Every context mutation is a tracked effect with an inverse; external emissions are classified honestly at the system boundary.
- Components declare dependencies as coeffects and receive only their committed dependency view.
- Providers become unavailable before recovery begins; affected dependents unload before provider inverses run.
- Component registration is parent-owned and reversible, so removing a parent recovers its subtree.
- Runtime composition is derived from a declarative component tree that components may propose governed patches to.
- A failed artifact or composition update restores the prior working composition.
- Untrusted component code receives no ambient credentials or filesystem authority. Privileged work crosses explicit capabilities and an isolation boundary.
- Durable model-visible state must be reconstructable from an append-only event stream.
- No built-in behavior may bypass the public component contract used by third-party modules.

## Quality bar

- No placeholders, compatibility shims, silent fallbacks, or hidden global registries.
- Public protocol changes require contract tests.
- Lifecycle changes require failure, cancellation, and replacement tests.
- Optimize measured hot paths; preserve the small idle path.
