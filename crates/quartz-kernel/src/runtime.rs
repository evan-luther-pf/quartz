use std::{
    cell::RefCell,
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
    rc::{Rc, Weak},
    sync::Arc,
};

use wasmtime::{
    Store, StoreContextMut,
    component::{Instance, Linker, TypedFunc},
};

use crate::{
    Error, HostCapability, InterfaceId, Result,
    module::{Artifact, ModuleLoader},
};

const STATUS_OK: i32 = 0;
const STATUS_UNDECLARED: i32 = 2;
const STATUS_UNSATISFIED: i32 = 3;
const STATUS_INVALID: i32 = 4;
const STATUS_LIMIT: i32 = 5;
const STATUS_COLLISION: i32 = 6;

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct FiberId(pub u64);

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ComponentSpec {
    pub entry: String,
    pub artifact: PathBuf,
    pub config: u64,
    pub children: Vec<ComponentSpec>,
}

impl ComponentSpec {
    pub fn new(entry: impl Into<String>, artifact: impl Into<PathBuf>) -> Self {
        Self {
            entry: entry.into(),
            artifact: artifact.into(),
            config: 0,
            children: Vec::new(),
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
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ComponentTree {
    pub roots: Vec<ComponentSpec>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Limits {
    pub max_components: usize,
    pub max_depth: usize,
    pub max_activation_steps: u32,
    pub max_reconciliation_steps: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_components: 128,
            max_depth: 16,
            max_activation_steps: 1024,
            max_reconciliation_steps: 100_000,
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
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ContextObservation {
    pub state_cells: usize,
    pub bindings: usize,
    pub registrations: usize,
    pub fibers: usize,
    pub roots: usize,
    pub live_artifacts: usize,
}

pub struct Runtime {
    loader: ModuleLoader,
    core: Rc<RefCell<Core>>,
    limits: Limits,
    desired: BTreeMap<String, PreparedSpec>,
}

#[derive(Clone)]
struct PreparedSpec {
    entry: String,
    artifact: Arc<Artifact>,
    config: u64,
    children: Vec<PreparedSpec>,
}

struct Core {
    limits: Limits,
    next_fiber: u64,
    next_effect: u64,
    next_registration: u64,
    fibers: BTreeMap<FiberId, Fiber>,
    roots: BTreeMap<String, FiberId>,
    registrations: BTreeMap<u64, Registration>,
    state_cells: BTreeMap<(FiberId, u64), u64>,
    bindings: BTreeMap<InterfaceId, ProviderBinding>,
    trace: Vec<TraceEvent>,
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
    value: u64,
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
}

impl Inverse {
    fn effect(&self) -> u64 {
        match self {
            Self::RestoreState { effect, .. }
            | Self::RemoveBinding { effect, .. }
            | Self::RetireChild { effect, .. } => *effect,
        }
    }

    fn kind(&self) -> &'static str {
        match self {
            Self::RestoreState { .. } => "state",
            Self::RemoveBinding { .. } => "coeffect",
            Self::RetireChild { .. } => "component-registration",
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
        })
    }

    pub fn declare_tree(&mut self, tree: ComponentTree) -> Result<()> {
        let prepared = self.prepare_tree(tree)?;
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
        Ok(())
    }

    pub fn apply_tree(&mut self, tree: ComponentTree) -> Result<()> {
        self.declare_tree(tree)?;
        self.reconcile_to_quiescence()
    }

    pub fn step(&mut self) -> Result<bool> {
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
        let artifact = self.loader.load(&spec.artifact)?;
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
        Ok(PreparedSpec {
            entry: spec.entry,
            artifact,
            config: spec.config,
            children,
        })
    }

    fn validate_graph<'a>(&self, roots: impl Iterator<Item = &'a PreparedSpec>) -> Result<()> {
        let mut entries = Vec::new();
        for root in roots {
            flatten_specs(root, &root.entry, &mut entries);
        }
        validate_entries(&entries)
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
                    fiber.state == InternalState::Unloading && !core.is_relied_on(**id)
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
                RecoveryAction::Inverse { fiber: id, inverse }
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
            fibers: BTreeMap::new(),
            roots: BTreeMap::new(),
            registrations: BTreeMap::new(),
            state_cells: BTreeMap::new(),
            bindings: BTreeMap::new(),
            trace: Vec::new(),
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
        if self.bindings.contains_key(&interface) {
            return STATUS_COLLISION;
        }
        self.bindings
            .insert(interface.clone(), ProviderBinding { fiber, value });
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
            .filter(|binding| binding.fiber == committed.fiber)
            .map(|binding| binding.value as i64)
            .unwrap_or(-(STATUS_UNSATISFIED as i64))
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
            "register-child",
            |store: StoreContextMut<'_, HostState>, (index,): (u32,)| {
                Ok((with_core(store, |core, fiber| {
                    core.host_register_child(fiber, index)
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
    operation(&mut core.borrow_mut(), fiber)
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
        && left.config == right.config
        && left.children.len() == right.children.len()
        && left
            .children
            .iter()
            .zip(&right.children)
            .all(|(left, right)| same_spec(left, right))
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
