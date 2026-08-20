# Terminology

- **Context** — the unified runtime state through which effects are tracked and dependencies are resolved.
- **Effect** — a context transformation paired with a one-sided inverse.
- **Effect accumulator** — a component-owned LIFO composition of inverses.
- **Coeffect** — a declared dependency resolved from the context.
- **Component** — a declaration of required keys, provided keys, and an effectful `apply` operation.
- **Fiber** — one runtime instantiation of a component, including parent, state, target, committed dependency view, and accumulator.
- **Provider** — an active fiber that has installed a declared key.
- **Target** — the provider-identity view currently implied by a fiber's declarations, or unsatisfied.
- **Committed view** — the provider identities a fiber activated against and continues to read during teardown.
- **Inertia** — the in-flight lifecycle transition; a changed target is followed only after the transition completes.
- **Recovery** — execution of accumulated inverses in reverse application order.
- **Reconciliation** — converging the runtime from its current fibers to a declarative component tree.
- **Quiescence** — no lifecycle transition remains applicable or in flight.
- **Module artifact** — loadable code containing one or more component implementations.
- **System boundary** — the set of locations Quartz can exclusively modify and restore; effects outside it require withholding, compensation, or explicit irreversibility.
- **Handover** — replacement of the kernel process through state transfer and re-exec.
- **Event stream** — ordered append-only facts from which durable and model-visible state can be reconstructed.
