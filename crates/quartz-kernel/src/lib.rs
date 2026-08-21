mod journal;
mod manifest;
mod module;
mod runtime;

pub use manifest::{
    ABI_VERSION, BindingKind, ComponentDeclaration, HostCapability, InterfaceId, Manifest,
    ProvidedBinding, RequiredBinding, Requirement,
};
pub use runtime::{
    ComponentSpec, ComponentTree, CompositionPatch, ContextObservation, FiberId, FiberState,
    Limits, Runtime, TraceEvent,
};

use std::path::PathBuf;
use thiserror::Error;

pub type Result<T, E = Error> = std::result::Result<T, E>;

#[derive(Debug, Error)]
pub enum Error {
    #[error("artifact ABI version {actual} is incompatible with host ABI {expected}")]
    AbiVersion { expected: u32, actual: u32 },
    #[error("artifact {0} does not contain a quartz:manifest custom section")]
    MissingManifest(PathBuf),
    #[error("invalid component manifest: {0}")]
    Manifest(String),
    #[error("failed to read artifact {path}: {source}")]
    ReadArtifact {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to parse component manifest: {0}")]
    ParseManifest(#[from] serde_json::Error),
    #[error("failed to parse WebAssembly component: {0}")]
    ParseComponent(String),
    #[error("failed to link WebAssembly component: {0}")]
    Link(String),
    #[error("component `{0}` is unavailable")]
    ComponentUnavailable(String),
    #[error("component tree contains duplicate entry `{0}`")]
    DuplicateEntry(String),
    #[error("component count {actual} exceeds limit {limit}")]
    ComponentLimit { actual: usize, limit: usize },
    #[error("component tree depth {actual} exceeds limit {limit}")]
    DepthLimit { actual: usize, limit: usize },
    #[error("component `{0}` declares an unbounded activation")]
    ActivationLimit(String),
    #[error("multiple components provide `{namespace}/{interface}` revision {revision}")]
    ProviderCollision {
        namespace: String,
        interface: String,
        revision: u32,
    },
    #[error("dependency graph contains a cycle: {0}")]
    DependencyCycle(String),
    #[error("reconciliation exceeded {0} steps")]
    ReconciliationLimit(usize),
    #[error("unknown component entry `{0}`")]
    UnknownEntry(String),
    #[error("component activation failed: {0}")]
    Activation(String),
    #[error("replacement failed and the prior composition was restored: {0}")]
    ReplacementRolledBack(String),
    #[error("invalid composition patch: {0}")]
    InvalidPatch(String),
    #[error("composition target `{0}` is owned by an unrecovered patch")]
    PatchTargetOwned(String),
    #[error("composition patch failed and the prior composition was restored: {0}")]
    PatchRolledBack(String),
    #[error("artifact digest mismatch for {path}: expected {expected}, found {actual}")]
    ArtifactDigestMismatch {
        path: PathBuf,
        expected: String,
        actual: String,
    },
    #[error("journal {operation} failed for {path}: {source}")]
    JournalIo {
        operation: &'static str,
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("composition journal is corrupt: {0}")]
    JournalCorrupt(String),
    #[error("journal record size {actual} exceeds limit {limit}")]
    JournalRecordLimit { actual: usize, limit: usize },
    #[error("persistent composition error: {0}")]
    Persistence(String),
    #[error("runtime invariant violated: {0}")]
    Invariant(String),
}
