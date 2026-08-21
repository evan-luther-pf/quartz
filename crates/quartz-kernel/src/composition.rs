use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
    sync::Arc,
};

use crate::{
    Error, HostCapability, InterfaceId, Result,
    component::{ComponentSpec, ComponentTree, FiberId, FiberState, TraceEvent},
    exchange::ExchangeGrant,
    fiber::{Core, FiberBackup, InternalState, Inverse},
    journal::{EventGrant, Journal, JournalEffect, JournalSnapshot},
    module::Artifact,
    repository::{PreparedSnapshot, PreparedWorkspace},
    runtime::Runtime,
    wasm_host::{
        STATUS_BUSY, STATUS_COLLISION, STATUS_DENIED, STATUS_INVALID, STATUS_OK, STATUS_STALE,
        STATUS_UNDECLARED,
    },
};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case", tag = "operation")]
pub enum CompositionPatch {
    AddRoot {
        root: Box<ComponentSpec>,
    },
    RemoveRoot {
        entry: String,
    },
    Replace {
        path: String,
        replacement: Box<ComponentSpec>,
    },
}

impl CompositionPatch {
    pub fn add_root(root: ComponentSpec) -> Self {
        Self::AddRoot {
            root: Box::new(root),
        }
    }

    pub fn remove_root(entry: impl Into<String>) -> Self {
        Self::RemoveRoot {
            entry: entry.into(),
        }
    }

    pub fn replace(path: impl Into<String>, replacement: ComponentSpec) -> Self {
        Self::Replace {
            path: path.into(),
            replacement: Box::new(replacement),
        }
    }

    fn target(&self) -> &str {
        match self {
            Self::AddRoot { root } => &root.entry,
            Self::RemoveRoot { entry } => entry,
            Self::Replace { path, .. } => path,
        }
    }
}

#[derive(Clone)]
pub(crate) struct PreparedSpec {
    pub(crate) entry: String,
    pub(crate) artifact: Arc<Artifact>,
    pub(crate) config: u64,
    pub(crate) children: Vec<PreparedSpec>,
    pub(crate) patches: Vec<PreparedPatch>,
    pub(crate) journal_paths: Vec<PathBuf>,
    pub(crate) event_stream_paths: Vec<PathBuf>,
    pub(crate) event_grants: Vec<EventGrant>,
    pub(crate) snapshots: Vec<PreparedSnapshot>,
    pub(crate) exchange_grants: Vec<ExchangeGrant>,
    pub(crate) workspaces: Vec<PreparedWorkspace>,
}

#[derive(Clone)]
pub(crate) enum PreparedPatch {
    AddRoot {
        root: PreparedSpec,
    },
    RemoveRoot {
        entry: String,
    },
    Replace {
        path: String,
        replacement: PreparedSpec,
    },
}

#[derive(Clone)]
pub(crate) enum PatchUndo {
    RemoveRoot {
        entry: String,
    },
    AddRoot {
        root: PreparedSpec,
    },
    Replace {
        path: String,
        replacement: PreparedSpec,
    },
}

pub(crate) struct PendingPatch {
    pub(crate) actor: FiberId,
    pub(crate) index: usize,
    pub(crate) base_revision: u64,
}

#[derive(Clone, Copy)]
pub(crate) struct PatchAuthorization {
    pub(crate) provider: FiberId,
    pub(crate) index: usize,
    pub(crate) base_revision: u64,
}

pub(crate) struct JournalRegistration {
    pub(crate) owner: FiberId,
    pub(crate) journal: Journal,
}

pub(crate) struct CommitFailure {
    pub(crate) error: Error,
    pub(crate) committed: bool,
}

impl Runtime {
    pub(crate) fn prepare_application_tree(
        &self,
        mut tree: ComponentTree,
    ) -> Result<BTreeMap<String, PreparedSpec>> {
        let Some(journal_root) = &self.persistent_root else {
            return self.prepare_tree(tree);
        };
        if tree.roots.iter().any(|root| root.entry == *journal_root) {
            return Err(Error::Persistence(
                "application tree cannot declare the persistence bootstrap root".into(),
            ));
        }
        let journal = self
            .desired
            .get(journal_root)
            .ok_or_else(|| Error::Invariant("persistence bootstrap root disappeared".into()))?;
        tree.roots.push(journal.to_component_spec());
        self.prepare_tree(tree)
    }

    pub(crate) fn declare_prepared(
        &mut self,
        prepared: BTreeMap<String, PreparedSpec>,
        increment_revision: bool,
    ) -> Result<bool> {
        {
            let core = self.core.borrow();
            for (target, actor) in &core.patch_owners {
                let target_root = target.split('/').next().unwrap_or_default();
                let target_changes =
                    self.desired.contains_key(target_root) != prepared.contains_key(target_root);
                if !target_changes {
                    continue;
                }
                let actor_path = core
                    .fibers
                    .get(actor)
                    .map(|fiber| fiber.path.as_str())
                    .ok_or_else(|| Error::Invariant("patch owner disappeared".into()))?;
                let actor_root = actor_path.split('/').next().unwrap_or_default();
                if prepared.contains_key(actor_root) {
                    return Err(Error::PatchTargetOwned(target.clone()));
                }
            }
        }
        let changed = !same_tree(&self.desired, &prepared);
        let old_entries: BTreeSet<_> = self.desired.keys().cloned().collect();
        let new_entries: BTreeSet<_> = prepared.keys().cloned().collect();

        for retained in old_entries.intersection(&new_entries) {
            let old = self
                .desired
                .get(retained)
                .ok_or_else(|| Error::Invariant("desired root disappeared".into()))?;
            let new = prepared
                .get(retained)
                .ok_or_else(|| Error::Invariant("prepared root disappeared".into()))?;
            if !same_spec(old, new) {
                return Err(Error::Manifest(format!(
                    "root `{retained}` changed; use replace_entry for an atomic generation change"
                )));
            }
        }

        let mut roots_to_insert = 0;
        for added in new_entries.difference(&old_entries) {
            let spec = prepared
                .get(added)
                .ok_or_else(|| Error::Invariant("prepared root disappeared".into()))?;
            let existing = self.core.borrow().fiber_by_path(added);
            if let Some(existing) = existing {
                let core = self.core.borrow();
                let fiber = core
                    .fibers
                    .get(&existing)
                    .ok_or_else(|| Error::Invariant("restored root disappeared".into()))?;
                if !same_spec(&fiber.spec, spec) {
                    return Err(Error::Manifest(format!(
                        "root `{added}` reappeared with a different declaration during recovery"
                    )));
                }
            } else {
                roots_to_insert += 1;
            }
        }
        let current_components = self.core.borrow().fibers.len();
        if current_components + roots_to_insert > self.limits.max_components {
            return Err(Error::ComponentLimit {
                actual: current_components + roots_to_insert,
                limit: self.limits.max_components,
            });
        }

        for removed in old_entries.difference(&new_entries) {
            let fiber = self.core.borrow().roots.get(removed).copied();
            if let Some(fiber) = fiber {
                self.core.borrow_mut().retire_fiber(fiber)?;
            }
        }
        for added in new_entries.difference(&old_entries) {
            let spec = prepared
                .get(added)
                .cloned()
                .ok_or_else(|| Error::Invariant("prepared root disappeared".into()))?;
            let existing = self.core.borrow().fiber_by_path(added);
            if let Some(existing) = existing {
                self.core
                    .borrow_mut()
                    .fibers
                    .get_mut(&existing)
                    .ok_or_else(|| Error::Invariant("restored root disappeared".into()))?
                    .retired = false;
            } else {
                self.insert_root(spec)?;
            }
        }
        self.desired = prepared;
        if changed && increment_revision {
            self.core.borrow_mut().composition_revision += 1;
        }
        Ok(changed)
    }

    pub(crate) fn append_current_composition(&mut self) -> std::result::Result<(), CommitFailure> {
        let snapshot = self
            .current_journal_snapshot()
            .map_err(|error| CommitFailure {
                error,
                committed: false,
            })?;
        if snapshot.event_outbox.is_empty()
            && self
                .core
                .borrow()
                .journal
                .as_ref()
                .is_some_and(|registration| registration.journal.contains(&snapshot))
        {
            return Ok(());
        }
        if !snapshot.event_outbox.is_empty() {
            let core = self.core.borrow();
            let registration = core.event_stream.as_ref().ok_or_else(|| CommitFailure {
                error: Error::Persistence(
                    "event outbox exists without an event stream provider".into(),
                ),
                committed: false,
            })?;
            registration
                .stream
                .validate(&snapshot.event_outbox)
                .map_err(|error| CommitFailure {
                    error,
                    committed: false,
                })?;
        }
        {
            let mut core = self.core.borrow_mut();
            let registration = core.journal.as_mut().ok_or_else(|| CommitFailure {
                error: Error::Persistence("composition journal is unavailable".into()),
                committed: false,
            })?;
            registration
                .journal
                .append(&snapshot)
                .map_err(|error| CommitFailure {
                    error,
                    committed: false,
                })?;
        }
        if snapshot.event_outbox.is_empty() {
            return Ok(());
        }

        for fact in &snapshot.event_outbox {
            let sequence = {
                let mut core = self.core.borrow_mut();
                let registration = core.event_stream.as_mut().ok_or_else(|| CommitFailure {
                    error: Error::Persistence("event stream became unavailable".into()),
                    committed: true,
                })?;
                registration
                    .stream
                    .append(fact)
                    .map_err(|error| CommitFailure {
                        error,
                        committed: true,
                    })?;
                registration
                    .stream
                    .sequence_for_id(fact.id)
                    .ok_or_else(|| CommitFailure {
                        error: Error::Invariant(
                            "committed event has no event-stream sequence".into(),
                        ),
                        committed: true,
                    })?
            };
            self.core
                .borrow_mut()
                .trace
                .push(TraceEvent::EventCommitted {
                    actor_path: fact.actor_path.clone(),
                    id: fact.id,
                    sequence,
                });
        }

        let mut cleared = snapshot;
        cleared.event_outbox.clear();
        {
            let mut core = self.core.borrow_mut();
            let registration = core.journal.as_mut().ok_or_else(|| CommitFailure {
                error: Error::Persistence("composition journal became unavailable".into()),
                committed: true,
            })?;
            registration
                .journal
                .append(&cleared)
                .map_err(|error| CommitFailure {
                    error,
                    committed: true,
                })?;
            core.event_outbox.clear();
        }
        Ok(())
    }

    pub(crate) fn current_journal_snapshot(&self) -> Result<JournalSnapshot> {
        let tree = self.application_tree();
        let core = self.core.borrow();
        let mut effects = Vec::with_capacity(core.patch_owners.len());
        for (target, actor) in &core.patch_owners {
            let fiber = core
                .fibers
                .get(actor)
                .ok_or_else(|| Error::Invariant("patch owner disappeared".into()))?;
            if desired_spec(&self.desired, &fiber.path).is_none() {
                continue;
            }
            let inverse = fiber
                .accumulator
                .iter()
                .rev()
                .find_map(|inverse| match inverse {
                    Inverse::RestoreComposition {
                        target: inverse_target,
                        undo,
                        ..
                    } if inverse_target == target => Some(undo.to_composition_patch()),
                    _ => None,
                })
                .ok_or_else(|| Error::Invariant("patch owner has no composition inverse".into()))?;
            effects.push(JournalEffect {
                actor_path: fiber.path.clone(),
                target: target.clone(),
                inverse,
            });
        }
        Ok(JournalSnapshot {
            composition_revision: core.composition_revision,
            tree,
            effects,
            next_event_id: core.next_event_id,
            event_outbox: core.event_outbox.clone(),
        })
    }

    pub(crate) fn restore_composition_effects(
        &mut self,
        effects: Vec<JournalEffect>,
    ) -> Result<()> {
        for record in effects {
            let actor = self
                .core
                .borrow()
                .fiber_by_path(&record.actor_path)
                .ok_or_else(|| {
                    Error::JournalCorrupt(format!(
                        "composition effect actor `{}` is absent",
                        record.actor_path
                    ))
                })?;
            let prepared = self.prepare_patch(record.inverse, &record.actor_path)?;
            if prepared.target() != record.target {
                return Err(Error::JournalCorrupt(format!(
                    "composition effect target `{}` does not match inverse `{}`",
                    record.target,
                    prepared.target()
                )));
            }
            let undo = PatchUndo::from_prepared(prepared);
            let mut core = self.core.borrow_mut();
            if core
                .patch_owners
                .keys()
                .any(|target| paths_overlap(target, &record.target))
            {
                return Err(Error::JournalCorrupt(format!(
                    "composition effect target `{}` overlaps another owner",
                    record.target
                )));
            }
            let journal_owner = core
                .journal
                .as_ref()
                .map(|journal| journal.owner)
                .ok_or_else(|| Error::Persistence("composition journal is unavailable".into()))?;
            let actor_record = core
                .fibers
                .get(&actor)
                .ok_or_else(|| Error::Invariant("composition effect actor disappeared".into()))?;
            if actor_record.state != InternalState::Active
                || actor_record
                    .committed
                    .values()
                    .all(|provider| provider.fiber != journal_owner)
            {
                return Err(Error::JournalCorrupt(format!(
                    "composition effect actor `{}` did not commit the journal provider",
                    record.actor_path
                )));
            }
            let effect = core.allocate_effect();
            core.patch_owners.insert(record.target.clone(), actor);
            core.fibers
                .get_mut(&actor)
                .expect("composition effect actor checked above")
                .accumulator
                .push(Inverse::RestoreComposition {
                    effect,
                    target: record.target,
                    undo,
                });
        }
        Ok(())
    }

    pub(crate) fn application_tree(&self) -> ComponentTree {
        let mut roots = self.desired.clone();
        if let Some(journal_root) = &self.persistent_root {
            roots.remove(journal_root);
        }
        tree_from_prepared(&roots)
    }

    pub(crate) fn restore_persistent_state(
        &mut self,
        previous: BTreeMap<String, PreparedSpec>,
        previous_revision: u64,
    ) -> Result<()> {
        self.defer_journal = true;
        let result = self
            .declare_prepared(previous, false)
            .and_then(|_| self.reconcile_to_quiescence());
        self.core.borrow_mut().composition_revision = previous_revision;
        self.defer_journal = false;
        result
    }

    pub(crate) fn replace_entry_internal(&mut self, path: &str, spec: ComponentSpec) -> Result<()> {
        if self
            .core
            .borrow()
            .patch_owners
            .keys()
            .any(|target| paths_overlap(target, path))
        {
            return Err(Error::PatchTargetOwned(path.into()));
        }
        let old_id = self
            .core
            .borrow()
            .fiber_by_path(path)
            .ok_or_else(|| Error::UnknownEntry(path.into()))?;
        let old_entry = self
            .core
            .borrow()
            .fibers
            .get(&old_id)
            .ok_or_else(|| Error::Invariant("replacement fiber disappeared".into()))?
            .spec
            .entry
            .clone();
        if spec.entry != old_entry {
            return Err(Error::Manifest(format!(
                "replacement entry `{}` does not match logical entry `{old_entry}`",
                spec.entry
            )));
        }
        let depth = path.split('/').count();
        let prepared = self.prepare_spec(spec, path, depth, &mut 0)?;
        self.validate_candidate(old_id, &prepared)?;
        let desired_candidate = prepared.clone();

        let candidate_id = self.core.borrow_mut().allocate_fiber();
        let staged = self.instantiate(candidate_id, &prepared)?;
        let backup = {
            let mut core = self.core.borrow_mut();
            let old = core
                .fibers
                .get_mut(&old_id)
                .ok_or_else(|| Error::Invariant("replacement fiber disappeared".into()))?;
            old.retired = true;
            old.pinned = true;
            FiberBackup {
                id: old.id,
                parent: old.parent,
                path: old.path.clone(),
                spec: old.spec.clone(),
            }
        };
        self.reconcile_to_quiescence()?;

        {
            let core = self.core.borrow();
            let old = core
                .fibers
                .get(&old_id)
                .ok_or_else(|| Error::Invariant("pinned old generation disappeared".into()))?;
            if old.state != InternalState::Inactive {
                return Err(Error::Invariant(
                    "old generation did not become inactive".into(),
                ));
            }
        }
        self.swap_in_candidate(
            backup.parent,
            &backup.path,
            old_id,
            candidate_id,
            prepared,
            staged,
        )?;
        self.reconcile_to_quiescence()?;

        let candidate_failure = {
            let core = self.core.borrow();
            let candidate = core
                .fibers
                .get(&candidate_id)
                .ok_or_else(|| Error::Invariant("candidate generation disappeared".into()))?;
            match candidate.state {
                InternalState::Active => None,
                InternalState::Failed => Some(
                    candidate
                        .outcome
                        .clone()
                        .unwrap_or_else(|| "candidate activation failed".into()),
                ),
                _ => Some("candidate did not reach active state".into()),
            }
        };

        if let Some(error) = candidate_failure {
            self.rollback_replacement(backup, candidate_id)?;
            return Err(Error::ReplacementRolledBack(error));
        }

        {
            let mut core = self.core.borrow_mut();
            if let Some(candidate) = core.fibers.get_mut(&candidate_id) {
                candidate.pinned = false;
            }
            core.trace.push(TraceEvent::ReplacementCommitted {
                old: old_id,
                new: candidate_id,
                path: path.into(),
            });
        }
        replace_desired(&mut self.desired, path, desired_candidate)?;
        self.sync_live_specs();
        self.core.borrow_mut().composition_revision += 1;
        Ok(())
    }

    pub(crate) fn prepare_tree(
        &self,
        tree: ComponentTree,
    ) -> Result<BTreeMap<String, PreparedSpec>> {
        let mut count = 0;
        let mut roots = BTreeMap::new();
        for root in tree.roots {
            if roots.contains_key(&root.entry) {
                return Err(Error::DuplicateEntry(root.entry));
            }
            let path = root.entry.clone();
            let prepared = self.prepare_spec(root, &path, 1, &mut count)?;
            roots.insert(prepared.entry.clone(), prepared);
        }
        self.validate_graph(roots.values())?;
        Ok(roots)
    }

    pub(crate) fn prepare_spec(
        &self,
        spec: ComponentSpec,
        path: &str,
        depth: usize,
        count: &mut usize,
    ) -> Result<PreparedSpec> {
        *count += 1;
        if *count > self.limits.max_components {
            return Err(Error::ComponentLimit {
                actual: *count,
                limit: self.limits.max_components,
            });
        }
        if depth > self.limits.max_depth {
            return Err(Error::DepthLimit {
                actual: depth,
                limit: self.limits.max_depth,
            });
        }
        if spec.entry.is_empty() || spec.entry.contains('/') {
            return Err(Error::Manifest(format!(
                "entry `{}` must be one non-empty path segment",
                spec.entry
            )));
        }
        let artifact = self
            .loader
            .load(&spec.artifact, spec.artifact_digest.as_deref())?;
        let requests_journal = artifact.manifest.requests(HostCapability::OpenJournal);
        if requests_journal != !spec.journal_paths.is_empty() {
            return Err(Error::Manifest(format!(
                "component `{path}` must pair open-journal authority with an admitted path"
            )));
        }
        if !spec.journal_paths.is_empty() && self.persistent_root.as_deref() != Some(path) {
            return Err(Error::Persistence(
                "journal paths are reserved for the persistence bootstrap root".into(),
            ));
        }
        let requests_event_stream = artifact.manifest.requests(HostCapability::OpenEventStream);
        if requests_event_stream != !spec.event_stream_paths.is_empty() {
            return Err(Error::Manifest(format!(
                "component `{path}` must pair open-event-stream authority with an admitted path"
            )));
        }
        if !spec.event_stream_paths.is_empty() && self.persistent_root.as_deref() != Some(path) {
            return Err(Error::Persistence(
                "event stream paths are reserved for the persistence bootstrap root".into(),
            ));
        }
        let requests_append = artifact.manifest.requests(HostCapability::AppendEvent);
        let requests_resume = artifact.manifest.requests(HostCapability::ResumeEvent);
        let requests_resume_snapshot = artifact.manifest.requests(HostCapability::ResumeSnapshot);
        let requests_resume_exchange = artifact.manifest.requests(HostCapability::ResumeExchange);
        let requests_replay_append =
            requests_resume || requests_resume_snapshot || requests_resume_exchange;
        if requests_append && requests_replay_append {
            return Err(Error::Manifest(format!(
                "component `{path}` cannot mix ordinary and replay-aware event append authority"
            )));
        }
        if (requests_append || requests_replay_append) != !spec.event_grants.is_empty() {
            return Err(Error::Manifest(format!(
                "component `{path}` must pair event append authority with admitted event grants"
            )));
        }
        let mut event_types = BTreeSet::new();
        for grant in &spec.event_grants {
            if grant.namespace.is_empty() || grant.name.is_empty() || grant.revision == 0 {
                return Err(Error::Manifest(format!(
                    "component `{path}` has an invalid event grant"
                )));
            }
            if !event_types.insert(grant.clone()) {
                return Err(Error::Manifest(format!(
                    "component `{path}` has a duplicate event grant"
                )));
            }
        }
        let requests_snapshot_read = artifact.manifest.requests(HostCapability::ReadSnapshot);
        if (requests_snapshot_read || requests_resume_snapshot) != !spec.snapshot_grants.is_empty()
        {
            return Err(Error::Manifest(format!(
                "component `{path}` must pair snapshot authority with admitted snapshot grants"
            )));
        }
        let requests_open_exchange = artifact.manifest.requests(HostCapability::OpenExchange);
        let requests_exchange = artifact.manifest.requests(HostCapability::Exchange);
        if (requests_open_exchange || requests_exchange) != !spec.exchange_grants.is_empty()
            || requests_open_exchange != requests_exchange
        {
            return Err(Error::Manifest(format!(
                "component `{path}` must pair open-exchange and exchange authority with admitted grants"
            )));
        }
        let mut exchange_identities = BTreeSet::new();
        for grant in &spec.exchange_grants {
            if grant.adapter.is_empty()
                || grant.max_request_bytes == 0
                || grant.max_response_bytes == 0
                || grant.timeout_ms == 0
                || !exchange_identities.insert(grant.adapter.clone())
            {
                return Err(Error::Manifest(format!(
                    "component `{path}` has an invalid or duplicate exchange grant"
                )));
            }
        }
        let requests_workspace_read = artifact.manifest.requests(HostCapability::WorkspaceRead);
        let requests_workspace_write = artifact.manifest.requests(HostCapability::WorkspaceWrite);
        let requests_workspace_publish =
            artifact.manifest.requests(HostCapability::WorkspacePublish);
        if !(requests_workspace_read == requests_workspace_write
            && requests_workspace_write == requests_workspace_publish)
            || requests_workspace_read != !spec.workspace_grants.is_empty()
        {
            return Err(Error::Manifest(format!(
                "component `{path}` must pair workspace read, write, and publish authority with admitted grants"
            )));
        }
        let snapshots = self.prepare_snapshots(path, &spec.snapshot_grants)?;
        let workspaces = self.stage_workspaces(path, &spec.workspace_grants)?;
        if artifact.manifest.component.max_activation_steps > self.limits.max_activation_steps {
            return Err(Error::ActivationLimit(
                artifact.manifest.component.id.clone(),
            ));
        }
        let mut child_names = BTreeSet::new();
        let mut children = Vec::with_capacity(spec.children.len());
        for child in spec.children {
            if !child_names.insert(child.entry.clone()) {
                return Err(Error::DuplicateEntry(format!("{path}/{}", child.entry)));
            }
            let child_path = format!("{path}/{}", child.entry);
            children.push(self.prepare_spec(child, &child_path, depth + 1, count)?);
        }
        let mut patches = Vec::with_capacity(spec.patches.len());
        for patch in spec.patches {
            patches.push(self.prepare_patch(patch, path)?);
        }
        Ok(PreparedSpec {
            entry: spec.entry,
            artifact,
            config: spec.config,
            children,
            patches,
            journal_paths: spec.journal_paths,
            event_stream_paths: spec.event_stream_paths,
            event_grants: spec.event_grants,
            snapshots,
            exchange_grants: spec.exchange_grants,
            workspaces,
        })
    }

    pub(crate) fn prepare_patch(
        &self,
        patch: CompositionPatch,
        actor_path: &str,
    ) -> Result<PreparedPatch> {
        if self
            .persistent_root
            .as_deref()
            .is_some_and(|root| paths_overlap(root, patch.target()))
        {
            return Err(Error::InvalidPatch(
                "a component cannot modify the persistence bootstrap root".into(),
            ));
        }
        match patch {
            CompositionPatch::AddRoot { root } => {
                if root.entry.is_empty() || root.entry.contains('/') {
                    return Err(Error::InvalidPatch(
                        "added root entry must be one non-empty path segment".into(),
                    ));
                }
                let root_path = root.entry.clone();
                let mut count = 0;
                let root = self.prepare_spec(*root, &root_path, 1, &mut count)?;
                self.validate_graph(std::iter::once(&root))?;
                Ok(PreparedPatch::AddRoot { root })
            }
            CompositionPatch::RemoveRoot { entry } => {
                if entry.is_empty() || entry.contains('/') {
                    return Err(Error::InvalidPatch(
                        "removed root entry must be one non-empty path segment".into(),
                    ));
                }
                if is_same_or_ancestor(&entry, actor_path) {
                    return Err(Error::InvalidPatch(
                        "a component cannot remove itself or an ancestor".into(),
                    ));
                }
                Ok(PreparedPatch::RemoveRoot { entry })
            }
            CompositionPatch::Replace { path, replacement } => {
                validate_patch_path(&path)?;
                if replacement.entry != path.rsplit('/').next().unwrap_or_default() {
                    return Err(Error::InvalidPatch(format!(
                        "replacement entry `{}` does not match target `{path}`",
                        replacement.entry
                    )));
                }
                if is_same_or_ancestor(&path, actor_path) {
                    return Err(Error::InvalidPatch(
                        "a component cannot replace itself or an ancestor".into(),
                    ));
                }
                let depth = path.split('/').count();
                let mut count = 0;
                let replacement = self.prepare_spec(*replacement, &path, depth, &mut count)?;
                Ok(PreparedPatch::Replace { path, replacement })
            }
        }
    }

    pub(crate) fn validate_graph<'a>(
        &self,
        roots: impl Iterator<Item = &'a PreparedSpec>,
    ) -> Result<()> {
        let mut entries = Vec::new();
        for root in roots {
            flatten_specs(root, &root.entry, &mut entries);
        }
        validate_entries(&entries)
    }

    pub(crate) fn process_pending_patch(&mut self) -> Result<bool> {
        let Some(request) = self.core.borrow_mut().pending_patches.pop_front() else {
            return Ok(false);
        };
        let cancelled_target = {
            let core = self.core.borrow();
            let Some(actor) = core.fibers.get(&request.actor) else {
                return Ok(true);
            };
            (!matches!(
                actor.state,
                InternalState::Activating | InternalState::Active
            ) || actor.outcome.is_some())
            .then(|| {
                actor
                    .spec
                    .patches
                    .get(request.index)
                    .map(|patch| patch.target().to_string())
                    .unwrap_or_default()
            })
        };
        if let Some(target) = cancelled_target {
            self.core
                .borrow_mut()
                .trace
                .push(TraceEvent::PatchRejected {
                    actor: request.actor,
                    target,
                    error: "requester activation did not commit".into(),
                });
            return Ok(true);
        }
        let patch = {
            let core = self.core.borrow();
            let Some(actor) = core.fibers.get(&request.actor) else {
                return Ok(true);
            };
            if request.base_revision != core.composition_revision {
                None
            } else {
                actor.spec.patches.get(request.index).cloned()
            }
        };
        let Some(patch) = patch else {
            self.reject_patch(
                request.actor,
                String::new(),
                "patch request became stale or unavailable".into(),
            );
            return Ok(true);
        };
        let target = patch.target().to_string();
        let previous_revision = self.composition_revision();
        let mut commit_error = None;
        match self.apply_prepared_patch(&patch) {
            Ok(undo) => {
                let rollback = undo.clone();
                let effect = {
                    let mut core = self.core.borrow_mut();
                    let effect = core.allocate_effect();
                    core.patch_owners.insert(target.clone(), request.actor);
                    let Some(actor) = core.fibers.get_mut(&request.actor) else {
                        return Err(Error::Invariant(
                            "patch actor disappeared after commit".into(),
                        ));
                    };
                    actor.accumulator.push(Inverse::RestoreComposition {
                        effect,
                        target: target.clone(),
                        undo,
                    });
                    effect
                };
                if self.persistent_root.is_some()
                    && !self.defer_journal
                    && let Err(failure) = self.append_current_composition()
                {
                    if failure.committed {
                        commit_error = Some(failure.error);
                    } else {
                        {
                            let mut core = self.core.borrow_mut();
                            core.patch_owners.remove(&target);
                            let actor = core.fibers.get_mut(&request.actor).ok_or_else(|| {
                                Error::Invariant("patch actor disappeared during rollback".into())
                            })?;
                            let removed = actor.accumulator.pop().ok_or_else(|| {
                                Error::Invariant("patch inverse disappeared during rollback".into())
                            })?;
                            if removed.effect() != effect {
                                return Err(Error::Invariant(
                                    "patch inverse was not last during rollback".into(),
                                ));
                            }
                        }
                        self.apply_patch_undo(&rollback)?.ok_or_else(|| {
                            Error::Invariant(
                                "journal failure rollback did not restore the patch target".into(),
                            )
                        })?;
                        self.core.borrow_mut().composition_revision = previous_revision;
                        self.reject_patch(request.actor, target, failure.error.to_string());
                        return Ok(true);
                    }
                }
                let mut core = self.core.borrow_mut();
                let revision = core.composition_revision;
                core.trace.push(TraceEvent::EffectApplied {
                    fiber: request.actor,
                    effect,
                    kind: "composition".into(),
                });
                core.trace.push(TraceEvent::PatchCommitted {
                    actor: request.actor,
                    target,
                    revision,
                });
            }
            Err(error) => {
                self.reject_patch(request.actor, target, error.to_string());
            }
        }
        if let Some(error) = commit_error {
            return Err(error);
        }
        Ok(true)
    }

    pub(crate) fn apply_prepared_patch(&mut self, patch: &PreparedPatch) -> Result<PatchUndo> {
        match patch {
            PreparedPatch::Replace { path, replacement } => {
                let previous = desired_spec(&self.desired, path)
                    .cloned()
                    .ok_or_else(|| Error::UnknownEntry(path.clone()))?;
                self.replace_entry_internal(path, replacement.to_component_spec())
                    .map_err(|error| match error {
                        Error::ReplacementRolledBack(error) => Error::PatchRolledBack(error),
                        error => error,
                    })?;
                Ok(PatchUndo::Replace {
                    path: path.clone(),
                    replacement: previous,
                })
            }
            PreparedPatch::AddRoot { root } => {
                if self.desired.contains_key(&root.entry) {
                    return Err(Error::InvalidPatch(format!(
                        "root `{}` already exists",
                        root.entry
                    )));
                }
                let previous = self.desired.clone();
                let previous_revision = self.composition_revision();
                let mut next = previous.clone();
                next.insert(root.entry.clone(), root.clone());
                self.declare_prepared(next, true)?;
                self.reconcile_to_quiescence()?;
                if let Some(FiberState::Failed(error)) = self.fiber_state(&root.entry) {
                    self.declare_prepared(previous, false)?;
                    self.reconcile_to_quiescence()?;
                    self.core.borrow_mut().composition_revision = previous_revision;
                    return Err(Error::PatchRolledBack(error));
                }
                Ok(PatchUndo::RemoveRoot {
                    entry: root.entry.clone(),
                })
            }
            PreparedPatch::RemoveRoot { entry } => {
                let previous = self
                    .desired
                    .get(entry)
                    .cloned()
                    .ok_or_else(|| Error::UnknownEntry(entry.clone()))?;
                let mut next = self.desired.clone();
                next.remove(entry);
                self.declare_prepared(next, true)?;
                self.reconcile_to_quiescence()?;
                Ok(PatchUndo::AddRoot { root: previous })
            }
        }
    }

    pub(crate) fn apply_patch_undo(&mut self, undo: &PatchUndo) -> Result<Option<PatchUndo>> {
        match undo {
            PatchUndo::RemoveRoot { entry } => {
                if self.desired.contains_key(entry) {
                    self.apply_prepared_patch(&PreparedPatch::RemoveRoot {
                        entry: entry.clone(),
                    })
                    .map(Some)
                } else {
                    Ok(None)
                }
            }
            PatchUndo::AddRoot { root } => {
                if self.desired.contains_key(&root.entry) {
                    Ok(None)
                } else {
                    self.apply_prepared_patch(&PreparedPatch::AddRoot { root: root.clone() })
                        .map(Some)
                }
            }
            PatchUndo::Replace { path, replacement } => {
                if desired_spec(&self.desired, path).is_some() {
                    self.apply_prepared_patch(&PreparedPatch::Replace {
                        path: path.clone(),
                        replacement: replacement.clone(),
                    })
                    .map(Some)
                } else {
                    Ok(None)
                }
            }
        }
    }

    pub(crate) fn reject_patch(&mut self, actor: FiberId, target: String, error: String) {
        let mut core = self.core.borrow_mut();
        let unavailable = core.fibers.get_mut(&actor).and_then(|fiber| {
            fiber.outcome = Some(error.clone());
            if matches!(
                fiber.state,
                InternalState::Activating | InternalState::Active
            ) {
                fiber.state = InternalState::Unloading;
                Some(fiber.path.clone())
            } else {
                None
            }
        });
        core.trace.push(TraceEvent::PatchRejected {
            actor,
            target,
            error,
        });
        if let Some(path) = unavailable {
            core.trace
                .push(TraceEvent::FiberUnavailable { fiber: actor, path });
        }
    }

    pub(crate) fn recover_composition(
        &mut self,
        actor: FiberId,
        effect: u64,
        target: String,
        undo: PatchUndo,
    ) -> Result<()> {
        let actor_path = {
            let mut core = self.core.borrow_mut();
            if core.patch_owners.get(&target) != Some(&actor) {
                return Err(Error::Invariant(
                    "composition inverse does not own its target".into(),
                ));
            }
            core.patch_owners.remove(&target);
            core.blocked_recovery.insert(actor);
            core.fibers
                .get(&actor)
                .map(|fiber| fiber.path.clone())
                .ok_or_else(|| Error::Invariant("composition owner disappeared".into()))?
        };

        let actor_still_declared = desired_spec(&self.desired, &actor_path).is_some();
        let target_still_declared = desired_spec(&self.desired, &target).is_some();
        let previous_revision = self.composition_revision();
        let mut committed_error = None;
        let result = if actor_still_declared || target_still_declared {
            match self.apply_patch_undo(&undo) {
                Ok(reverse) => {
                    if reverse.is_some() && self.persistent_root.is_some() && !self.defer_journal {
                        if let Err(failure) = self.append_current_composition() {
                            if failure.committed {
                                committed_error = Some(failure.error);
                                Ok(())
                            } else {
                                let reverse = reverse.ok_or_else(|| {
                                    Error::Invariant(
                                        "journal failure recovery lost its reverse patch".into(),
                                    )
                                })?;
                                self.apply_patch_undo(&reverse)?.ok_or_else(|| {
                                    Error::Invariant(
                                        "journal failure did not restore committed composition"
                                            .into(),
                                    )
                                })?;
                                self.core.borrow_mut().composition_revision = previous_revision;
                                Err(failure.error)
                            }
                        } else {
                            Ok(())
                        }
                    } else {
                        Ok(())
                    }
                }
                Err(error) => Err(error),
            }
        } else {
            Ok(())
        };

        let mut core = self.core.borrow_mut();
        core.blocked_recovery.remove(&actor);
        if let Err(error) = result {
            core.patch_owners.insert(target.clone(), actor);
            if let Some(fiber) = core.fibers.get_mut(&actor) {
                fiber.accumulator.push(Inverse::RestoreComposition {
                    effect,
                    target,
                    undo,
                });
            }
            return Err(error);
        }
        core.trace.push(TraceEvent::EffectRecovered {
            fiber: actor,
            effect,
            kind: "composition".into(),
        });
        if let Some(error) = committed_error {
            return Err(error);
        }
        Ok(())
    }

    pub(crate) fn validate_candidate(&self, old: FiberId, candidate: &PreparedSpec) -> Result<()> {
        let core = self.core.borrow();
        let mut entries = Vec::new();
        for fiber in core.fibers.values() {
            if fiber.id != old && !fiber.retired {
                entries.push((fiber.path.clone(), &fiber.spec));
            }
        }
        let path = core
            .fibers
            .get(&old)
            .ok_or_else(|| Error::Invariant("replacement fiber disappeared".into()))?
            .path
            .clone();
        flatten_specs(candidate, &path, &mut entries);
        validate_entries(&entries)
    }

    pub(crate) fn sync_live_specs(&mut self) {
        let updates: Vec<_> = {
            let core = self.core.borrow();
            core.fibers
                .iter()
                .filter_map(|(id, fiber)| {
                    desired_spec(&self.desired, &fiber.path)
                        .cloned()
                        .map(|spec| (*id, spec))
                })
                .collect()
        };
        let mut core = self.core.borrow_mut();
        for (id, spec) in updates {
            if let Some(fiber) = core.fibers.get_mut(&id) {
                fiber.spec = spec;
            }
        }
    }
}

impl Core {
    pub(crate) fn host_apply_patch(
        &mut self,
        actor: FiberId,
        index: u64,
        base_revision: u64,
    ) -> i32 {
        if self.replaying {
            return STATUS_BUSY;
        }
        let Ok(index) = usize::try_from(index) else {
            return STATUS_INVALID;
        };
        let Some(record) = self.fibers.get(&actor) else {
            return STATUS_INVALID;
        };
        if record.state != InternalState::Activating
            || !record
                .spec
                .artifact
                .manifest
                .requests(HostCapability::ApplyPatch)
        {
            return STATUS_UNDECLARED;
        }
        if base_revision != self.composition_revision {
            return STATUS_STALE;
        }
        let Some(patch) = record.spec.patches.get(index) else {
            return STATUS_UNDECLARED;
        };
        let Some(authorization) = record.patch_authorization else {
            return STATUS_DENIED;
        };
        if authorization.index != index
            || authorization.base_revision != base_revision
            || record
                .committed
                .values()
                .all(|provider| provider.fiber != authorization.provider)
        {
            return STATUS_DENIED;
        }
        if let Some(journal) = &self.journal
            && record
                .committed
                .values()
                .all(|provider| provider.fiber != journal.owner)
        {
            return STATUS_UNDECLARED;
        }
        let target = patch.target();
        if self
            .patch_owners
            .keys()
            .any(|owned| paths_overlap(owned, target))
            || self
                .pending_patches
                .iter()
                .any(|request| request.actor == actor)
        {
            return STATUS_BUSY;
        }
        if record.committed.values().any(|provider| {
            self.fibers
                .get(&provider.fiber)
                .is_some_and(|fiber| is_same_or_ancestor(target, &fiber.path))
        }) {
            return STATUS_DENIED;
        }
        self.fibers
            .get_mut(&actor)
            .expect("actor checked above")
            .patch_authorization = None;
        self.pending_patches.push_back(PendingPatch {
            actor,
            index,
            base_revision,
        });
        STATUS_OK
    }

    pub(crate) fn host_open_journal(&mut self, fiber: FiberId, index: u64) -> i32 {
        let Ok(index) = usize::try_from(index) else {
            return STATUS_INVALID;
        };
        let path = {
            let Some(record) = self.fibers.get(&fiber) else {
                return STATUS_INVALID;
            };
            if record.state != InternalState::Activating {
                return STATUS_INVALID;
            }
            if !record
                .spec
                .artifact
                .manifest
                .requests(HostCapability::OpenJournal)
            {
                return STATUS_UNDECLARED;
            }
            let Some(path) = record.spec.journal_paths.get(index) else {
                return STATUS_UNDECLARED;
            };
            path.clone()
        };
        if self.journal.is_some() {
            return STATUS_COLLISION;
        }
        let journal = match Journal::open(&path, self.limits.max_journal_record_bytes) {
            Ok(journal) => journal,
            Err(error) => {
                self.journal_failure = Some(error);
                return STATUS_INVALID;
            }
        };
        let effect = self.allocate_effect();
        self.journal = Some(JournalRegistration {
            owner: fiber,
            journal,
        });
        self.fibers
            .get_mut(&fiber)
            .expect("journal fiber checked above")
            .accumulator
            .push(Inverse::CloseJournal { effect });
        self.trace.push(TraceEvent::EffectApplied {
            fiber,
            effect,
            kind: "composition-journal".into(),
        });
        STATUS_OK
    }
}

fn flatten_specs<'a>(
    spec: &'a PreparedSpec,
    path: &str,
    entries: &mut Vec<(String, &'a PreparedSpec)>,
) {
    entries.push((path.into(), spec));
    for child in &spec.children {
        flatten_specs(child, &format!("{path}/{}", child.entry), entries);
    }
}

fn validate_entries(entries: &[(String, &PreparedSpec)]) -> Result<()> {
    let mut providers: BTreeMap<(String, String), (InterfaceId, String)> = BTreeMap::new();
    for (path, spec) in entries {
        for provision in &spec.artifact.manifest.component.provide {
            let interface = provision.interface_id();
            let key = (interface.namespace.clone(), interface.interface.clone());
            if providers
                .insert(key, (interface.clone(), path.clone()))
                .is_some()
            {
                return Err(Error::ProviderCollision {
                    namespace: interface.namespace,
                    interface: interface.interface,
                    revision: interface.revision,
                });
            }
        }
    }

    let mut edges: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    for (path, spec) in entries {
        edges.entry(path).or_default();
        for required in &spec.artifact.manifest.component.inject {
            let requirement = required.requirement();
            if let Some((_, provider_path)) = providers
                .values()
                .find(|(provided, _)| requirement.accepts(provided))
            {
                edges
                    .entry(provider_path)
                    .or_default()
                    .insert(path.as_str());
            }
        }
    }
    detect_cycle(&edges)
}

fn detect_cycle(edges: &BTreeMap<&str, BTreeSet<&str>>) -> Result<()> {
    fn visit<'a>(
        node: &'a str,
        edges: &BTreeMap<&'a str, BTreeSet<&'a str>>,
        visiting: &mut BTreeSet<&'a str>,
        visited: &mut BTreeSet<&'a str>,
        stack: &mut Vec<&'a str>,
    ) -> Result<()> {
        if visited.contains(node) {
            return Ok(());
        }
        if !visiting.insert(node) {
            let start = stack.iter().position(|entry| *entry == node).unwrap_or(0);
            let mut cycle = stack[start..].to_vec();
            cycle.push(node);
            return Err(Error::DependencyCycle(cycle.join(" -> ")));
        }
        stack.push(node);
        if let Some(children) = edges.get(node) {
            for child in children {
                visit(child, edges, visiting, visited, stack)?;
            }
        }
        stack.pop();
        visiting.remove(node);
        visited.insert(node);
        Ok(())
    }

    let mut visiting = BTreeSet::new();
    let mut visited = BTreeSet::new();
    let mut stack = Vec::new();
    for node in edges.keys() {
        visit(node, edges, &mut visiting, &mut visited, &mut stack)?;
    }
    Ok(())
}

fn same_spec(left: &PreparedSpec, right: &PreparedSpec) -> bool {
    left.entry == right.entry
        && left.artifact.path == right.artifact.path
        && left.artifact.digest == right.artifact.digest
        && left.config == right.config
        && left.journal_paths == right.journal_paths
        && left.event_stream_paths == right.event_stream_paths
        && left.event_grants == right.event_grants
        && left
            .snapshots
            .iter()
            .map(|snapshot| &snapshot.grant)
            .eq(right.snapshots.iter().map(|snapshot| &snapshot.grant))
        && left.children.len() == right.children.len()
        && left.exchange_grants == right.exchange_grants
        && left
            .workspaces
            .iter()
            .map(|workspace| &workspace.grant)
            .eq(right.workspaces.iter().map(|workspace| &workspace.grant))
        && left
            .children
            .iter()
            .zip(&right.children)
            .all(|(left, right)| same_spec(left, right))
        && left.patches.len() == right.patches.len()
        && left
            .patches
            .iter()
            .zip(&right.patches)
            .all(|(left, right)| same_patch(left, right))
}

fn replace_desired(
    roots: &mut BTreeMap<String, PreparedSpec>,
    path: &str,
    replacement: PreparedSpec,
) -> Result<()> {
    let mut segments = path.split('/');
    let root = segments
        .next()
        .ok_or_else(|| Error::UnknownEntry(path.into()))?;
    let current = roots
        .get_mut(root)
        .ok_or_else(|| Error::UnknownEntry(path.into()))?;
    let remaining: Vec<_> = segments.collect();
    if remaining.is_empty() {
        *current = replacement;
        return Ok(());
    }
    replace_child(current, &remaining, replacement)
        .then_some(())
        .ok_or_else(|| Error::UnknownEntry(path.into()))
}

fn replace_child(current: &mut PreparedSpec, segments: &[&str], replacement: PreparedSpec) -> bool {
    let Some((segment, remaining)) = segments.split_first() else {
        return false;
    };
    let Some(child) = current
        .children
        .iter_mut()
        .find(|child| child.entry == *segment)
    else {
        return false;
    };
    if remaining.is_empty() {
        *child = replacement;
        true
    } else {
        replace_child(child, remaining, replacement)
    }
}

fn desired_spec<'a>(
    roots: &'a BTreeMap<String, PreparedSpec>,
    path: &str,
) -> Option<&'a PreparedSpec> {
    let mut segments = path.split('/');
    let mut current = roots.get(segments.next()?)?;
    for segment in segments {
        current = current
            .children
            .iter()
            .find(|child| child.entry == segment)?;
    }
    Some(current)
}

fn same_tree(
    left: &BTreeMap<String, PreparedSpec>,
    right: &BTreeMap<String, PreparedSpec>,
) -> bool {
    left.len() == right.len()
        && left
            .iter()
            .all(|(entry, spec)| right.get(entry).is_some_and(|other| same_spec(spec, other)))
}

fn same_patch(left: &PreparedPatch, right: &PreparedPatch) -> bool {
    match (left, right) {
        (PreparedPatch::AddRoot { root: left }, PreparedPatch::AddRoot { root: right }) => {
            same_spec(left, right)
        }
        (PreparedPatch::RemoveRoot { entry: left }, PreparedPatch::RemoveRoot { entry: right }) => {
            left == right
        }
        (
            PreparedPatch::Replace {
                path: left_path,
                replacement: left,
            },
            PreparedPatch::Replace {
                path: right_path,
                replacement: right,
            },
        ) => left_path == right_path && same_spec(left, right),
        _ => false,
    }
}

fn tree_from_prepared(roots: &BTreeMap<String, PreparedSpec>) -> ComponentTree {
    ComponentTree {
        roots: roots
            .values()
            .map(PreparedSpec::to_component_spec)
            .collect(),
    }
}

impl PreparedSpec {
    fn to_component_spec(&self) -> ComponentSpec {
        ComponentSpec {
            entry: self.entry.clone(),
            artifact: self.artifact.path.clone(),
            artifact_digest: Some(self.artifact.digest.clone()),
            config: self.config,
            children: self
                .children
                .iter()
                .map(PreparedSpec::to_component_spec)
                .collect(),
            patches: self
                .patches
                .iter()
                .map(PreparedPatch::to_composition_patch)
                .collect(),
            journal_paths: self.journal_paths.clone(),
            event_stream_paths: self.event_stream_paths.clone(),
            event_grants: self.event_grants.clone(),
            snapshot_grants: self
                .snapshots
                .iter()
                .map(|snapshot| snapshot.grant.clone())
                .collect(),
            exchange_grants: self.exchange_grants.clone(),
            workspace_grants: self
                .workspaces
                .iter()
                .map(|workspace| workspace.grant.clone())
                .collect(),
        }
    }
}

impl PreparedPatch {
    fn target(&self) -> &str {
        match self {
            Self::AddRoot { root } => &root.entry,
            Self::RemoveRoot { entry } => entry,
            Self::Replace { path, .. } => path,
        }
    }

    fn to_composition_patch(&self) -> CompositionPatch {
        match self {
            Self::AddRoot { root } => CompositionPatch::add_root(root.to_component_spec()),
            Self::RemoveRoot { entry } => CompositionPatch::remove_root(entry),
            Self::Replace { path, replacement } => {
                CompositionPatch::replace(path, replacement.to_component_spec())
            }
        }
    }
}

impl PatchUndo {
    fn from_prepared(patch: PreparedPatch) -> Self {
        match patch {
            PreparedPatch::AddRoot { root } => Self::AddRoot { root },
            PreparedPatch::RemoveRoot { entry } => Self::RemoveRoot { entry },
            PreparedPatch::Replace { path, replacement } => Self::Replace { path, replacement },
        }
    }

    fn to_composition_patch(&self) -> CompositionPatch {
        match self {
            Self::AddRoot { root } => CompositionPatch::add_root(root.to_component_spec()),
            Self::RemoveRoot { entry } => CompositionPatch::remove_root(entry),
            Self::Replace { path, replacement } => {
                CompositionPatch::replace(path, replacement.to_component_spec())
            }
        }
    }
}

fn paths_overlap(left: &str, right: &str) -> bool {
    is_same_or_ancestor(left, right) || is_same_or_ancestor(right, left)
}

fn validate_patch_path(path: &str) -> Result<()> {
    if path.is_empty() || path.split('/').any(|segment| segment.is_empty()) {
        return Err(Error::InvalidPatch(
            "patch target must contain non-empty path segments".into(),
        ));
    }
    Ok(())
}

fn is_same_or_ancestor(ancestor: &str, path: &str) -> bool {
    path == ancestor
        || path
            .strip_prefix(ancestor)
            .is_some_and(|remaining| remaining.starts_with('/'))
}
