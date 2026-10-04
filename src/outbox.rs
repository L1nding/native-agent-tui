//! Durable intent records for side effects.
//!
//! The outbox deliberately stores request identity, a payload hash, and the
//! observed delivery state. It never stores the payload itself and never
//! sends anything. A single `Outbox` value owns the writer so callers must
//! route all state transitions through one execution owner.

use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io::{self, BufRead, BufReader, Write};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{de, Deserialize, Deserializer, Serialize, Serializer};
use thiserror::Error;

use crate::scheduler::TaskAttempt;

pub const SCHEMA_VERSION: u32 = 1;
const MAX_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PayloadHash(u64);

impl PayloadHash {
    /// Compute a stable, non-secret-bearing identity for a payload.
    ///
    /// The hash is used to detect that an intent was reconstructed with a
    /// different payload. It is not a password hash and the payload is never
    /// persisted.
    pub fn from_bytes(bytes: &[u8]) -> Self {
        let mut hash = 0xcbf29ce484222325u64;
        for byte in bytes {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(0x100000001b3);
        }
        Self(hash)
    }

    pub fn as_hex(self) -> String {
        format!("{:016x}", self.0)
    }
}

impl Serialize for PayloadHash {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.as_hex())
    }
}

impl<'de> Deserialize<'de> for PayloadHash {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        let parsed = u64::from_str_radix(&value, 16).map_err(de::Error::custom)?;
        Ok(Self(parsed))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutboxStatus {
    Pending,
    Sent,
    Confirmed,
    Unknown,
    Failed,
}

impl OutboxStatus {
    fn can_transition_to(self, next: Self) -> bool {
        matches!(
            (self, next),
            (Self::Pending, Self::Sent)
                | (Self::Pending, Self::Unknown)
                | (Self::Pending, Self::Failed)
                | (Self::Sent, Self::Confirmed)
                | (Self::Sent, Self::Unknown)
                | (Self::Sent, Self::Failed)
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OutboxIntent {
    pub id: u64,
    pub workflow_id: String,
    pub task: Option<TaskAttempt>,
    pub request_id: String,
    pub method: String,
    pub payload_hash: PayloadHash,
}

impl OutboxIntent {
    pub fn new(
        id: u64,
        workflow_id: impl Into<String>,
        task: Option<TaskAttempt>,
        request_id: impl Into<String>,
        method: impl Into<String>,
        payload: &[u8],
    ) -> Self {
        Self {
            id,
            workflow_id: workflow_id.into(),
            task,
            request_id: request_id.into(),
            method: method.into(),
            payload_hash: PayloadHash::from_bytes(payload),
        }
    }

    fn validate(&self) -> Result<(), OutboxError> {
        if self.workflow_id.is_empty() || self.request_id.is_empty() || self.method.is_empty() {
            return Err(OutboxError::InvalidIntent);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OutboxRecord {
    pub intent: OutboxIntent,
    pub status: OutboxStatus,
    pub updated_at: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[serde(rename_all = "snake_case", tag = "kind")]
enum OutboxEvent {
    Intent {
        intent: OutboxIntent,
        updated_at: u64,
    },
    Status {
        id: u64,
        status: OutboxStatus,
        updated_at: u64,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct OutboxLine {
    schema_version: u32,
    seq: u64,
    event: OutboxEvent,
}

#[derive(Debug, Error)]
pub enum OutboxError {
    #[error("outbox I/O failed")]
    Io(#[source] io::Error),
    #[error("outbox record is invalid or incomplete")]
    Corrupt,
    #[error("outbox intent is missing a workflow, request identity, or method")]
    InvalidIntent,
    #[error("outbox intent id already exists: {0}")]
    Duplicate(u64),
    #[error("outbox intent was not found: {0}")]
    NotFound(u64),
    #[error("invalid outbox transition for {id}: {from:?} -> {to:?}")]
    InvalidTransition {
        id: u64,
        from: OutboxStatus,
        to: OutboxStatus,
    },
    #[error("outbox sequence overflow")]
    SequenceOverflow,
    #[error("outbox storage budget is full")]
    Budget,
}

impl From<io::Error> for OutboxError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

/// The sole mutable owner of an outbox file.
pub struct Outbox {
    file: File,
    records: BTreeMap<u64, OutboxRecord>,
    next_seq: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutboxSnapshot {
    records: BTreeMap<u64, OutboxRecord>,
}

impl OutboxSnapshot {
    /// Read an existing outbox without creating or opening it for append.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, OutboxError> {
        let file = File::open(path)?;
        let (records, _) = load_records(BufReader::new(file))?;
        Ok(Self { records })
    }

    pub fn records(&self) -> impl Iterator<Item = &OutboxRecord> {
        self.records.values()
    }

    pub fn get(&self, id: u64) -> Option<&OutboxRecord> {
        self.records.get(&id)
    }
}

impl Outbox {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, OutboxError> {
        let path = path.as_ref();
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .read(true)
            .open(path)?;
        let reader = BufReader::new(file.try_clone()?);
        let (records, next_seq) = load_records(reader)?;
        Ok(Self {
            file,
            records,
            next_seq,
        })
    }

    pub fn records(&self) -> impl Iterator<Item = &OutboxRecord> {
        self.records.values()
    }

    pub fn get(&self, id: u64) -> Option<&OutboxRecord> {
        self.records.get(&id)
    }

    pub fn record_intent(&mut self, intent: OutboxIntent) -> Result<(), OutboxError> {
        intent.validate()?;
        if self.records.contains_key(&intent.id) {
            return Err(OutboxError::Duplicate(intent.id));
        }
        let updated_at = now();
        self.append(OutboxEvent::Intent {
            intent: intent.clone(),
            updated_at,
        })?;
        self.records.insert(
            intent.id,
            OutboxRecord {
                intent,
                status: OutboxStatus::Pending,
                updated_at,
            },
        );
        Ok(())
    }

    pub fn mark_sent(&mut self, id: u64) -> Result<(), OutboxError> {
        self.transition(id, OutboxStatus::Sent)
    }

    pub fn mark_confirmed(&mut self, id: u64) -> Result<(), OutboxError> {
        self.transition(id, OutboxStatus::Confirmed)
    }

    pub fn mark_unknown(&mut self, id: u64) -> Result<(), OutboxError> {
        self.transition(id, OutboxStatus::Unknown)
    }

    pub fn mark_failed(&mut self, id: u64) -> Result<(), OutboxError> {
        self.transition(id, OutboxStatus::Failed)
    }

    fn transition(&mut self, id: u64, status: OutboxStatus) -> Result<(), OutboxError> {
        let current = self
            .records
            .get(&id)
            .ok_or(OutboxError::NotFound(id))?
            .status;
        if !current.can_transition_to(status) {
            return Err(OutboxError::InvalidTransition {
                id,
                from: current,
                to: status,
            });
        }
        let updated_at = now();
        self.append(OutboxEvent::Status {
            id,
            status,
            updated_at,
        })?;
        let record = self.records.get_mut(&id).expect("record checked above");
        record.status = status;
        record.updated_at = updated_at;
        Ok(())
    }

    fn append(&mut self, event: OutboxEvent) -> Result<(), OutboxError> {
        let line = OutboxLine {
            schema_version: SCHEMA_VERSION,
            seq: self.next_seq,
            event,
        };
        let bytes = serde_json::to_vec(&line).map_err(|_| OutboxError::Corrupt)?;
        let current = self.file.metadata()?.len();
        if current.saturating_add(bytes.len() as u64).saturating_add(1) > MAX_BYTES {
            return Err(OutboxError::Budget);
        }
        self.file.write_all(&bytes)?;
        self.file.write_all(b"\n")?;
        self.file.sync_data()?;
        self.next_seq = self
            .next_seq
            .checked_add(1)
            .ok_or(OutboxError::SequenceOverflow)?;
        Ok(())
    }
}

fn load_records(reader: impl BufRead) -> Result<(BTreeMap<u64, OutboxRecord>, u64), OutboxError> {
    let mut records = BTreeMap::new();
    let mut next_seq = 0;
    for line in reader.lines() {
        let line = line.map_err(OutboxError::Io)?;
        let event: OutboxLine = serde_json::from_str(&line).map_err(|_| OutboxError::Corrupt)?;
        if event.schema_version != SCHEMA_VERSION || event.seq != next_seq {
            return Err(OutboxError::Corrupt);
        }
        match event.event {
            OutboxEvent::Intent { intent, updated_at } => {
                intent.validate()?;
                if records.contains_key(&intent.id) {
                    return Err(OutboxError::Corrupt);
                }
                records.insert(
                    intent.id,
                    OutboxRecord {
                        intent,
                        status: OutboxStatus::Pending,
                        updated_at,
                    },
                );
            }
            OutboxEvent::Status {
                id,
                status,
                updated_at,
            } => {
                let record = records.get_mut(&id).ok_or(OutboxError::Corrupt)?;
                if !record.status.can_transition_to(status) {
                    return Err(OutboxError::Corrupt);
                }
                record.status = status;
                record.updated_at = updated_at;
            }
        }
        next_seq = next_seq
            .checked_add(1)
            .ok_or(OutboxError::SequenceOverflow)?;
    }
    Ok((records, next_seq))
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    fn fixture(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "native-agent-tui-outbox-{name}-{}-{}.jsonl",
            std::process::id(),
            now()
        ));
        let _ = fs::remove_file(&path);
        path
    }

    fn intent(id: u64, payload: &[u8]) -> OutboxIntent {
        OutboxIntent::new(
            id,
            "workflow-1",
            Some(TaskAttempt {
                task: crate::scheduler::TaskId(3),
                attempt: 1,
            }),
            format!("rpc-{id}"),
            "turn/start",
            payload,
        )
    }

    #[test]
    fn persists_intent_and_delivery_state_without_payload() {
        let path = fixture("state");
        let secret = b"private prompt that must not be persisted";
        let mut outbox = Outbox::open(&path).unwrap();
        outbox.record_intent(intent(1, secret)).unwrap();
        outbox.mark_sent(1).unwrap();
        outbox.mark_unknown(1).unwrap();
        drop(outbox);

        let bytes = fs::read(&path).unwrap();
        assert!(!String::from_utf8_lossy(&bytes).contains("private prompt"));
        let reopened = Outbox::open(&path).unwrap();
        assert_eq!(reopened.get(1).unwrap().status, OutboxStatus::Unknown);
        assert_eq!(
            reopened.get(1).unwrap().intent.payload_hash,
            PayloadHash::from_bytes(secret)
        );
        let _ = fs::remove_file(path);
    }

    #[test]
    fn rejects_side_effect_replay_after_unknown() {
        let path = fixture("transition");
        let mut outbox = Outbox::open(&path).unwrap();
        outbox.record_intent(intent(1, b"payload")).unwrap();
        outbox.mark_unknown(1).unwrap();
        assert!(matches!(
            outbox.mark_sent(1),
            Err(OutboxError::InvalidTransition {
                from: OutboxStatus::Unknown,
                to: OutboxStatus::Sent,
                ..
            })
        ));
        let _ = fs::remove_file(path);
    }

    #[test]
    fn rejects_torn_or_mismatched_history() {
        let path = fixture("corrupt");
        fs::write(
            &path,
            br#"{"schema_version":1,"seq":1,"event":{"kind":"intent"}}\n"#,
        )
        .unwrap();
        assert!(matches!(Outbox::open(&path), Err(OutboxError::Corrupt)));
        let _ = fs::remove_file(path);
    }
}
