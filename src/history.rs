//! Read-only historical queries and export. No execution commands enter this seam.
use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc};
use std::thread;
use std::time::Duration;

use serde_json::Value;
use thiserror::Error;
use tokio::sync::watch;

use crate::journal::{self, JournalError, JournalSettings, Record, Replay, SessionInfo};

const ID_LIMIT: usize = 8192;
const ID_BYTES: usize = 2 * 1024 * 1024;
const PREVIEW_BYTES: usize = 8192;
static NEXT_FILE: AtomicU64 = AtomicU64::new(1);

pub mod search;

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum HistoryError {
    #[error(transparent)]
    Journal(#[from] JournalError),
    #[error("history worker is busy; wait for the current operation")]
    Busy,
    #[error("history worker closed before confirming the operation")]
    Closed,
    #[error("export was cancelled before publication")]
    Cancelled,
    #[error("export exceeds its identity budget; select a later event sequence")]
    IdentityLimit,
    #[error("export destination already exists; choose a new file")]
    Exists,
    #[error("exports must be outside the managed journal directory")]
    ManagedDirectory,
    #[error("export file operation failed; no partial destination was published")]
    ExportIo,
    #[error("export was published, but directory sync was not confirmed")]
    PublishedUnconfirmed,
    #[error("preview an export range before saving it")]
    PreviewRequired,
}

#[derive(Debug, Clone)]
pub struct HistoricalView {
    pub info: SessionInfo,
    pub selected: Record,
    pub uncommitted_tail: bool,
}

#[derive(Debug, Clone)]
pub struct ExportPreview {
    pub manifest: Value,
    pub excerpt: String,
}

#[derive(Debug, Clone)]
pub enum HistoryResult {
    Sessions(Vec<SessionInfo>),
    Loaded(Box<HistoricalView>),
    Preview(ExportPreview),
    Exported { bytes: u64 },
}

#[derive(Debug)]
pub enum HistoryRequest {
    List,
    Open {
        session: String,
        sequence: Option<u64>,
    },
    Preview {
        session: String,
        since: u64,
    },
    Export {
        destination: PathBuf,
    },
}

#[derive(Debug, Clone)]
pub struct HistoryStatus {
    pub request_id: u64,
    pub result: Option<Result<HistoryResult, HistoryError>>,
}

pub struct HistoryHandle {
    pub search: search::SearchHandle,
    sender: Option<mpsc::SyncSender<(u64, HistoryRequest)>>,
    pub status: watch::Receiver<HistoryStatus>,
    cancel: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
    next_id: u64,
}

impl HistoryHandle {
    pub fn start(settings: JournalSettings, cwd: PathBuf) -> Result<Self, HistoryError> {
        let search = search::SearchHandle::start(settings.clone(), cwd.clone())?;
        let (sender, requests) = mpsc::sync_channel::<(u64, HistoryRequest)>(1);
        let (updates, status) = watch::channel(HistoryStatus {
            request_id: 0,
            result: None,
        });
        let cancel = Arc::new(AtomicBool::new(false));
        let stopping = cancel.clone();
        let worker = thread::Builder::new()
            .name("history-reader".into())
            .spawn(move || {
                let mut prepared: Option<PreparedExport> = None;
                while let Ok((request_id, request)) = requests.recv() {
                    if stopping.load(Ordering::Acquire) {
                        break;
                    }
                    let result = match request {
                        HistoryRequest::List => {
                            prepared = None;
                            journal::sessions(&settings, &cwd)
                                .map(HistoryResult::Sessions)
                                .map_err(HistoryError::from)
                        }
                        HistoryRequest::Open { session, sequence } => {
                            prepared = None;
                            Replay::open(&settings, &cwd, &session, sequence.unwrap_or(0))
                                .map(|replay| {
                                    let selected = if sequence.is_some() {
                                        replay.baseline.clone()
                                    } else {
                                        replay.latest.clone()
                                    }
                                    .into_history();
                                    HistoryResult::Loaded(Box::new(HistoricalView {
                                        info: replay.info,
                                        selected,
                                        uncommitted_tail: replay.uncommitted_tail,
                                    }))
                                })
                                .map_err(HistoryError::from)
                        }
                        HistoryRequest::Preview { session, since } => {
                            prepared = None;
                            PreparedExport::open(&settings, &cwd, &session, since, &stopping).map(
                                |export| {
                                    let preview = export.preview.clone();
                                    prepared = Some(export);
                                    HistoryResult::Preview(preview)
                                },
                            )
                        }
                        HistoryRequest::Export { destination } => match prepared.take() {
                            Some(export) => export
                                .write_file(&destination, &stopping)
                                .map(|bytes| HistoryResult::Exported { bytes }),
                            None => Err(HistoryError::PreviewRequired),
                        },
                    };
                    updates.send_replace(HistoryStatus {
                        request_id,
                        result: Some(result),
                    });
                }
            })
            .map_err(|_| HistoryError::Closed)?;
        Ok(Self {
            search,
            sender: Some(sender),
            status,
            cancel,
            worker: Some(worker),
            next_id: 1,
        })
    }

    pub fn request(&mut self, request: HistoryRequest) -> Result<u64, HistoryError> {
        if self.status.has_changed().is_err() {
            return Err(HistoryError::Closed);
        }
        if self.next_id > 1 && self.status.borrow().request_id != self.next_id - 1 {
            return Err(HistoryError::Busy);
        }
        let id = self.next_id;
        self.sender
            .as_ref()
            .ok_or(HistoryError::Closed)?
            .try_send((id, request))
            .map_err(|error| match error {
                mpsc::TrySendError::Full(_) => HistoryError::Busy,
                mpsc::TrySendError::Disconnected(_) => HistoryError::Closed,
            })?;
        self.next_id = self.next_id.checked_add(1).ok_or(HistoryError::Closed)?;
        Ok(id)
    }

    pub async fn response(&mut self, id: u64) -> Result<HistoryResult, HistoryError> {
        if id == 0 || id >= self.next_id {
            return Err(HistoryError::Closed);
        }
        loop {
            let status = self.status.borrow().clone();
            if status.request_id > id {
                return Err(HistoryError::Closed);
            }
            if status.request_id == id {
                if let Some(result) = status.result {
                    return result;
                }
            }
            self.status
                .changed()
                .await
                .map_err(|_| HistoryError::Closed)?;
        }
    }

    pub async fn shutdown(&mut self) -> Result<(), HistoryError> {
        self.cancel.store(true, Ordering::Release);
        self.sender.take();
        let search_result = self.search.shutdown().await;
        let result = tokio::time::timeout(Duration::from_secs(1), async {
            while self
                .worker
                .as_ref()
                .is_some_and(|worker| !worker.is_finished())
            {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            if let Some(worker) = self.worker.take() {
                worker.join().map_err(|_| HistoryError::Closed)?;
            }
            Ok(())
        })
        .await
        .unwrap_or(Err(HistoryError::Closed));
        result.and(search_result)
    }
}

impl Drop for HistoryHandle {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Release);
        self.sender.take();
    }
}

pub struct PreparedExport {
    replay: Replay,
    managed_directory: PathBuf,
    pub preview: ExportPreview,
}

impl PreparedExport {
    pub fn open(
        settings: &JournalSettings,
        cwd: &Path,
        session: &str,
        since: u64,
        cancel: &AtomicBool,
    ) -> Result<Self, HistoryError> {
        let mut replay = Replay::open(settings, cwd, session, since)?;
        let managed_directory = settings
            .directory()?
            .canonicalize()
            .map_err(|_| HistoryError::ExportIo)?;
        let mut redactor = Redactor::default();
        let mut excerpt = String::new();
        let mut failure = None;
        replay
            .visit_records(|record| {
                if cancel.load(Ordering::Acquire) {
                    failure = Some(HistoryError::Cancelled);
                    return Err(JournalError::Output);
                }
                let value = match redactor.record(record) {
                    Ok(value) => value,
                    Err(error) => {
                        failure = Some(error);
                        return Err(JournalError::Output);
                    }
                };
                if excerpt.len() < PREVIEW_BYTES {
                    let line = serde_json::to_string(&value).map_err(|_| JournalError::Corrupt)?;
                    let remaining = PREVIEW_BYTES - excerpt.len();
                    let mut end = line.len().min(remaining.saturating_sub(1));
                    while !line.is_char_boundary(end) {
                        end -= 1;
                    }
                    excerpt.push_str(&line[..end]);
                    excerpt.push('\n');
                }
                Ok(())
            })
            .map_err(|error| failure.unwrap_or(error.into()))?;
        let manifest = serde_json::json!({
            "kind": "export_manifest", "export_version": 1,
            "source_schema_version": replay.info.schema_version,
            "since": since, "high_watermark": replay.info.committed_seq,
            "live_attached": false, "session_closed": replay.info.session_closed,
            "execution_result": replay.info.execution_result,
            "cleanup_confirmed": replay.latest_state().cleanup_confirmed,
            "needs_recovery": replay.info.needs_recovery,
            "uncommitted_tail": replay.uncommitted_tail,
            "identities": "stable_aliases", "reverse_mapping_included": false,
            "content_not_retained": ["prompts", "messages", "commands", "questions", "answers", "configuration", "raw_errors"]
        });
        Ok(Self {
            replay,
            managed_directory,
            preview: ExportPreview { manifest, excerpt },
        })
    }

    pub fn write_file(self, destination: &Path, cancel: &AtomicBool) -> Result<u64, HistoryError> {
        self.save(destination, cancel, |export, file| {
            export.write(file, cancel)
        })
    }

    fn save(
        mut self,
        destination: &Path,
        cancel: &AtomicBool,
        write: impl FnOnce(&mut Self, &mut File) -> Result<u64, HistoryError>,
    ) -> Result<u64, HistoryError> {
        if fs::symlink_metadata(destination).is_ok() {
            return Err(HistoryError::Exists);
        }
        let parent = destination
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        let parent = parent.canonicalize().map_err(|_| HistoryError::ExportIo)?;
        if parent.starts_with(&self.managed_directory) {
            return Err(HistoryError::ManagedDirectory);
        }
        let name = destination.file_name().ok_or(HistoryError::ExportIo)?;
        let destination = parent.join(name);
        let mut temporary = TemporaryExport::new(&parent)?;
        let bytes = write(&mut self, temporary.file.as_mut().unwrap())?;
        temporary
            .file
            .as_ref()
            .unwrap()
            .sync_all()
            .map_err(|_| HistoryError::ExportIo)?;
        temporary.file.take();
        if cancel.load(Ordering::Acquire) {
            return Err(HistoryError::Cancelled);
        }
        // A hard link atomically publishes a complete file and cannot replace a destination.
        fs::hard_link(&temporary.path, &destination).map_err(|error| {
            if error.kind() == io::ErrorKind::AlreadyExists {
                HistoryError::Exists
            } else {
                HistoryError::ExportIo
            }
        })?;
        #[cfg(unix)]
        File::open(&parent)
            .and_then(|file| file.sync_all())
            .map_err(|_| HistoryError::PublishedUnconfirmed)?;
        Ok(bytes)
    }

    fn write(&mut self, writer: &mut impl Write, cancel: &AtomicBool) -> Result<u64, HistoryError> {
        let mut redactor = Redactor::default();
        let mut bytes = 0u64;
        let manifest =
            serde_json::to_vec(&self.preview.manifest).map_err(|_| HistoryError::ExportIo)?;
        writer
            .write_all(&manifest)
            .and_then(|_| writer.write_all(b"\n"))
            .map_err(|_| HistoryError::ExportIo)?;
        bytes += manifest.len() as u64 + 1;
        let mut failure = None;
        self.replay
            .visit_records(|record| {
                if cancel.load(Ordering::Acquire) {
                    failure = Some(HistoryError::Cancelled);
                    return Err(JournalError::Output);
                }
                let value = match redactor.record(record) {
                    Ok(value) => value,
                    Err(error) => {
                        failure = Some(error);
                        return Err(JournalError::Output);
                    }
                };
                let line = serde_json::to_vec(&value).map_err(|_| JournalError::Corrupt)?;
                writer
                    .write_all(&line)
                    .and_then(|_| writer.write_all(b"\n"))
                    .map_err(|_| JournalError::Output)?;
                bytes += line.len() as u64 + 1;
                Ok(())
            })
            .map_err(|error| {
                failure.unwrap_or(if error == JournalError::Output {
                    HistoryError::ExportIo
                } else {
                    error.into()
                })
            })?;
        writer.flush().map_err(|_| HistoryError::ExportIo)?;
        Ok(bytes)
    }
}

#[derive(Default)]
struct Redactor {
    ids: BTreeMap<String, String>,
    bytes: usize,
}

impl Redactor {
    fn record(&mut self, record: Record) -> Result<Value, HistoryError> {
        let mut value = serde_json::to_value(record).map_err(|_| HistoryError::ExportIo)?;
        self.value("", &mut value)?;
        Ok(value)
    }

    fn value(&mut self, field: &str, value: &mut Value) -> Result<(), HistoryError> {
        match value {
            Value::String(text) => {
                // These fields are typed enums. Every free string is aliased, including IDs
                // containing tokens or private paths; a reverse map is never serialized.
                if [
                    "kind",
                    "phase",
                    "state",
                    "scope",
                    "execution_state",
                    "execution_result",
                    "freshness",
                    "source",
                    "level",
                    "reason",
                    "config_source",
                    "issue",
                    "interaction_state",
                    "tool_category",
                    "wait_reason",
                    "resume_condition",
                    "outcome",
                    "action",
                ]
                .contains(&field)
                {
                    return Ok(());
                }
                if let Some(alias) = self.ids.get(text) {
                    *text = alias.clone();
                    return Ok(());
                }
                if self.ids.len() >= ID_LIMIT || self.bytes + text.len() > ID_BYTES {
                    return Err(HistoryError::IdentityLimit);
                }
                let alias = format!("id-{}", self.ids.len() + 1);
                self.bytes += text.len();
                self.ids.insert(text.clone(), alias.clone());
                *text = alias;
            }
            Value::Array(values) => {
                for value in values {
                    self.value(field, value)?;
                }
            }
            Value::Object(values) => {
                for (field, value) in values {
                    if field == "provider_state" {
                        *value = Value::Null;
                    } else {
                        self.value(field, value)?;
                    }
                }
            }
            _ => {}
        }
        Ok(())
    }
}

struct TemporaryExport {
    path: PathBuf,
    file: Option<File>,
}

impl TemporaryExport {
    fn new(parent: &Path) -> Result<Self, HistoryError> {
        for _ in 0..16 {
            let id = NEXT_FILE.fetch_add(1, Ordering::Relaxed);
            let path = parent.join(format!(
                ".native-agent-export-{}-{id}.tmp",
                std::process::id()
            ));
            let mut options = OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            match options.open(&path) {
                Ok(file) => {
                    return Ok(Self {
                        path,
                        file: Some(file),
                    })
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(_) => return Err(HistoryError::ExportIo),
            }
        }
        Err(HistoryError::ExportIo)
    }
}

impl Drop for TemporaryExport {
    fn drop(&mut self) {
        self.file.take();
        let _ = fs::remove_file(&self.path);
    }
}

#[cfg(test)]
mod tests;
