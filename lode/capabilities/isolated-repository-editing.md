# Isolated repository editing

## Problem

Quartz can inspect immutable repository snapshots and call a production model, but no sandboxed component can prepare or publish a repository change. Giving a guest ambient filesystem or process authority would violate the component boundary, while writing directly during activation would leave no approval, source-identity guard, or honest recovery rule.

## Observable behavior

The host admits one exact regular file as a bounded mutable workspace. A sandboxed editor reads and changes only its indexed host-owned buffer. The source file remains unchanged until the editor invokes a committed `quartz.repository/mutation-authority@1` provider and publishes the exact approved workspace. Publication requires the admitted source digest, the admitted final digest, and a durable operation identity to match. The host records intent, verifies the live source digest, writes and synchronizes a same-directory temporary file, atomically renames it, synchronizes the directory, then records the terminal result.

A successful publication is a component-owned effect. Removing the editor restores the admitted original bytes only while the source still has the published digest. External source drift makes recovery fail closed and records an ambiguous outcome instead of overwriting unrelated work. Restart reconstructs an already-applied operation without publishing twice; a started operation with no terminal result is resolved only when the source has the exact result digest and is otherwise made durably ambiguous.

## Non-goals

- Directory trees, multiple-file transactions, renames, deletes, symlink targets, or permission changes.
- Shell commands, package managers, builds, tests, formatters, or generalized validation runners.
- Model-selected paths, ambient filesystem access, patch languages, merge conflict resolution, or Git commits.
- Automatic retry after an ambiguous mutation or automatic overwrite after source drift.
- Concurrent non-Quartz writers during the guarded replacement critical section; the admitted file must be exclusively writable by Quartz for that bounded operation.
- Treating durable mutation-ledger records as reversible context state.

## Invariants

- A workspace grant binds one canonical regular source file, one model-visible provenance label, one canonical mutation-ledger path, one non-zero stable operation identity, the admitted source SHA-256, the exact approved result SHA-256, and a positive byte bound.
- Declaration admission validates grant shape and canonical locations without reading mutable source bytes. Activation reads the source after any replaced generation has recovered, verifies the source digest and byte bound, and creates a private per-fiber buffer. Guest code receives only a numeric index and never a path.
- Workspace reads and writes require explicit manifest capabilities and are available only to the owning fiber during activation. Bounds are checked before every mutation; extending a buffer zero-fills only within the admitted limit, and a buffer mutation invalidates any previously staged approval.
- Publication requires activation state, one exact workspace index, a committed callable mutation-authority provider, and approval produced by that provider for the same operation and workspace index.
- Approval does not bypass host checks. The staged bytes must match the grant's exact result digest and the source must still match the admitted source digest before first publication.
- The mutation ledger synchronizes `started` before source replacement and `applied`, `reverted`, or `ambiguous` afterward. Operation reuse with different source, provenance, before digest, after digest, or bytes fails as a collision.
- Source replacement uses a same-directory temporary regular file, preserves the admitted file permissions, synchronizes the file, rechecks the live source digest, atomically renames the temporary file, and synchronizes the parent directory.
- An applied operation is reconstructed only when the source still matches the recorded result digest. Unless a separate durable promotion commit transferred ownership, recovery restores the recorded original bytes only from that same digest. A promoted operation reconstructs verification ownership and preserves that exact result instead. Any third digest is ambiguous and is never overwritten.
- Workspace buffers, staged approval, and workspace recovery effects are fiber-owned. Recovery withdraws them in LIFO order. Mutation-ledger records, promoted bytes, and a source change that cannot be safely restored remain honestly external.
- The kernel implements only bounded workspace mechanics, durable mutation identity, and capability enforcement. Editor and approval policy remain replaceable components.

## Public contract

The workspace boundary exposes five generic host imports:

- `workspace-len(index) -> s64` returns the owning fiber's bounded workspace length or a negative Quartz status;
- `workspace-byte(index, offset) -> s32` returns one workspace byte or a negative Quartz status;
- `workspace-set-len(index, length) -> s32` resizes only the private buffer within the grant bound;
- `workspace-write-byte(index, offset, value) -> s32` changes one existing private-buffer byte;
- `publish-workspace(index) -> s32` applies or reconstructs one approved durable publication.

`ComponentSpec::with_workspace_grants` supplies `WorkspaceGrant` values. `WorkspaceGrant::new` canonicalizes both locations and binds caller-supplied provenance, before/result digests, stable operation identity, and byte bound; `WorkspaceGrant::from_file` derives the before digest from the current source. Declaration staging does not freeze mutable bytes. Activation admits them only after prior-generation recovery. `Limits` independently bounds workspace grant count, per-grant bytes, total admitted bytes, and mutation-ledger record size.

The callable approval identity is `quartz.repository/mutation-authority@1`. Operation 1 receives the stable mutation identity and workspace index. Returning one stages approval only for that caller, provider fiber identity, operation, and index. Any later workspace mutation invalidates the approval. Replacing the authority invalidates the caller's committed view through ordinary lifecycle semantics.

## Acceptance scenario

1. Create one real temporary repository file containing the admitted original bytes and one mutation ledger outside the guest.
2. Admit a bounded workspace grant whose expected result digest names a deterministic replacement.
3. Activate a sandboxed editor against a denying authority. It may change its private buffer, but publication is denied and the source remains byte-for-byte unchanged.
4. Activate the editor against an approving authority. It writes the replacement, receives exact approval, and publishes once.
5. Reconstruct a fresh runtime with the same durable operation. It installs the publication inverse without replacing the source or appending a duplicate applied record.
6. Replace editor A with editor B. A's inverse restores the original before B's activation-time workspace admission; B then publishes through the same public capability.
7. Remove the editor. Recovery restores the original bytes, records the reversion, and withdraws all mutable workspace authority.
8. Exercise missing grants, byte limits, noncanonical traversal paths, approval denial, stale source content, operation collisions, and source drift during recovery. Every case fails closed without overwriting unadmitted bytes.
9. Remove all roots. Fibers, bindings, workspace buffers, approvals, inverses, and artifacts recover; mutation-ledger records remain external.

## Completion gate

Slice 7 closes only when focused contracts cover isolation bounds, approval denial, exact publication, durable reconstruction without duplicate mutation, stale-source rejection, collision handling, ambiguous recovery, and clean authority withdrawal; all existing contracts and Clippy remain clean; and the release executable exercises the real file path through sandboxed editor and authority components.
