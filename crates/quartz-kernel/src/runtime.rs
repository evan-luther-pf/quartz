use std::{
    cell::RefCell,
    collections::{BTreeMap, BTreeSet, VecDeque},
    path::PathBuf,
    rc::{Rc, Weak},
    sync::Arc,
};

use serde::{Deserialize, Serialize};
use wasmtime::{
    Store, StoreContextMut,
    component::{Instance, Linker, TypedFunc},
};

use crate::{
    BindingKind, Error, HostCapability, InterfaceId, Result,
    journal::{Journal, JournalEffect, JournalSnapshot},
    module::{Artifact, ModuleLoader},
};

const STATUS_OK: i32 = 0;
const STATUS_UNDECLARED: i32 = 2;
const STATUS_UNSATISFIED: i32 = 3;
const STATUS_INVALID: i32 = 4;
const STATUS_LIMIT: i32 = 5;
const STATUS_COLLISION: i32 = 6;
const STATUS_DENIED: i32 = 7;
const STATUS_STALE: i32 = 8;
const STATUS_BUSY: i32 = 9;

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
}

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
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_components: 128,
            max_depth: 16,
            max_activation_steps: 1024,
            max_reconciliation_steps: 100_000,
            max_journal_record_bytes: 1024 * 1024,
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
}

pub struct Runtime {
    loader: ModuleLoader,
    core: Rc<RefCell<Core>>,
    limits: Limits,
    desired: BTreeMap<String, PreparedSpec>,
    persistent_root: Option<String>,
    defer_journal: bool,
}

#[derive(Clone)]
struct PreparedSpec {
    entry: String,
    artifact: Arc<Artifact>,
    config: u64,
    children: Vec<PreparedSpec>,
    patches: Vec<PreparedPatch>,
    journal_paths: Vec<PathBuf>,
}

#[derive(Clone)]
enum PreparedPatch {
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
enum PatchUndo {
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

struct PendingPatch {
    actor: FiberId,
    index: usize,
    base_revision: u64,
}

#[derive(Clone, Copy)]
struct PatchAuthorization {
    provider: FiberId,
    index: usize,
    base_revision: u64,
}

struct JournalRegistration {
    owner: FiberId,
    journal: Journal,
}

struct Core {
    limits: Limits,
    next_fiber: u64,
    next_effect: u64,
    next_registration: u64,
    composition_revision: u64,
    fibers: BTreeMap<FiberId, Fiber>,
    roots: BTreeMap<String, FiberId>,
    registrations: BTreeMap<u64, Registration>,
    state_cells: BTreeMap<(FiberId, u64), u64>,
    bindings: BTreeMap<InterfaceId, ProviderBinding>,
    patch_owners: BTreeMap<String, FiberId>,
    pending_patches: VecDeque<PendingPatch>,
    blocked_recovery: BTreeSet<FiberId>,
    trace: Vec<TraceEvent>,
    journal: Option<JournalRegistration>,
    journal_failure: Option<Error>,
    replaying: bool,
}

struct Fiber {
    id: FiberId,
    parent: Option<FiberId>,
    path: String,
    spec: PreparedSpec,
    retired: bool,
    pinned: bool,
    state: InternalState,
    committed: ProviderView,
    accumulator: Vec<Inverse>,
    instance: Option<GuestInstance>,
    activation_steps: u32,
    outcome: Option<String>,
    patch_authorization: Option<PatchAuthorization>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum InternalState {
    Inactive,
    Activating,
    Active,
    Unloading,
    Failed,
}

type ProviderView = BTreeMap<u64, CommittedProvider>;

#[derive(Clone, Debug, Eq, PartialEq)]
struct CommittedProvider {
    fiber: FiberId,
    interface: InterfaceId,
}

#[derive(Clone)]
struct ProviderBinding {
    fiber: FiberId,
    kind: BindingKind,
    value: Option<u64>,
}

struct Registration {
    parent: FiberId,
    child: FiberId,
    entry: String,
}

enum Inverse {
    RestoreState {
        effect: u64,
        key: u64,
        previous: Option<u64>,
    },
    RemoveBinding {
        effect: u64,
        interface: InterfaceId,
    },
    RetireChild {
        effect: u64,
        registration: u64,
    },
    RestoreComposition {
        effect: u64,
        target: String,
        undo: PatchUndo,
    },
    CloseJournal {
        effect: u64,
    },
}

impl Inverse {
    fn effect(&self) -> u64 {
        match self {
            Self::RestoreState { effect, .. }
            | Self::RemoveBinding { effect, .. }
            | Self::RetireChild { effect, .. }
            | Self::RestoreComposition { effect, .. }
            | Self::CloseJournal { effect } => *effect,
        }
    }

    fn kind(&self) -> &'static str {
        match self {
            Self::RestoreState { .. } => "state",
            Self::RemoveBinding { .. } => "coeffect",
            Self::RetireChild { .. } => "component-registration",
            Self::RestoreComposition { .. } => "composition",
            Self::CloseJournal { .. } => "composition-journal",
        }
    }
}

struct HostState {
    core: Weak<RefCell<Core>>,
    fiber: FiberId,
}

struct GuestInstance {
    store: Store<HostState>,
    _instance: Instance,
    instance_id: u64,
    step: TypedFunc<(u64,), (i32,)>,
    drop_fn: TypedFunc<(u64,), ()>,
    invoke: TypedFunc<(u64, u64, u64, u64), (i64,)>,
}

impl Runtime {
    pub fn new(limits: Limits) -> Result<Self> {
        let loader = ModuleLoader::new()?;
        let core = Rc::new(RefCell::new(Core::new(limits)));
        Ok(Self {
            loader,
            core,
            limits,
            desired: BTreeMap::new(),
            persistent_root: None,
            defer_journal: false,
        })
    }

    pub fn open_persistent(limits: Limits, journal_component: ComponentSpec) -> Result<Self> {
        if journal_component.journal_paths.len() != 1
            || !journal_component.children.is_empty()
            || !journal_component.patches.is_empty()
        {
            return Err(Error::Persistence(
                "journal bootstrap must admit exactly one path and no children or patches".into(),
            ));
        }
        let journal_root = journal_component.entry.clone();
        let mut runtime = Self::new(limits)?;
        runtime.persistent_root = Some(journal_root.clone());
        let bootstrap = runtime.prepare_tree(ComponentTree {
            roots: vec![journal_component],
        })?;
        runtime.declare_prepared(bootstrap, false)?;
        runtime.reconcile_to_quiescence()?;
        if runtime.core.borrow().journal.is_none() {
            let failure = runtime
                .core
                .borrow_mut()
                .journal_failure
                .take()
                .unwrap_or_else(|| {
                    Error::Persistence("journal component did not register its path".into())
                });
            return Err(failure);
        }
        let recovered = {
            let core = runtime.core.borrow();
            let registration = core
                .journal
                .as_ref()
                .expect("journal registration checked above");
            let owner_path = core
                .fibers
                .get(&registration.owner)
                .map(|fiber| fiber.path.as_str());
            if owner_path != Some(journal_root.as_str()) {
                return Err(Error::Persistence(
                    "journal capability is not owned by the bootstrap root".into(),
                ));
            }
            registration.journal.recovered()
        };
        if let Some(snapshot) = recovered {
            runtime.core.borrow_mut().composition_revision = snapshot.composition_revision;
            let prepared = runtime.prepare_application_tree(snapshot.tree)?;
            runtime.declare_prepared(prepared, false)?;
            runtime.core.borrow_mut().replaying = true;
            runtime.reconcile_to_quiescence()?;
            runtime.core.borrow_mut().replaying = false;
            runtime.restore_composition_effects(snapshot.effects)?;
        }
        Ok(runtime)
    }

    pub fn composition_revision(&self) -> u64 {
        self.core.borrow().composition_revision
    }

    pub fn journal_sequence(&self) -> Option<u64> {
        self.core
            .borrow()
            .journal
            .as_ref()
            .map(|registration| registration.journal.sequence())
    }

    pub fn declare_tree(&mut self, tree: ComponentTree) -> Result<()> {
        if self.persistent_root.is_some() {
            return Err(Error::Persistence(
                "persistent declarations must reconcile through apply_tree".into(),
            ));
        }
        let prepared = self.prepare_tree(tree)?;
        self.declare_prepared(prepared, true)?;
        Ok(())
    }

    pub fn shutdown_persistent(&mut self) -> Result<()> {
        if self.persistent_root.is_none() {
            return Err(Error::Persistence("runtime is not persistent".into()));
        }
        self.apply_tree(ComponentTree::default())?;
        self.declare_prepared(BTreeMap::new(), false)?;
        self.reconcile_to_quiescence()?;
        self.persistent_root = None;
        Ok(())
    }

    fn prepare_application_tree(
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

    fn declare_prepared(
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

    fn append_current_composition(&mut self) -> Result<()> {
        let snapshot = self.current_journal_snapshot()?;
        let mut core = self.core.borrow_mut();
        let registration = core
            .journal
            .as_mut()
            .ok_or_else(|| Error::Persistence("composition journal is unavailable".into()))?;
        registration.journal.append(&snapshot)
    }

    fn current_journal_snapshot(&self) -> Result<JournalSnapshot> {
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
        })
    }

    fn restore_composition_effects(&mut self, effects: Vec<JournalEffect>) -> Result<()> {
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

    fn application_tree(&self) -> ComponentTree {
        let mut roots = self.desired.clone();
        if let Some(journal_root) = &self.persistent_root {
            roots.remove(journal_root);
        }
        tree_from_prepared(&roots)
    }

    fn restore_persistent_state(
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

    pub fn apply_tree(&mut self, tree: ComponentTree) -> Result<()> {
        if self.persistent_root.is_none() {
            self.declare_tree(tree)?;
            return self.reconcile_to_quiescence();
        }
        let prepared = self.prepare_application_tree(tree)?;
        let previous = self.desired.clone();
        let previous_revision = self.composition_revision();
        self.declare_prepared(prepared, true)?;
        self.defer_journal = true;
        if let Err(error) = self.reconcile_to_quiescence() {
            self.defer_journal = false;
            self.restore_persistent_state(previous, previous_revision)?;
            return Err(error);
        }
        self.defer_journal = false;
        if let Err(error) = self.append_current_composition() {
            self.restore_persistent_state(previous, previous_revision)?;
            return Err(error);
        }
        Ok(())
    }

    pub fn step(&mut self) -> Result<bool> {
        if self.process_pending_patch()? {
            return Ok(true);
        }
        if self.refresh_one()? {
            return Ok(true);
        }
        if self.recover_one()? {
            return Ok(true);
        }
        if self.advance_activation()? {
            return Ok(true);
        }
        if self.remove_one()? {
            return Ok(true);
        }
        if self.begin_activation()? {
            return Ok(true);
        }
        Ok(false)
    }

    pub fn reconcile_to_quiescence(&mut self) -> Result<()> {
        for _ in 0..self.limits.max_reconciliation_steps {
            if !self.step()? {
                return Ok(());
            }
        }
        Err(Error::ReconciliationLimit(
            self.limits.max_reconciliation_steps,
        ))
    }

    pub fn replace_entry(&mut self, path: &str, spec: ComponentSpec) -> Result<()> {
        if self.persistent_root.is_some() {
            return Err(Error::Persistence(
                "persistent replacement requires a governed composition patch".into(),
            ));
        }
        self.replace_entry_internal(path, spec)
    }

    fn replace_entry_internal(&mut self, path: &str, spec: ComponentSpec) -> Result<()> {
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

    pub fn fiber_id(&self, path: &str) -> Option<FiberId> {
        self.core.borrow().fiber_by_path(path)
    }

    pub fn fiber_state(&self, path: &str) -> Option<FiberState> {
        let core = self.core.borrow();
        let fiber = core.fibers.get(&core.fiber_by_path(path)?)?;
        Some(fiber.public_state())
    }

    pub fn committed_provider(&self, path: &str, slot: u64) -> Option<FiberId> {
        let core = self.core.borrow();
        let fiber = core.fibers.get(&core.fiber_by_path(path)?)?;
        fiber.committed.get(&slot).map(|provider| provider.fiber)
    }

    pub fn provider_identity(&self, interface: &InterfaceId) -> Option<FiberId> {
        let core = self.core.borrow();
        let binding = core.bindings.get(interface)?;
        core.fibers
            .get(&binding.fiber)
            .filter(|fiber| fiber.state == InternalState::Active)
            .map(|fiber| fiber.id)
    }

    pub fn state_value(&self, path: &str, key: u64) -> Option<u64> {
        let core = self.core.borrow();
        let fiber = core.fiber_by_path(path)?;
        core.state_cells.get(&(fiber, key)).copied()
    }

    pub fn trace(&self) -> Vec<TraceEvent> {
        self.core.borrow().trace.clone()
    }

    pub fn clear_trace(&mut self) {
        self.core.borrow_mut().trace.clear();
    }

    pub fn observation(&self) -> ContextObservation {
        let core = self.core.borrow();
        ContextObservation {
            state_cells: core.state_cells.len(),
            bindings: core.bindings.len(),
            registrations: core.registrations.len(),
            fibers: core.fibers.len(),
            roots: core.roots.len(),
            live_artifacts: self.loader.live_artifact_count(),
            composition_effects: core.patch_owners.len(),
            pending_patches: core.pending_patches.len(),
            journal_registrations: usize::from(core.journal.is_some()),
        }
    }

    pub fn is_observationally_clean(&self) -> bool {
        self.observation() == ContextObservation::default()
    }

    fn prepare_tree(&self, tree: ComponentTree) -> Result<BTreeMap<String, PreparedSpec>> {
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

    fn prepare_spec(
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
        })
    }

    fn prepare_patch(&self, patch: CompositionPatch, actor_path: &str) -> Result<PreparedPatch> {
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

    fn validate_graph<'a>(&self, roots: impl Iterator<Item = &'a PreparedSpec>) -> Result<()> {
        let mut entries = Vec::new();
        for root in roots {
            flatten_specs(root, &root.entry, &mut entries);
        }
        validate_entries(&entries)
    }

    fn process_pending_patch(&mut self) -> Result<bool> {
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
                    && let Err(error) = self.append_current_composition()
                {
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
                    self.reject_patch(request.actor, target, error.to_string());
                    return Ok(true);
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
        Ok(true)
    }

    fn apply_prepared_patch(&mut self, patch: &PreparedPatch) -> Result<PatchUndo> {
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

    fn apply_patch_undo(&mut self, undo: &PatchUndo) -> Result<Option<PatchUndo>> {
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

    fn reject_patch(&mut self, actor: FiberId, target: String, error: String) {
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

    fn recover_composition(
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
        let result = if actor_still_declared || target_still_declared {
            match self.apply_patch_undo(&undo) {
                Ok(reverse) => {
                    if reverse.is_some() && self.persistent_root.is_some() && !self.defer_journal {
                        if let Err(error) = self.append_current_composition() {
                            let reverse = reverse.ok_or_else(|| {
                                Error::Invariant(
                                    "journal failure recovery lost its reverse patch".into(),
                                )
                            })?;
                            self.apply_patch_undo(&reverse)?.ok_or_else(|| {
                                Error::Invariant(
                                    "journal failure did not restore committed composition".into(),
                                )
                            })?;
                            self.core.borrow_mut().composition_revision = previous_revision;
                            Err(error)
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
        Ok(())
    }

    fn validate_candidate(&self, old: FiberId, candidate: &PreparedSpec) -> Result<()> {
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

    fn sync_live_specs(&mut self) {
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

    fn insert_root(&mut self, spec: PreparedSpec) -> Result<FiberId> {
        let mut core = self.core.borrow_mut();
        let id = core.allocate_fiber();
        let path = spec.entry.clone();
        let fiber = Fiber::new(id, None, path, spec);
        core.roots.insert(fiber.path.clone(), id);
        core.fibers.insert(id, fiber);
        Ok(id)
    }

    fn refresh_one(&mut self) -> Result<bool> {
        let mut core = self.core.borrow_mut();
        let ids: Vec<_> = core.fibers.keys().copied().collect();
        for id in ids {
            let state = core
                .fibers
                .get(&id)
                .ok_or_else(|| Error::Invariant("fiber disappeared during refresh".into()))?
                .state;
            if !matches!(state, InternalState::Active | InternalState::Activating) {
                continue;
            }
            let target = core.target_for(id);
            let committed = &core
                .fibers
                .get(&id)
                .ok_or_else(|| Error::Invariant("fiber disappeared during refresh".into()))?
                .committed;
            if target.as_ref() != Some(committed) {
                let path = {
                    let fiber = core.fibers.get_mut(&id).ok_or_else(|| {
                        Error::Invariant("fiber disappeared during refresh".into())
                    })?;
                    fiber.state = InternalState::Unloading;
                    fiber.path.clone()
                };
                core.trace
                    .push(TraceEvent::FiberUnavailable { fiber: id, path });
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn recover_one(&mut self) -> Result<bool> {
        let action = {
            let mut core = self.core.borrow_mut();
            let id = core
                .fibers
                .iter()
                .find(|(id, fiber)| {
                    fiber.state == InternalState::Unloading
                        && !core.is_relied_on(**id)
                        && !core.blocked_recovery.contains(id)
                })
                .map(|(id, _)| *id);
            let Some(id) = id else {
                return Ok(false);
            };
            let fiber = core
                .fibers
                .get_mut(&id)
                .ok_or_else(|| Error::Invariant("unloading fiber disappeared".into()))?;
            if let Some(inverse) = fiber.accumulator.pop() {
                match inverse {
                    Inverse::RestoreComposition {
                        effect,
                        target,
                        undo,
                    } => RecoveryAction::Composition {
                        fiber: id,
                        effect,
                        target,
                        undo,
                    },
                    inverse => RecoveryAction::Inverse { fiber: id, inverse },
                }
            } else {
                RecoveryAction::Finish {
                    fiber: id,
                    instance: fiber.instance.take(),
                    failed: fiber.outcome.clone(),
                }
            }
        };

        match action {
            RecoveryAction::Inverse { fiber, inverse } => {
                self.core.borrow_mut().apply_inverse(fiber, inverse)?;
            }
            RecoveryAction::Composition {
                fiber,
                effect,
                target,
                undo,
            } => {
                self.recover_composition(fiber, effect, target, undo)?;
            }
            RecoveryAction::Finish {
                fiber,
                mut instance,
                failed,
            } => {
                let drop_error = if let Some(instance) = instance.as_mut() {
                    instance
                        .drop_fn
                        .call(&mut instance.store, (instance.instance_id,))
                        .err()
                        .map(|error| error.to_string())
                } else {
                    None
                };
                drop(instance);
                let mut core = self.core.borrow_mut();
                let event = {
                    let fiber_record = core
                        .fibers
                        .get_mut(&fiber)
                        .ok_or_else(|| Error::Invariant("unloading fiber disappeared".into()))?;
                    fiber_record.committed.clear();
                    let path = fiber_record.path.clone();
                    if let Some(error) = failed {
                        fiber_record.state = InternalState::Failed;
                        fiber_record.outcome = Some(error.clone());
                        TraceEvent::FiberFailed { fiber, path, error }
                    } else {
                        fiber_record.state = InternalState::Inactive;
                        TraceEvent::FiberInactive { fiber, path }
                    }
                };
                if let Some(error) = drop_error {
                    let path = core
                        .fibers
                        .get(&fiber)
                        .map(|fiber| fiber.path.clone())
                        .unwrap_or_default();
                    core.trace
                        .push(TraceEvent::DisposalFailed { fiber, path, error });
                }
                core.trace.push(event);
            }
        }
        Ok(true)
    }

    fn advance_activation(&mut self) -> Result<bool> {
        let (fiber_id, mut instance, max_steps) = {
            let mut core = self.core.borrow_mut();
            let Some(id) = core
                .fibers
                .iter()
                .find(|(_, fiber)| fiber.state == InternalState::Activating)
                .map(|(id, _)| *id)
            else {
                return Ok(false);
            };
            let fiber = core
                .fibers
                .get_mut(&id)
                .ok_or_else(|| Error::Invariant("activating fiber disappeared".into()))?;
            let instance = fiber
                .instance
                .take()
                .ok_or_else(|| Error::Invariant("activating fiber has no guest instance".into()))?;
            (
                id,
                instance,
                fiber.spec.artifact.manifest.component.max_activation_steps,
            )
        };

        let result = instance
            .step
            .call(&mut instance.store, (instance.instance_id,))
            .map(|result| result.0)
            .map_err(|error| error.to_string());

        let mut core = self.core.borrow_mut();
        let activated_path = {
            let fiber = core
                .fibers
                .get_mut(&fiber_id)
                .ok_or_else(|| Error::Invariant("activating fiber disappeared".into()))?;
            fiber.instance = Some(instance);
            fiber.activation_steps += 1;
            match result {
                Ok(1) => {
                    fiber.state = InternalState::Active;
                    Some(fiber.path.clone())
                }
                result => {
                    let outcome = match result {
                        Ok(0) if fiber.activation_steps < max_steps => None,
                        Ok(0) => Some(format!("activation exceeded {max_steps} declared steps")),
                        Ok(code) if code < 0 => Some(format!("guest returned status {}", -code)),
                        Ok(code) => Some(format!("guest returned invalid step code {code}")),
                        Err(error) => Some(format!("guest trapped: {error}")),
                    };
                    if let Some(error) = outcome {
                        fiber.outcome = Some(error);
                        fiber.state = InternalState::Unloading;
                    }
                    None
                }
            }
        };
        if let Some(path) = activated_path {
            core.trace.push(TraceEvent::FiberActivated {
                fiber: fiber_id,
                path,
            });
        }
        Ok(true)
    }

    fn begin_activation(&mut self) -> Result<bool> {
        let candidate = {
            let core = self.core.borrow();
            core.fibers.iter().find_map(|(id, fiber)| {
                (fiber.state == InternalState::Inactive
                    && !fiber.retired
                    && fiber.outcome.is_none())
                .then(|| core.target_for(*id).map(|target| (*id, target)))
                .flatten()
            })
        };
        let Some((id, target)) = candidate else {
            return Ok(false);
        };
        let spec = self
            .core
            .borrow()
            .fibers
            .get(&id)
            .ok_or_else(|| Error::Invariant("activation candidate disappeared".into()))?
            .spec
            .clone();
        match self.instantiate(id, &spec) {
            Ok(instance) => {
                let mut core = self.core.borrow_mut();
                let path = {
                    let fiber = core.fibers.get_mut(&id).ok_or_else(|| {
                        Error::Invariant("activation candidate disappeared".into())
                    })?;
                    fiber.committed = target;
                    fiber.activation_steps = 0;
                    fiber.instance = Some(instance);
                    fiber.state = InternalState::Activating;
                    fiber.path.clone()
                };
                core.trace
                    .push(TraceEvent::FiberActivating { fiber: id, path });
            }
            Err(error) => {
                let mut core = self.core.borrow_mut();
                let error = error.to_string();
                let path = {
                    let fiber = core.fibers.get_mut(&id).ok_or_else(|| {
                        Error::Invariant("activation candidate disappeared".into())
                    })?;
                    fiber.state = InternalState::Failed;
                    fiber.outcome = Some(error.clone());
                    fiber.path.clone()
                };
                core.trace.push(TraceEvent::FiberFailed {
                    fiber: id,
                    path,
                    error,
                });
            }
        }
        Ok(true)
    }

    fn remove_one(&mut self) -> Result<bool> {
        let mut core = self.core.borrow_mut();
        let removable = core.fibers.iter().find_map(|(id, fiber)| {
            (fiber.retired
                && !fiber.pinned
                && matches!(fiber.state, InternalState::Inactive | InternalState::Failed)
                && !core.fibers.values().any(|child| child.parent == Some(*id)))
            .then_some(*id)
        });
        let Some(id) = removable else {
            return Ok(false);
        };
        core.remove_fiber(id)?;
        Ok(true)
    }

    fn instantiate(&self, fiber: FiberId, spec: &PreparedSpec) -> Result<GuestInstance> {
        let mut linker = Linker::new(self.loader.engine());
        link_host(&mut linker)?;
        let mut store = Store::new(
            self.loader.engine(),
            HostState {
                core: Rc::downgrade(&self.core),
                fiber,
            },
        );
        let instance = linker
            .instantiate(&mut store, &spec.artifact.component)
            .map_err(|error| Error::Link(error.to_string()))?;
        let start = instance
            .get_typed_func::<(u64,), (u64,)>(&mut store, "start")
            .map_err(|error| Error::Link(error.to_string()))?;
        let step = instance
            .get_typed_func::<(u64,), (i32,)>(&mut store, "step")
            .map_err(|error| Error::Link(error.to_string()))?;
        let drop_fn = instance
            .get_typed_func::<(u64,), ()>(&mut store, "drop")
            .map_err(|error| Error::Link(error.to_string()))?;
        let invoke = instance
            .get_typed_func::<(u64, u64, u64, u64), (i64,)>(&mut store, "invoke")
            .map_err(|error| Error::Link(error.to_string()))?;
        let instance_id = start
            .call(&mut store, (spec.config,))
            .map_err(|error| Error::Activation(format!("start trapped: {error}")))?
            .0;
        Ok(GuestInstance {
            store,
            _instance: instance,
            instance_id,
            step,
            drop_fn,
            invoke,
        })
    }

    fn swap_in_candidate(
        &mut self,
        parent: Option<FiberId>,
        path: &str,
        old: FiberId,
        candidate: FiberId,
        spec: PreparedSpec,
        instance: GuestInstance,
    ) -> Result<()> {
        let mut core = self.core.borrow_mut();
        let old_record = core
            .fibers
            .remove(&old)
            .ok_or_else(|| Error::Invariant("old generation disappeared during swap".into()))?;
        if !old_record.accumulator.is_empty() || old_record.instance.is_some() {
            return Err(Error::Invariant(
                "old generation retained effects at swap".into(),
            ));
        }
        core.retarget_slot(parent, path, old, candidate)?;
        let target = core.target_for_spec(&spec, true);
        let mut fiber = Fiber::new(candidate, parent, path.into(), spec);
        fiber.pinned = true;
        if let Some(target) = target {
            fiber.state = InternalState::Activating;
            fiber.committed = target;
            fiber.instance = Some(instance);
            core.trace.push(TraceEvent::FiberActivating {
                fiber: candidate,
                path: path.into(),
            });
        } else {
            drop(instance);
        }
        core.fibers.insert(candidate, fiber);
        Ok(())
    }

    fn rollback_replacement(&mut self, backup: FiberBackup, candidate: FiberId) -> Result<()> {
        {
            let mut core = self.core.borrow_mut();
            let unavailable = {
                let candidate_record = core.fibers.get_mut(&candidate).ok_or_else(|| {
                    Error::Invariant("candidate disappeared before rollback".into())
                })?;
                candidate_record.retired = true;
                candidate_record.pinned = true;
                if candidate_record.state == InternalState::Active {
                    candidate_record.state = InternalState::Unloading;
                    Some(candidate_record.path.clone())
                } else {
                    None
                }
            };
            if let Some(path) = unavailable {
                core.trace.push(TraceEvent::FiberUnavailable {
                    fiber: candidate,
                    path,
                });
            }
        }
        self.reconcile_to_quiescence()?;
        {
            let mut core = self.core.borrow_mut();
            let rejected = core
                .fibers
                .remove(&candidate)
                .ok_or_else(|| Error::Invariant("candidate disappeared during rollback".into()))?;
            if !rejected.accumulator.is_empty() || rejected.instance.is_some() {
                return Err(Error::Invariant(
                    "candidate retained effects after rollback".into(),
                ));
            }
            core.retarget_slot(backup.parent, &backup.path, candidate, backup.id)?;
            let restored = Fiber::new(backup.id, backup.parent, backup.path.clone(), backup.spec);
            core.fibers.insert(backup.id, restored);
            core.trace.push(TraceEvent::ReplacementRolledBack {
                restored: backup.id,
                rejected: candidate,
                path: backup.path,
            });
        }
        self.reconcile_to_quiescence()
    }
}

struct FiberBackup {
    id: FiberId,
    parent: Option<FiberId>,
    path: String,
    spec: PreparedSpec,
}

enum RecoveryAction {
    Inverse {
        fiber: FiberId,
        inverse: Inverse,
    },
    Composition {
        fiber: FiberId,
        effect: u64,
        target: String,
        undo: PatchUndo,
    },
    Finish {
        fiber: FiberId,
        instance: Option<GuestInstance>,
        failed: Option<String>,
    },
}

impl Core {
    fn new(limits: Limits) -> Self {
        Self {
            limits,
            next_fiber: 1,
            next_effect: 1,
            next_registration: 1,
            composition_revision: 0,
            fibers: BTreeMap::new(),
            blocked_recovery: BTreeSet::new(),
            roots: BTreeMap::new(),
            registrations: BTreeMap::new(),
            state_cells: BTreeMap::new(),
            bindings: BTreeMap::new(),
            patch_owners: BTreeMap::new(),
            pending_patches: VecDeque::new(),
            trace: Vec::new(),
            journal: None,
            journal_failure: None,
            replaying: false,
        }
    }

    fn allocate_fiber(&mut self) -> FiberId {
        let id = FiberId(self.next_fiber);
        self.next_fiber += 1;
        id
    }

    fn allocate_effect(&mut self) -> u64 {
        let id = self.next_effect;
        self.next_effect += 1;
        id
    }

    fn fiber_by_path(&self, path: &str) -> Option<FiberId> {
        self.fibers
            .iter()
            .find_map(|(id, fiber)| (fiber.path == path).then_some(*id))
    }

    fn target_for(&self, fiber: FiberId) -> Option<ProviderView> {
        let record = self.fibers.get(&fiber)?;
        if record.retired {
            return None;
        }
        self.target_for_spec(&record.spec, true)
    }

    fn target_for_spec(&self, spec: &PreparedSpec, require_active: bool) -> Option<ProviderView> {
        let mut target = ProviderView::new();
        for binding in &spec.artifact.manifest.component.inject {
            let requirement = binding.requirement();
            let resolved = self.bindings.iter().find_map(|(interface, provider)| {
                if !requirement.accepts(interface) {
                    return None;
                }
                let active = self
                    .fibers
                    .get(&provider.fiber)
                    .is_some_and(|fiber| fiber.state == InternalState::Active);
                (!require_active || active).then(|| CommittedProvider {
                    fiber: provider.fiber,
                    interface: interface.clone(),
                })
            })?;
            target.insert(binding.slot, resolved);
        }
        Some(target)
    }

    fn is_relied_on(&self, provider: FiberId) -> bool {
        self.fibers.values().any(|fiber| {
            matches!(
                fiber.state,
                InternalState::Activating | InternalState::Active | InternalState::Unloading
            ) && fiber
                .committed
                .values()
                .any(|committed| committed.fiber == provider)
        })
    }

    fn retire_fiber(&mut self, fiber: FiberId) -> Result<()> {
        let fiber = self
            .fibers
            .get_mut(&fiber)
            .ok_or_else(|| Error::Invariant("retired fiber does not exist".into()))?;
        fiber.retired = true;
        Ok(())
    }

    fn apply_inverse(&mut self, fiber_id: FiberId, inverse: Inverse) -> Result<()> {
        let effect = inverse.effect();
        let kind = inverse.kind().to_string();
        match inverse {
            Inverse::RestoreState { key, previous, .. } => {
                if let Some(previous) = previous {
                    self.state_cells.insert((fiber_id, key), previous);
                } else {
                    self.state_cells.remove(&(fiber_id, key));
                }
            }
            Inverse::RemoveBinding { interface, .. } => {
                let binding = self.bindings.remove(&interface).ok_or_else(|| {
                    Error::Invariant("coeffect inverse found no installed binding".into())
                })?;
                if binding.fiber != fiber_id {
                    return Err(Error::Invariant(
                        "coeffect inverse targeted another provider".into(),
                    ));
                }
            }
            Inverse::RetireChild { registration, .. } => {
                let registration = self.registrations.get(&registration).ok_or_else(|| {
                    Error::Invariant("registration inverse found no registration".into())
                })?;
                let child = registration.child;
                let parent = registration.parent;
                let path = self
                    .fibers
                    .get(&child)
                    .map(|fiber| fiber.path.clone())
                    .unwrap_or_else(|| registration.entry.clone());
                self.retire_fiber(child)?;
                self.trace.push(TraceEvent::ChildRetired {
                    parent,
                    child,
                    path,
                });
            }
            Inverse::RestoreComposition { .. } => {
                return Err(Error::Invariant(
                    "composition inverse reached core recovery".into(),
                ));
            }
            Inverse::CloseJournal { .. } => {
                let registration = self.journal.take().ok_or_else(|| {
                    Error::Invariant("journal inverse found no registered journal".into())
                })?;
                if registration.owner != fiber_id {
                    return Err(Error::Invariant(
                        "journal inverse targeted another provider".into(),
                    ));
                }
            }
        }
        self.trace.push(TraceEvent::EffectRecovered {
            fiber: fiber_id,
            effect,
            kind,
        });
        Ok(())
    }

    fn remove_fiber(&mut self, id: FiberId) -> Result<()> {
        let fiber = self
            .fibers
            .remove(&id)
            .ok_or_else(|| Error::Invariant("removed fiber does not exist".into()))?;
        if !fiber.accumulator.is_empty()
            || fiber.instance.is_some()
            || self.state_cells.keys().any(|(owner, _)| *owner == id)
            || self.bindings.values().any(|binding| binding.fiber == id)
            || self.patch_owners.values().any(|owner| *owner == id)
            || self
                .pending_patches
                .iter()
                .any(|request| request.actor == id)
            || self
                .journal
                .as_ref()
                .is_some_and(|journal| journal.owner == id)
        {
            return Err(Error::Invariant(
                "removed fiber retained context effects".into(),
            ));
        }
        self.roots.retain(|_, fiber| *fiber != id);
        self.registrations
            .retain(|_, registration| registration.child != id);
        self.trace.push(TraceEvent::FiberRemoved {
            fiber: id,
            path: fiber.path,
        });
        Ok(())
    }

    fn retarget_slot(
        &mut self,
        parent: Option<FiberId>,
        path: &str,
        old: FiberId,
        new: FiberId,
    ) -> Result<()> {
        if let Some(parent) = parent {
            let registration = self
                .registrations
                .values_mut()
                .find(|registration| registration.parent == parent && registration.child == old)
                .ok_or_else(|| Error::Invariant("parent registration disappeared".into()))?;
            registration.child = new;
        } else {
            let root = path
                .split('/')
                .next()
                .ok_or_else(|| Error::Invariant("root path is empty".into()))?;
            let slot = self
                .roots
                .get_mut(root)
                .ok_or_else(|| Error::Invariant("root registration disappeared".into()))?;
            if *slot != old {
                return Err(Error::Invariant(
                    "root registration targets another fiber".into(),
                ));
            }
            *slot = new;
        }
        Ok(())
    }

    fn host_set_state(&mut self, fiber: FiberId, key: u64, value: u64) -> i32 {
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
            .requests(HostCapability::SetState)
        {
            return STATUS_UNDECLARED;
        }
        let previous = self.state_cells.insert((fiber, key), value);
        let effect = self.allocate_effect();
        let inverse = Inverse::RestoreState {
            effect,
            key,
            previous,
        };
        self.fibers
            .get_mut(&fiber)
            .expect("fiber checked above")
            .accumulator
            .push(inverse);
        self.trace.push(TraceEvent::EffectApplied {
            fiber,
            effect,
            kind: "state".into(),
        });
        STATUS_OK
    }

    fn host_publish(&mut self, fiber: FiberId, slot: u64, value: u64) -> i32 {
        let Some(record) = self.fibers.get(&fiber) else {
            return STATUS_INVALID;
        };
        if record.state != InternalState::Activating || value > i64::MAX as u64 {
            return STATUS_INVALID;
        }
        if !record
            .spec
            .artifact
            .manifest
            .requests(HostCapability::Publish)
        {
            return STATUS_UNDECLARED;
        }
        let Some(interface) = record
            .spec
            .artifact
            .manifest
            .provided_by_slot()
            .get(&slot)
            .cloned()
        else {
            return STATUS_UNDECLARED;
        };
        if interface.kind != BindingKind::Value {
            return STATUS_UNDECLARED;
        }
        if self.bindings.contains_key(&interface) {
            return STATUS_COLLISION;
        }
        self.bindings.insert(
            interface.clone(),
            ProviderBinding {
                fiber,
                kind: BindingKind::Value,
                value: Some(value),
            },
        );
        let effect = self.allocate_effect();
        self.fibers
            .get_mut(&fiber)
            .expect("fiber checked above")
            .accumulator
            .push(Inverse::RemoveBinding { effect, interface });
        self.trace.push(TraceEvent::EffectApplied {
            fiber,
            effect,
            kind: "coeffect".into(),
        });
        STATUS_OK
    }

    fn host_resolve(&self, fiber: FiberId, slot: u64) -> i64 {
        let Some(record) = self.fibers.get(&fiber) else {
            return -(STATUS_INVALID as i64);
        };
        if record.state != InternalState::Activating {
            return -(STATUS_INVALID as i64);
        }
        if !record
            .spec
            .artifact
            .manifest
            .requests(HostCapability::Resolve)
            || !record
                .spec
                .artifact
                .manifest
                .required_by_slot()
                .contains_key(&slot)
        {
            return -(STATUS_UNDECLARED as i64);
        }
        let Some(committed) = record.committed.get(&slot) else {
            return -(STATUS_UNSATISFIED as i64);
        };
        self.bindings
            .get(&committed.interface)
            .filter(|binding| {
                binding.fiber == committed.fiber && binding.kind == BindingKind::Value
            })
            .and_then(|binding| binding.value)
            .map(|value| value as i64)
            .unwrap_or(-(STATUS_UNSATISFIED as i64))
    }

    fn host_publish_callable(&mut self, fiber: FiberId, slot: u64) -> i32 {
        let Some(record) = self.fibers.get(&fiber) else {
            return STATUS_INVALID;
        };
        if record.state != InternalState::Activating
            || !record
                .spec
                .artifact
                .manifest
                .requests(HostCapability::PublishCallable)
        {
            return STATUS_UNDECLARED;
        }
        let Some(interface) = record
            .spec
            .artifact
            .manifest
            .provided_by_slot()
            .get(&slot)
            .filter(|interface| interface.kind == BindingKind::Callable)
            .cloned()
        else {
            return STATUS_UNDECLARED;
        };
        if self.bindings.contains_key(&interface) {
            return STATUS_COLLISION;
        }
        self.bindings.insert(
            interface.clone(),
            ProviderBinding {
                fiber,
                kind: BindingKind::Callable,
                value: None,
            },
        );
        let effect = self.allocate_effect();
        self.fibers
            .get_mut(&fiber)
            .expect("fiber checked above")
            .accumulator
            .push(Inverse::RemoveBinding { effect, interface });
        self.trace.push(TraceEvent::EffectApplied {
            fiber,
            effect,
            kind: "callable-coeffect".into(),
        });
        STATUS_OK
    }

    fn host_invoke(
        &mut self,
        caller: FiberId,
        slot: u64,
        operation: u64,
        arg0: u64,
        arg1: u64,
    ) -> i64 {
        let Some(caller_record) = self.fibers.get(&caller) else {
            return -(STATUS_INVALID as i64);
        };
        if caller_record.state != InternalState::Activating
            || !caller_record
                .spec
                .artifact
                .manifest
                .requests(HostCapability::Invoke)
        {
            return -(STATUS_UNDECLARED as i64);
        }
        let Some(committed) = caller_record.committed.get(&slot).cloned() else {
            return -(STATUS_UNSATISFIED as i64);
        };
        if committed.interface.kind != BindingKind::Callable {
            return -(STATUS_UNDECLARED as i64);
        }
        let Some(binding) = self.bindings.get(&committed.interface) else {
            return -(STATUS_UNSATISFIED as i64);
        };
        if binding.fiber != committed.fiber || binding.kind != BindingKind::Callable {
            return -(STATUS_UNSATISFIED as i64);
        }
        let provider = committed.fiber;
        let Some(mut instance) = self.fibers.get_mut(&provider).and_then(|record| {
            (record.state == InternalState::Active)
                .then(|| record.instance.take())
                .flatten()
        }) else {
            return -(STATUS_UNSATISFIED as i64);
        };
        let result = instance
            .invoke
            .call(
                &mut instance.store,
                (instance.instance_id, operation, arg0, arg1),
            )
            .map(|result| result.0)
            .unwrap_or(-(STATUS_INVALID as i64));
        if let Some(record) = self.fibers.get_mut(&provider) {
            record.instance = Some(instance);
        } else {
            return -(STATUS_INVALID as i64);
        }
        if result == 1
            && operation == 1
            && committed.interface.namespace == "quartz.composition"
            && committed.interface.interface == "patch-authority"
            && committed.interface.revision == 1
        {
            let Ok(index) = usize::try_from(arg0) else {
                return -(STATUS_INVALID as i64);
            };
            if let Some(record) = self.fibers.get_mut(&caller) {
                record.patch_authorization = Some(PatchAuthorization {
                    provider,
                    index,
                    base_revision: arg1,
                });
            }
        }
        result
    }

    fn host_apply_patch(&mut self, actor: FiberId, index: u64, base_revision: u64) -> i32 {
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

    fn host_register_child(&mut self, parent: FiberId, index: u32) -> i32 {
        let (spec, parent_path) = {
            let Some(parent_record) = self.fibers.get(&parent) else {
                return STATUS_INVALID;
            };
            if parent_record.state != InternalState::Activating {
                return STATUS_INVALID;
            }
            if !parent_record
                .spec
                .artifact
                .manifest
                .requests(HostCapability::RegisterChild)
            {
                return STATUS_UNDECLARED;
            }
            let Some(spec) = parent_record.spec.children.get(index as usize).cloned() else {
                return STATUS_INVALID;
            };
            (spec, parent_record.path.clone())
        };
        if self
            .registrations
            .values()
            .any(|registration| registration.parent == parent && registration.entry == spec.entry)
        {
            return STATUS_COLLISION;
        }
        if self.fibers.len() >= self.limits.max_components {
            return STATUS_LIMIT;
        }
        let child = self.allocate_fiber();
        let registration_id = self.next_registration;
        self.next_registration += 1;
        let path = format!("{parent_path}/{}", spec.entry);
        self.registrations.insert(
            registration_id,
            Registration {
                parent,
                child,
                entry: spec.entry.clone(),
            },
        );
        self.fibers
            .insert(child, Fiber::new(child, Some(parent), path.clone(), spec));
        let effect = self.allocate_effect();
        self.fibers
            .get_mut(&parent)
            .expect("parent checked above")
            .accumulator
            .push(Inverse::RetireChild {
                effect,
                registration: registration_id,
            });
        self.trace.push(TraceEvent::EffectApplied {
            fiber: parent,
            effect,
            kind: "component-registration".into(),
        });
        self.trace.push(TraceEvent::ChildRegistered {
            parent,
            child,
            path,
        });
        STATUS_OK
    }

    fn host_open_journal(&mut self, fiber: FiberId, index: u64) -> i32 {
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

impl Fiber {
    fn new(id: FiberId, parent: Option<FiberId>, path: String, spec: PreparedSpec) -> Self {
        Self {
            id,
            parent,
            path,
            spec,
            retired: false,
            pinned: false,
            state: InternalState::Inactive,
            committed: BTreeMap::new(),
            accumulator: Vec::new(),
            instance: None,
            activation_steps: 0,
            outcome: None,
            patch_authorization: None,
        }
    }

    fn public_state(&self) -> FiberState {
        match self.state {
            InternalState::Inactive => FiberState::Inactive,
            InternalState::Activating => FiberState::Activating,
            InternalState::Active => FiberState::Active,
            InternalState::Unloading => FiberState::Unloading,
            InternalState::Failed => FiberState::Failed(
                self.outcome
                    .clone()
                    .unwrap_or_else(|| "unknown failure".into()),
            ),
        }
    }
}

fn link_host(linker: &mut Linker<HostState>) -> Result<()> {
    linker
        .root()
        .func_wrap(
            "set-state",
            |store: StoreContextMut<'_, HostState>, (key, value): (u64, u64)| {
                Ok((with_core(store, |core, fiber| {
                    core.host_set_state(fiber, key, value)
                }),))
            },
        )
        .map_err(|error| Error::Link(error.to_string()))?;
    linker
        .root()
        .func_wrap(
            "publish",
            |store: StoreContextMut<'_, HostState>, (slot, value): (u64, u64)| {
                Ok((with_core(store, |core, fiber| {
                    core.host_publish(fiber, slot, value)
                }),))
            },
        )
        .map_err(|error| Error::Link(error.to_string()))?;
    linker
        .root()
        .func_wrap(
            "resolve",
            |store: StoreContextMut<'_, HostState>, (slot,): (u64,)| {
                Ok((with_core(store, |core, fiber| {
                    core.host_resolve(fiber, slot)
                }),))
            },
        )
        .map_err(|error| Error::Link(error.to_string()))?;
    linker
        .root()
        .func_wrap(
            "publish-callable",
            |store: StoreContextMut<'_, HostState>, (slot,): (u64,)| {
                Ok((with_core(store, |core, fiber| {
                    core.host_publish_callable(fiber, slot)
                }),))
            },
        )
        .map_err(|error| Error::Link(error.to_string()))?;
    linker
        .root()
        .func_wrap(
            "call-provider",
            |store: StoreContextMut<'_, HostState>,
             (slot, operation, arg0, arg1): (u64, u64, u64, u64)| {
                Ok((with_core(store, |core, fiber| {
                    core.host_invoke(fiber, slot, operation, arg0, arg1)
                }),))
            },
        )
        .map_err(|error| Error::Link(error.to_string()))?;
    linker
        .root()
        .func_wrap(
            "apply-patch",
            |store: StoreContextMut<'_, HostState>, (index, revision): (u64, u64)| {
                Ok((with_core(store, |core, fiber| {
                    core.host_apply_patch(fiber, index, revision)
                }),))
            },
        )
        .map_err(|error| Error::Link(error.to_string()))?;
    linker
        .root()
        .func_wrap(
            "register-child",
            |store: StoreContextMut<'_, HostState>, (index,): (u32,)| {
                Ok((with_core(store, |core, fiber| {
                    core.host_register_child(fiber, index)
                }),))
            },
        )
        .map_err(|error| Error::Link(error.to_string()))?;
    linker
        .root()
        .func_wrap(
            "open-journal",
            |store: StoreContextMut<'_, HostState>, (index,): (u64,)| {
                Ok((with_core(store, |core, fiber| {
                    core.host_open_journal(fiber, index)
                }),))
            },
        )
        .map_err(|error| Error::Link(error.to_string()))?;
    Ok(())
}

fn with_core<T>(
    store: StoreContextMut<'_, HostState>,
    operation: impl FnOnce(&mut Core, FiberId) -> T,
) -> T
where
    T: HostFailure,
{
    let fiber = store.data().fiber;
    let Some(core) = store.data().core.upgrade() else {
        return T::host_failure();
    };
    let Ok(mut core) = core.try_borrow_mut() else {
        return T::host_failure();
    };
    operation(&mut core, fiber)
}

trait HostFailure {
    fn host_failure() -> Self;
}

impl HostFailure for i32 {
    fn host_failure() -> Self {
        STATUS_INVALID
    }
}

impl HostFailure for i64 {
    fn host_failure() -> Self {
        -(STATUS_INVALID as i64)
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
        && left.children.len() == right.children.len()
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
