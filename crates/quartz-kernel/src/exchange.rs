use serde::{Deserialize, Serialize};
use std::{path::PathBuf, time::Duration};

use crate::{
    HostCapability,
    component::{FiberId, TraceEvent},
    fiber::{Core, InternalState, Inverse},
    journal::{
        DurablePayload, ExchangeLedger, ExchangeLedgerFailure, ExchangeLedgerOutcome, sha256_hex,
    },
    wasm_host::{
        STATUS_AMBIGUOUS, STATUS_AUTHENTICATION, STATUS_COLLISION, STATUS_DENIED,
        STATUS_EMPTY_RESPONSE, STATUS_EXCHANGE_AMBIGUOUS, STATUS_INVALID, STATUS_LIMIT, STATUS_OK,
        STATUS_PROTOCOL, STATUS_REMOTE_FAILED, STATUS_REQUEST_REJECTED, STATUS_RESPONSE_LIMIT,
        STATUS_UNDECLARED, STATUS_UNSATISFIED,
    },
};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ExchangeGrant {
    pub adapter: String,
    pub ledger_path: PathBuf,
    pub max_request_bytes: usize,
    pub max_response_bytes: usize,
    pub timeout_ms: u64,
}

impl ExchangeGrant {
    pub fn new(
        adapter: impl Into<String>,
        ledger_path: impl Into<PathBuf>,
        max_request_bytes: usize,
        max_response_bytes: usize,
        timeout_ms: u64,
    ) -> Self {
        Self {
            adapter: adapter.into(),
            ledger_path: ledger_path.into(),
            max_request_bytes,
            max_response_bytes,
            timeout_ms,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExchangeResponse {
    pub provenance: String,
    pub bytes: Vec<u8>,
    pub usage: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExchangeFailure {
    Authentication,
    RequestRejected,
    RemoteFailed,
    EmptyResponse,
    ResponseLimit,
    Protocol,
    Ambiguous,
}

pub trait ExchangeAdapter: Send + Sync {
    fn identity(&self) -> &str;

    fn exchange(
        &self,
        request: &[u8],
        timeout: Duration,
        max_response_bytes: usize,
    ) -> std::result::Result<ExchangeResponse, ExchangeFailure>;
}

pub(crate) struct ExchangeRegistration {
    pub(crate) grant: ExchangeGrant,
    pub(crate) ledger: ExchangeLedger,
}

impl Core {
    pub(crate) fn host_open_exchange(&mut self, fiber: FiberId, index: u64) -> i32 {
        let Ok(index) = usize::try_from(index) else {
            return STATUS_INVALID;
        };
        let grant = {
            let Some(record) = self.fibers.get(&fiber) else {
                return STATUS_INVALID;
            };
            if record.state != InternalState::Activating
                || !record
                    .spec
                    .artifact
                    .manifest
                    .requests(HostCapability::OpenExchange)
            {
                return STATUS_UNDECLARED;
            }
            let Some(grant) = record.spec.exchange_grants.get(index) else {
                return STATUS_UNDECLARED;
            };
            grant.clone()
        };
        if !self.exchange_adapters.contains_key(&grant.adapter) {
            return STATUS_DENIED;
        }
        if self.exchange.contains_key(&fiber) {
            return STATUS_COLLISION;
        }
        let ledger =
            match ExchangeLedger::open(&grant.ledger_path, self.limits.max_exchange_record_bytes) {
                Ok(ledger) => ledger,
                Err(error) => {
                    self.exchange_failure = Some(error);
                    return STATUS_INVALID;
                }
            };
        let effect = self.allocate_effect();
        self.exchange
            .insert(fiber, ExchangeRegistration { grant, ledger });
        self.fibers
            .get_mut(&fiber)
            .expect("exchange fiber checked above")
            .accumulator
            .push(Inverse::CloseExchange { effect });
        self.trace.push(TraceEvent::EffectApplied {
            fiber,
            effect,
            kind: "exchange-ledger".into(),
        });
        STATUS_OK
    }

    pub(crate) fn host_exchange(
        &mut self,
        fiber: FiberId,
        event_index: u64,
        invocation: u64,
    ) -> i64 {
        if invocation == 0 {
            return -(STATUS_INVALID as i64);
        }
        let Ok(event_index) = usize::try_from(event_index) else {
            return -(STATUS_INVALID as i64);
        };
        let (request, request_sha256, grant, adapter) = {
            let Some(record) = self.fibers.get(&fiber) else {
                return -(STATUS_INVALID as i64);
            };
            if !matches!(
                record.state,
                InternalState::Activating | InternalState::Active
            ) || (record.state == InternalState::Active && !self.invoking.contains(&fiber))
                || !record
                    .spec
                    .artifact
                    .manifest
                    .requests(HostCapability::Exchange)
            {
                return -(STATUS_UNDECLARED as i64);
            }
            let Some(registration) = self.exchange.get(&fiber) else {
                return -(STATUS_UNSATISFIED as i64);
            };
            let Some(stream) = self.event_stream.as_ref() else {
                return -(STATUS_UNSATISFIED as i64);
            };
            if record
                .committed
                .values()
                .all(|provider| provider.fiber != stream.owner)
            {
                return -(STATUS_UNSATISFIED as i64);
            }
            let Some(payload) = stream
                .stream
                .records()
                .get(event_index)
                .and_then(|event| event.payload.as_ref())
            else {
                return -(STATUS_INVALID as i64);
            };
            if payload.bytes.is_empty()
                || payload.bytes.len() > registration.grant.max_request_bytes
                || std::str::from_utf8(&payload.bytes).is_err()
            {
                return -(STATUS_LIMIT as i64);
            }
            let Some(adapter) = self
                .exchange_adapters
                .get(&registration.grant.adapter)
                .cloned()
            else {
                return -(STATUS_DENIED as i64);
            };
            (
                payload.bytes.clone(),
                sha256_hex(&payload.bytes),
                registration.grant.clone(),
                adapter,
            )
        };

        let recovered = {
            let registration = self
                .exchange
                .get(&fiber)
                .expect("exchange registration checked above");
            match registration.ledger.outcome(invocation, &request_sha256) {
                Ok(outcome) => outcome.cloned(),
                Err(error) => {
                    self.exchange_failure = Some(error);
                    return -(STATUS_INVALID as i64);
                }
            }
        };
        if let Some(mut outcome) = recovered {
            if outcome == ExchangeLedgerOutcome::Started {
                outcome = ExchangeLedgerOutcome::Failed {
                    failure: ExchangeLedgerFailure::Ambiguous,
                };
                if let Err(error) = self
                    .exchange
                    .get_mut(&fiber)
                    .expect("exchange registration checked above")
                    .ledger
                    .append_terminal(invocation, request_sha256.clone(), outcome.clone())
                {
                    self.exchange_failure = Some(error);
                    return -(STATUS_INVALID as i64);
                }
            }
            return self.stage_exchange_outcome(fiber, outcome);
        }
        let mut running = Vec::new();
        for worker in self.exchange_workers.drain(..) {
            if worker.is_finished() {
                let _ = worker.join();
            } else {
                running.push(worker);
            }
        }
        self.exchange_workers = running;
        if !self.exchange_workers.is_empty() {
            return -(STATUS_AMBIGUOUS as i64);
        }
        if let Err(error) = self
            .exchange
            .get_mut(&fiber)
            .expect("exchange registration checked above")
            .ledger
            .append_started(invocation, request_sha256.clone())
        {
            self.exchange_failure = Some(error);
            return -(STATUS_INVALID as i64);
        }

        let timeout = Duration::from_millis(grant.timeout_ms);
        let (sender, receiver) = std::sync::mpsc::sync_channel(1);
        let adapter_request = request.clone();
        let worker = std::thread::spawn(move || {
            let result = adapter.exchange(&adapter_request, timeout, grant.max_response_bytes);
            let _ = sender.send(result);
        });
        let received = receiver.recv_timeout(timeout);
        if matches!(received, Err(std::sync::mpsc::RecvTimeoutError::Timeout)) {
            self.exchange_workers.push(worker);
        } else {
            let _ = worker.join();
        }
        let outcome = match received {
            Ok(Ok(response)) if response.bytes.is_empty() => ExchangeLedgerOutcome::Failed {
                failure: ExchangeLedgerFailure::EmptyResponse,
            },
            Ok(Ok(response)) if response.bytes.len() > grant.max_response_bytes => {
                ExchangeLedgerOutcome::Failed {
                    failure: ExchangeLedgerFailure::ResponseLimit,
                }
            }
            Ok(Ok(response))
                if response.usage > i64::MAX as u64
                    || response.provenance.is_empty()
                    || std::str::from_utf8(&response.bytes).is_err() =>
            {
                ExchangeLedgerOutcome::Failed {
                    failure: ExchangeLedgerFailure::Protocol,
                }
            }
            Ok(Ok(response)) => ExchangeLedgerOutcome::Succeeded {
                payload: DurablePayload {
                    provenance: response.provenance,
                    sha256: sha256_hex(&response.bytes),
                    bytes: response.bytes,
                },
                usage: response.usage,
            },
            Ok(Err(failure)) => ExchangeLedgerOutcome::Failed {
                failure: match failure {
                    ExchangeFailure::Authentication => ExchangeLedgerFailure::Authentication,
                    ExchangeFailure::RequestRejected => ExchangeLedgerFailure::RequestRejected,
                    ExchangeFailure::RemoteFailed => ExchangeLedgerFailure::RemoteFailed,
                    ExchangeFailure::EmptyResponse => ExchangeLedgerFailure::EmptyResponse,
                    ExchangeFailure::ResponseLimit => ExchangeLedgerFailure::ResponseLimit,
                    ExchangeFailure::Protocol => ExchangeLedgerFailure::Protocol,
                    ExchangeFailure::Ambiguous => ExchangeLedgerFailure::Ambiguous,
                },
            },
            Err(std::sync::mpsc::RecvTimeoutError::Timeout)
            | Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                ExchangeLedgerOutcome::Failed {
                    failure: ExchangeLedgerFailure::Ambiguous,
                }
            }
        };
        if let Err(error) = self
            .exchange
            .get_mut(&fiber)
            .expect("exchange registration checked above")
            .ledger
            .append_terminal(invocation, request_sha256, outcome.clone())
        {
            self.exchange_failure = Some(error);
            return -(STATUS_AMBIGUOUS as i64);
        }
        self.stage_exchange_outcome(fiber, outcome)
    }

    pub(crate) fn stage_exchange_outcome(
        &mut self,
        fiber: FiberId,
        outcome: ExchangeLedgerOutcome,
    ) -> i64 {
        match outcome {
            ExchangeLedgerOutcome::Started => -(STATUS_AMBIGUOUS as i64),
            ExchangeLedgerOutcome::Succeeded { payload, usage } => {
                let Some(record) = self.fibers.get_mut(&fiber) else {
                    return -(STATUS_INVALID as i64);
                };
                record.staged_response = Some(payload);
                record.staged_usage = Some(usage);
                usage as i64
            }
            ExchangeLedgerOutcome::Failed { failure } => {
                -(match failure {
                    ExchangeLedgerFailure::Authentication => STATUS_AUTHENTICATION,
                    ExchangeLedgerFailure::RequestRejected => STATUS_REQUEST_REJECTED,
                    ExchangeLedgerFailure::RemoteFailed => STATUS_REMOTE_FAILED,
                    ExchangeLedgerFailure::EmptyResponse => STATUS_EMPTY_RESPONSE,
                    ExchangeLedgerFailure::ResponseLimit => STATUS_RESPONSE_LIMIT,
                    ExchangeLedgerFailure::Protocol => STATUS_PROTOCOL,
                    ExchangeLedgerFailure::Ambiguous => STATUS_EXCHANGE_AMBIGUOUS,
                } as i64)
            }
        }
    }
}
