use std::{
    collections::BTreeMap,
    fs::{File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{ComponentTree, CompositionPatch, Error, Result};

const COMPOSITION_MAGIC: &[u8; 8] = b"QUARTZJ2";
const EVENT_MAGIC: &[u8; 8] = b"QUARTZE1";
const HEADER_LEN: usize = 12;
const CHECKSUM_LEN: usize = 32;
const COMPOSITION_SCHEMA_VERSION: u32 = 2;
const EVENT_SCHEMA_VERSION: u32 = 1;

type FramedPayloads = Vec<(u64, Vec<u8>)>;
type DecodedLog = (u64, FramedPayloads, usize);

#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(deny_unknown_fields)]
pub struct EventGrant {
    pub namespace: String,
    pub name: String,
    pub revision: u32,
}

impl EventGrant {
    pub fn new(namespace: impl Into<String>, name: impl Into<String>, revision: u32) -> Self {
        Self {
            namespace: namespace.into(),
            name: name.into(),
            revision,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct EventRecord {
    pub sequence: u64,
    pub id: u64,
    pub actor_path: String,
    pub event: EventGrant,
    pub value: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct EventFact {
    pub id: u64,
    pub actor_path: String,
    pub event: EventGrant,
    pub value: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct JournalEffect {
    pub actor_path: String,
    pub target: String,
    pub inverse: CompositionPatch,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct JournalSnapshot {
    pub composition_revision: u64,
    pub tree: ComponentTree,
    pub effects: Vec<JournalEffect>,
    pub next_event_id: u64,
    pub event_outbox: Vec<EventFact>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct JournalPayload {
    schema: u32,
    snapshot: JournalSnapshot,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct EventPayload {
    schema: u32,
    fact: EventFact,
}

pub(crate) struct Journal {
    log: FramedLog,
    recovered_snapshot: Option<JournalSnapshot>,
}

impl Journal {
    pub(crate) fn open(path: &Path, max_record_bytes: usize) -> Result<Self> {
        let (log, payloads) = FramedLog::open(
            path,
            COMPOSITION_MAGIC,
            max_record_bytes,
            LogKind::Composition,
        )?;
        let mut recovered_snapshot = None;
        for (_, payload) in payloads {
            let record: JournalPayload = serde_json::from_slice(&payload)
                .map_err(|error| Error::JournalCorrupt(error.to_string()))?;
            if record.schema != COMPOSITION_SCHEMA_VERSION {
                return Err(Error::JournalCorrupt(format!(
                    "unsupported composition journal schema {}",
                    record.schema
                )));
            }
            recovered_snapshot = Some(record.snapshot);
        }
        Ok(Self {
            log,
            recovered_snapshot,
        })
    }

    pub(crate) fn recovered(&self) -> Option<JournalSnapshot> {
        self.recovered_snapshot.clone()
    }

    pub(crate) fn sequence(&self) -> u64 {
        self.log.sequence()
    }

    pub(crate) fn contains(&self, snapshot: &JournalSnapshot) -> bool {
        self.recovered_snapshot.as_ref() == Some(snapshot)
    }
    pub(crate) fn append(&mut self, snapshot: &JournalSnapshot) -> Result<()> {
        let payload = serde_json::to_vec(&JournalPayload {
            schema: COMPOSITION_SCHEMA_VERSION,
            snapshot: snapshot.clone(),
        })?;
        self.log.append(&payload)?;
        self.recovered_snapshot = Some(snapshot.clone());
        Ok(())
    }
}

pub(crate) struct EventStream {
    log: FramedLog,
    records: Vec<EventRecord>,
    by_id: BTreeMap<u64, EventFact>,
    max_records: usize,
}

impl EventStream {
    pub(crate) fn open(path: &Path, max_record_bytes: usize, max_records: usize) -> Result<Self> {
        let (log, payloads) = FramedLog::open(path, EVENT_MAGIC, max_record_bytes, LogKind::Event)?;
        if payloads.len() > max_records {
            return Err(Error::EventRecordLimit {
                actual: payloads.len(),
                limit: max_records,
            });
        }
        let mut records = Vec::with_capacity(payloads.len());
        let mut by_id = BTreeMap::new();
        for (sequence, payload) in payloads {
            let record: EventPayload = serde_json::from_slice(&payload)
                .map_err(|error| Error::EventCorrupt(error.to_string()))?;
            if record.schema != EVENT_SCHEMA_VERSION {
                return Err(Error::EventCorrupt(format!(
                    "unsupported event stream schema {}",
                    record.schema
                )));
            }
            if record.fact.id != sequence {
                return Err(Error::EventCorrupt(format!(
                    "durable event id {} was stored at sequence {sequence}",
                    record.fact.id
                )));
            }
            if by_id.insert(record.fact.id, record.fact.clone()).is_some() {
                return Err(Error::EventCorrupt(format!(
                    "duplicate durable event id {}",
                    record.fact.id
                )));
            }
            records.push(EventRecord {
                sequence,
                id: record.fact.id,
                actor_path: record.fact.actor_path,
                event: record.fact.event,
                value: record.fact.value,
            });
        }
        Ok(Self {
            log,
            records,
            by_id,
            max_records,
        })
    }

    pub(crate) fn sequence(&self) -> u64 {
        self.log.sequence()
    }

    pub(crate) fn records(&self) -> &[EventRecord] {
        &self.records
    }

    pub(crate) fn next_id(&self) -> u64 {
        self.by_id.last_key_value().map_or(1, |(id, _)| id + 1)
    }

    pub(crate) fn sequence_for_id(&self, id: u64) -> Option<u64> {
        self.records
            .iter()
            .find_map(|record| (record.id == id).then_some(record.sequence))
    }

    pub(crate) fn validate(&self, facts: &[EventFact]) -> Result<()> {
        let mut next_id = self.next_id();
        let mut new_records = 0;
        for fact in facts {
            if let Some(existing) = self.by_id.get(&fact.id) {
                if existing != fact {
                    return Err(Error::EventCorrupt(format!(
                        "durable event id {} has conflicting content",
                        fact.id
                    )));
                }
                continue;
            }
            if fact.id != next_id {
                return Err(Error::EventCorrupt(format!(
                    "durable event id {} does not follow {}",
                    fact.id,
                    next_id - 1
                )));
            }
            next_id += 1;
            new_records += 1;
            let payload = serde_json::to_vec(&EventPayload {
                schema: EVENT_SCHEMA_VERSION,
                fact: fact.clone(),
            })?;
            self.log.validate_payload(&payload)?;
        }
        let actual = self.records.len() + new_records;
        if actual > self.max_records {
            return Err(Error::EventRecordLimit {
                actual,
                limit: self.max_records,
            });
        }
        Ok(())
    }

    pub(crate) fn append(&mut self, fact: &EventFact) -> Result<bool> {
        if let Some(existing) = self.by_id.get(&fact.id) {
            if existing == fact {
                return Ok(false);
            }
            return Err(Error::EventCorrupt(format!(
                "durable event id {} has conflicting content",
                fact.id
            )));
        }
        self.validate(std::slice::from_ref(fact))?;
        let payload = serde_json::to_vec(&EventPayload {
            schema: EVENT_SCHEMA_VERSION,
            fact: fact.clone(),
        })?;
        let sequence = self.log.append(&payload)?;
        self.by_id.insert(fact.id, fact.clone());
        self.records.push(EventRecord {
            sequence,
            id: fact.id,
            actor_path: fact.actor_path.clone(),
            event: fact.event.clone(),
            value: fact.value,
        });
        Ok(true)
    }
}

#[derive(Clone, Copy)]
enum LogKind {
    Composition,
    Event,
}

impl LogKind {
    fn name(self) -> &'static str {
        match self {
            Self::Composition => "composition journal",
            Self::Event => "event stream",
        }
    }

    fn corrupt(self, message: impl Into<String>) -> Error {
        match self {
            Self::Composition => Error::JournalCorrupt(message.into()),
            Self::Event => Error::EventCorrupt(message.into()),
        }
    }

    fn record_limit(self, actual: usize, limit: usize) -> Error {
        match self {
            Self::Composition => Error::JournalRecordLimit { actual, limit },
            Self::Event => Error::EventRecordBytesLimit { actual, limit },
        }
    }
}

struct FramedLog {
    path: PathBuf,
    file: File,
    magic: [u8; 8],
    sequence: u64,
    max_record_bytes: usize,
    kind: LogKind,
}

impl FramedLog {
    fn open(
        path: &Path,
        magic: &[u8; 8],
        max_record_bytes: usize,
        kind: LogKind,
    ) -> Result<(Self, FramedPayloads)> {
        let mut file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(path)
            .map_err(|source| durable_io(kind, "open", path, source))?;
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)
            .map_err(|source| durable_io(kind, "read", path, source))?;
        let (sequence, payloads, valid_len) = decode(&bytes, magic, max_record_bytes, kind)?;
        if valid_len != bytes.len() {
            file.set_len(valid_len as u64)
                .map_err(|source| durable_io(kind, "repair torn tail", path, source))?;
            file.sync_data()
                .map_err(|source| durable_io(kind, "synchronize torn-tail repair", path, source))?;
        }
        file.seek(SeekFrom::End(0))
            .map_err(|source| durable_io(kind, "seek", path, source))?;
        Ok((
            Self {
                path: path.to_path_buf(),
                file,
                magic: *magic,
                sequence,
                max_record_bytes,
                kind,
            },
            payloads,
        ))
    }

    fn sequence(&self) -> u64 {
        self.sequence
    }

    fn validate_payload(&self, payload: &[u8]) -> Result<()> {
        if payload.len() > self.max_record_bytes {
            return Err(self.kind.record_limit(payload.len(), self.max_record_bytes));
        }
        if payload.len() > u32::MAX as usize {
            return Err(self.kind.record_limit(payload.len(), u32::MAX as usize));
        }
        Ok(())
    }

    fn append(&mut self, payload: &[u8]) -> Result<u64> {
        self.validate_payload(payload)?;
        let length = payload.len() as u32;
        let sequence = self.sequence + 1;
        let mut frame = Vec::with_capacity(
            usize::from(self.sequence == 0) * self.magic.len()
                + HEADER_LEN
                + payload.len()
                + CHECKSUM_LEN,
        );
        if self.sequence == 0 {
            frame.extend_from_slice(&self.magic);
        }
        frame.extend_from_slice(&sequence.to_le_bytes());
        frame.extend_from_slice(&length.to_le_bytes());
        frame.extend_from_slice(payload);
        frame.extend_from_slice(&checksum(sequence, length, payload));

        let start = self
            .file
            .stream_position()
            .map_err(|source| durable_io(self.kind, "read append position", &self.path, source))?;
        if let Err(source) = self
            .file
            .write_all(&frame)
            .and_then(|()| self.file.sync_data())
        {
            self.file.set_len(start).map_err(|rollback| {
                durable_io(self.kind, "roll back failed append", &self.path, rollback)
            })?;
            self.file.seek(SeekFrom::Start(start)).map_err(|rollback| {
                durable_io(self.kind, "seek after failed append", &self.path, rollback)
            })?;
            return Err(durable_io(
                self.kind,
                "append and synchronize",
                &self.path,
                source,
            ));
        }
        self.sequence = sequence;
        Ok(sequence)
    }
}

fn decode(
    bytes: &[u8],
    magic: &[u8; 8],
    max_record_bytes: usize,
    kind: LogKind,
) -> Result<DecodedLog> {
    if bytes.is_empty() {
        return Ok((0, Vec::new(), 0));
    }
    if bytes.len() < magic.len() {
        return Ok((0, Vec::new(), 0));
    }
    if &bytes[..magic.len()] != magic {
        return Err(kind.corrupt(format!("invalid {} magic", kind.name())));
    }

    let mut offset = magic.len();
    let mut sequence = 0;
    let mut payloads = Vec::new();
    while offset < bytes.len() {
        if bytes.len() - offset < HEADER_LEN {
            break;
        }
        let frame_start = offset;
        let next_sequence = u64::from_le_bytes(
            bytes[offset..offset + 8]
                .try_into()
                .expect("fixed sequence width"),
        );
        offset += 8;
        let length = u32::from_le_bytes(
            bytes[offset..offset + 4]
                .try_into()
                .expect("fixed length width"),
        );
        offset += 4;
        let payload_len = length as usize;
        if payload_len > max_record_bytes {
            return Err(kind.record_limit(payload_len, max_record_bytes));
        }
        let Some(frame_end) = offset
            .checked_add(payload_len)
            .and_then(|end| end.checked_add(CHECKSUM_LEN))
        else {
            return Err(kind.corrupt(format!("{} frame length overflow", kind.name())));
        };
        if frame_end > bytes.len() {
            offset = frame_start;
            break;
        }
        if next_sequence != sequence + 1 {
            return Err(kind.corrupt(format!(
                "{} sequence {next_sequence} followed {sequence}",
                kind.name()
            )));
        }
        let payload = &bytes[offset..offset + payload_len];
        let actual_checksum = &bytes[offset + payload_len..frame_end];
        if actual_checksum != checksum(next_sequence, length, payload) {
            return Err(kind.corrupt(format!(
                "{} record {next_sequence} checksum mismatch",
                kind.name()
            )));
        }
        payloads.push((next_sequence, payload.to_vec()));
        sequence = next_sequence;
        offset = frame_end;
    }
    Ok((sequence, payloads, offset))
}

fn checksum(sequence: u64, length: u32, payload: &[u8]) -> [u8; CHECKSUM_LEN] {
    let mut digest = Sha256::new();
    digest.update(sequence.to_le_bytes());
    digest.update(length.to_le_bytes());
    digest.update(payload);
    digest.finalize().into()
}

fn durable_io(
    kind: LogKind,
    operation: &'static str,
    path: &Path,
    source: std::io::Error,
) -> Error {
    match kind {
        LogKind::Composition => Error::JournalIo {
            operation,
            path: path.to_path_buf(),
            source,
        },
        LogKind::Event => Error::EventIo {
            operation,
            path: path.to_path_buf(),
            source,
        },
    }
}
