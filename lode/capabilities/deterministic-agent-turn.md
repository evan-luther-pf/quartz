# Deterministic agent turn

## Problem

Slices 0 through 3 prove replaceable composition and durable scalar facts, but no product behavior uses them to complete agent work. Slice 4 must prove one restart-safe agent turn without adding production transport, repository mutation, or agent-domain behavior to the kernel.

## Observable behavior

A sandboxed prompt client invokes the public `quartz.agent/submit@1` callable and commits one admitted turn fact. A replaceable agent-loop component scans only committed turn facts, derives the next owed action, and commits at most one new fact per activation. It invokes a deterministic provider, then a read-only tool over a host-selected fixture snapshot, persists the tool result before the next provider request, and finally persists assistant text, usage, and one terminal stop. Each process generation may end after that one boundary; fresh fibers reconstruct the next obligation from the event stream.

The tool is replaced through the governed composition capability. A second submitted turn uses the replacement provider identity and fixture generation. The first turn's committed facts remain byte-for-byte unchanged.

## Non-goals

- Production HTTP, credentials, model selection, streaming, or retries with backoff.
- General model, tool-registry, filesystem, prompt-template, or content-block frameworks.
- Mutable repository access, package operations, compaction, a TUI, or kernel handover.
- Arbitrary text payloads. Slice 4 uses a closed deterministic fixture vocabulary represented by stable numeric identities.
- Scheduling agent-domain work in `quartz-kernel`.

## Invariants

- Agent gateway, loop, provider, tool, client, and projection behavior are components. The kernel knows only component lifecycles, callable coeffects, governed patches, and durable events.
- The closed `quartz.agent/turn@1` fact vocabulary is: user prompt, provider request, tool call, tool result, assistant message, usage, stop, and interrupted/unknown.
- Every fact carries a turn identity. Provider and tool requests also carry stable invocation identities derived from the turn and stage, never from a fiber or process generation.
- The agent loop derives history and the next obligation only from committed event records. Guest-local state is disposable.
- One loop activation commits at most one new fact. A terminal stop suppresses every later action for that turn.
- A tool result must precede the second provider request. Assistant text must precede usage, and usage must precede exactly one stop.
- Deterministic provider calls and read-only fixture-tool calls may be retried with the original invocation identity when no result fact exists.
- A tool call classified non-idempotent with no committed result is never executed by this slice. Recovery commits `interrupted/unknown` with the original invocation identity.
- Ordinary `append-event` remains suppressed during historical activation replay. The agent loop uses a separate resumable-event authority after projecting committed facts; it receives the same event-grant and transactional-outbox enforcement.
- Provider and tool calls cross committed callable-coeffect views. Replacing either provider identity reactivates dependents through normal target semantics.
- Fixture repositories are immutable data selected by the host through a component artifact and scalar configuration. Tools receive no ambient path or filesystem import.
- Prompt, request, result, response, usage, stop, and interrupted facts are irreversible external records. Component recovery withdraws capabilities and state but never edits committed history.

## Public contract

ABI 5 adds `resume-event(index, value) -> status`. It has the same admitted event-grant, committed event-stream provider, size, count, outbox, and activation-commit rules as `append-event`, but it is allowed while current component activations are reconstructing after journal replay. A manifest may request either ordinary append or resumable append authority for its admitted grants, not both.

The deterministic turn value is a non-negative `u64` within the existing event value range:

- bits 56..62: fact kind;
- bits 48..55: turn identity;
- bits 32..47: stable invocation identity;
- bits 0..31: closed fixture payload identity.

The public agent gateway is `quartz.agent/submit@1`. Operation 1 validates a fixture prompt identity and returns its stable turn identity. The caller then commits the admitted user-prompt fact. The provider is `quartz.agent/provider@1`; operation 1 returns one typed tool call and operation 2 returns final text for a committed tool result. The read-only tool is `quartz.agent/repository-read@1`; operation 1 returns one fixture inspection result. These are Slice 4 protocols, not generalized registries.

## Acceptance scenario

1. Open the composition journal and event stream through the existing storage component.
2. Submit fixture prompt 1 through the public gateway and terminate after its durable user-prompt fact.
3. Across fresh process generations, the loop commits provider request 1, one typed read-only tool call, one tool result, provider request 2, final assistant text, usage, and one stop. Each generation derives exactly one next fact from prior committed events.
4. Simulated provider failure leaves the committed request intact. Replacing the provider and restarting reuses its invocation identity.
5. A non-idempotent tool call with no result becomes `interrupted/unknown`; the tool is not silently invoked.
6. A governed controller replaces fixture tool A with tool B.
7. Submit fixture prompt 2. Fresh generations complete the same sequence through tool B while turn 1 records remain unchanged.
8. Remove the application and persistence roots. Every live capability, fiber, binding, state cell, request, and artifact is recovered while durable facts remain external.

## Production-track readiness gate

Slice 4 is sufficient to evaluate, not assume, readiness for production
providers and real repository tools.

| Track | Decision | Evidence and blocker |
| --- | --- | --- |
| Real repository inspection | Closed | The tool boundary carries closed scalar fixture identities and imports no filesystem authority or byte payloads. |
| Contained repository edits | Closed | Quartz has no isolated workspace capability, approval boundary, mutation-result protocol, or compensation rule. |
| Durable work resumption | Ready for the closed Slice 4 protocol only | Every fact boundary reconstructs one owed action without duplicate commits. The fixed `u64` vocabulary is not a production session format. |
| Cancellation and provider failure | Closed for production | Deterministic provider failure preserves the committed request and retries after governed recovery with the stable identity. Callable invocation is synchronous and has no in-flight cancellation protocol. |
| End-to-end repository task | Closed | The acceptance path proves deterministic fixture inspection, not real repository bytes, edits, validation, or task completion. |

Decision: do not open the production-provider or repository-mutation track.
The next qualifying vertical slice must first carry bounded typed byte payloads
through durable facts and an isolated read-only repository capability. Editing
remains closed until isolation, approval, and ambiguous-mutation recovery are
specified and exercised.

## Verification

- `cargo test -p quartz --test slice4` passes three focused contracts.
- `cargo clippy --workspace --all-targets -- -D warnings` is clean.
- `cargo test --workspace` passes 35 contracts across eight suites.
- `cargo run --release -p quartz` runs 17 fresh Slice 4 process generations:
  prompt 1, seven owed facts, governed tool replacement plus prompt 2, seven
  owed facts, and final no-duplicate reconstruction with clean shutdown. The
  event stream contains exactly 16 turn facts.
