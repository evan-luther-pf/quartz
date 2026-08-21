# Dogfood repository task

## Problem

Quartz can produce durable model responses, review and promote repository edits,
run explicitly approved commands, and continue from their exact results. One
authoritative session log now binds those operations into a reconstructible
repository-task loop. The remaining architectural debt is product orchestration
resident in the native executable rather than a component.

## Observable behavior

`quartz --propose <model> <task-path> <session-dir> <source> <source>
[source]` currently admits two or three exact UTF-8 repository files and one
bounded task. The host canonicalizes the sources beneath the repository root,
records their relative paths, SHA-256 identities, and bytes, and submits one
bounded immutable prompt through the existing production exchange. The response
is one strict JSON object containing at least two unique, path-bound complete-file
candidates selected from the admitted sources. Proposal production never mutates
the repository.

`quartz --resume-proposals <session-dir>` reconstructs proposal state from the
ordered facts in `<session-dir>/session.qe` without credentials or another
exchange. It renders every candidate as an exact diff and identifies current,
superseded, rejected, interrupted, and completed state.

`quartz --revise-proposal <model> <session-dir> <index> <feedback-path>` records
one explicit bounded rejection and may emit one correction exchange for the
selected current generation. The correction is bound to the original admission,
model, source, before digest, rejected bytes, and feedback. Interrupted exchange
state remains interrupted/unknown and is never implicitly retried.

`quartz --promote-proposal <session-dir> <index>` is the user's exact approval of
the displayed current candidate. It publishes and retains that candidate through
the existing ABI 10 workspace, mutation-authority, and promotion-authority
contracts. Each file is an independent operation; Quartz claims no multi-file
transaction. Superseded, rejected, stale, or already consumed generations cannot
pass the promotion gate.

`quartz --run-approved-command <session-dir> -- <executable> [arg ...]` treats
the exact UTF-8 argument vector as a renewed user approval. Every current
candidate must already be promoted. Quartz synchronizes `CommandStarted` before
spawning the vector once in the canonical repository root, drains both pipes,
and synchronizes one bounded terminal result. A started-only attempt reconstructs
as interrupted/unknown, is never run again, and blocks all later session facts.
A later command is legal only after a completed nonterminal continuation and
requires another explicit approval with another monotonic attempt identity.

`quartz --continue-task <model> <session-dir>` consumes the latest unconsumed
finished command in sequence order and performs one bounded production exchange.
The exact response grammar is:

```text
PROPOSE <admitted-path-index>
<exact complete-file candidate bytes>
```

or:

```text
COMPLETE
<bounded final summary>
```

`PROPOSE` creates the next generation for one admitted source. After separate
review and promotion, the user may approve another command. `COMPLETE` is legal
only after a successful command and closes the session. Pending or interrupted
continuation state, explicit completion, a finished command awaiting
continuation, or an unpromoted current proposal blocks actions that would skip
that state.

## Native residency violation — Slice D

The repository-task orchestrator has no valid native-host residency
justification under `architecture/component-contract.md`. Today
`crates/quartz/src/main.rs` and `crates/quartz/src/proposals.rs` own product
behavior that belongs behind the public component contract:

- the proposal and continuation session state machine;
- strict model-response grammar and candidate validation;
- reconstruction of candidate generations from durable session facts;
- command and continuation dispatch policy;
- review presentation and action sequencing.

Credential custody, canonical filesystem admission, exact host process spawning,
and terminal input/output may remain resident under the recorded credential,
privileged-I/O, and terminal-I/O conditions. Slice D must move the state machine
and policy behind a component boundary, migrate every caller, and delete the
native implementation. It must first identify any missing ABI primitive exactly;
adding a broad orchestrator-specific host API is not an acceptable substitute.

## Superseded implementation decisions

Two current restrictions are implementation constraints, not product
invariants:

- Complete-file candidates and the two-or-three-source ceiling remain only until
  Slice C replaces them with digest-anchored ranged edits carrying a source
  digest, byte range, exact replacement bytes, and expected result digest.
- Host-only source selection remains only until Slice E admits a bounded manifest
  of canonical paths and digests and permits the model to select numeric manifest
  indices. The host continues to own every path identity; model-authored path
  strings and ambient filesystem discovery remain prohibited.

Both are clean cutovers. There are no compatibility shims, dual parsers, legacy
aliases, or permanent translation layers.

## Non-goals

- Tool invocation, model-selected executables or arguments, ambient shell
  authority, autonomous retry, or an open-ended conversation loop.
- Concurrent commands or continuations, background processes, package-manager
  authority, or a security-sandbox claim for approved child processes.
- Atomic multi-file publication, automatic edit approval, merge resolution,
  formatting, Git operations, or rollback of already promoted siblings.
- Kernel handover, kernel or WIT replacement, ACP, subagents, or streaming model
  responses.

## Invariants

- The public component ABI remains version 10. Kernel source and WIT do not gain
  repository-task policy.
- Every admitted source is a canonical regular file beneath one canonical
  repository root. Before bytes, byte length, and SHA-256 identity are exact and
  bounded.
- Initial responses, revisions, and continuations are strict bounded grammars.
  Invalid or ambiguous durable responses remain evidence and never trigger an
  automatic exchange retry.
- Candidate review precedes mutation approval. Promotion is a separate exact
  authority bound to source, before and result digests, operation identity,
  workspace index, and approving provider identity.
- Command approval binds the executable, every argument, working directory,
  repository identity, admitted-file identities, and attempt identity. The
  displayed and executed vectors are identical; no shell-string parsing occurs.
- The child inherits the user's ordinary environment and operating-system
  authority. The 120-second deadline bounds host waiting but does not undo child
  writes or descendants. Retained stdout and stderr are independently capped at
  32 KiB and truncation is explicit.
- `CommandStarted` is synchronized before spawn. `CommandFinished` binds that
  start and records exit code or signal, timeout or spawn failure, bounded
  output, truncation, duration, and post-command admitted-file identities.
- Continuation sequence `n` consumes the `n`th finished command. It can create
  proposal revision `n + 1`. Sequence gaps, swapped command evidence, orphaned
  artifacts, stale generations, and repository drift fail closed.
- A completed or interrupted external operation is never emitted again.
  Credential-free reconstruction performs no command or model call.
- Command facts, model facts, promotion commits, and source mutations are honest
  external emissions. Recovery withdraws live capabilities without claiming to
  erase committed history or retained promoted bytes.

## Session log contract

One `session.qe` file is the authoritative task history. It uses the existing
bounded `QUARTZE2` event framing and contains only
`quartz.session/fact@1` payloads. Monotonic fact IDs and checksummed frames
establish order. Every payload is strict, versioned, independently bounded, and
binds the identities and bytes needed to validate its predecessor.

The closed fact set is:

- initial proposal turn started and completed;
- proposal rejection and revision turn started and completed;
- exact candidate approval, promotion started, and promotion completed;
- approved command started and finished;
- continuation started and either completed with one proposal or completed the
  task with an explicit summary.

The session reducer consumes facts in ID order and is the only source of task
state. Proposal turns, generations, current selection, rejections, approvals,
promotions, command attempts, continuation sequences, and completion are never
inferred from cache filenames, directory contents, composition journals, event
payload caches, exchange ledgers, promotion journals, or mutation ledgers.
Those operation-specific ledgers may remain as bounded idempotency and recovery
evidence at privileged I/O boundaries, but they cannot authorize a task action
or supply missing session history.

Each external operation has a synchronized started fact before emission and at
most one exact terminal fact. A model turn, promotion, or command with a started
fact and no terminal fact derives as interrupted/unknown and is never emitted
again. Non-external decisions such as rejection and approval may be followed
only by their uniquely determined next fact. Invalid transitions, identity
reuse, sequence gaps, duplicate terminal facts, stale generations, and facts
after completion fail closed.

Prompt, response, feedback, command, result, and completion bytes live in fact
payloads. Files materialized beside the log are disposable caches: restart may
rebuild or replace them from facts, and deleting or adding one cannot change
derived state. Appending one fact synchronizes its complete frame before
returning. A torn final frame is removed on open; interior corruption fails
closed.

The cutover deleted task-state readers and writers for parallel proposal,
revision, command, and continuation journals. Operation journals remain only at
the model-exchange and repository-mutation boundaries. A child-process crash
harness aborts after one, two, and three returned session-log appends and proves
that every returned prefix reopens with its exact monotonic IDs. Reducer
contracts separately prove interrupted initial, revision, promotion, command,
and later-continuation operations remain terminal and cannot append later facts.

## Public contract

ABI 10 and WIT remain unchanged. The kernel exposes the existing event framing
to justified host storage code as `DurableEventLog`: open one admitted path
under explicit `Limits`, observe committed `EventRecord` values, and append one
exact event plus optional `DurablePayload`. It is a synchronized wrapper over
the same event-stream implementation, not another ledger format or product
state machine.

The repository task uses `quartz.session/fact@1` as its sole task event schema.
The existing `quartz.agent/repository-turn@2` component protocol and exchange
ledgers still bound actual model emission; ABI 10 workspace, callable mutation,
publication, and promotion capabilities still bound actual source mutation.
Their results enter task state only after an exact session fact commits.

Each command argument is non-empty and capped at 4 KiB; total argument bytes are
capped at 32 KiB. Candidate bytes are UTF-8 and capped at 32 KiB. Rejection
feedback and completion summaries are UTF-8 and capped at 4 KiB. Individual
session facts, the total session log, prompt, response, workspace, and
operation-ledger records remain independently bounded.

## Acceptance scenario

1. Admit a bounded task and repository sources, complete one credentialed
   proposal turn, and stop with every source unchanged.
2. Restart without credentials, reconstruct exact candidates, render their
   diffs, and separately promote the approved current generations.
3. Approve one exact command that fails; synchronize its start and terminal
   evidence and never run that attempt again.
4. Restart, consume the exact failure in one continuation, and accept one
   corrected generation for an admitted source.
5. Separately review and promote that correction, approve a second exact command
   that succeeds, and synchronize its terminal evidence.
6. Restart and consume the successful command in an explicit `COMPLETE` turn.
7. Restart without credentials and reconstruct proposals, revisions, promotions,
   both command attempts, both model decisions, and completion in causal order
   without repeating an external operation.
8. Reject stale promotion, post-completion command, post-completion revision,
   post-completion continuation, sequence tampering, and identity tampering.

## Verification

Focused contracts cover exact chronological reconstruction, failed-command
correction followed by successful completion, restart across repeated cycles,
every started-only external-operation class, sequence and terminal-identity
tampering, cache deletion, stale promotion, and post-completion closure.

The credentialed Slice B dogfood session emitted one failed command, one
reviewed and promoted correction, one successful command, and explicit
completion. Credential-free restart reconstructed both command results, both
model decisions, promotions, and completion after all materialized candidate,
prompt, response-metadata, and completion-summary caches were deleted. Attempts
to command, revise, promote, or continue after completion each exited 2 without
emitting work.

`cargo test -p quartz --bin quartz` passes 34 focused contracts.
`cargo test --workspace --all-targets` passes 93 tests across 12 suites. WIT,
component ABI 10, and module manifests are unchanged. The kernel adds only the
schema-agnostic `DurableEventLog` wrapper described in `durable-events.md`;
`quartz` adds a direct workspace `serde` dependency for strict session facts.
