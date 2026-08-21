use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use crate::{
    composition::CompositionPatch,
    exchange::ExchangeGrant,
    journal::{EventGrant, SnapshotGrant},
};

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct FiberId(pub u64);

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ComponentSpec {
    pub entry: String,
    pub artifact: PathBuf,
    pub artifact_digest: Option<String>,
    pub config: u64,
    pub children: Vec<ComponentSpec>,
    pub patches: Vec<CompositionPatch>,
    pub journal_paths: Vec<PathBuf>,
    pub event_stream_paths: Vec<PathBuf>,
    pub event_grants: Vec<EventGrant>,
    pub snapshot_grants: Vec<SnapshotGrant>,
    pub exchange_grants: Vec<ExchangeGrant>,
}

impl ComponentSpec {
    pub fn new(entry: impl Into<String>, artifact: impl Into<PathBuf>) -> Self {
        Self {
            entry: entry.into(),
            artifact: artifact.into(),
            artifact_digest: None,
            config: 0,
            children: Vec::new(),
            patches: Vec::new(),
            journal_paths: Vec::new(),
            event_stream_paths: Vec::new(),
            event_grants: Vec::new(),
            snapshot_grants: Vec::new(),
            exchange_grants: Vec::new(),
        }
    }

    pub fn with_config(mut self, config: u64) -> Self {
        self.config = config;
        self
    }

    pub fn with_children(mut self, children: Vec<ComponentSpec>) -> Self {
        self.children = children;
        self
    }

    pub fn with_patches(mut self, patches: Vec<CompositionPatch>) -> Self {
        self.patches = patches;
        self
    }

    pub fn with_artifact_digest(mut self, digest: impl Into<String>) -> Self {
        self.artifact_digest = Some(digest.into());
        self
    }

    pub fn with_journal_paths(mut self, paths: Vec<PathBuf>) -> Self {
        self.journal_paths = paths;
        self
    }

    pub fn with_event_stream_paths(mut self, paths: Vec<PathBuf>) -> Self {
        self.event_stream_paths = paths;
        self
    }

    pub fn with_event_grants(mut self, grants: Vec<EventGrant>) -> Self {
        self.event_grants = grants;
        self
    }

    pub fn with_snapshot_grants(mut self, grants: Vec<SnapshotGrant>) -> Self {
        self.snapshot_grants = grants;
        self
    }

    pub fn with_exchange_grants(mut self, grants: Vec<ExchangeGrant>) -> Self {
        self.exchange_grants = grants;
        self
    }
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ComponentTree {
    pub roots: Vec<ComponentSpec>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Limits {
    pub max_components: usize,
    pub max_depth: usize,
    pub max_activation_steps: u32,
    pub max_reconciliation_steps: usize,
    pub max_journal_record_bytes: usize,
    pub max_event_record_bytes: usize,
    pub max_event_records: usize,
    pub max_snapshot_grants: usize,
    pub max_snapshot_bytes: usize,
    pub max_payload_records: usize,
    pub max_payload_bytes: usize,
    pub max_payload_total_bytes: usize,
    pub max_exchange_record_bytes: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_components: 128,
            max_depth: 16,
            max_activation_steps: 1024,
            max_reconciliation_steps: 100_000,
            max_journal_record_bytes: 1024 * 1024,
            max_event_record_bytes: 256 * 1024,
            max_event_records: 512,
            max_snapshot_grants: 8,
            max_snapshot_bytes: 64 * 1024,
            max_payload_records: 64,
            max_payload_bytes: 64 * 1024,
            max_payload_total_bytes: 512 * 1024,
            max_exchange_record_bytes: 128 * 1024,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FiberState {
    Inactive,
    Activating,
    Active,
    Unloading,
    Failed(String),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TraceEvent {
    FiberActivating {
        fiber: FiberId,
        path: String,
    },
    FiberActivated {
        fiber: FiberId,
        path: String,
    },
    FiberUnavailable {
        fiber: FiberId,
        path: String,
    },
    FiberInactive {
        fiber: FiberId,
        path: String,
    },
    FiberFailed {
        fiber: FiberId,
        path: String,
        error: String,
    },
    DisposalFailed {
        fiber: FiberId,
        path: String,
        error: String,
    },
    EffectApplied {
        fiber: FiberId,
        effect: u64,
        kind: String,
    },
    EffectRecovered {
        fiber: FiberId,
        effect: u64,
        kind: String,
    },
    ChildRegistered {
        parent: FiberId,
        child: FiberId,
        path: String,
    },
    ChildRetired {
        parent: FiberId,
        child: FiberId,
        path: String,
    },
    FiberRemoved {
        fiber: FiberId,
        path: String,
    },
    ReplacementCommitted {
        old: FiberId,
        new: FiberId,
        path: String,
    },
    ReplacementRolledBack {
        restored: FiberId,
        rejected: FiberId,
        path: String,
    },
    PatchCommitted {
        actor: FiberId,
        target: String,
        revision: u64,
    },
    PatchRejected {
        actor: FiberId,
        target: String,
        error: String,
    },
    EventQueued {
        actor: FiberId,
        id: u64,
    },
    EventCommitted {
        actor_path: String,
        id: u64,
        sequence: u64,
    },
    EventRejected {
        actor: FiberId,
        error: String,
    },
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ContextObservation {
    pub state_cells: usize,
    pub bindings: usize,
    pub registrations: usize,
    pub fibers: usize,
    pub roots: usize,
    pub live_artifacts: usize,
    pub composition_effects: usize,
    pub pending_patches: usize,
    pub journal_registrations: usize,
    pub pending_events: usize,
    pub staged_events: usize,
    pub event_stream_registrations: usize,
    pub exchange_registrations: usize,
    pub exchange_workers: usize,
}
