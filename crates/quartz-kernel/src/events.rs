use crate::{
    Error, HostCapability, Result,
    component::{FiberId, TraceEvent},
    fiber::{Core, InternalState, Inverse},
    journal::{DurablePayload, EventFact, EventRecord, EventStream, JournalSnapshot, sha256_hex},
    runtime::Runtime,
    wasm_host::{
        STATUS_BUSY, STATUS_COLLISION, STATUS_INVALID, STATUS_LIMIT, STATUS_OK, STATUS_UNDECLARED,
        STATUS_UNSATISFIED,
    },
};

pub(crate) struct PendingEvent {
    pub(crate) actor: FiberId,
    pub(crate) index: usize,
    pub(crate) value: u64,
    pub(crate) payload: Option<DurablePayload>,
}

#[derive(Clone, Copy)]
pub(crate) enum EventPayloadSource {
    None,
    Snapshot(u64),
    Exchange,
    Output(u64),
}

pub(crate) struct EventStreamRegistration {
    pub(crate) owner: FiberId,
    pub(crate) stream: EventStream,
}

impl Runtime {
    pub(crate) fn drain_recovered_event_outbox(
        &mut self,
        snapshot: &mut JournalSnapshot,
    ) -> Result<()> {
        if snapshot.event_outbox.is_empty() {
            return Ok(());
        }
        {
            let core = self.core.borrow();
            core.event_stream
                .as_ref()
                .ok_or_else(|| {
                    Error::Persistence("recovered event outbox has no event stream provider".into())
                })?
                .stream
                .validate(&snapshot.event_outbox)?;
        }
        for fact in &snapshot.event_outbox {
            let sequence = {
                let mut core = self.core.borrow_mut();
                let stream = core
                    .event_stream
                    .as_mut()
                    .ok_or_else(|| Error::Persistence("event stream became unavailable".into()))?;
                stream.stream.append(fact)?;
                stream.stream.sequence_for_id(fact.id).ok_or_else(|| {
                    Error::Invariant("recovered event has no event-stream sequence".into())
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
        snapshot.event_outbox.clear();
        self.core
            .borrow_mut()
            .journal
            .as_mut()
            .ok_or_else(|| Error::Persistence("composition journal became unavailable".into()))?
            .journal
            .append(snapshot)?;
        Ok(())
    }

    pub fn events(&self) -> Vec<EventRecord> {
        self.core
            .borrow()
            .event_stream
            .as_ref()
            .map(|registration| registration.stream.records().to_vec())
            .unwrap_or_default()
    }

    pub fn event_sequence(&self) -> Option<u64> {
        self.core
            .borrow()
            .event_stream
            .as_ref()
            .map(|registration| registration.stream.sequence())
    }

    pub(crate) fn retry_committed_event_outbox(&mut self) -> Result<bool> {
        if self.core.borrow().event_outbox.is_empty() {
            return Ok(false);
        }
        self.append_current_composition()
            .map_err(|failure| failure.error)?;
        Ok(true)
    }
    pub(crate) fn process_pending_event(&mut self) -> Result<bool> {
        let position = {
            let core = self.core.borrow();
            core.pending_events.iter().position(|request| {
                core.fibers
                    .get(&request.actor)
                    .is_none_or(|fiber| fiber.state != InternalState::Activating)
            })
        };
        let Some(position) = position else {
            return Ok(false);
        };
        let request = self
            .core
            .borrow_mut()
            .pending_events
            .remove(position)
            .ok_or_else(|| Error::Invariant("eligible event request disappeared".into()))?;
        let candidate = {
            let core = self.core.borrow();
            let Some(actor) = core.fibers.get(&request.actor) else {
                return Ok(true);
            };
            if actor.state != InternalState::Active || actor.outcome.is_some() {
                None
            } else {
                let grant = actor
                    .spec
                    .event_grants
                    .get(request.index)
                    .cloned()
                    .ok_or_else(|| Error::Invariant("committed event grant disappeared".into()))?;
                Some((actor.path.clone(), grant))
            }
        };
        let Some((actor_path, event)) = candidate else {
            self.core
                .borrow_mut()
                .trace
                .push(TraceEvent::EventRejected {
                    actor: request.actor,
                    error: "event requester activation did not commit".into(),
                });
            return Ok(true);
        };
        let mut core = self.core.borrow_mut();
        let id = core.next_event_id;
        let fact = EventFact {
            id,
            actor_path,
            event,
            value: request.value,
            payload: request.payload,
        };
        let mut staged = core.event_outbox.clone();
        staged.push(fact.clone());
        core.event_stream
            .as_ref()
            .ok_or_else(|| Error::Persistence("event stream is unavailable".into()))?
            .stream
            .validate(&staged)?;
        core.next_event_id += 1;
        core.event_outbox.push(fact);
        core.trace.push(TraceEvent::EventQueued {
            actor: request.actor,
            id,
        });
        drop(core);
        if let Err(failure) = self.append_current_composition() {
            if !failure.committed {
                let mut core = self.core.borrow_mut();
                let removed = core
                    .event_outbox
                    .pop()
                    .ok_or_else(|| Error::Invariant("failed event outbox disappeared".into()))?;
                if removed.id != id {
                    return Err(Error::Invariant(
                        "failed event append did not own the outbox tail".into(),
                    ));
                }
                core.next_event_id = id;
            }
            return Err(failure.error);
        }
        Ok(true)
    }
}

impl Core {
    pub(crate) fn host_open_event_stream(&mut self, fiber: FiberId, index: u64) -> i32 {
        let Ok(index) = usize::try_from(index) else {
            return STATUS_INVALID;
        };
        let path = {
            let Some(record) = self.fibers.get(&fiber) else {
                return STATUS_INVALID;
            };
            if record.state != InternalState::Activating
                || !record
                    .spec
                    .artifact
                    .manifest
                    .requests(HostCapability::OpenEventStream)
            {
                return STATUS_UNDECLARED;
            }
            let Some(path) = record.spec.event_stream_paths.get(index) else {
                return STATUS_UNDECLARED;
            };
            path.clone()
        };
        if self.event_stream.is_some() {
            return STATUS_COLLISION;
        }
        let stream = match EventStream::open(
            &path,
            self.limits.max_event_record_bytes,
            self.limits.max_event_records,
            self.limits.max_payload_records,
            self.limits.max_payload_bytes,
            self.limits.max_payload_total_bytes,
        ) {
            Ok(stream) => stream,
            Err(error) => {
                self.event_failure = Some(error);
                return STATUS_INVALID;
            }
        };
        self.next_event_id = self.next_event_id.max(stream.next_id());
        let effect = self.allocate_effect();
        self.event_stream = Some(EventStreamRegistration {
            owner: fiber,
            stream,
        });
        self.fibers
            .get_mut(&fiber)
            .expect("event-stream fiber checked above")
            .accumulator
            .push(Inverse::CloseEventStream { effect });
        self.trace.push(TraceEvent::EffectApplied {
            fiber,
            effect,
            kind: "event-stream".into(),
        });
        STATUS_OK
    }

    pub(crate) fn host_append_event(
        &mut self,
        fiber: FiberId,
        index: u64,
        value: u64,
        resumable: bool,
        payload_source: EventPayloadSource,
    ) -> i32 {
        if value > i64::MAX as u64 {
            return STATUS_INVALID;
        }
        let Ok(index) = usize::try_from(index) else {
            return STATUS_INVALID;
        };
        let (actor_path, grant, payload) = {
            let Some(record) = self.fibers.get(&fiber) else {
                return STATUS_INVALID;
            };
            let capability = match payload_source {
                EventPayloadSource::None if resumable => HostCapability::ResumeEvent,
                EventPayloadSource::None => HostCapability::AppendEvent,
                EventPayloadSource::Snapshot(_) => HostCapability::ResumeSnapshot,
                EventPayloadSource::Exchange => HostCapability::ResumeExchange,
                EventPayloadSource::Output(_) => HostCapability::ResumeEventOutput,
            };
            if record.state != InternalState::Activating
                || !record.spec.artifact.manifest.requests(capability)
            {
                return STATUS_UNDECLARED;
            }
            let Some(grant) = record.spec.event_grants.get(index) else {
                return STATUS_UNDECLARED;
            };
            let Some(stream) = self.event_stream.as_ref() else {
                return STATUS_UNSATISFIED;
            };
            if record
                .committed
                .values()
                .all(|provider| provider.fiber != stream.owner)
            {
                return STATUS_UNSATISFIED;
            }
            let payload = match payload_source {
                EventPayloadSource::None => None,
                EventPayloadSource::Snapshot(snapshot_index) => {
                    let Ok(snapshot_index) = usize::try_from(snapshot_index) else {
                        return STATUS_INVALID;
                    };
                    let Some(snapshot) = record.spec.snapshots.get(snapshot_index) else {
                        return STATUS_UNDECLARED;
                    };
                    Some(DurablePayload {
                        provenance: snapshot.grant.provenance.clone(),
                        sha256: snapshot.grant.sha256.clone(),
                        bytes: snapshot.bytes.to_vec(),
                    })
                }
                EventPayloadSource::Output(output_index) => {
                    let Ok(output_index) = usize::try_from(output_index) else {
                        return STATUS_INVALID;
                    };
                    let Some(grant) = record.spec.event_output_grants.get(output_index) else {
                        return STATUS_UNDECLARED;
                    };
                    let Some(bytes) = record.event_output_buffers.get(output_index) else {
                        return STATUS_UNDECLARED;
                    };
                    Some(DurablePayload {
                        provenance: grant.provenance.clone(),
                        sha256: sha256_hex(bytes),
                        bytes: bytes.clone(),
                    })
                }
                EventPayloadSource::Exchange => {
                    let Some(payload) = record.inbound_response.clone() else {
                        return STATUS_UNSATISFIED;
                    };
                    Some(payload)
                }
            };
            (record.path.clone(), grant.clone(), payload)
        };
        if self.replaying && !resumable {
            return STATUS_BUSY;
        }

        let mut staged = self.event_outbox.clone();
        for (offset, pending) in self.pending_events.iter().enumerate() {
            let Some(record) = self.fibers.get(&pending.actor) else {
                return STATUS_INVALID;
            };
            let Some(event) = record.spec.event_grants.get(pending.index) else {
                return STATUS_INVALID;
            };
            staged.push(EventFact {
                id: self.next_event_id + offset as u64,
                actor_path: record.path.clone(),
                event: event.clone(),
                value: pending.value,
                payload: pending.payload.clone(),
            });
        }
        staged.push(EventFact {
            id: self.next_event_id + self.pending_events.len() as u64,
            actor_path,
            event: grant,
            value,
            payload: payload.clone(),
        });
        let Some(stream) = self.event_stream.as_ref() else {
            return STATUS_UNSATISFIED;
        };
        if let Err(error) = stream.stream.validate(&staged) {
            return match error {
                Error::EventRecordLimit { .. }
                | Error::EventRecordBytesLimit { .. }
                | Error::PayloadCountLimit { .. }
                | Error::PayloadBytesLimit { .. }
                | Error::PayloadTotalBytesLimit { .. } => STATUS_LIMIT,
                error => {
                    self.event_failure = Some(error);
                    STATUS_INVALID
                }
            };
        }
        self.pending_events.push_back(PendingEvent {
            actor: fiber,
            index,
            value,
            payload,
        });
        match payload_source {
            EventPayloadSource::Exchange => {
                if let Some(record) = self.fibers.get_mut(&fiber) {
                    record.inbound_response = None;
                }
            }
            EventPayloadSource::Output(index) => {
                if let Ok(index) = usize::try_from(index)
                    && let Some(record) = self.fibers.get_mut(&fiber)
                    && let Some(buffer) = record.event_output_buffers.get_mut(index)
                {
                    buffer.clear();
                }
            }
            EventPayloadSource::None | EventPayloadSource::Snapshot(_) => {}
        }
        STATUS_OK
    }

    pub(crate) fn host_event_output_set_len(
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
                .requests(HostCapability::EventOutputWrite)
        {
            return STATUS_UNDECLARED;
        }
        let Some(grant) = record.spec.event_output_grants.get(index) else {
            return STATUS_UNDECLARED;
        };
        if length > grant.max_bytes {
            return STATUS_LIMIT;
        }
        record.event_output_buffers[index].resize(length, 0);
        STATUS_OK
    }

    pub(crate) fn host_event_output_write_byte(
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
                .requests(HostCapability::EventOutputWrite)
        {
            return STATUS_UNDECLARED;
        }
        let Some(buffer) = record.event_output_buffers.get_mut(index) else {
            return STATUS_UNDECLARED;
        };
        let Some(byte) = buffer.get_mut(offset) else {
            return STATUS_INVALID;
        };
        *byte = value;
        STATUS_OK
    }

    pub(crate) fn host_event_count(&self, fiber: FiberId) -> i64 {
        let Some(record) = self.fibers.get(&fiber) else {
            return -(STATUS_INVALID as i64);
        };
        let Some(stream) = self.event_stream.as_ref() else {
            return -(STATUS_UNSATISFIED as i64);
        };
        if !matches!(
            record.state,
            InternalState::Activating | InternalState::Active
        ) || (record.state == InternalState::Active && !self.invoking.contains(&fiber))
            || !record
                .spec
                .artifact
                .manifest
                .requests(HostCapability::EventCount)
            || record
                .committed
                .values()
                .all(|provider| provider.fiber != stream.owner)
        {
            return -(STATUS_UNDECLARED as i64);
        }
        stream.stream.records().len() as i64
    }

    fn event_record_for_read(
        &self,
        fiber: FiberId,
        index: u64,
        capability: HostCapability,
    ) -> std::result::Result<&EventRecord, i64> {
        let Some(record) = self.fibers.get(&fiber) else {
            return Err(-(STATUS_INVALID as i64));
        };
        let Some(stream) = self.event_stream.as_ref() else {
            return Err(-(STATUS_UNSATISFIED as i64));
        };
        if !matches!(
            record.state,
            InternalState::Activating | InternalState::Active
        ) || (record.state == InternalState::Active && !self.invoking.contains(&fiber))
            || !record.spec.artifact.manifest.requests(capability)
            || record
                .committed
                .values()
                .all(|provider| provider.fiber != stream.owner)
        {
            return Err(-(STATUS_UNDECLARED as i64));
        }
        let Ok(index) = usize::try_from(index) else {
            return Err(-(STATUS_INVALID as i64));
        };
        stream
            .stream
            .records()
            .get(index)
            .ok_or(-(STATUS_INVALID as i64))
    }

    pub(crate) fn host_read_event(&self, fiber: FiberId, index: u64) -> i64 {
        self.event_record_for_read(fiber, index, HostCapability::ReadEvent)
            .map_or_else(|status| status, |event| event.value as i64)
    }

    pub(crate) fn host_event_payload_len(&self, fiber: FiberId, index: u64) -> i64 {
        self.event_record_for_read(fiber, index, HostCapability::EventPayloadLen)
            .and_then(|event| {
                let payload = event.payload.as_ref().ok_or(-(STATUS_UNSATISFIED as i64))?;
                i64::try_from(payload.bytes.len()).map_err(|_| -(STATUS_LIMIT as i64))
            })
            .unwrap_or_else(|status| status)
    }

    pub(crate) fn host_event_payload_byte(&self, fiber: FiberId, index: u64, offset: u64) -> i32 {
        let event = match self.event_record_for_read(fiber, index, HostCapability::EventPayloadByte)
        {
            Ok(event) => event,
            Err(status) => return status as i32,
        };
        let Some(payload) = event.payload.as_ref() else {
            return -STATUS_UNSATISFIED;
        };
        let Ok(offset) = usize::try_from(offset) else {
            return -STATUS_INVALID;
        };
        payload
            .bytes
            .get(offset)
            .map_or(-STATUS_INVALID, |byte| i32::from(*byte))
    }
}
