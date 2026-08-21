# Durable edit promotion

## Problem

Quartz can durably publish a reviewed full-file candidate, but that publication remains owned by the editor fiber and is restored when the editor unloads. Approval to mutate a bounded workspace is not approval to retain that mutation beyond the component lifecycle. Quartz needs a separate durable promotion decision without making ordinary publication irreversible or granting sandboxed code ambient repository authority.

## Observable behavior

`publish-workspace` remains a reversible, component-owned effect. After publication, a sandboxed editor may invoke a distinct committed `quartz.repository/promotion-authority@1` provider and then request promotion of the same workspace. The host synchronizes promotion intent, synchronizes the committed promotion, and only then replaces the source-restoration inverse with non-mutating promoted-state verification. Workspace authority still withdraws during ordinary recovery, while approved source bytes remain.

A fresh runtime reconstructs the mutation ledger. An applied publication without a durable promotion commit retains restoration ownership. A committed promotion reconstructs verification ownership and leaves approved bytes intact. Denial, cancellation, activation failure, or process loss before the promotion commit restores the original when recovery runs. Process loss after the durable commit preserves the candidate. Any third source digest is ambiguous and untouched.

## Non-goals

- Making `publish-workspace` itself permanent or combining mutation and promotion approval.
- Validation commands, build or test execution, formatter authority, semantic review, or reviewable diff generation.
- Multi-file transactions, renames, deletes, directory mutation, Git staging, or commits.
- Automatic promotion, approval inferred from model output, or promotion without an exact host-admitted workspace.
- Overwriting third-party drift or claiming mutation-ledger facts are reversible context state.

## Invariants

- Promotion requires a successful live or reconstructed publication plus a separate committed callable promotion authority. Mutation approval cannot stage promotion approval.
- The staged promotion approval binds the caller fiber, provider fiber, stable operation identity, workspace index, and the approving provider artifact identity. Workspace mutation invalidates both mutation and promotion approval.
- The durable promotion identity includes the canonical source path, provenance, before and candidate SHA-256 digests, exact before and candidate bytes, operation identity, and stable approver artifact identity.
- The mutation ledger synchronizes `promotion-intent` before `promoted`. The restoration inverse remains armed until the `promoted` record is durable. Failure between those records therefore retains restoration ownership.
- A recovered `started`, `applied`, or `promotion-intent` publication is reversible. Recovery restores only from the exact candidate digest and records `reverted`.
- A recovered `promoted` publication never restores the source. It installs a non-mutating verification inverse so authority withdrawal still checks that the source is either the approved candidate or honest third-party drift.
- Promotion is idempotent only for the exact recorded approver and mutation identity. Reuse with another operation, path, provenance, digest, byte sequence, or approver fails closed.
- During persistent replay, an already committed promotion may reconstruct. A not-yet-committed promotion cannot be advanced by replay; activation fails and ordinary recovery restores the original.
- If the source differs from both admitted digests, Quartz records `ambiguous`, returns failure, and does not write it. This rule applies before promotion, during restoration, during promoted verification, and on restart.
- Successful shutdown removes fibers, bindings, workspace buffers, staged approvals, and publication effects. A promoted source remains byte-for-byte equal to the approved candidate; durable mutation records remain external.

## Public contract

ABI 10 adds one host capability and import:

- `promote-workspace(index) -> s32` consumes one exact staged promotion approval and durably commits retention of the indexed publication.

The callable approval identity is `quartz.repository/promotion-authority@1`. Operation 1 receives the stable mutation identity and workspace index. Returning one stages promotion approval only when that caller owns the matching publication. The host records the provider's stable module, version, and artifact digest as the approver identity; guest code cannot supply it.

`publish-workspace` keeps its ABI 9 behavior and always establishes restoration ownership unless it is reconstructing an already committed promotion. `promote-workspace` does not write source bytes. It validates the current source, exact ledger identity, staged approval, committed provider view, and restoration effect; appends and synchronizes promotion intent and commit; then converts that effect to promoted verification.

The mutation ledger uses these ordered outcomes:

- `started` before atomic source replacement;
- `applied` after the candidate is durable;
- `promotion-intent` after exact authority and identity validation;
- `promoted` before restoration ownership is disarmed;
- `reverted` after safe original-byte restoration; or
- `ambiguous` after unrecognized external state.

Valid forward paths are `started -> applied -> promotion-intent -> promoted`, restoration from any pre-promotion state to `reverted`, and ambiguity from any live state. `promoted` never transitions to `reverted`.

## Acceptance scenario

1. Produce and durably retain one reviewed candidate while the host-selected source remains original.
2. Reconstruct the candidate and publish it through the existing mutation authority. Verify publication alone is still restored on editor removal.
3. Run the same path with a denying promotion authority. Activation fails and recovery restores the original.
4. Cancel the promotion editor after publication but before promotion. Recovery restores the original.
5. Simulate process loss with an applied publication and no durable promotion commit. A fresh runtime reconstructs restoration ownership and restores the original during recovery.
6. Approve promotion with one exact authority. Verify intent is durable before commit and commit is durable before the restoration inverse changes ownership.
7. Simulate process loss after the durable promotion commit. A fresh runtime reconstructs promoted verification without republishing or restoring, and the candidate remains.
8. Attempt to reconstruct or promote the operation through a different approver identity. The mismatch fails closed without changing source bytes.
9. Introduce a third-party source digest after promotion. Verification and restart classify it as ambiguous and never overwrite it.
10. Remove all roots after an undrifted promotion. Context authority and effects recover cleanly while the exact approved candidate remains.

## Completion gate

Slice 9 closes only when focused contracts cover reversible publication, separate exact promotion approval, denial and cancellation, process-loss recovery on both sides of the durable commit, approver collision, promoted drift ambiguity, and clean shutdown with retained candidate bytes; all existing contracts and Clippy remain clean; and the release executable exercises a real promoted source across fresh runtime generations.
