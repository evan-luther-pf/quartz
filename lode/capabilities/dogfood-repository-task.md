# Dogfood repository task

## Problem

Quartz can produce durable model responses, review and promote repository edits,
run explicitly approved commands, and continue from their exact results. One
authoritative session log now binds those operations into a reconstructible
repository-task loop. The remaining architectural debt is product orchestration
resident in the native executable rather than a component.

## Observable behavior

`quartz task <model> <task-path> <session-dir> <source> <source> [source ...]
-- <executable> [arg ...]` is the sole public coordinator for this workflow. An
empty session runs the proposal operation; a retained session reconstructs facts
without repeating completed or interrupted emissions. The coordinator displays
current diffs, requests `approve` or `reject` for each unpromoted generation,
accepts one bounded UTF-8 feedback line for rejection, and invokes revision or
promotion. Once every current generation is promoted, it displays the exact
argument vector as JSON and requires a fresh `approve` before invoking the
command operation. It then invokes continuation and repeats review, promotion,
command approval, and continuation until `COMPLETE`, whose bounded summary is
printed. `stop` or terminal EOF exits without adding a fact. Invalid, stale,
pending, or interrupted state fails closed at the underlying operation boundary.

Proposal, reconstruction, revision, promotion, approved-command, and
continuation operations remain internal implementation boundaries used by the
coordinator; they are not separate CLI routes.

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

## Superseded implementation decision

Host-only source selection remains an implementation constraint, not a product
invariant. Slice E admits a bounded manifest of canonical paths and digests and
permits the model to select numeric manifest indices. The host continues to own
every path identity; model-authored path strings and ambient filesystem discovery
remain prohibited.

Complete-file candidates and the two-or-three-source ceiling were removed by
the ranged-edit cutover. There is no legacy parser, alias, or translation layer.
The later manifest change is likewise a clean cutover.

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
- A model-authored ranged edit contains exactly `source_sha256`, `byte_start`,
  `byte_end`, and `replacement`, plus `path` for initial and revision responses.
  Supplying `result_sha256` is an unknown-field error.
- The range endpoints are UTF-8 boundaries in the exact admitted source.
  Replacement bytes are UTF-8, may be empty, and must produce a changed,
  non-empty result within the source byte bound.
- The host materializes `source[..start] + replacement + source[end..]`, validates
  the result, and computes its SHA-256 before the edit enters proposal state.
  Review, durable identity, and promotion continue to bind that host-computed
  digest.
- Initial responses, revisions, and continuations use strict prompt schema 2.
  Invalid or ambiguous durable responses remain evidence and never trigger an
  automatic exchange retry.
- Candidate review precedes mutation approval. Promotion first requires the live
  source to retain the ranged edit's admitted source digest, then uses a separate
  exact authority bound to source and result digests, operation identity,
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
- Every completed proposal or correction generation has a monotonic per-proposal
  revision identity. A correction is exactly the rejected current revision plus
  one. A continuation proposal advances the selected proposal's current revision
  by one; a newly selected admitted path begins revision zero.
- Rejection and promotion bind the exact current proposal index, revision, and
  candidate digest. Superseded, promoted, pending, interrupted, mismatched, and
  post-completion generations fail closed.
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
- proposal rejection and revision turn started and completed, repeated in
  generation order as needed;
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
capped at 32 KiB. Each admitted source and materialized result is UTF-8 and
capped at 32 KiB; prompt size independently bounds the admitted file count.
Replacement bytes share the response bound and may be empty. Rejection feedback
and completion summaries are UTF-8 and capped at 4 KiB. Individual session
facts, the total session log, prompt, response, workspace, and operation-ledger
records remain independently bounded.

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

Focused contracts cover the shared initial, revision, and continuation ranged
grammar; host-computed result digest binding; rejection of model-authored result
digests, incorrect source digests, invalid ranges, UTF-8 splits, unchanged
results, and oversized results; exact chronological reconstruction, repeated
correction cycles, every started-only external-operation class, sequence and
identity tampering, stale promotion, and post-completion closure. The unified
`task` contract drives reject, correct, approve, promote, failing command,
continuation, reject, correct, approve, passing command, and `COMPLETE` through
the same operations while reconstructing session state between actions.

Prompt schema 1 sessions remain retained evidence but are not resumable after
the clean schema 2 cutover. Quartz does not reinterpret their model-authored
result digests.

`cargo test -p quartz --bin quartz` passes 39 focused contracts.
`cargo test --workspace --all-targets` passes 98 tests across 12 suites. Twenty
release runs measure 3.220 ms p50 cold readiness, 3.000 MiB p50 idle RSS, and a
14.173 MiB executable: respectively +3.4%, +3.2%, and -6.2% against the Slice 0
budget. The release profile strips symbols and uses thin LTO. WIT, component ABI
10, kernel source, and module manifests are unchanged.
