use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    sync::Arc,
};

use crate::{
    BindingKind, Error, HostCapability, InterfaceId, Result,
    component::{FiberId, FiberState, Limits, TraceEvent},
    composition::{JournalRegistration, PatchAuthorization, PatchUndo, PendingPatch, PreparedSpec},
    events::{EventStreamRegistration, PendingEvent},
    exchange::{ExchangeAdapter, ExchangeRegistration},
    journal::{DurablePayload, EventFact},
    repository::{WorkspaceAuthorization, WorkspaceGrant, recover_workspace_publication},
    wasm_host::{
        GuestInstance, STATUS_COLLISION, STATUS_INVALID, STATUS_LIMIT, STATUS_OK,
        STATUS_UNDECLARED, STATUS_UNSATISFIED,
    },
};

pub(crate) struct Core {
    pub(crate) limits: Limits,
    pub(crate) next_fiber: u64,
    pub(crate) next_effect: u64,
    pub(crate) next_registration: u64,
    pub(crate) composition_revision: u64,
    pub(crate) fibers: BTreeMap<FiberId, Fiber>,
    pub(crate) roots: BTreeMap<String, FiberId>,
    pub(crate) registrations: BTreeMap<u64, Registration>,
    pub(crate) state_cells: BTreeMap<(FiberId, u64), u64>,
    pub(crate) bindings: BTreeMap<InterfaceId, ProviderBinding>,
    pub(crate) patch_owners: BTreeMap<String, FiberId>,
    pub(crate) pending_patches: VecDeque<PendingPatch>,
    pub(crate) blocked_recovery: BTreeSet<FiberId>,
    pub(crate) invoking: BTreeSet<FiberId>,
    pub(crate) pending_events: VecDeque<PendingEvent>,
    pub(crate) event_outbox: Vec<EventFact>,
    pub(crate) next_event_id: u64,
    pub(crate) trace: Vec<TraceEvent>,
    pub(crate) journal: Option<JournalRegistration>,
    pub(crate) journal_failure: Option<Error>,
    pub(crate) event_stream: Option<EventStreamRegistration>,
    pub(crate) event_failure: Option<Error>,
    pub(crate) exchange: Option<ExchangeRegistration>,
    pub(crate) exchange_adapter: Option<Arc<dyn ExchangeAdapter>>,
    pub(crate) exchange_failure: Option<Error>,
    pub(crate) exchange_workers: Vec<std::thread::JoinHandle<()>>,
    pub(crate) replaying: bool,
}

pub(crate) struct Fiber {
    pub(crate) id: FiberId,
    pub(crate) parent: Option<FiberId>,
    pub(crate) path: String,
    pub(crate) spec: PreparedSpec,
    pub(crate) retired: bool,
    pub(crate) pinned: bool,
    pub(crate) state: InternalState,
    pub(crate) committed: ProviderView,
    pub(crate) accumulator: Vec<Inverse>,
    pub(crate) instance: Option<GuestInstance>,
    pub(crate) activation_steps: u32,
    pub(crate) outcome: Option<String>,
    pub(crate) patch_authorization: Option<PatchAuthorization>,
    pub(crate) staged_response: Option<DurablePayload>,
    pub(crate) staged_usage: Option<u64>,
    pub(crate) inbound_response: Option<DurablePayload>,
    pub(crate) workspace_buffers: Vec<Vec<u8>>,
    pub(crate) workspace_authorization: Option<WorkspaceAuthorization>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum InternalState {
    Inactive,
    Activating,
    Active,
    Unloading,
    Failed,
}

pub(crate) type ProviderView = BTreeMap<u64, CommittedProvider>;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct CommittedProvider {
    pub(crate) fiber: FiberId,
    pub(crate) interface: InterfaceId,
}

#[derive(Clone)]
pub(crate) struct ProviderBinding {
    pub(crate) fiber: FiberId,
    pub(crate) kind: BindingKind,
    pub(crate) value: Option<u64>,
}

pub(crate) struct Registration {
    pub(crate) parent: FiberId,
    pub(crate) child: FiberId,
    pub(crate) entry: String,
}

pub(crate) enum Inverse {
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
    CloseEventStream {
        effect: u64,
    },
    CloseExchange {
        effect: u64,
    },
    RestoreWorkspace {
        effect: u64,
        grant: WorkspaceGrant,
        before_bytes: Vec<u8>,
        result_bytes: Vec<u8>,
    },
}

impl Inverse {
    pub(crate) fn effect(&self) -> u64 {
        match self {
            Self::RestoreState { effect, .. }
            | Self::RemoveBinding { effect, .. }
            | Self::RetireChild { effect, .. }
            | Self::RestoreComposition { effect, .. }
            | Self::CloseJournal { effect }
            | Self::CloseEventStream { effect }
            | Self::CloseExchange { effect }
            | Self::RestoreWorkspace { effect, .. } => *effect,
        }
    }

    fn kind(&self) -> &'static str {
        match self {
            Self::RestoreState { .. } => "state",
            Self::RemoveBinding { .. } => "coeffect",
            Self::RetireChild { .. } => "component-registration",
            Self::RestoreComposition { .. } => "composition",
            Self::CloseJournal { .. } => "composition-journal",
            Self::CloseEventStream { .. } => "event-stream",
            Self::CloseExchange { .. } => "exchange-ledger",
            Self::RestoreWorkspace { .. } => "workspace-publication",
        }
    }
}

pub(crate) struct FiberBackup {
    pub(crate) id: FiberId,
    pub(crate) parent: Option<FiberId>,
    pub(crate) path: String,
    pub(crate) spec: PreparedSpec,
}

pub(crate) enum RecoveryAction {
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
    pub(crate) fn new(limits: Limits) -> Self {
        Self {
            limits,
            next_fiber: 1,
            next_effect: 1,
            next_registration: 1,
            composition_revision: 0,
            fibers: BTreeMap::new(),
            blocked_recovery: BTreeSet::new(),
            invoking: BTreeSet::new(),
            roots: BTreeMap::new(),
            registrations: BTreeMap::new(),
            state_cells: BTreeMap::new(),
            bindings: BTreeMap::new(),
            patch_owners: BTreeMap::new(),
            pending_patches: VecDeque::new(),
            pending_events: VecDeque::new(),
            event_outbox: Vec::new(),
            next_event_id: 1,
            trace: Vec::new(),
            journal: None,
            journal_failure: None,
            event_stream: None,
            event_failure: None,
            exchange: None,
            exchange_adapter: None,
            exchange_failure: None,
            exchange_workers: Vec::new(),
            replaying: false,
        }
    }

    pub(crate) fn allocate_fiber(&mut self) -> FiberId {
        let id = FiberId(self.next_fiber);
        self.next_fiber += 1;
        id
    }

    pub(crate) fn allocate_effect(&mut self) -> u64 {
        let id = self.next_effect;
        self.next_effect += 1;
        id
    }

    pub(crate) fn fiber_by_path(&self, path: &str) -> Option<FiberId> {
        self.fibers
            .iter()
            .find_map(|(id, fiber)| (fiber.path == path).then_some(*id))
    }

    pub(crate) fn target_for(&self, fiber: FiberId) -> Option<ProviderView> {
        let record = self.fibers.get(&fiber)?;
        if record.retired {
            return None;
        }
        self.target_for_spec(&record.spec, true)
    }

    pub(crate) fn target_for_spec(
        &self,
        spec: &PreparedSpec,
        require_active: bool,
    ) -> Option<ProviderView> {
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

    pub(crate) fn is_relied_on(&self, provider: FiberId) -> bool {
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

    pub(crate) fn retire_fiber(&mut self, fiber: FiberId) -> Result<()> {
        let fiber = self
            .fibers
            .get_mut(&fiber)
            .ok_or_else(|| Error::Invariant("retired fiber does not exist".into()))?;
        fiber.retired = true;
        Ok(())
    }

    pub(crate) fn apply_inverse(&mut self, fiber_id: FiberId, inverse: Inverse) -> Result<()> {
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
            Inverse::CloseEventStream { .. } => {
                let registration = self.event_stream.take().ok_or_else(|| {
                    Error::Invariant("event-stream inverse found no registered stream".into())
                })?;
                if registration.owner != fiber_id {
                    return Err(Error::Invariant(
                        "event-stream inverse targeted another provider".into(),
                    ));
                }
            }
            Inverse::CloseExchange { .. } => {
                let registration = self.exchange.take().ok_or_else(|| {
                    Error::Invariant("exchange inverse found no registered ledger".into())
                })?;
                if registration.owner != fiber_id {
                    return Err(Error::Invariant(
                        "exchange inverse targeted another provider".into(),
                    ));
                }
                for worker in self.exchange_workers.drain(..) {
                    let _ = worker.join();
                }
            }
            Inverse::RestoreWorkspace {
                effect,
                grant,
                before_bytes,
                result_bytes,
            } => {
                if let Err(error) = recover_workspace_publication(
                    &grant,
                    &before_bytes,
                    &result_bytes,
                    self.limits.max_mutation_record_bytes,
                ) {
                    self.fibers
                        .get_mut(&fiber_id)
                        .ok_or_else(|| {
                            Error::Invariant("workspace inverse owner disappeared".into())
                        })?
                        .accumulator
                        .push(Inverse::RestoreWorkspace {
                            effect,
                            grant,
                            before_bytes,
                            result_bytes,
                        });
                    return Err(error);
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

    pub(crate) fn remove_fiber(&mut self, id: FiberId) -> Result<()> {
        let fiber = self
            .fibers
            .remove(&id)
            .ok_or_else(|| Error::Invariant("removed fiber does not exist".into()))?;
        if !fiber.accumulator.is_empty()
            || fiber.instance.is_some()
            || self.invoking.contains(&id)
            || self.state_cells.keys().any(|(owner, _)| *owner == id)
            || self.bindings.values().any(|binding| binding.fiber == id)
            || self.patch_owners.values().any(|owner| *owner == id)
            || self
                .pending_patches
                .iter()
                .any(|request| request.actor == id)
            || self
                .pending_events
                .iter()
                .any(|request| request.actor == id)
            || self
                .journal
                .as_ref()
                .is_some_and(|journal| journal.owner == id)
            || self
                .event_stream
                .as_ref()
                .is_some_and(|stream| stream.owner == id)
            || self
                .exchange
                .as_ref()
                .is_some_and(|exchange| exchange.owner == id)
            || fiber.staged_response.is_some()
            || fiber.staged_usage.is_some()
            || fiber.inbound_response.is_some()
            || fiber.workspace_authorization.is_some()
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

    pub(crate) fn retarget_slot(
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

    pub(crate) fn host_set_state(&mut self, fiber: FiberId, key: u64, value: u64) -> i32 {
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

    pub(crate) fn host_publish(&mut self, fiber: FiberId, slot: u64, value: u64) -> i32 {
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

    pub(crate) fn host_resolve(&self, fiber: FiberId, slot: u64) -> i64 {
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

    pub(crate) fn host_publish_callable(&mut self, fiber: FiberId, slot: u64) -> i32 {
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

    pub(crate) fn host_register_child(&mut self, parent: FiberId, index: u32) -> i32 {
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
    pub(crate) fn new(
        id: FiberId,
        parent: Option<FiberId>,
        path: String,
        spec: PreparedSpec,
    ) -> Self {
        let workspace_buffers = spec
            .workspaces
            .iter()
            .map(|workspace| workspace.bytes.to_vec())
            .collect();
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
            staged_response: None,
            staged_usage: None,
            inbound_response: None,
            workspace_buffers,
            workspace_authorization: None,
        }
    }

    pub(crate) fn public_state(&self) -> FiberState {
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
