# Dogfood repository task

## Problem

Quartz can produce durable model responses, review and promote repository edits,
run one explicitly approved command, and continue from its exact result. Those
parts now form one useful repository-task loop, but the implementation has two
architectural debts: session truth is split across parallel ledgers and filename
conventions, and product orchestration is resident in the native executable
rather than a component.

## Observable behavior

`quartz --propose <model> <task-path> <session-dir> <source> <source>
[source]` currently admits two or three exact UTF-8 repository files and one
bounded task. The host canonicalizes the sources beneath the repository root,
records their relative paths, SHA-256 identities, and bytes, and submits one
bounded immutable prompt through the existing production exchange. The response
is one strict JSON object containing at least two unique, path-bound complete-file
candidates selected from the admitted sources. Proposal production never mutates
the repository.

`quartz --resume-proposals <session-dir>` reconstructs proposal state from
committed facts without credentials or another exchange. It renders every
candidate as an exact diff and identifies current, superseded, rejected, pending,
interrupted, and completed state.

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
as interrupted/unknown and is never run again. Another command requires another
explicit approval and receives another monotonic attempt identity.

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
- reconstruction of candidate generations from durable facts and filenames;
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

## Current durable representation

The current implementation reconstructs one logical task from the proposal turn
journal, event stream, exchange ledger, revision and continuation journals,
command facts, promotion journals, mutation ledgers, and sequence-bearing cache
filenames. Checksums and cross-record identities reject many mismatches, but the
parallel ledgers and filename-derived state are not one authoritative session
history. Slice B replaces them with one append-only session log and derived
state. Crash injection must cover fsync boundaries; a started external operation
without a terminal fact remains interrupted/unknown.

## Public contract

No new kernel, WIT, event-schema, command-fact, or permanent command-runner
contract is introduced by the current loop. It composes:

- `quartz.agent/repository-turn@2` for bounded production exchanges;
- `quartz.command/approved@1` facts for exact command attempts;
- ABI 10 snapshot, payload-read, workspace, callable mutation, publication, and
  promotion capabilities.

Each command argument is non-empty and capped at 4 KiB; total argument bytes are
capped at 32 KiB. Candidate bytes are UTF-8 and capped at 32 KiB. Rejection
feedback and completion summaries are UTF-8 and capped at 4 KiB. Prompt,
response, payload, workspace, and ledger limits remain independently enforced.

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

The latest focused contracts cover failed-command correction followed by
successful completion, restart across repeated cycles, later-command and
later-continuation interruption, sequence and command identity tampering, stale
promotion, and post-completion closure. The credentialed dogfood scenario
exercised the full acceptance path through one failed command, one reviewed and
promoted correction, one successful command, explicit completion, and
credential-free reconstruction without rerunning either external operation.

`cargo test -p quartz --bin quartz` passes 28 focused contracts.
`cargo test --workspace --all-targets` passes 87 tests across 12 suites. Kernel
source, WIT, ABI, module manifests, and package dependencies were unchanged by
that implementation.
