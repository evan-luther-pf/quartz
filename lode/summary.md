# Quartz

Quartz is a small native coding harness that can revise its own composition while it is running.

## Goal

Keep the idle path and interaction model as small and direct as Pi and fx while making DSH/Cordis-style spatiotemporal composability a hard invariant. Replaceability is a consequence; the product requirement is self-modification without stale state or broken dependents.

## Boundary

The host kernel is not the agent. It owns a unified context, tracked reversible effects, reactive dependencies, component lifecycles, declarative reconciliation, and code loading. Agent loops, providers, tools, persistence, policy, context maintenance, and interfaces are components.

A component may change the desired component tree through the same context it uses for every other effect. The loader reconciles that change, activates newly supported components, deactivates unsupported dependents before providers, and recovers removed effects in LIFO order.

## Implemented foundation

Slices 0 through 9 are complete. Quartz has a Rust context kernel and loads
every acceptance component as a Wasmtime component through the public WIT
contract. The runtime tracks structural inverses, resolves scalar and callable
dependencies by provider fiber identity, orders dependent recovery before
provider recovery, owns child registrations in parent accumulators, and
reconciles declared trees to quiescence.

A sandboxed controller can invoke a callable governor and select an explicit
host-admitted add, remove, or replacement grant against a composition revision.
Successful patches belong to the controller accumulator; denied, stale,
malformed, cancelled, and failed requests leave or restore the prior
composition.

A sandboxed persistence component can register one host-admitted composition
journal and one host-admitted event stream. Committed desired-tree snapshots
carry canonical artifact paths, SHA-256 digests, composition revisions,
committed patch inverses, next event identity, and the transactional event
outbox. Restart verifies the latest complete records, drains recovered event
requests idempotently, creates fresh fibers from the declaration, and preserves
committed patch inverses without replaying historical lifecycle emissions.
Authorized appenders emit typed facts only after activation commit. Facts retain
their scalar projection value and may carry one bounded, checksummed durable
payload. Projections reconstruct model-visible state through their committed
storage-provider view. Torn final writes are removed and interior corruption
fails closed.

A replaceable agent gateway, loop, deterministic provider, and read-only fixture
tool now complete a closed turn protocol from committed facts. Each restart
projects the exact transcript, derives one owed action, and commits at most one
new fact with a stable invocation identity. Provider failure preserves the
request for retry; an ambiguous non-idempotent call becomes
`interrupted/unknown`. A governed tool replacement changes the second turn
without rewriting the first.

Host-admitted snapshot grants bind a canonical regular-file path, provenance,
byte length, and SHA-256 identity. Sandboxed inspectors can read only their
immutable admitted bytes during activation. The replay-aware agent loop may
attach one admitted snapshot to a tool-result fact through the transactional
outbox. Every restart re-verifies source identity before activation. The release
scenario inspects real `README.md` and `lode/summary.md` bytes across two turns,
governedly replaces the inspector, preserves the first transcript exactly, and
recovers all live inspection authority on shutdown.

A production provider now implements the same `quartz.agent/provider@1`
callable as the deterministic provider. One explicit host adapter owns
credential-bearing network access; sandboxed code receives only indexed,
bounded exchange authority. The checksummed exchange ledger synchronizes
started and terminal outcomes by invocation identity and request digest,
reconstructs exact successes without another emission, and permanently blocks
automatic retry after any started outcome without success. The OpenAI adapter
starts a background Responses API request with `store: false`, polls under the
host deadline, and normalizes assistant text. Response, provenance, digest, and
usage enter the durable turn only after terminal ledger synchronization. Any
started exchange without success durably closes as `interrupted/unknown` and
then stop. Provider replacement and shutdown recover live exchange authority
while requests, temporary remote retention, billing, and ledger records remain
honestly external.

Host-admitted workspace grants bind one canonical regular source file, one
provenance label, bounded private bytes, exact before/result digests, a stable
operation identity, and a durable mutation ledger. Sandboxed editors mutate
only their indexed buffer and publish only after invoking a committed callable
authority that approves that exact operation and workspace. Publication records
intent, verifies the live source, atomically replaces it, and records the
outcome. Restart reconstructs an applied operation without publishing twice.
Editor replacement restores the prior generation before admitting the next
workspace, and removal restores the original only while the source retains the
published digest. Source drift and mutation collisions fail closed.

Committed event payloads are now available to explicitly authorized sandboxed
consumers through bounded length and byte reads over their committed
event-stream view. A proposal editor selects one durable model response by turn
identity, copies its exact bytes into a private workspace, and can publish only
after the host separately admits that candidate digest for one fixed source and
the callable mutation authority approves it. Proposal generation leaves the
repository untouched and may end in a different process generation from
application. Denial, wrong turn, missing payload, digest mismatch, and source
drift fail closed. Governed editor replacement recovers the old publication
before the new editor applies the same durable candidate.

Retention is a separate authority from mutation. A sandboxed promotion editor
must invoke a committed callable promotion provider whose approval is bound to
the exact operation, source and candidate digests, bytes, workspace index, and
approver fiber identity. The host synchronizes promotion intent before changing
recovery ownership and synchronizes the terminal promotion before restoration
is disarmed. Restart therefore restores an applied-only publication but
reconstructs and verifies a committed promotion without republishing. Denial,
cancellation, or failure before commit restores the original; failure after
commit preserves the candidate. Third-party drift remains durably ambiguous and
untouched.

Removing the application and persistence roots leaves no fibers, bindings,
state cells, child registrations, pending patches or events, composition
effects, desired roots, journal, event, or exchange registrations, outbox
entries, in-flight provider calls or exchange workers, staged responses,
workspace buffers or approvals, restoration or promoted-verification effects,
or live payload-read authority or module artifacts. Durable journal, event,
candidate, exchange, and mutation-ledger records remain honestly external.

The first dogfood repository task now crosses those boundaries without ABI or
kernel changes. Credentialed production turns inspected the two package
manifests and produced exact candidates adding `publish = false`. Review
rejected candidates that also removed a final newline; corrected candidates
were separately approved and durably promoted through existing workspace
grants. The approved argv `["cargo", "test", "--workspace", "--all-targets"]`
then passed all 62 contracts from the repository root. Its bounded exact result
became a durable production-turn prompt and the model classified it as passed.
No command runner, validation ledger, recorder component, retry protocol, or
security-sandbox claim was added.

The release CLI is now safe to discover: empty and `--help` invocations print
usage, `--version` prints the package version, and the full architectural smoke
runs only through `--acceptance`. One parser validates every preserved scenario
command and rejects malformed or trailing arguments. Two independently
reviewed candidates changed `main.rs` and README through the existing ABI 10
workspace and promotion authorities, with restart reconstruction proven for
each. The approved workspace test command passed 67 tests across 12 suites; its
bounded result entered a durable production turn and the model returned
`VALIDATION PASSED.` No kernel, dependency, or tracked-file addition was needed.

The production CLI now admits two or three exact UTF-8 repository files and one
task into a bounded immutable prompt, completes one credentialed model turn,
and accepts only a strict JSON response containing at least two unique
path-bound complete-file proposals. The durable prompt and assistant-message
events reconstruct byte-identical candidate caches without credentials or a
second exchange. The CLI renders exact bounded diffs, and each separately
approved proposal publishes through the existing ABI 10 workspace, mutation,
and promotion authorities with restart verification and clean withdrawal.

The live dogfood session inspected both package manifests in one `gpt-5.4`
turn. It reconstructed both pending candidates offline, rendered their exact
description-only diffs, and durably promoted each user-approved digest. The
workspace test command then passed 74 tests across 12 suites. No kernel, WIT,
package dependency, retry path, or atomic multi-file transaction was added.

Proposal sessions now support one explicit correction turn. A revision binds
the requested model, complete original admission, rejected proposal identity
and bytes, and bounded feedback into a second durable prompt. Restart
reconstructs the rejection, superseded generation, corrected current
generation, and unaffected siblings without credentials; rerunning a completed
revision emits no exchange. Promotion resolves only the current generation.

The correction dogfood run rejected the first of two package-description
proposals, committed exact feedback, and received one corrected candidate in a
second `gpt-5.4` turn. A credential-free process reconstructed both turn
histories and all proposal states. The user separately approved the correction
and unaffected sibling; ABI 10 promotion retained both exact digests after
restart and recovered cleanly. The workspace test command passed 78 tests
across 12 suites. No kernel, WIT, or dependency change was required.

Proposal sessions now extend through one bounded host-approved command and one
model continuation. The CLI synchronizes exact `CommandStarted` evidence before
spawning the user-approved argv once, then commits terminal status, bounded
stdout and stderr, timing, repository identity, and post-command admitted-file
identity as `CommandFinished`. Restart reconstructs started-only attempts as
interrupted/unknown and never reruns them. A continuation binds the terminal
command facts and current source bytes, then accepts only one path-indexed
complete-file correction or an explicit bounded completion summary. Command
success alone does not complete the task, and stale proposal generations remain
non-promotable.

The live correction session then ran the exact user-approved argv `cargo test -p
quartz --bin quartz` once and committed its successful 26-test output. After a
process restart, one `gpt-5.4` continuation received those exact facts and
returned explicit `COMPLETE` with a bounded summary. A later credential-free
process reconstructed the command and completion without rerunning either
external operation. The final workspace command passed 85 tests across 12
suites; kernel source, WIT, and ABI remained unchanged.

## Next boundary

The bounded production loop deliberately stops after one host-approved command
and one model decision. Quartz still has no model-selected tools, ambient shell
authority, autonomous retry, or general multi-turn conversation. Any broader
agent loop must preserve exact user authority, durable owed-work reconstruction,
and interrupted/unknown handling rather than bypassing them with an in-process
tool router.

## Non-goals

- Streaming production responses, automatic model retries, model-selected
  tools, or unbounded conversation/session payloads.
- Model-selected repository paths, directory mutation, or multi-file
  transactions.
- Automatic edit approval, diff parsing, merge resolution, formatting, or Git
  operations.
- TUI.
- Package installation or remote transport.
- Compatibility with the existing Quartz repository.

## Parking lot (donated from ~/quartz, archived 2026-08-21)

~/quartz is abandoned at `762ee84` with a clean tree except
`docs/acp-implementation-plan.md` (untracked). Do not implement these without
a measured product need:

- ACP v1 external protocol via `agent-client-protocol = "=1.3.0"`; v2 is
  draft and stays out. Relevant only if quartz-next gains an external client.
- Subagent/task-agent bootstrap (OMP-style task + IRC): revisit only after the
  single-agent loop wins its measured comparison.
- Pi-derived TS product surface: superseded; salvage ideas only.

Consolidation decision: all effort goes to ~/quartz-next; ~/quartz receives no
further commits except archival notes.
