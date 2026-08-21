# Dogfood repository task

## Problem

Quartz has credentialed model exchange, immutable repository snapshots, reviewed single-file publication, and durable promotion, but those capabilities have not completed one useful repository task together. Building a validation transaction protocol before that path exists would optimize an unproven harness.

## Observable behavior

One credentialed production-model run inspects host-selected repository bytes and produces exact reviewed candidates. Quartz shows each candidate as a diff before asking the user for approval. Approval applies and durably promotes each file through the existing workspace, mutation-authority, and promotion-authority contracts. Multiple files are changed as separate exact single-file operations; Quartz does not claim transactionality across them.

After promotion, the user may approve one command represented as an explicit argument vector. The supervising harness runs that ordinary child process with the repository root as its working directory, a fixed timeout, and bounded stdout and stderr capture. It never retries. The exact command and terminal result become the next durable production-model prompt so failure evidence reaches both the user and the model before any correction is proposed.

## Non-goals

- A validation kernel primitive, ABI change, distributed transaction, automatic rollback, or automatic retry.
- Shell command text, implicit shell startup, model-selected execution, package-manager authority, or ambient command authority for guest code.
- A security sandbox. The approved process runs with the Quartz user's ordinary operating-system authority.
- Atomic multi-file edits, automatic approval, or treating a passing command as permission to mutate more files.

## Invariants

- The public component ABI remains version 10.
- A model response never selects a repository path. The host supplies each canonical source path, exact before and candidate digests, byte bound, mutation identity, and durable ledgers.
- Candidate review precedes mutation approval. Promotion remains a separate exact authority and retains each approved file independently.
- A command is `[program, arg0, ...]`, never shell text. The displayed and executed vectors are identical.
- Command approval is explicit and single-use. Cancellation executes nothing. A recorded terminal result is never retried automatically.
- The child working directory is the canonical repository root. The process inherits the user's ordinary environment and operating-system authority; Quartz makes no containment claim.
- Timeout bounds host waiting; it does not claim to undo writes or subprocesses the command may already have created.
- Retained stdout and stderr are bounded, and truncation is explicit in the result supplied to the model.
- The command specification and result reuse the existing production-turn event stream as the next prompt. No command ledger or recorder component is introduced before dogfood demonstrates a need.
- Validation happens only after reviewed bytes are promoted. A non-zero exit or timeout is evidence presented to the user and model, not a trigger for repository rollback.

## Public contract

No kernel, WIT, component, or permanent command-runner contract is added. ABI
10 remains frozen. The first dogfood task is supervised acceptance work:
existing production exchange creates durable candidates; existing workspace
and promotion contracts retain approved files; the host harness executes one
user-approved argv and supplies its bounded result through the existing
production-model prompt path.

## Acceptance scenario

1. Make a credentialed production-model call over exact real repository file bytes.
2. Produce exact candidates for at least two host-selected files and stop with both sources unchanged.
3. Render both diffs. Obtain explicit approval for each and durably promote both through existing workspace grants.
4. Approve one explicit build/test argv and run it once from the repository root with bounded time and output.
5. Persist the exact command and result as the next production-turn prompt, show the result to the user, and submit that evidence to the production model.
6. If the command failed, review and approve exact corrective candidates before one new explicitly approved command. Never retry the prior execution automatically.
7. Exit with promoted files retained, durable model/edit/result-turn records present, and no live component authority.

## Verification

- Credentialed `gpt-5.4` production turns inspected the exact workspace and
  package manifests. Both initial candidates were review-rejected because they
  removed the source file's final LF; new explicit turns produced byte-preserving
  candidates.
- The user approved one-line diffs for `crates/quartz/Cargo.toml` and
  `crates/quartz-kernel/Cargo.toml`. Existing workspace grants, mutation
  authority, and promotion authority retained both changes as separate
  operations.
- The exact approved argv
  `["cargo", "test", "--workspace", "--all-targets"]` ran once from the
  repository root with a 120-second timeout and 8 KiB retained per stream. It
  exited zero in 11.844 seconds; all 62 tests passed and neither stream was
  truncated.
- The 7,049-byte command/result prompt entered a fresh durable production turn.
  The credentialed model returned `VALIDATION PASSED.` No corrective edit or
  retry was necessary.
- The executable and ABI remain unchanged. Session journals, event streams,
  exchange ledgers, mutation ledgers, prompts, and bounded command evidence
  remain under `.quartz/dogfood-publish-private/` for resume and inspection.
