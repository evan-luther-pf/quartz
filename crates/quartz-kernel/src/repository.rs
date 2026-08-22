use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeSet,
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::Arc,
};

use crate::{
    Error, HostCapability, Result,
    component::{FiberId, TraceEvent},
    fiber::{Core, InternalState, Inverse},
    journal::{
        MutationLedger, MutationLedgerIdentity, MutationLedgerOutcome, MutationLedgerRecord,
        SnapshotGrant, sha256_hex, valid_sha256,
    },
    runtime::Runtime,
    wasm_host::{
        STATUS_AMBIGUOUS, STATUS_COLLISION, STATUS_DENIED, STATUS_INVALID, STATUS_LIMIT, STATUS_OK,
        STATUS_STALE, STATUS_UNDECLARED,
    },
};

#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceGrant {
    pub source_path: PathBuf,
    pub ledger_path: PathBuf,
    pub operation: u64,
    pub provenance: String,
    pub before_sha256: String,
    pub result_sha256: String,
    pub dynamic: bool,
    pub max_bytes: usize,
}

impl WorkspaceGrant {
    pub fn new(
        source_path: impl AsRef<Path>,
        ledger_path: impl AsRef<Path>,
        operation: u64,
        provenance: impl Into<String>,
        before_sha256: impl Into<String>,
        result_sha256: impl Into<String>,
        max_bytes: usize,
    ) -> Result<Self> {
        let source_path =
            fs::canonicalize(source_path.as_ref()).map_err(|source| Error::WorkspaceIo {
                operation: "canonicalize source",
                path: source_path.as_ref().to_path_buf(),
                source,
            })?;
        let metadata = fs::symlink_metadata(&source_path).map_err(|source| Error::WorkspaceIo {
            operation: "inspect source",
            path: source_path.clone(),
            source,
        })?;
        if !metadata.file_type().is_file() {
            return Err(Error::WorkspaceIo {
                operation: "inspect source",
                path: source_path,
                source: std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "workspace source is not a regular file",
                ),
            });
        }
        let provenance = provenance.into();
        let before_sha256 = before_sha256.into();
        let result_sha256 = result_sha256.into();
        if operation == 0
            || max_bytes == 0
            || provenance.is_empty()
            || !valid_sha256(&before_sha256)
            || !valid_sha256(&result_sha256)
        {
            return Err(Error::Manifest(
                "workspace operation, provenance, byte bound, and digests must be valid".into(),
            ));
        }
        Ok(Self {
            source_path,
            ledger_path: canonical_output_path(ledger_path.as_ref())?,
            operation,
            provenance,
            before_sha256,
            result_sha256,
            dynamic: false,
            max_bytes,
        })
    }

    pub fn from_file(
        source_path: impl AsRef<Path>,
        ledger_path: impl AsRef<Path>,
        operation: u64,
        provenance: impl Into<String>,
        result_sha256: impl Into<String>,
        max_bytes: usize,
    ) -> Result<Self> {
        let source_path =
            fs::canonicalize(source_path.as_ref()).map_err(|source| Error::WorkspaceIo {
                operation: "canonicalize source",
                path: source_path.as_ref().to_path_buf(),
                source,
            })?;
        let bytes = read_workspace_source(&source_path, max_bytes)?;
        Self::new(
            source_path,
            ledger_path,
            operation,
            provenance,
            sha256_hex(&bytes),
            result_sha256,
            max_bytes,
        )
    }

    pub fn dynamic(
        source_path: impl AsRef<Path>,
        ledger_path: impl AsRef<Path>,
        provenance: impl Into<String>,
        max_bytes: usize,
    ) -> Result<Self> {
        let source_path =
            fs::canonicalize(source_path.as_ref()).map_err(|source| Error::WorkspaceIo {
                operation: "canonicalize source",
                path: source_path.as_ref().to_path_buf(),
                source,
            })?;
        let metadata = fs::symlink_metadata(&source_path).map_err(|source| Error::WorkspaceIo {
            operation: "inspect source",
            path: source_path.clone(),
            source,
        })?;
        if !metadata.file_type().is_file() || max_bytes == 0 {
            return Err(Error::Manifest(
                "dynamic workspace source and byte bound must be valid".into(),
            ));
        }
        let provenance = provenance.into();
        if provenance.is_empty() {
            return Err(Error::Manifest(
                "dynamic workspace provenance must not be empty".into(),
            ));
        }
        Ok(Self {
            source_path,
            ledger_path: canonical_output_path(ledger_path.as_ref())?,
            operation: 0,
            provenance,
            before_sha256: String::new(),
            result_sha256: String::new(),
            dynamic: true,
            max_bytes,
        })
    }
}

fn canonical_output_path(path: &Path) -> Result<PathBuf> {
    let name = path
        .file_name()
        .ok_or_else(|| Error::Manifest("mutation ledger path must name one file".into()))?;
    let parent = path
        .parent()
        .ok_or_else(|| Error::Manifest("mutation ledger path must have a parent".into()))?;
    let parent = fs::canonicalize(parent).map_err(|source| Error::MutationIo {
        operation: "canonicalize parent",
        path: parent.to_path_buf(),
        source,
    })?;
    let canonical = parent.join(name);
    match fs::symlink_metadata(&canonical) {
        Ok(metadata) => {
            if !metadata.file_type().is_file() {
                return Err(Error::MutationIo {
                    operation: "inspect",
                    path: canonical,
                    source: std::io::Error::new(
                        std::io::ErrorKind::InvalidInput,
                        "mutation ledger is not a regular file",
                    ),
                });
            }
            let existing = fs::canonicalize(&canonical).map_err(|source| Error::MutationIo {
                operation: "canonicalize",
                path: canonical.clone(),
                source,
            })?;
            if existing != canonical {
                return Err(Error::Manifest(
                    "mutation ledger path must not traverse a symlink".into(),
                ));
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(source) => {
            return Err(Error::MutationIo {
                operation: "inspect",
                path: canonical,
                source,
            });
        }
    }
    Ok(canonical)
}

fn read_workspace_source(path: &Path, max_bytes: usize) -> Result<Vec<u8>> {
    let file = fs::File::open(path).map_err(|source| Error::WorkspaceIo {
        operation: "open source",
        path: path.to_path_buf(),
        source,
    })?;
    let declared = usize::try_from(
        file.metadata()
            .map_err(|source| Error::WorkspaceIo {
                operation: "inspect source",
                path: path.to_path_buf(),
                source,
            })?
            .len(),
    )
    .unwrap_or(usize::MAX);
    if declared > max_bytes {
        return Err(Error::WorkspaceBytesLimit {
            actual: declared,
            limit: max_bytes,
        });
    }
    let read_limit = u64::try_from(max_bytes)
        .unwrap_or(u64::MAX)
        .saturating_add(1);
    let mut bytes = Vec::with_capacity(declared);
    file.take(read_limit)
        .read_to_end(&mut bytes)
        .map_err(|source| Error::WorkspaceIo {
            operation: "read source",
            path: path.to_path_buf(),
            source,
        })?;
    if bytes.len() > max_bytes {
        return Err(Error::WorkspaceBytesLimit {
            actual: bytes.len(),
            limit: max_bytes,
        });
    }
    Ok(bytes)
}

#[derive(Clone)]
pub(crate) struct PreparedWorkspace {
    pub(crate) grant: WorkspaceGrant,
    pub(crate) bytes: Arc<[u8]>,
}

#[derive(Clone, Copy)]
pub(crate) struct WorkspaceAuthorization {
    pub(crate) provider: FiberId,
    pub(crate) index: usize,
    pub(crate) operation: u64,
}

#[derive(Clone)]
pub(crate) struct PromotionAuthorization {
    pub(crate) provider: FiberId,
    pub(crate) index: usize,
    pub(crate) operation: u64,
    pub(crate) approver: String,
}

#[derive(Clone)]
pub(crate) struct PreparedSnapshot {
    pub(crate) grant: SnapshotGrant,
    pub(crate) bytes: Arc<[u8]>,
}

impl Runtime {
    pub(crate) fn prepare_snapshots(
        &self,
        component_path: &str,
        grants: &[SnapshotGrant],
    ) -> Result<Vec<PreparedSnapshot>> {
        if grants.len() > self.limits.max_snapshot_grants {
            return Err(Error::SnapshotGrantLimit {
                actual: grants.len(),
                limit: self.limits.max_snapshot_grants,
            });
        }
        let mut identities = BTreeSet::new();
        let mut snapshots = Vec::with_capacity(grants.len());
        for grant in grants {
            if grant.provenance.is_empty() || !identities.insert(grant.clone()) {
                return Err(Error::Manifest(format!(
                    "component `{component_path}` has an invalid or duplicate snapshot grant"
                )));
            }
            let canonical = fs::canonicalize(&grant.path).map_err(|source| Error::SnapshotIo {
                operation: "canonicalize",
                path: grant.path.clone(),
                source,
            })?;
            if canonical != grant.path {
                return Err(Error::Manifest(format!(
                    "component `{component_path}` snapshot path `{}` is not canonical",
                    grant.path.display()
                )));
            }
            let file = fs::File::open(&canonical).map_err(|source| Error::SnapshotIo {
                operation: "open",
                path: canonical.clone(),
                source,
            })?;
            let metadata = file.metadata().map_err(|source| Error::SnapshotIo {
                operation: "inspect",
                path: canonical.clone(),
                source,
            })?;
            if !metadata.is_file() {
                return Err(Error::SnapshotIo {
                    operation: "inspect",
                    path: canonical,
                    source: std::io::Error::new(
                        std::io::ErrorKind::InvalidInput,
                        "snapshot source is not a regular file",
                    ),
                });
            }
            let declared_len = usize::try_from(metadata.len()).unwrap_or(usize::MAX);
            if declared_len > self.limits.max_snapshot_bytes {
                return Err(Error::SnapshotBytesLimit {
                    actual: declared_len,
                    limit: self.limits.max_snapshot_bytes,
                });
            }
            let read_limit = u64::try_from(self.limits.max_snapshot_bytes)
                .unwrap_or(u64::MAX)
                .saturating_add(1);
            let mut bytes = Vec::with_capacity(declared_len);
            file.take(read_limit)
                .read_to_end(&mut bytes)
                .map_err(|source| Error::SnapshotIo {
                    operation: "read",
                    path: grant.path.clone(),
                    source,
                })?;
            if bytes.len() > self.limits.max_snapshot_bytes {
                return Err(Error::SnapshotBytesLimit {
                    actual: bytes.len(),
                    limit: self.limits.max_snapshot_bytes,
                });
            }
            let actual = sha256_hex(&bytes);
            if grant.byte_len != bytes.len() as u64 || grant.sha256 != actual {
                return Err(Error::SnapshotDigestMismatch {
                    path: grant.path.clone(),
                    expected: grant.sha256.clone(),
                    actual,
                });
            }
            snapshots.push(PreparedSnapshot {
                grant: grant.clone(),
                bytes: Arc::from(bytes),
            });
        }
        Ok(snapshots)
    }

    pub(crate) fn stage_workspaces(
        &self,
        component_path: &str,
        grants: &[WorkspaceGrant],
    ) -> Result<Vec<PreparedWorkspace>> {
        if grants.len() > self.limits.max_workspace_grants {
            return Err(Error::WorkspaceGrantLimit {
                actual: grants.len(),
                limit: self.limits.max_workspace_grants,
            });
        }
        let mut identities = BTreeSet::new();
        let mut total_bytes = 0_usize;
        let mut workspaces = Vec::with_capacity(grants.len());
        for grant in grants {
            if grant.provenance.is_empty()
                || grant.max_bytes == 0
                || grant.max_bytes > self.limits.max_workspace_bytes
                || (!grant.dynamic
                    && (grant.operation == 0
                        || !valid_sha256(&grant.before_sha256)
                        || !valid_sha256(&grant.result_sha256)))
                || (grant.dynamic
                    && (grant.operation != 0
                        || !grant.before_sha256.is_empty()
                        || !grant.result_sha256.is_empty()))
                || !identities.insert((
                    grant.source_path.clone(),
                    grant.ledger_path.clone(),
                    grant.operation,
                ))
            {
                return Err(Error::Manifest(format!(
                    "component `{component_path}` has an invalid or duplicate workspace grant"
                )));
            }
            total_bytes =
                total_bytes
                    .checked_add(grant.max_bytes)
                    .ok_or(Error::WorkspaceBytesLimit {
                        actual: usize::MAX,
                        limit: self.limits.max_workspace_bytes,
                    })?;
            if total_bytes > self.limits.max_workspace_bytes {
                return Err(Error::WorkspaceBytesLimit {
                    actual: total_bytes,
                    limit: self.limits.max_workspace_bytes,
                });
            }
            let canonical =
                fs::canonicalize(&grant.source_path).map_err(|source| Error::WorkspaceIo {
                    operation: "canonicalize source",
                    path: grant.source_path.clone(),
                    source,
                })?;
            if canonical != grant.source_path {
                return Err(Error::Manifest(format!(
                    "component `{component_path}` workspace source `{}` is not canonical",
                    grant.source_path.display()
                )));
            }
            let ledger_path = canonical_output_path(&grant.ledger_path)?;
            if ledger_path != grant.ledger_path {
                return Err(Error::Manifest(format!(
                    "component `{component_path}` mutation ledger `{}` is not canonical",
                    grant.ledger_path.display()
                )));
            }
            workspaces.push(PreparedWorkspace {
                grant: grant.clone(),
                bytes: Arc::from([]),
            });
        }
        Ok(workspaces)
    }

    pub(crate) fn prepare_workspaces(
        &self,
        component_path: &str,
        grants: &[WorkspaceGrant],
    ) -> Result<Vec<PreparedWorkspace>> {
        let mut workspaces = self.stage_workspaces(component_path, grants)?;
        for workspace in &mut workspaces {
            let grant = &workspace.grant;
            if grant.dynamic {
                workspace.bytes =
                    Arc::from(read_workspace_source(&grant.source_path, grant.max_bytes)?);
                continue;
            }
            let source_bytes = read_workspace_source(&grant.source_path, grant.max_bytes)?;
            let source_sha256 = sha256_hex(&source_bytes);
            let mut ledger = if grant.ledger_path.exists() {
                Some(MutationLedger::open(
                    &grant.ledger_path,
                    self.limits.max_mutation_record_bytes,
                )?)
            } else {
                None
            };
            let recovered = ledger
                .as_ref()
                .and_then(|ledger| ledger.lookup(grant.operation).cloned());
            let bytes = if let Some(record) = recovered {
                if record.source_path != grant.source_path
                    || record.provenance != grant.provenance
                    || record.before_sha256 != grant.before_sha256
                    || record.result_sha256 != grant.result_sha256
                    || record.before_bytes.len() > grant.max_bytes
                    || record.result_bytes.len() > grant.max_bytes
                {
                    return Err(Error::MutationCorrupt(format!(
                        "operation {} was reused with a different workspace grant",
                        grant.operation
                    )));
                }
                match record.outcome {
                    MutationLedgerOutcome::Started
                    | MutationLedgerOutcome::Applied
                    | MutationLedgerOutcome::PromotionIntent
                        if source_sha256 == grant.before_sha256 =>
                    {
                        ledger
                            .as_mut()
                            .expect("recovered mutation has a ledger")
                            .append_outcome(grant.operation, MutationLedgerOutcome::Reverted)?;
                    }
                    MutationLedgerOutcome::Started
                    | MutationLedgerOutcome::Applied
                    | MutationLedgerOutcome::PromotionIntent
                    | MutationLedgerOutcome::Promoted
                        if source_sha256 == grant.result_sha256 => {}
                    MutationLedgerOutcome::Reverted if source_sha256 == grant.before_sha256 => {}
                    MutationLedgerOutcome::Started
                    | MutationLedgerOutcome::Applied
                    | MutationLedgerOutcome::PromotionIntent
                    | MutationLedgerOutcome::Promoted => {
                        ledger
                            .as_mut()
                            .expect("recovered mutation has a ledger")
                            .append_outcome(grant.operation, MutationLedgerOutcome::Ambiguous)?;
                        return Err(Error::MutationAmbiguous(grant.source_path.clone()));
                    }
                    MutationLedgerOutcome::Reverted | MutationLedgerOutcome::Ambiguous => {
                        return Err(Error::MutationAmbiguous(grant.source_path.clone()));
                    }
                }
                record.before_bytes
            } else {
                if source_sha256 != grant.before_sha256 {
                    return Err(Error::WorkspaceDigestMismatch {
                        path: grant.source_path.clone(),
                        expected: grant.before_sha256.clone(),
                        actual: source_sha256,
                    });
                }
                source_bytes
            };
            workspace.bytes = Arc::from(bytes);
        }
        Ok(workspaces)
    }
}

impl Core {
    pub(crate) fn host_snapshot_len(&self, fiber: FiberId, index: u64) -> i64 {
        let Some(record) = self.fibers.get(&fiber) else {
            return -(STATUS_INVALID as i64);
        };
        if record.state != InternalState::Activating
            || !record
                .spec
                .artifact
                .manifest
                .requests(HostCapability::ReadSnapshot)
        {
            return -(STATUS_UNDECLARED as i64);
        }
        let Ok(index) = usize::try_from(index) else {
            return -(STATUS_INVALID as i64);
        };
        record
            .spec
            .snapshots
            .get(index)
            .map_or(-(STATUS_UNDECLARED as i64), |snapshot| {
                snapshot.bytes.len() as i64
            })
    }

    pub(crate) fn host_snapshot_byte(&self, fiber: FiberId, index: u64, offset: u64) -> i32 {
        let Some(record) = self.fibers.get(&fiber) else {
            return -STATUS_INVALID;
        };
        if record.state != InternalState::Activating
            || !record
                .spec
                .artifact
                .manifest
                .requests(HostCapability::ReadSnapshot)
        {
            return -STATUS_UNDECLARED;
        }
        let (Ok(index), Ok(offset)) = (usize::try_from(index), usize::try_from(offset)) else {
            return -STATUS_INVALID;
        };
        record
            .spec
            .snapshots
            .get(index)
            .and_then(|snapshot| snapshot.bytes.get(offset))
            .map_or(-STATUS_INVALID, |byte| i32::from(*byte))
    }
}

impl Core {
    pub(crate) fn host_workspace_len(&self, fiber: FiberId, index: u64) -> i64 {
        let Some(record) = self.fibers.get(&fiber) else {
            return -(STATUS_INVALID as i64);
        };
        if record.state != InternalState::Activating
            || !record
                .spec
                .artifact
                .manifest
                .requests(HostCapability::WorkspaceRead)
        {
            return -(STATUS_UNDECLARED as i64);
        }
        let Ok(index) = usize::try_from(index) else {
            return -(STATUS_INVALID as i64);
        };
        record
            .workspace_buffers
            .get(index)
            .map_or(-(STATUS_UNDECLARED as i64), |bytes| bytes.len() as i64)
    }

    pub(crate) fn host_workspace_byte(&self, fiber: FiberId, index: u64, offset: u64) -> i32 {
        let Some(record) = self.fibers.get(&fiber) else {
            return -STATUS_INVALID;
        };
        if record.state != InternalState::Activating
            || !record
                .spec
                .artifact
                .manifest
                .requests(HostCapability::WorkspaceRead)
        {
            return -STATUS_UNDECLARED;
        }
        let (Ok(index), Ok(offset)) = (usize::try_from(index), usize::try_from(offset)) else {
            return -STATUS_INVALID;
        };
        record
            .workspace_buffers
            .get(index)
            .and_then(|bytes| bytes.get(offset))
            .map_or(-STATUS_INVALID, |byte| i32::from(*byte))
    }

    pub(crate) fn host_workspace_set_len(
        &mut self,
        fiber: FiberId,
        index: u64,
        length: u64,
    ) -> i32 {
        let (Ok(index), Ok(length)) = (usize::try_from(index), usize::try_from(length)) else {
            return STATUS_INVALID;
        };
        let Some(record) = self.fibers.get_mut(&fiber) else {
            return STATUS_INVALID;
        };
        if record.state != InternalState::Activating
            || !record
                .spec
                .artifact
                .manifest
                .requests(HostCapability::WorkspaceWrite)
        {
            return STATUS_UNDECLARED;
        }
        let Some(workspace) = record.spec.workspaces.get(index) else {
            return STATUS_UNDECLARED;
        };
        if length > workspace.grant.max_bytes {
            return STATUS_LIMIT;
        }
        record.workspace_authorization = None;
        record.promotion_authorization = None;
        record.workspace_buffers[index].resize(length, 0);
        STATUS_OK
    }

    pub(crate) fn host_workspace_write_byte(
        &mut self,
        fiber: FiberId,
        index: u64,
        offset: u64,
        value: u32,
    ) -> i32 {
        let (Ok(index), Ok(offset), Ok(value)) = (
            usize::try_from(index),
            usize::try_from(offset),
            u8::try_from(value),
        ) else {
            return STATUS_INVALID;
        };
        let Some(record) = self.fibers.get_mut(&fiber) else {
            return STATUS_INVALID;
        };
        if record.state != InternalState::Activating
            || !record
                .spec
                .artifact
                .manifest
                .requests(HostCapability::WorkspaceWrite)
        {
            return STATUS_UNDECLARED;
        }
        record.workspace_authorization = None;
        record.promotion_authorization = None;
        let Some(bytes) = record.workspace_buffers.get_mut(index) else {
            return STATUS_UNDECLARED;
        };
        let Some(byte) = bytes.get_mut(offset) else {
            return STATUS_INVALID;
        };
        *byte = value;
        STATUS_OK
    }

    pub(crate) fn host_publish_workspace(&mut self, fiber: FiberId, index: u64) -> i32 {
        let Ok(index) = usize::try_from(index) else {
            return STATUS_INVALID;
        };
        let (grant, before_bytes, result_bytes) = {
            let Some(record) = self.fibers.get_mut(&fiber) else {
                return STATUS_INVALID;
            };
            if record.state != InternalState::Activating
                || !record
                    .spec
                    .artifact
                    .manifest
                    .requests(HostCapability::WorkspacePublish)
            {
                return STATUS_UNDECLARED;
            }
            let Some(workspace) = record.spec.workspaces.get(index) else {
                return STATUS_UNDECLARED;
            };
            let Some(authorization) = record.workspace_authorization.take() else {
                return STATUS_DENIED;
            };
            if authorization.index != index
                || authorization.operation != workspace.grant.operation
                || !record
                    .committed
                    .values()
                    .any(|committed| committed.fiber == authorization.provider)
            {
                return STATUS_DENIED;
            }
            let result_bytes = record.workspace_buffers[index].clone();
            if sha256_hex(&result_bytes) != workspace.grant.result_sha256 {
                return STATUS_INVALID;
            }
            (
                workspace.grant.clone(),
                workspace.bytes.to_vec(),
                result_bytes,
            )
        };

        apply_workspace_publication(self, fiber, grant, before_bytes, result_bytes)
    }

    pub(crate) fn host_publish_dynamic_workspace(
        &mut self,
        fiber: FiberId,
        index: u64,
        operation: u64,
    ) -> i32 {
        let Ok(index) = usize::try_from(index) else {
            return STATUS_INVALID;
        };
        let (template, result_bytes) = {
            let Some(record) = self.fibers.get_mut(&fiber) else {
                return STATUS_INVALID;
            };
            if record.state != InternalState::Activating
                || !record
                    .spec
                    .artifact
                    .manifest
                    .requests(HostCapability::WorkspaceDynamicPublish)
            {
                return STATUS_UNDECLARED;
            }
            let Some(workspace) = record.spec.workspaces.get(index) else {
                return STATUS_UNDECLARED;
            };
            if !workspace.grant.dynamic || operation == 0 {
                return STATUS_INVALID;
            }
            let Some(authorization) = record.workspace_authorization.take() else {
                return STATUS_DENIED;
            };
            if authorization.index != index
                || authorization.operation != operation
                || !record
                    .committed
                    .values()
                    .any(|committed| committed.fiber == authorization.provider)
            {
                return STATUS_DENIED;
            }
            (
                workspace.grant.clone(),
                record.workspace_buffers[index].clone(),
            )
        };
        let (grant, before_bytes, result_bytes) = match resolve_dynamic_workspace(
            &template,
            operation,
            result_bytes,
            self.limits.max_mutation_record_bytes,
        ) {
            Ok(workspace) => workspace,
            Err(error) => return workspace_status(&error),
        };
        apply_workspace_publication(self, fiber, grant, before_bytes, result_bytes)
    }

    pub(crate) fn host_promote_workspace(&mut self, fiber: FiberId, index: u64) -> i32 {
        let Ok(index) = usize::try_from(index) else {
            return STATUS_INVALID;
        };
        let (grant, before_bytes, result_bytes, approver, inverse_index, is_promoted) = {
            let Some(record) = self.fibers.get_mut(&fiber) else {
                return STATUS_INVALID;
            };
            if record.state != InternalState::Activating
                || !record
                    .spec
                    .artifact
                    .manifest
                    .requests(HostCapability::WorkspacePromote)
            {
                return STATUS_UNDECLARED;
            }
            let Some(workspace) = record.spec.workspaces.get(index) else {
                return STATUS_UNDECLARED;
            };
            let Some(authorization) = record.promotion_authorization.take() else {
                return STATUS_DENIED;
            };
            if authorization.index != index
                || authorization.operation != workspace.grant.operation
                || !record
                    .committed
                    .values()
                    .any(|committed| committed.fiber == authorization.provider)
            {
                return STATUS_DENIED;
            }
            let Some((inverse_index, is_promoted)) = record
                .accumulator
                .iter()
                .enumerate()
                .rev()
                .find_map(|(inverse_index, inverse)| match inverse {
                    Inverse::RestoreWorkspace { grant, .. } if grant == &workspace.grant => {
                        Some((inverse_index, false))
                    }
                    Inverse::VerifyPromotedWorkspace { grant, .. } if grant == &workspace.grant => {
                        Some((inverse_index, true))
                    }
                    _ => None,
                })
            else {
                return STATUS_DENIED;
            };
            (
                workspace.grant.clone(),
                workspace.bytes.to_vec(),
                record.workspace_buffers[index].clone(),
                authorization.approver,
                inverse_index,
                is_promoted,
            )
        };
        let promotion = promote_workspace(
            &grant,
            &before_bytes,
            &result_bytes,
            &approver,
            self.replaying,
            is_promoted,
            self.limits.max_mutation_record_bytes,
        );
        if let Err(error) = promotion {
            return workspace_status(&error);
        }
        if !is_promoted {
            let record = self
                .fibers
                .get_mut(&fiber)
                .expect("workspace owner checked above");
            let effect = record.accumulator[inverse_index].effect();
            record.accumulator[inverse_index] = Inverse::VerifyPromotedWorkspace {
                effect,
                grant,
                before_bytes,
                result_bytes,
                approver,
            };
        }
        STATUS_OK
    }

    pub(crate) fn host_promote_dynamic_workspace(
        &mut self,
        fiber: FiberId,
        index: u64,
        operation: u64,
    ) -> i32 {
        let Ok(index) = usize::try_from(index) else {
            return STATUS_INVALID;
        };
        let (grant, before_bytes, result_bytes, approver, inverse_index, is_promoted) =
            {
                let Some(record) = self.fibers.get_mut(&fiber) else {
                    return STATUS_INVALID;
                };
                if record.state != InternalState::Activating
                    || !record
                        .spec
                        .artifact
                        .manifest
                        .requests(HostCapability::WorkspaceDynamicPromote)
                {
                    return STATUS_UNDECLARED;
                }
                let Some(workspace) = record.spec.workspaces.get(index) else {
                    return STATUS_UNDECLARED;
                };
                if !workspace.grant.dynamic || operation == 0 {
                    return STATUS_INVALID;
                }
                let Some(authorization) = record.promotion_authorization.take() else {
                    return STATUS_DENIED;
                };
                if authorization.index != index
                    || authorization.operation != operation
                    || !record
                        .committed
                        .values()
                        .any(|committed| committed.fiber == authorization.provider)
                {
                    return STATUS_DENIED;
                }
                let Some((inverse_index, is_promoted, grant, before_bytes, result_bytes)) =
                    record.accumulator.iter().enumerate().rev().find_map(
                        |(inverse_index, inverse)| match inverse {
                            Inverse::RestoreWorkspace {
                                grant,
                                before_bytes,
                                result_bytes,
                                ..
                            } if grant.source_path == workspace.grant.source_path
                                && grant.operation == operation =>
                            {
                                Some((
                                    inverse_index,
                                    false,
                                    grant.clone(),
                                    before_bytes.clone(),
                                    result_bytes.clone(),
                                ))
                            }
                            Inverse::VerifyPromotedWorkspace {
                                grant,
                                before_bytes,
                                result_bytes,
                                ..
                            } if grant.source_path == workspace.grant.source_path
                                && grant.operation == operation =>
                            {
                                Some((
                                    inverse_index,
                                    true,
                                    grant.clone(),
                                    before_bytes.clone(),
                                    result_bytes.clone(),
                                ))
                            }
                            _ => None,
                        },
                    )
                else {
                    return STATUS_DENIED;
                };
                (
                    grant,
                    before_bytes,
                    result_bytes,
                    authorization.approver,
                    inverse_index,
                    is_promoted,
                )
            };
        if let Err(error) = promote_workspace(
            &grant,
            &before_bytes,
            &result_bytes,
            &approver,
            self.replaying,
            is_promoted,
            self.limits.max_mutation_record_bytes,
        ) {
            return workspace_status(&error);
        }
        if !is_promoted {
            let record = self
                .fibers
                .get_mut(&fiber)
                .expect("workspace owner checked above");
            let effect = record.accumulator[inverse_index].effect();
            record.accumulator[inverse_index] = Inverse::VerifyPromotedWorkspace {
                effect,
                grant,
                before_bytes,
                result_bytes,
                approver,
            };
        }
        STATUS_OK
    }
}

fn resolve_dynamic_workspace(
    template: &WorkspaceGrant,
    operation: u64,
    result_bytes: Vec<u8>,
    max_record_bytes: usize,
) -> Result<(WorkspaceGrant, Vec<u8>, Vec<u8>)> {
    if result_bytes.len() > template.max_bytes {
        return Err(Error::WorkspaceBytesLimit {
            limit: template.max_bytes,
            actual: result_bytes.len(),
        });
    }
    let existing = if template.ledger_path.exists() {
        MutationLedger::open(&template.ledger_path, max_record_bytes)?
            .lookup(operation)
            .cloned()
    } else {
        None
    };
    let (before_bytes, expected_result) = if let Some(record) = existing {
        if record.source_path != template.source_path
            || record.provenance != template.provenance
            || record.before_bytes.len() > template.max_bytes
            || record.result_bytes.len() > template.max_bytes
        {
            return Err(Error::MutationCorrupt(format!(
                "operation {operation} was reused with a different dynamic workspace"
            )));
        }
        (record.before_bytes, Some(record.result_bytes))
    } else {
        (
            read_workspace_source(&template.source_path, template.max_bytes)?,
            None,
        )
    };
    if expected_result
        .as_ref()
        .is_some_and(|expected| expected != &result_bytes)
    {
        return Err(Error::MutationCorrupt(format!(
            "operation {operation} was reused with different result bytes"
        )));
    }
    let mut grant = template.clone();
    grant.operation = operation;
    grant.before_sha256 = sha256_hex(&before_bytes);
    grant.result_sha256 = sha256_hex(&result_bytes);
    grant.dynamic = false;
    Ok((grant, before_bytes, result_bytes))
}

fn apply_workspace_publication(
    core: &mut Core,
    fiber: FiberId,
    grant: WorkspaceGrant,
    before_bytes: Vec<u8>,
    result_bytes: Vec<u8>,
) -> i32 {
    let publication = publish_workspace(
        &grant,
        &before_bytes,
        &result_bytes,
        core.limits.max_mutation_record_bytes,
    );
    let status = publication.as_ref().err().map(workspace_status);
    if let Some(ownership) = publication_ownership(
        &grant,
        &before_bytes,
        &result_bytes,
        core.limits.max_mutation_record_bytes,
    ) {
        let effect = core.allocate_effect();
        let inverse = match ownership {
            PublicationOwnership::Restore => Inverse::RestoreWorkspace {
                effect,
                grant,
                before_bytes,
                result_bytes,
            },
            PublicationOwnership::Promoted(approver) => Inverse::VerifyPromotedWorkspace {
                effect,
                grant,
                before_bytes,
                result_bytes,
                approver,
            },
        };
        core.fibers
            .get_mut(&fiber)
            .expect("workspace owner checked above")
            .accumulator
            .push(inverse);
        core.trace.push(TraceEvent::EffectApplied {
            fiber,
            effect,
            kind: "workspace-publication".into(),
        });
    }
    status.unwrap_or(STATUS_OK)
}

fn mutation_ledger_identity<'a>(
    grant: &'a WorkspaceGrant,
    before_bytes: &'a [u8],
    result_bytes: &'a [u8],
) -> MutationLedgerIdentity<'a> {
    MutationLedgerIdentity {
        source_path: &grant.source_path,
        provenance: &grant.provenance,
        before_sha256: &grant.before_sha256,
        result_sha256: &grant.result_sha256,
        before_bytes,
        result_bytes,
    }
}

fn publish_workspace(
    grant: &WorkspaceGrant,
    before_bytes: &[u8],
    result_bytes: &[u8],
    max_record_bytes: usize,
) -> Result<()> {
    let mut ledger = MutationLedger::open(&grant.ledger_path, max_record_bytes)?;
    let source_bytes = read_workspace_source(&grant.source_path, grant.max_bytes)?;
    let source_sha256 = sha256_hex(&source_bytes);
    let existing = ledger
        .record(
            grant.operation,
            mutation_ledger_identity(grant, before_bytes, result_bytes),
        )?
        .map(|record| record.outcome);
    match existing {
        None => {
            if source_sha256 != grant.before_sha256 {
                return Err(Error::WorkspaceDigestMismatch {
                    path: grant.source_path.clone(),
                    expected: grant.before_sha256.clone(),
                    actual: source_sha256,
                });
            }
            ledger.append_started(
                grant.operation,
                MutationLedgerRecord {
                    source_path: grant.source_path.clone(),
                    provenance: grant.provenance.clone(),
                    before_sha256: grant.before_sha256.clone(),
                    result_sha256: grant.result_sha256.clone(),
                    before_bytes: before_bytes.to_vec(),
                    result_bytes: result_bytes.to_vec(),
                    approver: None,
                    outcome: MutationLedgerOutcome::Started,
                },
            )?;
            if atomic_replace(
                &grant.source_path,
                result_bytes,
                grant.operation,
                &grant.before_sha256,
                grant.max_bytes,
            )
            .is_err()
            {
                let current = read_workspace_source(&grant.source_path, grant.max_bytes)
                    .map(|bytes| sha256_hex(&bytes));
                if current
                    .as_deref()
                    .is_ok_and(|digest| digest == grant.result_sha256)
                {
                    ledger.append_outcome(grant.operation, MutationLedgerOutcome::Applied)?;
                    return Ok(());
                }
                ledger.append_outcome(grant.operation, MutationLedgerOutcome::Ambiguous)?;
                return Err(Error::MutationAmbiguous(grant.source_path.clone()));
            }
            ledger.append_outcome(grant.operation, MutationLedgerOutcome::Applied)?;
            Ok(())
        }
        Some(MutationLedgerOutcome::Started) if source_sha256 == grant.result_sha256 => {
            ledger.append_outcome(grant.operation, MutationLedgerOutcome::Applied)
        }
        Some(
            MutationLedgerOutcome::Applied
            | MutationLedgerOutcome::PromotionIntent
            | MutationLedgerOutcome::Promoted,
        ) if source_sha256 == grant.result_sha256 => Ok(()),
        Some(
            MutationLedgerOutcome::Started
            | MutationLedgerOutcome::Applied
            | MutationLedgerOutcome::PromotionIntent
            | MutationLedgerOutcome::Promoted,
        ) => {
            ledger.append_outcome(grant.operation, MutationLedgerOutcome::Ambiguous)?;
            Err(Error::MutationAmbiguous(grant.source_path.clone()))
        }
        Some(MutationLedgerOutcome::Reverted) => Err(Error::WorkspaceDigestMismatch {
            path: grant.source_path.clone(),
            expected: grant.result_sha256.clone(),
            actual: source_sha256,
        }),
        Some(MutationLedgerOutcome::Ambiguous) => {
            Err(Error::MutationAmbiguous(grant.source_path.clone()))
        }
    }
}

enum PublicationOwnership {
    Restore,
    Promoted(String),
}

fn publication_ownership(
    grant: &WorkspaceGrant,
    before_bytes: &[u8],
    result_bytes: &[u8],
    max_record_bytes: usize,
) -> Option<PublicationOwnership> {
    if !read_workspace_source(&grant.source_path, grant.max_bytes)
        .is_ok_and(|bytes| sha256_hex(&bytes) == grant.result_sha256)
    {
        return None;
    }
    MutationLedger::open(&grant.ledger_path, max_record_bytes)
        .and_then(|ledger| {
            ledger
                .record(
                    grant.operation,
                    mutation_ledger_identity(grant, before_bytes, result_bytes),
                )
                .map(
                    |record| match record.map(|record| (record.outcome, &record.approver)) {
                        Some((
                            MutationLedgerOutcome::Started
                            | MutationLedgerOutcome::Applied
                            | MutationLedgerOutcome::PromotionIntent,
                            _,
                        )) => Some(PublicationOwnership::Restore),
                        Some((MutationLedgerOutcome::Promoted, Some(approver))) => {
                            Some(PublicationOwnership::Promoted(approver.clone()))
                        }
                        _ => None,
                    },
                )
        })
        .unwrap_or(None)
}

fn promote_workspace(
    grant: &WorkspaceGrant,
    before_bytes: &[u8],
    result_bytes: &[u8],
    approver: &str,
    replaying: bool,
    has_promoted_inverse: bool,
    max_record_bytes: usize,
) -> Result<()> {
    let mut ledger = MutationLedger::open(&grant.ledger_path, max_record_bytes)?;
    let record = ledger
        .record(
            grant.operation,
            mutation_ledger_identity(grant, before_bytes, result_bytes),
        )?
        .ok_or_else(|| Error::MutationCorrupt("promotion has no publication record".into()))?
        .clone();
    let source_bytes = read_workspace_source(&grant.source_path, grant.max_bytes)?;
    if sha256_hex(&source_bytes) != grant.result_sha256 {
        ledger.append_outcome(grant.operation, MutationLedgerOutcome::Ambiguous)?;
        return Err(Error::MutationAmbiguous(grant.source_path.clone()));
    }
    match record.outcome {
        MutationLedgerOutcome::Applied if !replaying && !has_promoted_inverse => {
            ledger.append_promotion_intent(grant.operation, approver.to_owned())?;
            ledger.append_outcome(grant.operation, MutationLedgerOutcome::Promoted)
        }
        MutationLedgerOutcome::Promoted
            if has_promoted_inverse && record.approver.as_deref() == Some(approver) =>
        {
            Ok(())
        }
        MutationLedgerOutcome::Promoted => Err(Error::MutationCorrupt(format!(
            "operation {} promotion approver changed",
            grant.operation
        ))),
        MutationLedgerOutcome::Applied | MutationLedgerOutcome::PromotionIntent => {
            Err(Error::MutationAmbiguous(grant.source_path.clone()))
        }
        MutationLedgerOutcome::Started
        | MutationLedgerOutcome::Reverted
        | MutationLedgerOutcome::Ambiguous => {
            Err(Error::MutationAmbiguous(grant.source_path.clone()))
        }
    }
}

pub(crate) fn recover_workspace_publication(
    grant: &WorkspaceGrant,
    before_bytes: &[u8],
    result_bytes: &[u8],
    max_record_bytes: usize,
) -> Result<()> {
    let mut ledger = MutationLedger::open(&grant.ledger_path, max_record_bytes)?;
    let outcome = ledger
        .record(
            grant.operation,
            mutation_ledger_identity(grant, before_bytes, result_bytes),
        )?
        .ok_or_else(|| Error::MutationCorrupt("publication inverse has no ledger record".into()))?
        .outcome;
    let source_bytes = read_workspace_source(&grant.source_path, grant.max_bytes)?;
    let source_sha256 = sha256_hex(&source_bytes);
    match outcome {
        MutationLedgerOutcome::Applied
        | MutationLedgerOutcome::Started
        | MutationLedgerOutcome::PromotionIntent
            if source_sha256 == grant.result_sha256 =>
        {
            match atomic_replace(
                &grant.source_path,
                before_bytes,
                grant.operation,
                &grant.result_sha256,
                grant.max_bytes,
            ) {
                Ok(()) => ledger.append_outcome(grant.operation, MutationLedgerOutcome::Reverted),
                Err(error) => {
                    let current = read_workspace_source(&grant.source_path, grant.max_bytes)
                        .map(|bytes| sha256_hex(&bytes));
                    if current
                        .as_deref()
                        .is_ok_and(|digest| digest == grant.before_sha256)
                    {
                        ledger.append_outcome(grant.operation, MutationLedgerOutcome::Reverted)
                    } else if current
                        .as_deref()
                        .is_ok_and(|digest| digest == grant.result_sha256)
                    {
                        Err(error)
                    } else {
                        ledger.append_outcome(grant.operation, MutationLedgerOutcome::Ambiguous)?;
                        Err(Error::MutationAmbiguous(grant.source_path.clone()))
                    }
                }
            }
        }
        MutationLedgerOutcome::Started if source_sha256 == grant.before_sha256 => {
            ledger.append_outcome(grant.operation, MutationLedgerOutcome::Reverted)
        }
        MutationLedgerOutcome::Reverted if source_sha256 == grant.before_sha256 => Ok(()),
        MutationLedgerOutcome::Applied
        | MutationLedgerOutcome::Started
        | MutationLedgerOutcome::PromotionIntent => {
            ledger.append_outcome(grant.operation, MutationLedgerOutcome::Ambiguous)?;
            Err(Error::MutationAmbiguous(grant.source_path.clone()))
        }
        MutationLedgerOutcome::Promoted => Err(Error::MutationCorrupt(
            "restoration inverse remained armed after promotion commit".into(),
        )),
        MutationLedgerOutcome::Reverted | MutationLedgerOutcome::Ambiguous => {
            Err(Error::MutationAmbiguous(grant.source_path.clone()))
        }
    }
}

fn promoted_chain_reaches<'a>(
    records: impl Iterator<Item = &'a MutationLedgerRecord> + Clone,
    source_path: &Path,
    first_result: &str,
    current: &str,
) -> bool {
    let mut digest = first_result.to_owned();
    let mut seen = BTreeSet::from([digest.clone()]);
    while digest != current {
        let mut successors = records.clone().filter(|record| {
            record.source_path == source_path
                && record.outcome == MutationLedgerOutcome::Promoted
                && record.before_sha256 == digest
        });
        let Some(successor) = successors.next() else {
            return false;
        };
        if successors.next().is_some() || !seen.insert(successor.result_sha256.clone()) {
            return false;
        }
        digest.clone_from(&successor.result_sha256);
    }
    true
}

pub(crate) fn verify_promoted_workspace(
    grant: &WorkspaceGrant,
    before_bytes: &[u8],
    result_bytes: &[u8],
    approver: &str,
    max_record_bytes: usize,
) -> Result<()> {
    let mut ledger = MutationLedger::open(&grant.ledger_path, max_record_bytes)?;
    let record = ledger
        .record(
            grant.operation,
            mutation_ledger_identity(grant, before_bytes, result_bytes),
        )?
        .ok_or_else(|| Error::MutationCorrupt("promotion inverse has no ledger record".into()))?
        .clone();
    if record.approver.as_deref() != Some(approver) {
        return Err(Error::MutationCorrupt(
            "promotion inverse does not match the durable approver".into(),
        ));
    }
    if record.outcome == MutationLedgerOutcome::Ambiguous {
        return Err(Error::MutationAmbiguous(grant.source_path.clone()));
    }
    if record.outcome != MutationLedgerOutcome::Promoted {
        return Err(Error::MutationCorrupt(
            "promotion inverse does not match the durable commit".into(),
        ));
    }
    let source_bytes = read_workspace_source(&grant.source_path, grant.max_bytes)?;
    let source_sha256 = sha256_hex(&source_bytes);
    if source_sha256 == grant.result_sha256
        || promoted_chain_reaches(
            ledger.records(),
            &grant.source_path,
            &grant.result_sha256,
            &source_sha256,
        )
    {
        return Ok(());
    }
    ledger.append_outcome(grant.operation, MutationLedgerOutcome::Ambiguous)?;
    Err(Error::MutationAmbiguous(grant.source_path.clone()))
}

fn atomic_replace(
    path: &Path,
    bytes: &[u8],
    operation: u64,
    expected_sha256: &str,
    max_bytes: usize,
) -> Result<()> {
    let metadata = fs::symlink_metadata(path).map_err(|source| Error::WorkspaceIo {
        operation: "inspect before replacement",
        path: path.to_path_buf(),
        source,
    })?;
    if !metadata.file_type().is_file() {
        return Err(Error::WorkspaceIo {
            operation: "inspect before replacement",
            path: path.to_path_buf(),
            source: std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "workspace source is not a regular file",
            ),
        });
    }
    let parent = path
        .parent()
        .ok_or_else(|| Error::Manifest("workspace source must have a parent directory".into()))?;
    let name = path
        .file_name()
        .ok_or_else(|| Error::Manifest("workspace source must name one file".into()))?;
    let temporary = parent.join(format!(
        ".{}.quartz-{}-{operation}.tmp",
        name.to_string_lossy(),
        std::process::id()
    ));
    let write_result = (|| {
        let mut file = fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temporary)
            .map_err(|source| Error::WorkspaceIo {
                operation: "create replacement",
                path: temporary.clone(),
                source,
            })?;
        file.set_permissions(metadata.permissions())
            .map_err(|source| Error::WorkspaceIo {
                operation: "preserve permissions",
                path: temporary.clone(),
                source,
            })?;
        file.write_all(bytes)
            .and_then(|()| file.sync_all())
            .map_err(|source| Error::WorkspaceIo {
                operation: "write and synchronize replacement",
                path: temporary.clone(),
                source,
            })?;
        let current = read_workspace_source(path, max_bytes)?;
        let actual = sha256_hex(&current);
        if actual != expected_sha256 {
            return Err(Error::WorkspaceDigestMismatch {
                path: path.to_path_buf(),
                expected: expected_sha256.into(),
                actual,
            });
        }
        fs::rename(&temporary, path).map_err(|source| Error::WorkspaceIo {
            operation: "publish replacement",
            path: path.to_path_buf(),
            source,
        })?;
        fs::File::open(parent)
            .and_then(|directory| directory.sync_all())
            .map_err(|source| Error::WorkspaceIo {
                operation: "synchronize repository directory",
                path: parent.to_path_buf(),
                source,
            })
    })();
    if write_result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    write_result
}

fn workspace_status(error: &Error) -> i32 {
    match error {
        Error::WorkspaceBytesLimit { .. } | Error::MutationRecordLimit { .. } => STATUS_LIMIT,
        Error::WorkspaceDigestMismatch { .. } => STATUS_STALE,
        Error::MutationCorrupt(_) => STATUS_COLLISION,
        Error::MutationAmbiguous(_) => STATUS_AMBIGUOUS,
        _ => STATUS_INVALID,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn promoted(path: &Path, before: &str, result: &str) -> MutationLedgerRecord {
        MutationLedgerRecord {
            source_path: path.to_owned(),
            provenance: "test".into(),
            before_sha256: before.into(),
            result_sha256: result.into(),
            before_bytes: Vec::new(),
            result_bytes: Vec::new(),
            approver: Some("approver".into()),
            outcome: MutationLedgerOutcome::Promoted,
        }
    }

    #[test]
    fn promoted_verification_accepts_only_a_contiguous_committed_chain() {
        let path = Path::new("/repository/source");
        let records = [
            promoted(path, "original", "first"),
            promoted(path, "first", "corrected"),
        ];

        assert!(promoted_chain_reaches(
            records.iter(),
            path,
            "first",
            "corrected"
        ));
        assert!(!promoted_chain_reaches(
            records.iter(),
            path,
            "first",
            "third-party"
        ));
    }
}
