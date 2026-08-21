use std::{collections::BTreeSet, fs, io::Read, sync::Arc};

use crate::{
    Error, HostCapability, Result,
    component::FiberId,
    fiber::{Core, InternalState},
    journal::{SnapshotGrant, sha256_hex},
    runtime::Runtime,
    wasm_host::{STATUS_INVALID, STATUS_UNDECLARED},
};

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
