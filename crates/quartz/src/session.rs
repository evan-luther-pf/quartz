use std::{
    fs,
    path::{Path, PathBuf},
};

use quartz_kernel::{
    ComponentSpec, ComponentTree, DurableEventLog, EventGrant, EventOutputGrant, Limits, Runtime,
    SnapshotGrant,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[cfg(test)]
use quartz_kernel::DurablePayload;

const SESSION_FILE: &str = "session.qe";
const SESSION_ACTOR: &str = "fact";
const SESSION_NAMESPACE: &str = "quartz.session";
const SESSION_EVENT: &str = "fact";
const SESSION_REVISION: u32 = 1;
const SESSION_PROVENANCE: &str = "quartz.session/fact@1";
const MAX_FACT_BYTES: usize = 512 * 1024;
const MAX_FACTS: usize = 1024;
const MAX_SESSION_BYTES: usize = 16 * 1024 * 1024;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case", tag = "kind")]
pub(crate) enum ModelTurn {
    Initial,
    Revision {
        proposal_index: usize,
        revision: u32,
    },
    Continuation {
        sequence: u32,
    },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case", tag = "kind")]
pub(crate) enum SessionFact {
    ModelStarted {
        turn: ModelTurn,
        model: String,
        prompt_sha256: String,
        prompt: String,
    },
    ModelCompleted {
        turn: ModelTurn,
        prompt_sha256: String,
        response_sha256: String,
        response: String,
        provenance: String,
    },
    ProposalRejected {
        proposal_index: usize,
        revision: u32,
        candidate_sha256: String,
        model: String,
        feedback: String,
    },
    ProposalApproved {
        proposal_index: usize,
        revision: u32,
        candidate_sha256: String,
    },
    PromotionStarted {
        proposal_index: usize,
        revision: u32,
        candidate_sha256: String,
        operation: u64,
    },
    ProposalPromoted {
        proposal_index: usize,
        revision: u32,
        candidate_sha256: String,
        operation: u64,
    },
    CommandStarted {
        attempt: u64,
        payload_sha256: String,
        payload: String,
    },
    CommandFinished {
        attempt: u64,
        start_sha256: String,
        payload_sha256: String,
        payload: String,
    },
    TaskCompleted {
        sequence: u32,
        prompt_sha256: String,
        response_sha256: String,
        response: String,
        provenance: String,
    },
}

fn fact_code(fact: &SessionFact) -> u64 {
    match fact {
        SessionFact::ModelStarted { .. } => 1,
        SessionFact::ModelCompleted { .. } => 2,
        SessionFact::ProposalRejected { .. } => 3,
        SessionFact::ProposalApproved { .. } => 4,
        SessionFact::PromotionStarted { .. } => 5,
        SessionFact::ProposalPromoted { .. } => 6,
        SessionFact::CommandStarted { .. } => 7,
        SessionFact::CommandFinished { .. } => 8,
        SessionFact::TaskCompleted { .. } => 9,
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RecordedFact {
    pub(crate) id: u64,
    pub(crate) fact: SessionFact,
}

pub(crate) struct SessionLog {
    session: PathBuf,
    facts: Vec<RecordedFact>,
}

impl SessionLog {
    pub(crate) fn open(session: &Path) -> Result<Self, String> {
        let path = path(session);
        let log = DurableEventLog::open(&path, limits())
            .map_err(|error| format!("open session log `{}`: {error}", path.display()))?;
        let mut facts = Vec::with_capacity(log.records().len());
        for record in log.records() {
            if record.actor_path != SESSION_ACTOR
                || record.event
                    != EventGrant::new(SESSION_NAMESPACE, SESSION_EVENT, SESSION_REVISION)
            {
                return Err(format!(
                    "session fact {} has an invalid actor, event identity, or scalar value",
                    record.id
                ));
            }
            let payload = record
                .payload
                .as_ref()
                .ok_or_else(|| format!("session fact {} has no payload", record.id))?;
            if payload.provenance != SESSION_PROVENANCE {
                return Err(format!(
                    "session fact {} has invalid provenance `{}`",
                    record.id, payload.provenance
                ));
            }
            let fact = serde_json::from_slice(&payload.bytes)
                .map_err(|error| format!("decode session fact {}: {error}", record.id))?;
            if record.value != fact_code(&fact) {
                return Err(format!(
                    "session fact {} scalar does not match its payload",
                    record.id
                ));
            }
            facts.push(RecordedFact {
                id: record.id,
                fact,
            });
        }
        Ok(Self {
            session: session.to_path_buf(),
            facts,
        })
    }

    pub(crate) fn facts(&self) -> &[RecordedFact] {
        &self.facts
    }

    pub(crate) fn append(&mut self, fact: SessionFact) -> Result<u64, String> {
        let bytes =
            serde_json::to_vec(&fact).map_err(|error| format!("encode session fact: {error}"))?;
        if bytes.len() > MAX_FACT_BYTES {
            return Err(format!(
                "session fact is {} bytes; limit is {MAX_FACT_BYTES}",
                bytes.len()
            ));
        }
        let pending = self.session.join("fact.pending");
        fs::write(&pending, &bytes)
            .map_err(|error| format!("stage session fact `{}`: {error}", pending.display()))?;
        let result = self.commit_pending(&pending, bytes.len(), fact_code(&fact));
        let remove_result = fs::remove_file(&pending).map_err(|error| {
            format!(
                "remove staged session fact `{}`: {error}",
                pending.display()
            )
        });
        let id = result?;
        remove_result?;
        self.facts.push(RecordedFact { id, fact });
        Ok(id)
    }

    fn commit_pending(&self, pending: &Path, length: usize, fact_code: u64) -> Result<u64, String> {
        let fixtures = Path::new(env!("QUARTZ_FIXTURE_DIR"));
        let journal = self.session.join("session.qj");
        let events = path(&self.session);
        let storage = ComponentSpec::new("event-store", artifact(fixtures, "event-store"))
            .with_journal_paths(vec![journal.clone()])
            .with_event_stream_paths(vec![events]);
        let mut runtime = Runtime::open_persistent(limits(), storage.clone())
            .map_err(|error| format!("open component session runtime: {error}"))?;
        runtime
            .apply_tree(ComponentTree {
                roots: vec![
                    ComponentSpec::new("fact", artifact(fixtures, "repository-task-orchestrator"))
                        .with_config(fact_code)
                        .with_event_grants(vec![EventGrant::new(
                            SESSION_NAMESPACE,
                            SESSION_EVENT,
                            SESSION_REVISION,
                        )])
                        .with_event_output_grants(vec![EventOutputGrant::new(
                            SESSION_PROVENANCE,
                            length,
                        )])
                        .with_snapshot_grants(vec![
                            SnapshotGrant::from_file(pending, SESSION_PROVENANCE)
                                .map_err(|error| format!("admit staged session fact: {error}"))?,
                        ]),
                ],
            })
            .map_err(|error| format!("commit session fact component: {error}"))?;
        let records = runtime.events();
        let committed = if records.len() == self.facts.len() + 1 {
            Ok(records.last().expect("new session event").id)
        } else {
            Err(format!(
                "session fact component rejected the transition: state={:?}",
                runtime.fiber_state("fact")
            ))
        };
        runtime
            .apply_tree(ComponentTree::default())
            .and_then(|_| runtime.shutdown_persistent())
            .map_err(|error| format!("recover session fact component: {error}"))?;
        committed
    }
}

fn artifact(fixtures: &Path, module: &str) -> PathBuf {
    fixtures.join(module).with_extension("wasm")
}

pub(crate) fn path(session: &Path) -> PathBuf {
    session.join(SESSION_FILE)
}

pub(crate) fn sha256(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut output = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write as _;
        write!(&mut output, "{byte:02x}").expect("writing to a String cannot fail");
    }
    output
}

fn limits() -> Limits {
    Limits {
        max_event_record_bytes: MAX_FACT_BYTES + 64 * 1024,
        max_event_records: MAX_FACTS,
        max_payload_records: MAX_FACTS,
        max_payload_bytes: MAX_FACT_BYTES,
        max_payload_total_bytes: MAX_SESSION_BYTES,
        ..Limits::default()
    }
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        process::Command,
        time::{SystemTime, UNIX_EPOCH},
    };

    use super::*;

    const CRASH_PATH: &str = "QUARTZ_SESSION_CRASH_PATH";
    const CRASH_COUNT: &str = "QUARTZ_SESSION_CRASH_COUNT";

    #[test]
    fn session_log_crash_child() {
        let Ok(path) = std::env::var(CRASH_PATH) else {
            return;
        };
        let count: usize = std::env::var(CRASH_COUNT).unwrap().parse().unwrap();
        fs::create_dir_all(&path).unwrap();
        let mut log = SessionLog::open(Path::new(&path)).unwrap();
        for index in 0..count {
            log.append(crash_fact(index)).unwrap();
        }
        std::process::abort();
    }

    #[test]
    fn every_returned_append_survives_process_crash() {
        for count in 1..=3 {
            let directory = temporary_directory(&format!("crash-{count}"));
            let status = Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "session::tests::session_log_crash_child",
                    "--nocapture",
                ])
                .env(CRASH_PATH, &directory)
                .env(CRASH_COUNT, count.to_string())
                .status()
                .unwrap();
            assert!(!status.success());
            let reopened = SessionLog::open(&directory).unwrap();
            assert_eq!(reopened.facts().len(), count);
            assert_eq!(
                reopened
                    .facts()
                    .iter()
                    .map(|record| fact_code(&record.fact))
                    .collect::<Vec<_>>(),
                (0..count)
                    .map(|index| fact_code(&crash_fact(index)))
                    .collect::<Vec<_>>()
            );
            fs::remove_dir_all(directory).unwrap();
        }
    }

    #[test]
    fn component_rejects_invalid_model_response_before_commit() {
        let directory = temporary_directory("invalid-response");
        let mut log = SessionLog::open(&directory).unwrap();
        let started = crash_fact(0);
        let prompt_sha256 = match &started {
            SessionFact::ModelStarted { prompt_sha256, .. } => prompt_sha256.clone(),
            _ => unreachable!(),
        };
        log.append(started).unwrap();
        let response = "{}";
        assert!(
            log.append(SessionFact::ModelCompleted {
                turn: ModelTurn::Initial,
                prompt_sha256,
                response_sha256: sha256(response.as_bytes()),
                response: response.into(),
                provenance: "test".into(),
            })
            .is_err()
        );
        assert_eq!(SessionLog::open(&directory).unwrap().facts().len(), 1);
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn event_identity_tampering_is_rejected() {
        let directory = temporary_directory("identity");
        let mut log = SessionLog::open(&directory).unwrap();
        log.append(crash_fact(0)).unwrap();
        drop(log);

        let fact = SessionFact::ProposalApproved {
            proposal_index: 1,
            revision: 0,
            candidate_sha256: "1".repeat(64),
        };
        let bytes = serde_json::to_vec(&fact).unwrap();
        let mut raw = DurableEventLog::open(path(&directory), limits()).unwrap();
        raw.append(
            "tampered/actor",
            EventGrant::new(SESSION_NAMESPACE, SESSION_EVENT, SESSION_REVISION),
            0,
            Some(DurablePayload {
                provenance: SESSION_PROVENANCE.into(),
                sha256: sha256(&bytes),
                bytes,
            }),
        )
        .unwrap();
        drop(raw);
        assert!(SessionLog::open(&directory).is_err());
        fs::remove_dir_all(directory).unwrap();
    }

    fn crash_fact(index: usize) -> SessionFact {
        let prompt = serde_json::json!({
            "schema": 1,
            "instructions": "test",
            "task": "test",
            "files": [
                {"path": "a", "before_sha256": sha256(b"a"), "content": "a"},
                {"path": "b", "before_sha256": sha256(b"b"), "content": "b"}
            ],
            "required_response": {"proposals": []}
        })
        .to_string();
        let response = serde_json::json!({
            "proposals": [
                {
                    "path": "a",
                    "source_sha256": sha256(b"a"),
                    "byte_start": 0,
                    "byte_end": 1,
                    "replacement": "A",
                    "result_sha256": sha256(b"A")
                },
                {
                    "path": "b",
                    "source_sha256": sha256(b"b"),
                    "byte_start": 0,
                    "byte_end": 1,
                    "replacement": "B",
                    "result_sha256": sha256(b"B")
                }
            ]
        })
        .to_string();
        match index {
            0 => SessionFact::ModelStarted {
                turn: ModelTurn::Initial,
                model: "test".into(),
                prompt_sha256: sha256(prompt.as_bytes()),
                prompt,
            },
            1 => SessionFact::ModelCompleted {
                turn: ModelTurn::Initial,
                prompt_sha256: sha256(prompt.as_bytes()),
                response_sha256: sha256(response.as_bytes()),
                response,
                provenance: "test".into(),
            },
            2 => SessionFact::ProposalApproved {
                proposal_index: 0,
                revision: 0,
                candidate_sha256: sha256(b"A"),
            },
            _ => unreachable!(),
        }
    }

    fn temporary_directory(label: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "quartz-session-{label}-{}-{nonce}",
            std::process::id()
        ));
        if path.exists() {
            fs::remove_dir_all(&path).unwrap();
        }
        fs::create_dir_all(&path).unwrap();
        path
    }
}
