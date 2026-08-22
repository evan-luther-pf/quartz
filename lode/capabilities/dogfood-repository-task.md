# Dogfood repository task

## Problem

Quartz must coordinate a bounded repository task without placing proposal grammar,
validation, command policy, continuation policy, or review order in the native
executable.

## Observable behavior

`quartz task <model> <task> <session> <source> <source> [source ...] -- <executable> [arg ...]`
admits one task, 2 through 64 canonical regular source files, and one exact argv.
The external `repository-task` WASM component reconstructs the unique next action
from one append-only event stream. The model never selects or changes argv.

The component:

1. requests strict schema-3 line-addressed edits;
2. validates and materializes each generation;
3. supports repeated rejection and correction;
4. separately reviews, publishes, and promotes each accepted generation;
5. requests renewed terminal approval for the same admitted argv before every
   command attempt;
6. commits command start before the host spawns the process and validates the
   exact terminal `CommandFinished` response;
7. requests a continuation after every terminal command result;
8. accepts a corrected ranged edit, or accepts `COMPLETE` only after success.

A failed command therefore cannot complete the task. A continuation correction
is reviewed and promoted before the next monotonic command attempt. Started-only
model, terminal, command, publication, or promotion operations remain terminal
unknown and are never retried.

## Outcome semantics

The repository-task root reaches successful `Active` quiescence only after an
explicit valid `COMPLETE` response following a successful command. User stop,
exchange authentication failure, request rejection, remote failure, empty
response, response limit, protocol failure, and ambiguity terminate activation
with distinct bounded generic failure categories.

The native `quartz task` path reads only the root's public `FiberState`; it does
not parse repository-task facts or infer product policy. It records the failed
state, performs ordinary persistent shutdown, then reports the category and
returns exit 2. Explicit completion returns exit 0. Diagnostics contain only
the category: never response bodies, headers, credentials, task text, or source
content.

A terminal exchange failure is synchronized in the exchange ledger before the
component observes it. Replay reconstructs the exact same category without
calling the adapter. A started-only exchange is durably closed as `ambiguous`
on replay and likewise emits no second external request. Both success and
failure shutdown recover every live fiber, capability, and accumulated effect.

## Proposal protocol

Schema-3 sessions are a clean cutover; retained schema-2 sessions are not
resumable. Every model prompt renders each admitted UTF-8 source under its
stable numeric path index with 1-based line numbers. Paths, source digests,
result digests, and byte offsets remain component-owned and never appear as
model-authored fields.

Initial responses contain only `proposals`. Each proposal contains exactly
`path_index`, inclusive `start_line`, inclusive `end_line`, and exact UTF-8
`replacement`. Revision responses contain only `start_line`, `end_line`, and
`replacement`; they remain bound to the rejected generation's path index.
Continuations are exactly
`PROPOSE <path-index>\n<line-edit JSON>` or
`COMPLETE\n<bounded summary>`.

The component maps the inclusive line range to the exact complete-line byte
span, including each selected line's existing terminator. Bytes outside that
span are unchanged. Materialization preserves whether the file ends in a
newline and does not normalize LF or CRLF bytes. Empty replacement is valid
only when the resulting file remains non-empty. The component computes full
source and result SHA-256 identities after materialization.

Admission rejects invalid path indices, zero, reversed, or out-of-range lines,
duplicate initial path indices, unknown or legacy fields, unchanged results,
invalid UTF-8, oversized results, source drift, non-monotonic revisions,
out-of-order facts, and post-completion facts.

## Boundary

The native executable owns only:

- canonical task, source, session, and argv admission;
- the credential-bearing OpenAI adapter;
- terminal byte I/O;
- exact approved child-process spawning and bounded output capture;
- durable journals and ledgers; and
- host-owned atomic workspace publication and promotion.

The command adapter receives the exact CLI argv as authority. It rejects any
component request whose argv differs, captures admitted source identities and
bounded UTF-8 bytes before and after execution, and returns one strict
`CommandFinished` payload. Capturing evidence is privileged I/O; interpreting
exit status and selecting the next task transition remain component policy.


`CommandFinished` contains exactly schema, kind, attempt, argv,
`command_started_sha256`, duration, exit code, signal, timeout and spawn status,
bounded stdout/stderr, and before/after repository identities. Each repository
identity binds the canonical root and every admitted path to status, length,
SHA-256, and UTF-8 content. Missing, extra, stale, reordered, or inconsistent
evidence is invalid.
The WIT ABI does not change: ABI 11 already exposes the bounded snapshot,
exchange, workspace, event, and callable primitives this workflow needs.
`CommandFinished` is a strict schema-1 application payload carried through the
existing exchange bytes; adding a repository-specific host import would move
policy into the privileged kernel without adding authority or safety.

Rust-produced WASI Preview 2 components receive no arguments, environment,
preopened filesystem, standard input, output, sockets, ambient credentials, or
repository authority.

## Durable ordering

Every model, terminal, and command request is first committed as a durable event.
The exchange ledger then records its stable invocation as started before calling
the adapter and records one terminal outcome. Replay resumes the same committed
fiber only after the corresponding response fact is durable. Publication and
promotion likewise require adjacent component-authored authorization facts and
host ledgers.

On restart the component validates the entire fact sequence and either derives
one unique owed action, reports a completed/stopped state, or fails closed. A
governed A-to-B artifact replacement uses the same facts and cannot repeat an
external effect.

## Bounds

- Task: 1 through 4 KiB UTF-8.
- Source and materialized result: 1 through 32 KiB UTF-8 each.
- Sources: 2 through 64; aggregate workspace authority at most 2 MiB.
- Model and terminal payloads: bounded by admitted exchange grants.
- Event payload and record counts: bounded by kernel limits.
- Argv: 1 through 1024 non-empty arguments, each at most 4 KiB and 32 KiB total.
- Command: 120-second deadline; stdout and stderr independently capped at 32 KiB.
- Completion and feedback: independently bounded.

Promoted bytes are honest external emissions. Recovery verifies their exact
committed result; it does not claim to erase them.

## Artifact deployment

Production resolves components from `components/` beside the Quartz executable.
`QUARTZ_COMPONENT_DIR` is the sole explicit development/test override. Cargo
builds and stages artifacts into that binary-adjacent directory but no absolute
`OUT_DIR` path is compiled into the executable. Copying the executable and that
directory forms a relocatable bundle.

## Acceptance

Component-focused contracts cover strict initial/revision/continuation grammar,
repeated corrections, exact argv preservation, failed-command correction through
successful explicit completion, restart at every external boundary, terminal
ambiguity, stale and tampered facts, A-to-B orchestrator replacement, and
relocated component discovery. The release acceptance path and a copied clean
bundle load real external components.

Outcome contracts deterministically fake every terminal exchange category, an
invalid model response, and user stop. They assert the root's generic failed
state, exit-2 result, safe terminal metadata, identical category after restart
with zero adapter calls, and clean shutdown. Explicit completion remains the
only exit-0 path.

Schema-3 provider dogfood identified the prior opaque failure exactly. At the
1,024-token ceiling, the terminal Response was
`incomplete:max_output_tokens`, usage was 4,672 tokens, and the response ID was
retained only as SHA-256. Quartz returned exit 2 after 8 seconds without review,
mutation, or command authority. The ceiling then rose deliberately to 4,096
while response-byte bounds stayed unchanged.

The one fresh 4,096-token run completed the provider exchange with 4,084 usage
tokens and a 1,919-byte response, then the component failed `protocol`: the
otherwise well-shaped two-proposal response selected README byte range
5362..5532 against a 4,782-byte admitted source. No review, mutation, promotion,
or command ran. Reopening the session returned the same failure in 0.59 seconds;
the provider ledger SHA-256 was unchanged, proving zero repeated model calls.

The first real schema-3 line-edit task produced valid proposals for both
admitted files, then required five reviewed corrections for exact wording and
paragraph spacing. Two accepted generations were published and promoted. The
exact `cargo test --workspace --all-targets` attempt ran once, timed out with
signal 9 after 223,009 ms, and returned no exit code. Although its captured
output showed the main binary's 20 tests passing, the complete all-target result
was not established. The model then returned `COMPLETE` against failed command
evidence; the component correctly rejected it as `protocol`.

That run lasted 417 seconds after credential entry: seven successful model calls
used 34,546 tokens; ten terminal exchanges recorded five rejections, two
candidate approvals, two promotion approvals, and one command approval; one
command ran. Reopening the failed session returned the same `protocol` result in
0.62 seconds. Model, terminal, command, and both mutation-ledger SHA-256 values
were unchanged, proving zero repeated effects. The accurate 21-line
documentation diff remains uncommitted because validation did not complete.

The credentialed smoke command is:

```sh
OPENAI_API_KEY="$OPENAI_API_KEY" target/release/quartz task gpt-5.4 \
  <task> <session> <source-a> <source-b> -- \
  /usr/bin/grep -q validated source-a.txt
```

One complete supervised run exercised rejection and corrected revision, two
initial promotions, an expected failed command, a promoted continuation
correction, renewed approval, a passing second attempt, explicit completion,
and a fresh-process final restart. It completed in 101 seconds with four
successful model exchanges totaling 4,455 tokens, nine terminal exchanges, and
two command exchanges. The final restart exited in 5.7 seconds; the durable
ledgers remained at those exact counts, proving no external operation repeated.
Deterministic fault-injection contracts separately cover process loss at every
external boundary, including reconstruction from a failed command into the
continuation correction.

## Non-goals

- Autonomous mode, model-selected tools or argv, ambient shell authority, path
  discovery, or package installation.
- Concurrent commands, automatic retries, unbounded dialogue, or multi-file
  atomic promotion.
- TUI, kernel handover, WIT self-replacement, or Slice E path selection.
