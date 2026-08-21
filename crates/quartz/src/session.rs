use std::path::{Path, PathBuf};

use quartz_kernel::{DurableEventLog, DurablePayload, EventGrant, Limits};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const SESSION_FILE: &str = "session.qe";
const SESSION_ACTOR: &str = "host/repository-task";
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

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RecordedFact {
    pub(crate) id: u64,
    pub(crate) fact: SessionFact,
}

pub(crate) struct SessionLog {
    log: DurableEventLog,
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
                || record.value != 0
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
            facts.push(RecordedFact {
                id: record.id,
                fact,
            });
        }
        Ok(Self { log, facts })
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
        let payload = DurablePayload {
            provenance: SESSION_PROVENANCE.to_owned(),
            sha256: sha256(&bytes),
            bytes,
        };
        let id = self
            .log
            .append(
                SESSION_ACTOR,
                EventGrant::new(SESSION_NAMESPACE, SESSION_EVENT, SESSION_REVISION),
                0,
                Some(payload),
            )
            .map_err(|error| format!("append session fact: {error}"))?;
        self.facts.push(RecordedFact { id, fact });
        Ok(id)
    }
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
            log.append(SessionFact::ProposalApproved {
                proposal_index: index,
                revision: 0,
                candidate_sha256: format!("{index:064x}"),
            })
            .unwrap();
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
            for (index, record) in reopened.facts().iter().enumerate() {
                assert_eq!(record.id, u64::try_from(index + 1).unwrap());
                assert!(matches!(
                    record.fact,
                    SessionFact::ProposalApproved { proposal_index, .. }
                        if proposal_index == index
                ));
            }
            fs::remove_dir_all(directory).unwrap();
        }
    }

    #[test]
    fn event_identity_tampering_is_rejected() {
        let directory = temporary_directory("identity");
        let mut log = SessionLog::open(&directory).unwrap();
        log.append(SessionFact::ProposalApproved {
            proposal_index: 0,
            revision: 0,
            candidate_sha256: "0".repeat(64),
        })
        .unwrap();
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
