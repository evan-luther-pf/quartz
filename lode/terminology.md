# Terminology

- **Context** — the unified runtime state through which effects are tracked and dependencies are resolved.
- **Effect** — a context transformation paired with a one-sided inverse.
- **Effect accumulator** — a component-owned LIFO composition of inverses.
- **Coeffect** — a declared value or callable dependency resolved from the context.
- **Callable coeffect** — a versioned provider interface invoked synchronously through a consumer's committed view.
- **Component** — a declaration of injected and provided interfaces, admitted host capabilities, and an effectful lifecycle.
- **Fiber** — one runtime instantiation of a component, including parent, state, target, committed dependency view, and accumulator.
- **Provider** — an active fiber that has installed a declared key.
- **Target** — the provider-identity view currently implied by a fiber's declarations, or unsatisfied.
- **Committed view** — the provider identities a fiber activated against and continues to read during teardown.
- **Inertia** — the in-flight lifecycle transition; a changed target is followed only after the transition completes.
- **Recovery** — execution of accumulated inverses in reverse application order.
- **Reconciliation** — converging the runtime from its current fibers to a declarative component tree.
- **Quiescence** — no lifecycle transition remains applicable or in flight.
- **Composition revision** — the monotonic in-memory version checked by governed patch requests.
- **Patch grant** — one exact host-admitted add, remove, or replacement operation exposed to guest code by index.
- **Composition journal** — the versioned append-only stream of committed desired-tree snapshots used for restart reconstruction.
- **Artifact digest** — the SHA-256 identity bound to module bytes when a composition declaration is admitted.
- **Module artifact** — loadable code containing one or more component implementations.
- **System boundary** — the set of locations Quartz can exclusively modify and restore; effects outside it require withholding, compensation, or explicit irreversibility.
- **Handover** — replacement of the kernel process through state transfer and re-exec.
- **Event grant** — one exact host-admitted event type identity exposed to guest code by index.
- **Event stream** — the versioned append-only stream of typed durable facts from which model-visible state can be reconstructed.
- **Transactional outbox** — event requests synchronized in the composition journal before idempotent event-stream delivery.
- **Snapshot grant** — one exact canonical regular file, provenance label, byte length, and SHA-256 identity admitted to a component by numeric index.
- **Durable payload** — bounded evidence bytes with provenance and SHA-256 attached to an irreversible event fact.
- **Guest payload buffer** — a fiber-private bounded byte buffer copied into a
  durable event request without granting ambient storage authority.
- **Turn fact** — one closed agent-protocol event carrying fact kind, turn identity, stable invocation identity, scalar projection value, and optionally bounded durable evidence.
- **Owed work** — the unique next action derived from committed turn facts when no terminal fact exists.
- **Stable invocation identity** — a turn-derived provider or tool call identity preserved across process and component generations.
- **Interrupted/unknown** — an explicit terminal result for an ambiguous non-idempotent operation that Quartz must not retry.
- **Exchange grant** — one exact adapter identity, durable ledger path, byte bounds, and timeout admitted to a component by numeric index.
- **Exchange ledger** — the checksummed append-only record of started and terminal external invocations used to prevent unsafe retries and replay exact successes.
- **Staged response** — bounded adapter output held by a provider fiber during callable dispatch and transferred only through committed callable authority.
- **Reviewed candidate** — one bounded ranged edit carrying an admitted source
  identity, a half-open UTF-8 byte range, exact replacement bytes, and a
  host-computed materialized result identity.
- **Proposal generation** — one exact original or corrected ranged edit for an
  admitted path; only the latest completed generation remains promotable.
- **Approved command attempt** — one exact user-approved argv with a synchronized
  start fact and at most one terminal fact; a started-only attempt is
  interrupted/unknown and never implicitly retried.
- **Task continuation** — one bounded model decision bound to terminal approved
  command evidence and exact post-command admitted sources; it yields one
  corrected proposal generation or explicit completion.
- **Supervised task** — the user-driven repository loop that reviews current candidates, separately approves publication and exact command argv, and reconstructs from durable facts.
- **Workspace grant** — one exact host-admitted source file, mutation ledger,
  byte bound, and mutable guest buffer exposed by index; publication binds its
  current before/result digests and operation identity atomically.
- **Mutation authority** — the committed callable provider whose exact approval is required before a workspace publication.
- **Mutation ledger** — the checksummed append-only record used to prevent duplicate publication and classify incomplete or unsafe repository mutations.
- **Workspace publication** — an approved, durable, digest-guarded atomic
  replacement of one admitted source file with an inverse guarded by the
  published digest.
- **Promotion authority** — the committed callable provider whose separate
  exact approval is required to retain a workspace publication after recovery.
- **Promotion commit** — the durable decision that transfers a publication
  effect from source restoration to non-mutating promoted-state verification.
