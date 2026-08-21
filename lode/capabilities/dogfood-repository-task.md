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

## Safe CLI dogfood

### Problem

The executable treats an empty argument list as permission to run the full
architectural acceptance scenario. It has no help or version surface, and each
scenario arm validates arguments independently. This makes the release binary
unsafe to invoke casually and difficult to discover.

### Observable behavior

`quartz` and `quartz --help` print the same concise usage and succeed.
`quartz --version` prints `quartz <package-version>`. The former default
scenario runs only through `--acceptance`. Existing internal scenario commands
retain their behavior, but every command rejects missing, malformed, or
trailing arguments with a command-specific error. README examples use the
explicit acceptance command.

### Non-goals

- New kernel, WIT, component, or dependency contracts.
- Renaming or removing internal scenario commands.
- A general CLI framework, completion generator, configuration file, or TUI.
- Model-selected paths or direct repository writes.

### Invariants

- The host admits `crates/quartz/src/main.rs` and `README.md` as the only
  mutable sources and `crates/quartz/Cargo.toml` as read-only package metadata.
- Two credentialed production turns retain separate bounded proposals, one per
  mutable source. The supervising harness may materialize exact candidate bytes
  in isolated staging, but publication still crosses the existing workspace,
  mutation-authority, and promotion-authority contracts.
- Review shows the exact candidate diff before approval. Each file is promoted
  independently; no multi-file transaction is claimed.
- Parser tests live in `main.rs`. The final task adds no files or dependencies.
- Validation uses the exact argv
  `["cargo", "test", "--workspace", "--all-targets"]` only after both
  promotions. Its bounded result becomes the next durable production turn.
- Failure evidence is shown to the user and model. Correction requires a new
  reviewed proposal and a separately approved command; there is no automatic
  retry.

### Acceptance scenario

1. Retain one production-model proposal for `main.rs` after inspecting all
   three admitted files.
2. Retain a second production-model proposal for `README.md` against the first
   proposal's declared CLI surface.
3. Materialize exact candidates outside the repository, show both diffs, and
   obtain explicit approval.
4. Promote both candidates through separate ABI 10 workspace grants and
   reconstruct each promotion in a fresh runtime.
5. Exercise empty, help, version, acceptance, missing-argument, trailing-
   argument, and preserved internal-command behavior.
6. Run the approved workspace test argv once and submit its bounded terminal
   evidence through a fresh production turn.
7. Remove temporary orchestration artifacts from the tracked tree while
   retaining resumable records under `.quartz/`.

### Verification

- Separate credentialed `gpt-5.4` turns retained the `main.rs` parser design and
  the matching README diff. Their exchange records remain under
  `.quartz/dogfood-cli-safety/`.
- The user reviewed and separately approved candidate digests
  `1e5cc1d1729044e85f64419ce83ed93e63a0c7c295786fc61d9d771db132b066`
  for `main.rs` and
  `606e20637b84c0b347bd236a57c037675528b9f260fc2a043750a630790fb51e`
  for README. ABI 10 workspace, mutation, and promotion authorities published
  each exact candidate and reconstructed it in a fresh runtime before clean
  authority withdrawal.
- `cargo test --workspace --all-targets` ran once after both promotions: 67
  tests passed across 12 suites. The bounded result entered a fresh durable
  production turn, and the model returned `VALIDATION PASSED.` No correction or
  retry ran.
- The release executable produced identical successful output for empty and
  `--help` invocations, printed exactly `quartz 0.1.0` for `--version`, preserved
  `--idle`, rejected missing, trailing, and unknown arguments with exit status
  2, and completed the full prior smoke only through `--acceptance`.
- ABI 10, the kernel, package dependencies, and the tracked file set are
  unchanged. Promotion, exchange, event, and mutation records remain under
  `.quartz/dogfood-cli-safety/` for inspection.
