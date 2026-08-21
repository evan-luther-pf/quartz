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

## Resumable multi-proposal orchestration

### Problem

The production path accepts one prebuilt prompt and returns one opaque response.
It cannot yet admit a small set of repository files, let the model choose among
only those files, validate multiple path-bound candidates, or reconstruct those
pending proposals without another model call.

### Observable behavior

`quartz --propose <model> <task-path> <session-dir> <source> <source>
[source]` admits two or three UTF-8 repository files and one task. The host
builds one bounded immutable prompt snapshot containing exact relative paths,
before digests, and bytes. The existing sandboxed production client submits
that snapshot through the existing durable turn and exchange path. A successful
response contains two or three complete-file proposals selected only from the
admitted paths.

Quartz validates the strict response shape, path membership, uniqueness, exact
before digests, candidate bounds, and changed bytes before materializing
derived candidate files under the session directory.
`quartz --resume-proposals <session-dir>` reconstructs the same candidates from
the durable response event without credentials or another exchange.
`quartz --promote-proposal <session-dir> <index>` is the explicit approval for
one displayed candidate and promotes only that candidate through the existing
workspace, mutation-authority, and promotion-authority contracts.

### Non-goals

- A new kernel capability, ABI revision, prompt framework, tool registry, or
  provider protocol.
- Model-selected filesystem access, ambient directory traversal, automatic
  diff approval, or automatic promotion.
- Atomic multi-file publication, rollback of an already promoted sibling, merge
  resolution, retries, or validation-command orchestration.
- More than three admitted files or binary repository content.

### Invariants

- ABI 10 and all kernel contracts remain unchanged.
- The host canonicalizes each source under the repository root, rejects
  duplicates, and records its exact relative path, SHA-256, and bytes before the
  model request.
- The generated prompt, each source, the response, and each candidate are
  independently bounded. Guest code receives one numeric snapshot grant and no
  path or ambient filesystem authority.
- The response is one strict JSON object with a `proposals` array. Each proposal
  has exactly `path`, `before_sha256`, and `content`; at least two unique
  admitted paths must be present.
- The durable assistant-message payload is the source of truth. Candidate files
  are reconstructible caches and may be replaced only from that payload.
- Invalid or ambiguous responses remain durable evidence and never trigger an
  automatic model retry.
- Promotion is per proposal. The approval command binds the admitted canonical
  source, exact before and result digests, positive operation identity, byte
  bound, and separate durable mutation ledger.
- Model exchange, event facts, promotion commits, and approved source bytes are
  external emissions. Component recovery withdraws live authority without
  claiming to erase them.

### Public contract

No WIT or kernel API changes. One new sandboxed promoter component composes
existing snapshot reads, workspace buffering, callable mutation approval,
publication, and promotion. The executable owns bounded admission, strict JSON
validation, candidate reconstruction, explicit command dispatch, and review
presentation.

### Acceptance scenario

1. Admit one task plus two exact real repository files into a fresh bounded
   session.
2. Complete one credentialed production turn returning two valid path-bound
   complete-file proposals.
3. Terminate, reopen without credentials, and reconstruct byte-identical
   candidates from the durable response event without another exchange.
4. Render both exact diffs and obtain separate explicit user approval.
5. Promote each approved candidate independently through ABI 10 authorities.
6. Reopen each promotion runtime, verify the exact candidate remains published,
   then withdraw all live authority cleanly.
7. Retain the production turn, proposal materialization metadata, and promotion
   ledgers under `.quartz/`; add no kernel code or package dependency.

### Verification

- One credentialed `gpt-5.4` turn admitted `crates/quartz/Cargo.toml` and
  `crates/quartz-kernel/Cargo.toml`, returned two valid complete-file
  candidates, and left both sources unchanged.
- A credential-free process reconstructed proposal digests
  `c3c1fdf1131958957b50bb308de289ff921acfce98e8014e6a7b71c071371c68`
  and
  `93f7f6676a52375f12a4c43dcf0ebf0a50682beae14944863ea9f67e2ba28532`
  from the durable turn and rendered exact description-only diffs.
- The user approved each diff separately. Independent promotion commands
  published each exact digest, reconstructed it after restart, and withdrew to
  a clean context while retaining the approved bytes.
- `cargo test -p quartz --bin quartz` passed 15 focused contracts.
  `cargo test --workspace --all-targets` passed 74 tests across 12 suites.
  The production turn, derived proposal metadata and candidates, and separate
  promotion journals and mutation ledgers remain under
  `.quartz/dogfood-multi-proposal/`.

## Resumable proposal correction

### Problem

A completed proposal session reconstructs exact candidates, but its production
conversation ends after the first response. Rejecting one candidate loses the
feedback at the process boundary, and a replacement would require an unrelated
new turn with no durable binding to the original admission or response.

### Observable behavior

`quartz --revise-proposal <model> <session-dir> <index> <feedback-path>`
records one explicit rejection and runs at most one follow-up model turn for
that session. The bounded revision prompt contains the requested model, exact
original task and admitted file snapshot set, selected proposal index and
bytes, prior result digest, and exact UTF-8 feedback. The response must contain
one complete-file replacement for that same admitted path and before digest.

`quartz --resume-proposals <session-dir>` reconstructs the original and
revision turns without credentials. It renders the rejected generation as
superseded, the corrected generation as current, and every unaffected proposal
as current. `--promote-proposal` resolves only the current generation; a
rejected proposal with no completed correction cannot be promoted.

### Non-goals

- More than one follow-up turn per session, automatic retries, command-result
  ingestion, validation orchestration, or an open-ended conversation format.
- Model-selected paths or tools, automatic approval, atomic multi-file
  publication, merge handling, or rollback of promoted siblings.
- A new kernel capability, WIT revision, provider registry, or prompt
  framework.

### Invariants

- The original `turn.qj`, event stream, exchange ledger, admission prompt, and
  proposal caches remain unchanged. Revision 1 uses separate prompt, journal,
  event-stream, exchange-ledger, metadata, and candidate paths.
- The revision prompt's committed user-prompt payload is the durable rejection
  and feedback record. The assistant payload is the corrected-candidate source
  of truth. Files beside the ledgers are reconstructible caches.
- Revision parsing revalidates the complete original admission, its prompt
  digest, selected index, path, before digest, prior result digest and bytes,
  feedback bound, model identity, and exact response shape.
- The correction must differ from both the admitted source and rejected
  generation. Unaffected proposal bytes and identities cannot change.
- A started revision exchange without a durable successful result remains
  `interrupted/unknown`; reopening never emits it again. No correction is
  materialized and the rejected generation remains non-promotable.
- Revision request snapshots and payloads are independently capped at 256 KiB;
  feedback is capped at 4 KiB; response and candidate limits remain 64 KiB and
  32 KiB. OpenAI's 1,024-output-token cap remains unchanged.
- Rejection, feedback, exchange, and promotion records are irreversible
  external facts. Recovery closes live authority without deleting history.

### Public contract

No WIT or kernel API changes. The executable composes a second instance of the
existing production turn for the revision and gives it a separate durable
ledger set. Existing ABI 10 promotion resolves the reconstructed current
candidate path and otherwise remains unchanged.

### Acceptance scenario

1. Produce two proposals from one fresh credentialed session and leave both
   admitted sources unchanged.
2. Persist explicit bounded feedback rejecting one displayed candidate.
3. Complete one credentialed revision turn for only that candidate.
4. Terminate and reconstruct the original admission, rejection, feedback,
   superseded candidate, corrected candidate, and unaffected sibling without
   credentials or another exchange.
5. Obtain separate approval for the corrected candidate and unaffected sibling,
   then promote each through existing ABI 10 authorities.
6. Reopen both promotion runtimes, verify exact retained bytes, recover all live
   authority, and retain the two turn histories and promotion ledgers.

### Verification

- One initial `gpt-5.4` turn proposed description-only changes for
  `crates/quartz/Cargo.toml` and `crates/quartz-kernel/Cargo.toml` without
  changing either source. Proposal 0 digest
  `1e0d2282b462277c0bc3e278ec301a84b6a201d546e669fba7a5cbbcb9e601fd`
  was rejected with exact bounded wording feedback.
- One revision turn retained that feedback in its committed prompt and produced
  corrected proposal 0 digest
  `29c35546dc921646c7a07da1a1738a88e84632a089fd30613842582ee2c003f7`.
  A credential-free process reconstructed the superseded and corrected
  generations, unaffected proposal 1 digest
  `fec6d4412890f0393c8ceb450c13360b08baec5174aa6911b5b5b5836969e1c2`,
  and exact diffs. Reinvoking the completed revision emitted no exchange.
- The user separately approved the corrected proposal and unaffected sibling.
  Independent ABI 10 promotions reconstructed both retained digests after
  restart and recovered to clean contexts.
- `cargo test -p quartz --bin quartz` passed 19 focused contracts.
  `cargo test --workspace --all-targets` passed 78 tests across 12 suites.
  Initial and revision prompts, journals, event streams, exchange ledgers,
  derived generation metadata and candidates, and promotion ledgers remain
  under `.quartz/dogfood-proposal-correction/`. No kernel, WIT, component
  manifest, or package dependency changed.
