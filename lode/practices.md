# Practices

## Slice loop

1. State one observable acceptance scenario.
2. Update the owning Lode contract.
3. Implement the smallest complete runtime path.
4. Run the executable scenario.
5. Add contract tests only for durable behavior and failure boundaries.
6. Remove obsolete scaffolding and update Lode to current reality.

## Architectural review

Every change must answer:

- Is this kernel mechanism or replaceable product behavior?
- Which capability owns it?
- What authority does the module receive?
- What reverses its registrations and side effects?
- What happens if activation, invocation, cancellation, or disposal fails?
- Can an active generation remain available while its replacement is prepared?

## Performance

Measure startup time, idle resident memory, binary sizes, invocation overhead, and swap latency separately. IPC is acceptable only at replaceable boundaries. Do not add a daemon, database, scheduler, or generic framework before a slice requires it.
