use std::{
    fs::{File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{ComponentTree, CompositionPatch, Error, Result};

const MAGIC: &[u8; 8] = b"QUARTZJ1";
const HEADER_LEN: usize = 12;
const CHECKSUM_LEN: usize = 32;
const SCHEMA_VERSION: u32 = 1;

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
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct JournalPayload {
    schema: u32,
    snapshot: JournalSnapshot,
}

pub(crate) struct Journal {
    path: PathBuf,
    file: File,
    sequence: u64,
    recovered_snapshot: Option<JournalSnapshot>,
    max_record_bytes: usize,
}

impl Journal {
    pub(crate) fn open(path: &Path, max_record_bytes: usize) -> Result<Self> {
        let mut file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(path)
            .map_err(|source| journal_io("open", path, source))?;
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)
            .map_err(|source| journal_io("read", path, source))?;

        let (sequence, recovered_snapshot, valid_len) = decode(&bytes, max_record_bytes)?;
        if valid_len != bytes.len() {
            file.set_len(valid_len as u64)
                .map_err(|source| journal_io("repair torn tail", path, source))?;
            file.sync_data()
                .map_err(|source| journal_io("synchronize torn-tail repair", path, source))?;
        }
        file.seek(SeekFrom::End(0))
            .map_err(|source| journal_io("seek", path, source))?;

        Ok(Self {
            path: path.to_path_buf(),
            file,
            sequence,
            recovered_snapshot,
            max_record_bytes,
        })
    }

    pub(crate) fn recovered(&self) -> Option<JournalSnapshot> {
        self.recovered_snapshot.clone()
    }

    pub(crate) fn sequence(&self) -> u64 {
        self.sequence
    }

    pub(crate) fn append(&mut self, snapshot: &JournalSnapshot) -> Result<()> {
        let payload = serde_json::to_vec(&JournalPayload {
            schema: SCHEMA_VERSION,
            snapshot: snapshot.clone(),
        })?;
        if payload.len() > self.max_record_bytes {
            return Err(Error::JournalRecordLimit {
                actual: payload.len(),
                limit: self.max_record_bytes,
            });
        }
        let length = u32::try_from(payload.len()).map_err(|_| Error::JournalRecordLimit {
            actual: payload.len(),
            limit: u32::MAX as usize,
        })?;
        let sequence = self.sequence + 1;
        let mut frame = Vec::with_capacity(
            usize::from(self.sequence == 0) * MAGIC.len()
                + HEADER_LEN
                + payload.len()
                + CHECKSUM_LEN,
        );
        if self.sequence == 0 {
            frame.extend_from_slice(MAGIC);
        }
        frame.extend_from_slice(&sequence.to_le_bytes());
        frame.extend_from_slice(&length.to_le_bytes());
        frame.extend_from_slice(&payload);
        frame.extend_from_slice(&checksum(sequence, length, &payload));

        let start = self
            .file
            .stream_position()
            .map_err(|source| journal_io("read append position", &self.path, source))?;
        if let Err(source) = self
            .file
            .write_all(&frame)
            .and_then(|()| self.file.sync_data())
        {
            let _ = self.file.set_len(start);
            let _ = self.file.seek(SeekFrom::Start(start));
            return Err(journal_io("append and synchronize", &self.path, source));
        }

        self.sequence = sequence;
        self.recovered_snapshot = Some(snapshot.clone());
        Ok(())
    }
}

fn decode(bytes: &[u8], max_record_bytes: usize) -> Result<(u64, Option<JournalSnapshot>, usize)> {
    if bytes.is_empty() {
        return Ok((0, None, 0));
    }
    if bytes.len() < MAGIC.len() {
        return Ok((0, None, 0));
    }
    if &bytes[..MAGIC.len()] != MAGIC {
        return Err(Error::JournalCorrupt("invalid journal magic".into()));
    }

    let mut offset = MAGIC.len();
    let mut sequence = 0;
    let mut recovered_snapshot = None;
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
            return Err(Error::JournalRecordLimit {
                actual: payload_len,
                limit: max_record_bytes,
            });
        }
        let Some(frame_end) = offset
            .checked_add(payload_len)
            .and_then(|end| end.checked_add(CHECKSUM_LEN))
        else {
            return Err(Error::JournalCorrupt(
                "journal frame length overflow".into(),
            ));
        };
        if frame_end > bytes.len() {
            offset = frame_start;
            break;
        }
        if next_sequence != sequence + 1 {
            return Err(Error::JournalCorrupt(format!(
                "journal sequence {next_sequence} followed {sequence}"
            )));
        }
        let payload = &bytes[offset..offset + payload_len];
        let actual_checksum = &bytes[offset + payload_len..frame_end];
        if actual_checksum != checksum(next_sequence, length, payload) {
            return Err(Error::JournalCorrupt(format!(
                "journal record {next_sequence} checksum mismatch"
            )));
        }
        let record: JournalPayload = serde_json::from_slice(payload)
            .map_err(|error| Error::JournalCorrupt(error.to_string()))?;
        if record.schema != SCHEMA_VERSION {
            return Err(Error::JournalCorrupt(format!(
                "unsupported journal schema {}",
                record.schema
            )));
        }
        sequence = next_sequence;
        recovered_snapshot = Some(record.snapshot);
        offset = frame_end;
    }

    Ok((sequence, recovered_snapshot, offset))
}

fn checksum(sequence: u64, length: u32, payload: &[u8]) -> [u8; CHECKSUM_LEN] {
    let mut digest = Sha256::new();
    digest.update(sequence.to_le_bytes());
    digest.update(length.to_le_bytes());
    digest.update(payload);
    digest.finalize().into()
}

fn journal_io(operation: &'static str, path: &Path, source: std::io::Error) -> Error {
    Error::JournalIo {
        operation,
        path: path.to_path_buf(),
        source,
    }
}
