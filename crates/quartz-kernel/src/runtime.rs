use std::{cell::RefCell, collections::BTreeMap, rc::Rc, sync::Arc};

use crate::{
    Error, InterfaceId, Result,
    component::{
        ComponentSpec, ComponentTree, ContextObservation, FiberId, FiberState, Limits, TraceEvent,
    },
    composition::PreparedSpec,
    exchange::ExchangeAdapter,
    fiber::{Core, Fiber, FiberBackup, InternalState, Inverse, RecoveryAction},
    module::ModuleLoader,
    wasm_host::GuestInstance,
};

pub struct Runtime {
    pub(crate) loader: ModuleLoader,
    pub(crate) core: Rc<RefCell<Core>>,
    pub(crate) limits: Limits,
    pub(crate) desired: BTreeMap<String, PreparedSpec>,
    pub(crate) persistent_root: Option<String>,
    pub(crate) defer_journal: bool,
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

    pub fn new_with_exchange(limits: Limits, adapter: Arc<dyn ExchangeAdapter>) -> Result<Self> {
        let runtime = Self::new(limits)?;
        runtime.core.borrow_mut().exchange_adapter = Some(adapter);
        Ok(runtime)
    }
    pub fn open_persistent(limits: Limits, journal_component: ComponentSpec) -> Result<Self> {
        Self::open_persistent_inner(limits, journal_component, None)
    }

    pub fn open_persistent_with_exchange(
        limits: Limits,
        journal_component: ComponentSpec,
        adapter: Arc<dyn ExchangeAdapter>,
    ) -> Result<Self> {
        Self::open_persistent_inner(limits, journal_component, Some(adapter))
    }

    pub(crate) fn open_persistent_inner(
        limits: Limits,
        journal_component: ComponentSpec,
        adapter: Option<Arc<dyn ExchangeAdapter>>,
    ) -> Result<Self> {
        if journal_component.journal_paths.len() != 1
            || journal_component.event_stream_paths.len() > 1
            || !journal_component.event_grants.is_empty()
            || !journal_component.snapshot_grants.is_empty()
            || !journal_component.exchange_grants.is_empty()
            || !journal_component.children.is_empty()
            || !journal_component.patches.is_empty()
        {
            return Err(Error::Persistence(
                "persistence bootstrap must admit one journal path, at most one event path, and no grants, children, or patches".into(),
            ));
        }
        let journal_root = journal_component.entry.clone();
        let expects_events = !journal_component.event_stream_paths.is_empty();
        let mut runtime = match adapter {
            Some(adapter) => Self::new_with_exchange(limits, adapter)?,
            None => Self::new(limits)?,
        };
        runtime.persistent_root = Some(journal_root.clone());
        let bootstrap = runtime.prepare_tree(ComponentTree {
            roots: vec![journal_component],
        })?;
        runtime.declare_prepared(bootstrap, false)?;
        runtime.reconcile_to_quiescence()?;
        if runtime.core.borrow().journal.is_none() {
            let failure = {
                let mut core = runtime.core.borrow_mut();
                core.event_failure
                    .take()
                    .or_else(|| core.journal_failure.take())
                    .unwrap_or_else(|| {
                        Error::Persistence(
                            "persistence component did not register its journal".into(),
                        )
                    })
            };
            return Err(failure);
        }
        if expects_events && runtime.core.borrow().event_stream.is_none() {
            let failure = runtime
                .core
                .borrow_mut()
                .event_failure
                .take()
                .unwrap_or_else(|| {
                    Error::Persistence(
                        "persistence component did not register its event stream".into(),
                    )
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
            if let Some(event_stream) = &core.event_stream
                && event_stream.owner != registration.owner
            {
                return Err(Error::Persistence(
                    "event stream and journal must have the same bootstrap owner".into(),
                ));
            }
            registration.journal.recovered()
        };
        if let Some(mut snapshot) = recovered {
            let prepared = runtime.prepare_application_tree(snapshot.tree.clone())?;
            let outbox_next = snapshot
                .event_outbox
                .iter()
                .map(|fact| fact.id)
                .max()
                .map_or(1, |id| id + 1);
            if snapshot.next_event_id < outbox_next {
                return Err(Error::JournalCorrupt(
                    "next event id precedes the recovered outbox".into(),
                ));
            }
            let stream_next = runtime
                .core
                .borrow()
                .event_stream
                .as_ref()
                .map_or(1, |registration| registration.stream.next_id());
            snapshot.next_event_id = snapshot.next_event_id.max(stream_next);
            runtime.drain_recovered_event_outbox(&mut snapshot)?;
            {
                let mut core = runtime.core.borrow_mut();
                core.composition_revision = snapshot.composition_revision;
                core.next_event_id = snapshot.next_event_id;
            }
            runtime.declare_prepared(prepared, false)?;
            runtime.core.borrow_mut().replaying = true;
            let replay = runtime.reconcile_to_quiescence();
            runtime.core.borrow_mut().replaying = false;
            replay?;
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
        if let Err(failure) = self.append_current_composition() {
            if !failure.committed {
                self.restore_persistent_state(previous, previous_revision)?;
            }
            return Err(failure.error);
        }
        Ok(())
    }

    pub fn step(&mut self) -> Result<bool> {
        if self.retry_committed_event_outbox()? {
            return Ok(true);
        }
        if self.process_pending_event()? {
            return Ok(true);
        }
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
            pending_events: core.pending_events.len(),
            staged_events: core.event_outbox.len(),
            event_stream_registrations: usize::from(core.event_stream.is_some()),
            exchange_registrations: usize::from(core.exchange.is_some()),
            exchange_workers: core.exchange_workers.len(),
        }
    }

    pub fn is_observationally_clean(&self) -> bool {
        self.observation() == ContextObservation::default()
    }

    pub(crate) fn insert_root(&mut self, spec: PreparedSpec) -> Result<FiberId> {
        let mut core = self.core.borrow_mut();
        let id = core.allocate_fiber();
        let path = spec.entry.clone();
        let fiber = Fiber::new(id, None, path, spec);
        core.roots.insert(fiber.path.clone(), id);
        core.fibers.insert(id, fiber);
        Ok(id)
    }

    pub(crate) fn refresh_one(&mut self) -> Result<bool> {
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

    pub(crate) fn recover_one(&mut self) -> Result<bool> {
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
                    fiber_record.staged_response = None;
                    fiber_record.staged_usage = None;
                    fiber_record.inbound_response = None;
                    fiber_record.workspace_authorization = None;
                    fiber_record.promotion_authorization = None;
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

    pub(crate) fn advance_activation(&mut self) -> Result<bool> {
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

    pub(crate) fn begin_activation(&mut self) -> Result<bool> {
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
        let (path, mut spec) = {
            let core = self.core.borrow();
            let fiber = core
                .fibers
                .get(&id)
                .ok_or_else(|| Error::Invariant("activation candidate disappeared".into()))?;
            (fiber.path.clone(), fiber.spec.clone())
        };
        let activation = if spec.workspaces.is_empty() {
            self.instantiate(id, &spec)
        } else {
            let grants: Vec<_> = spec
                .workspaces
                .iter()
                .map(|workspace| workspace.grant.clone())
                .collect();
            match self.prepare_workspaces(&path, &grants) {
                Ok(workspaces) => {
                    let workspace_buffers = workspaces
                        .iter()
                        .map(|workspace| workspace.bytes.to_vec())
                        .collect();
                    let mut core = self.core.borrow_mut();
                    let fiber = core.fibers.get_mut(&id).ok_or_else(|| {
                        Error::Invariant("activation candidate disappeared".into())
                    })?;
                    fiber.workspace_buffers = workspace_buffers;
                    fiber.spec.workspaces = workspaces.clone();
                    drop(core);
                    spec.workspaces = workspaces;
                    self.instantiate(id, &spec)
                }
                Err(error) => Err(error),
            }
        };
        match activation {
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

    pub(crate) fn remove_one(&mut self) -> Result<bool> {
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

    pub(crate) fn swap_in_candidate(
        &mut self,
        parent: Option<FiberId>,
        path: &str,
        old: FiberId,
        candidate: FiberId,
        mut spec: PreparedSpec,
        instance: GuestInstance,
    ) -> Result<()> {
        if !spec.workspaces.is_empty() {
            let grants: Vec<_> = spec
                .workspaces
                .iter()
                .map(|workspace| workspace.grant.clone())
                .collect();
            match self.prepare_workspaces(path, &grants) {
                Ok(workspaces) => spec.workspaces = workspaces,
                Err(error) => {
                    let mut core = self.core.borrow_mut();
                    let old = core.fibers.get_mut(&old).ok_or_else(|| {
                        Error::Invariant(
                            "old generation disappeared during workspace staging".into(),
                        )
                    })?;
                    old.retired = false;
                    old.pinned = false;
                    drop(core);
                    drop(instance);
                    self.reconcile_to_quiescence()?;
                    return Err(Error::ReplacementRolledBack(error.to_string()));
                }
            }
        }
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

    pub(crate) fn rollback_replacement(
        &mut self,
        backup: FiberBackup,
        candidate: FiberId,
    ) -> Result<()> {
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
