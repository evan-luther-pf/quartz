use std::{
    cell::RefCell,
    rc::{Rc, Weak},
};
use wasmtime::{
    Store, StoreContextMut,
    component::{Instance, Linker, ResourceTable, TypedFunc},
};
use wasmtime_wasi::{WasiCtx, WasiCtxBuilder, WasiCtxView, WasiView};

use crate::{
    BindingKind, Error, HostCapability, Result,
    component::FiberId,
    composition::{PatchAuthorization, PreparedSpec},
    events::EventPayloadSource,
    fiber::{Core, InternalState, Inverse},
    repository::{PromotionAuthorization, WorkspaceAuthorization},
    runtime::Runtime,
};

pub(crate) const STATUS_OK: i32 = 0;
pub(crate) const STATUS_UNDECLARED: i32 = 2;
pub(crate) const STATUS_UNSATISFIED: i32 = 3;
pub(crate) const STATUS_INVALID: i32 = 4;
pub(crate) const STATUS_LIMIT: i32 = 5;
pub(crate) const STATUS_COLLISION: i32 = 6;
pub(crate) const STATUS_DENIED: i32 = 7;
pub(crate) const STATUS_STALE: i32 = 8;
pub(crate) const STATUS_BUSY: i32 = 9;
pub(crate) const STATUS_AMBIGUOUS: i32 = 10;
pub(crate) const STATUS_AUTHENTICATION: i32 = 11;
pub(crate) const STATUS_REQUEST_REJECTED: i32 = 12;
pub(crate) const STATUS_REMOTE_FAILED_OTHER: i32 = 13;
pub(crate) const STATUS_EMPTY_RESPONSE: i32 = 14;
pub(crate) const STATUS_RESPONSE_LIMIT: i32 = 15;
pub(crate) const STATUS_PROTOCOL: i32 = 16;
pub(crate) const STATUS_EXCHANGE_AMBIGUOUS: i32 = 17;
pub(crate) const STATUS_REMOTE_CANCELLED: i32 = 19;
pub(crate) const STATUS_INCOMPLETE_MAX_OUTPUT_TOKENS: i32 = 20;
pub(crate) const STATUS_INCOMPLETE_CONTENT_FILTER: i32 = 21;
pub(crate) const STATUS_INCOMPLETE_OTHER: i32 = 22;
pub(crate) const STATUS_REMOTE_FAILED_SERVER_ERROR: i32 = 23;
pub(crate) const STATUS_REMOTE_FAILED_RATE_LIMIT: i32 = 24;
pub(crate) const STATUS_REMOTE_FAILED_INVALID_PROMPT: i32 = 25;
pub(crate) const STATUS_REMOTE_FAILED_VECTOR_STORE_TIMEOUT: i32 = 26;

pub(crate) struct HostState {
    pub(crate) core: Weak<RefCell<Core>>,
    pub(crate) fiber: FiberId,
    wasi_ctx: WasiCtx,
    wasi_table: ResourceTable,
}
// The store is owned by Runtime's Rc-confined fiber graph and is never moved
// to an exchange worker; WASI's synchronous host traits nevertheless require Send.
unsafe impl Send for HostState {}

impl WasiView for HostState {
    fn ctx(&mut self) -> WasiCtxView<'_> {
        WasiCtxView {
            ctx: &mut self.wasi_ctx,
            table: &mut self.wasi_table,
        }
    }
}

pub(crate) struct GuestInstance {
    pub(crate) store: Store<HostState>,
    pub(crate) _instance: Instance,
    pub(crate) instance_id: u64,
    pub(crate) step: TypedFunc<(u64,), (i32,)>,
    pub(crate) drop_fn: TypedFunc<(u64,), ()>,
    pub(crate) invoke: TypedFunc<(u64, u64, u64, u64), (i64,)>,
}

impl Runtime {
    pub(crate) fn instantiate(&self, fiber: FiberId, spec: &PreparedSpec) -> Result<GuestInstance> {
        let mut linker = Linker::new(self.loader.engine());
        link_host(&mut linker)?;
        wasmtime_wasi::p2::add_to_linker_sync(&mut linker)
            .map_err(|error| Error::Link(error.to_string()))?;
        let mut store = Store::new(
            self.loader.engine(),
            HostState {
                core: Rc::downgrade(&self.core),
                fiber,
                wasi_ctx: WasiCtxBuilder::new().build(),
                wasi_table: ResourceTable::new(),
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
                Ok((host_invoke(store, slot, operation, arg0, arg1),))
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
    linker
        .root()
        .func_wrap(
            "open-event-stream",
            |store: StoreContextMut<'_, HostState>, (index,): (u64,)| {
                Ok((with_core(store, |core, fiber| {
                    core.host_open_event_stream(fiber, index)
                }),))
            },
        )
        .map_err(|error| Error::Link(error.to_string()))?;
    linker
        .root()
        .func_wrap(
            "open-exchange",
            |store: StoreContextMut<'_, HostState>, (index,): (u64,)| {
                Ok((with_core(store, |core, fiber| {
                    core.host_open_exchange(fiber, index)
                }),))
            },
        )
        .map_err(|error| Error::Link(error.to_string()))?;
    linker
        .root()
        .func_wrap(
            "exchange",
            |store: StoreContextMut<'_, HostState>, (event_index, invocation): (u64, u64)| {
                Ok((with_core(store, |core, fiber| {
                    core.host_exchange(fiber, event_index, invocation)
                }),))
            },
        )
        .map_err(|error| Error::Link(error.to_string()))?;
    linker
        .root()
        .func_wrap(
            "event-buffer-set-len",
            |store: StoreContextMut<'_, HostState>, (length,): (u64,)| {
                Ok((with_core(store, |core, fiber| {
                    core.host_event_buffer_set_len(fiber, length)
                }),))
            },
        )
        .map_err(|error| Error::Link(error.to_string()))?;
    linker
        .root()
        .func_wrap(
            "event-buffer-write-byte",
            |store: StoreContextMut<'_, HostState>, (offset, value): (u64, u32)| {
                Ok((with_core(store, |core, fiber| {
                    core.host_event_buffer_write_byte(fiber, offset, value)
                }),))
            },
        )
        .map_err(|error| Error::Link(error.to_string()))?;
    linker
        .root()
        .func_wrap(
            "append-buffered-event",
            |store: StoreContextMut<'_, HostState>, (index, value): (u64, u64)| {
                Ok((with_core(store, |core, fiber| {
                    core.host_append_event(
                        fiber,
                        index,
                        value,
                        false,
                        false,
                        EventPayloadSource::Buffered,
                    )
                }),))
            },
        )
        .map_err(|error| Error::Link(error.to_string()))?;
    linker
        .root()
        .func_wrap(
            "resume-buffered-event",
            |store: StoreContextMut<'_, HostState>, (index, value): (u64, u64)| {
                Ok((with_core(store, |core, fiber| {
                    core.host_append_event(
                        fiber,
                        index,
                        value,
                        true,
                        false,
                        EventPayloadSource::Buffered,
                    )
                }),))
            },
        )
        .map_err(|error| Error::Link(error.to_string()))?;
    linker
        .root()
        .func_wrap(
            "continue-buffered-event",
            |store: StoreContextMut<'_, HostState>, (index, value): (u64, u64)| {
                Ok((with_core(store, |core, fiber| {
                    core.host_append_event(
                        fiber,
                        index,
                        value,
                        true,
                        true,
                        EventPayloadSource::Buffered,
                    )
                }),))
            },
        )
        .map_err(|error| Error::Link(error.to_string()))?;
    linker
        .root()
        .func_wrap(
            "append-event",
            |store: StoreContextMut<'_, HostState>, (index, value): (u64, u64)| {
                Ok((with_core(store, |core, fiber| {
                    core.host_append_event(
                        fiber,
                        index,
                        value,
                        false,
                        false,
                        EventPayloadSource::None,
                    )
                }),))
            },
        )
        .map_err(|error| Error::Link(error.to_string()))?;
    linker
        .root()
        .func_wrap(
            "resume-event",
            |store: StoreContextMut<'_, HostState>, (index, value): (u64, u64)| {
                Ok((with_core(store, |core, fiber| {
                    core.host_append_event(
                        fiber,
                        index,
                        value,
                        true,
                        false,
                        EventPayloadSource::None,
                    )
                }),))
            },
        )
        .map_err(|error| Error::Link(error.to_string()))?;
    linker
        .root()
        .func_wrap(
            "snapshot-len",
            |store: StoreContextMut<'_, HostState>, (index,): (u64,)| {
                Ok((with_core(store, |core, fiber| {
                    core.host_snapshot_len(fiber, index)
                }),))
            },
        )
        .map_err(|error| Error::Link(error.to_string()))?;
    linker
        .root()
        .func_wrap(
            "snapshot-byte",
            |store: StoreContextMut<'_, HostState>, (index, offset): (u64, u64)| {
                Ok((with_core(store, |core, fiber| {
                    core.host_snapshot_byte(fiber, index, offset)
                }),))
            },
        )
        .map_err(|error| Error::Link(error.to_string()))?;
    linker
        .root()
        .func_wrap(
            "resume-snapshot",
            |store: StoreContextMut<'_, HostState>,
             (event_index, snapshot_index, value): (u64, u64, u64)| {
                Ok((with_core(store, |core, fiber| {
                    core.host_append_event(
                        fiber,
                        event_index,
                        value,
                        true,
                        false,
                        EventPayloadSource::Snapshot(snapshot_index),
                    )
                }),))
            },
        )
        .map_err(|error| Error::Link(error.to_string()))?;
    linker
        .root()
        .func_wrap(
            "resume-exchange",
            |store: StoreContextMut<'_, HostState>, (event_index, value): (u64, u64)| {
                Ok((with_core(store, |core, fiber| {
                    core.host_append_event(
                        fiber,
                        event_index,
                        value,
                        true,
                        false,
                        EventPayloadSource::Exchange,
                    )
                }),))
            },
        )
        .map_err(|error| Error::Link(error.to_string()))?;
    linker
        .root()
        .func_wrap(
            "continue-exchange",
            |store: StoreContextMut<'_, HostState>, (event_index, value): (u64, u64)| {
                Ok((with_core(store, |core, fiber| {
                    core.host_append_event(
                        fiber,
                        event_index,
                        value,
                        true,
                        true,
                        EventPayloadSource::Exchange,
                    )
                }),))
            },
        )
        .map_err(|error| Error::Link(error.to_string()))?;
    linker
        .root()
        .func_wrap(
            "workspace-len",
            |store: StoreContextMut<'_, HostState>, (index,): (u64,)| {
                Ok((with_core(store, |core, fiber| {
                    core.host_workspace_len(fiber, index)
                }),))
            },
        )
        .map_err(|error| Error::Link(error.to_string()))?;
    linker
        .root()
        .func_wrap(
            "workspace-byte",
            |store: StoreContextMut<'_, HostState>, (index, offset): (u64, u64)| {
                Ok((with_core(store, |core, fiber| {
                    core.host_workspace_byte(fiber, index, offset)
                }),))
            },
        )
        .map_err(|error| Error::Link(error.to_string()))?;
    linker
        .root()
        .func_wrap(
            "workspace-set-len",
            |store: StoreContextMut<'_, HostState>, (index, length): (u64, u64)| {
                Ok((with_core(store, |core, fiber| {
                    core.host_workspace_set_len(fiber, index, length)
                }),))
            },
        )
        .map_err(|error| Error::Link(error.to_string()))?;
    linker
        .root()
        .func_wrap(
            "workspace-write-byte",
            |store: StoreContextMut<'_, HostState>, (index, offset, value): (u64, u64, u32)| {
                Ok((with_core(store, |core, fiber| {
                    core.host_workspace_write_byte(fiber, index, offset, value)
                }),))
            },
        )
        .map_err(|error| Error::Link(error.to_string()))?;
    linker
        .root()
        .func_wrap(
            "publish-workspace",
            |store: StoreContextMut<'_, HostState>, (index,): (u64,)| {
                Ok((with_core(store, |core, fiber| {
                    core.host_publish_workspace(fiber, index)
                }),))
            },
        )
        .map_err(|error| Error::Link(error.to_string()))?;
    linker
        .root()
        .func_wrap(
            "publish-dynamic-workspace",
            |store: StoreContextMut<'_, HostState>, (index, operation): (u64, u64)| {
                Ok((with_core(store, |core, fiber| {
                    core.host_publish_dynamic_workspace(fiber, index, operation)
                }),))
            },
        )
        .map_err(|error| Error::Link(error.to_string()))?;
    linker
        .root()
        .func_wrap(
            "promote-workspace",
            |store: StoreContextMut<'_, HostState>, (index,): (u64,)| {
                Ok((with_core(store, |core, fiber| {
                    core.host_promote_workspace(fiber, index)
                }),))
            },
        )
        .map_err(|error| Error::Link(error.to_string()))?;
    linker
        .root()
        .func_wrap(
            "promote-dynamic-workspace",
            |store: StoreContextMut<'_, HostState>, (index, operation): (u64, u64)| {
                Ok((with_core(store, |core, fiber| {
                    core.host_promote_dynamic_workspace(fiber, index, operation)
                }),))
            },
        )
        .map_err(|error| Error::Link(error.to_string()))?;
    linker
        .root()
        .func_wrap(
            "event-count",
            |store: StoreContextMut<'_, HostState>, (): ()| {
                Ok((with_core(store, |core, fiber| core.host_event_count(fiber)),))
            },
        )
        .map_err(|error| Error::Link(error.to_string()))?;
    linker
        .root()
        .func_wrap(
            "read-event",
            |store: StoreContextMut<'_, HostState>, (index,): (u64,)| {
                Ok((with_core(store, |core, fiber| {
                    core.host_read_event(fiber, index)
                }),))
            },
        )
        .map_err(|error| Error::Link(error.to_string()))?;
    linker
        .root()
        .func_wrap(
            "event-payload-len",
            |store: StoreContextMut<'_, HostState>, (index,): (u64,)| {
                Ok((with_core(store, |core, fiber| {
                    core.host_event_payload_len(fiber, index)
                }),))
            },
        )
        .map_err(|error| Error::Link(error.to_string()))?;
    linker
        .root()
        .func_wrap(
            "event-payload-byte",
            |store: StoreContextMut<'_, HostState>, (index, offset): (u64, u64)| {
                Ok((with_core(store, |core, fiber| {
                    core.host_event_payload_byte(fiber, index, offset)
                }),))
            },
        )
        .map_err(|error| Error::Link(error.to_string()))?;
    Ok(())
}

fn host_invoke(
    store: StoreContextMut<'_, HostState>,
    slot: u64,
    operation: u64,
    arg0: u64,
    arg1: u64,
) -> i64 {
    let caller = store.data().fiber;
    let Some(core) = store.data().core.upgrade() else {
        return -(STATUS_INVALID as i64);
    };
    let (committed, provider, mut instance) = {
        let Ok(mut core) = core.try_borrow_mut() else {
            return -(STATUS_INVALID as i64);
        };
        let Some(caller_record) = core.fibers.get(&caller) else {
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
        let Some(binding) = core.bindings.get(&committed.interface) else {
            return -(STATUS_UNSATISFIED as i64);
        };
        if binding.fiber != committed.fiber || binding.kind != BindingKind::Callable {
            return -(STATUS_UNSATISFIED as i64);
        }
        let provider = committed.fiber;
        if !core.invoking.insert(provider) {
            return -(STATUS_BUSY as i64);
        }
        let Some(instance) = core.fibers.get_mut(&provider).and_then(|record| {
            (record.state == InternalState::Active)
                .then(|| record.instance.take())
                .flatten()
        }) else {
            core.invoking.remove(&provider);
            return -(STATUS_UNSATISFIED as i64);
        };
        (committed, provider, instance)
    };

    let result = instance
        .invoke
        .call(
            &mut instance.store,
            (instance.instance_id, operation, arg0, arg1),
        )
        .map(|result| result.0)
        .unwrap_or(-(STATUS_INVALID as i64));

    let Ok(mut core) = core.try_borrow_mut() else {
        return -(STATUS_INVALID as i64);
    };
    core.invoking.remove(&provider);
    let staged = {
        let Some(provider_record) = core.fibers.get_mut(&provider) else {
            return -(STATUS_INVALID as i64);
        };
        provider_record.instance = Some(instance);
        match (
            provider_record.staged_response.take(),
            provider_record.staged_usage.take(),
        ) {
            (Some(payload), Some(usage)) if result >= 0 && usage == result as u64 => {
                Some(Ok(payload))
            }
            (None, None) => None,
            _ => Some(Err(())),
        }
    };
    match staged {
        Some(Ok(payload)) => {
            let Some(caller_record) = core.fibers.get_mut(&caller) else {
                return -(STATUS_INVALID as i64);
            };
            if caller_record.inbound_response.is_some() {
                return -(STATUS_BUSY as i64);
            }
            caller_record.inbound_response = Some(payload);
        }
        Some(Err(())) => return -(STATUS_INVALID as i64),
        None => {}
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
        if let Some(record) = core.fibers.get_mut(&caller) {
            record.patch_authorization = Some(PatchAuthorization {
                provider,
                index,
                base_revision: arg1,
            });
        }
    }
    if result == 1
        && operation == 1
        && committed.interface.namespace == "quartz.repository"
        && committed.interface.interface == "mutation-authority"
        && committed.interface.revision == 1
    {
        let Ok(index) = usize::try_from(arg1) else {
            return -(STATUS_INVALID as i64);
        };
        let Some(record) = core.fibers.get_mut(&caller) else {
            return -(STATUS_INVALID as i64);
        };
        let Some(workspace) = record.spec.workspaces.get(index) else {
            return -(STATUS_INVALID as i64);
        };
        if arg0 == 0 || (!workspace.grant.dynamic && workspace.grant.operation != arg0) {
            return -(STATUS_INVALID as i64);
        }
        record.workspace_authorization = Some(WorkspaceAuthorization {
            provider,
            index,
            operation: arg0,
        });
    }
    if result == 1
        && operation == 1
        && committed.interface.namespace == "quartz.repository"
        && committed.interface.interface == "promotion-authority"
        && committed.interface.revision == 1
    {
        let Ok(index) = usize::try_from(arg1) else {
            return -(STATUS_INVALID as i64);
        };
        let Some(provider_record) = core.fibers.get(&provider) else {
            return -(STATUS_INVALID as i64);
        };
        let approver = format!(
            "{}@{}#{}",
            provider_record.spec.artifact.manifest.module,
            provider_record.spec.artifact.manifest.version,
            provider_record.spec.artifact.digest
        );
        let Some(record) = core.fibers.get_mut(&caller) else {
            return -(STATUS_INVALID as i64);
        };
        let Some(workspace) = record.spec.workspaces.get(index) else {
            return -(STATUS_INVALID as i64);
        };
        if arg0 == 0
            || (!workspace.grant.dynamic && workspace.grant.operation != arg0)
            || !record.accumulator.iter().any(|inverse| match inverse {
                Inverse::RestoreWorkspace { grant, .. }
                | Inverse::VerifyPromotedWorkspace { grant, .. } => {
                    if workspace.grant.dynamic {
                        grant.source_path == workspace.grant.source_path && grant.operation == arg0
                    } else {
                        grant == &workspace.grant
                    }
                }
                _ => false,
            })
        {
            return -(STATUS_INVALID as i64);
        }
        record.promotion_authorization = Some(PromotionAuthorization {
            provider,
            index,
            operation: arg0,
            approver,
        });
    }
    result
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
