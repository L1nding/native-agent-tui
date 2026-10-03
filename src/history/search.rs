//! Cancellable, bounded search over a fixed committed journal prefix.
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::Duration;

use tokio::sync::watch;

use super::{HistoricalView, HistoryError};
use crate::journal::{JournalError, JournalSettings, Payload, Replay, SessionInfo};
use crate::observation::{ActivityIdentity, ActivityScope, Evidence, EvidenceKind};

pub const FIELD_BYTES: usize = 1024;
pub const HIT_LIMIT: usize = 128;
pub const HIT_BYTES: usize = 128 * 1024;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Category {
    #[default]
    All,
    Lifecycle,
    Output,
    Tool,
    Request,
    Waiting,
}

impl Category {
    pub fn label(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Lifecycle => "lifecycle",
            Self::Output => "output",
            Self::Tool => "tool",
            Self::Request => "request",
            Self::Waiting => "waiting",
        }
    }

    pub fn next(self) -> Self {
        match self {
            Self::All => Self::Lifecycle,
            Self::Lifecycle => Self::Output,
            Self::Output => Self::Tool,
            Self::Tool => Self::Request,
            Self::Request => Self::Waiting,
            Self::Waiting => Self::All,
        }
    }
    fn accepts(self, kind: EvidenceKind) -> bool {
        use EvidenceKind::*;
        let category = match kind {
            Output | MessageFinalized => Self::Output,
            ToolStarted | ToolCompleted => Self::Tool,
            RequestCreated | RequestAnswered | RequestResolved | RequestExpired => Self::Request,
            GateEntered | GateReleased | ChildTurnBound | ChildTerminal => Self::Waiting,
            _ => Self::Lifecycle,
        };
        self == Self::All || self == category
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Query {
    pub text: String,
    pub thread: String,
    pub turn: String,
    pub category: Category,
}

impl Query {
    fn validate(&self) -> Result<(), HistoryError> {
        if [&self.text, &self.thread, &self.turn]
            .iter()
            .any(|text| text.len() > FIELD_BYTES)
        {
            Err(HistoryError::Journal(JournalError::RecordLimit))
        } else {
            Ok(())
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hit {
    pub session_id: String,
    /// First retained journal record containing this accepted evidence.
    pub event_seq: u64,
    pub activity_id: String,
    pub identity: ActivityIdentity,
    pub scope: ActivityScope,
    pub evidence: Evidence,
}

impl Hit {
    pub fn metadata(&self) -> String {
        format!(
            "#{} {:?} {:?} | {:?} | agent <redacted> | thread {} | turn {} | item {} | request {} | task {} attempt {:?} generation {:?} | recorded ms {:?} | bytes {}",
            self.evidence.id,
            self.evidence.kind,
            self.evidence.source,
            self.scope,
            presence(self.identity.thread_id.as_deref()),
            presence(self.identity.turn_id.as_deref()),
            presence(self.evidence.item_id.as_deref()),
            presence(self.evidence.request_id.as_ref().map(|_| "present")),
            presence(self.identity.task_id.as_ref().map(|_| "present")),
            self.identity.attempt_id,
            self.identity.generation,
            self.evidence.recorded_at_ms,
            self.evidence.output_bytes
        )
    }

    fn searchable_metadata(&self) -> String {
        format!(
            "#{} {:?} {:?} | {:?} | agent {} | thread {} | turn {} | item {:?} | request {:?} | task {:?} attempt {:?} generation {:?} | recorded ms {:?} | bytes {}",
            self.evidence.id,
            self.evidence.kind,
            self.evidence.source,
            self.scope,
            self.identity.agent_id,
            self.identity.thread_id.as_deref().unwrap_or("unavailable"),
            self.identity.turn_id.as_deref().unwrap_or("unavailable"),
            self.evidence.item_id,
            self.evidence.request_id,
            self.identity.task_id,
            self.identity.attempt_id,
            self.identity.generation,
            self.evidence.recorded_at_ms,
            self.evidence.output_bytes
        )
    }

    fn bytes(&self) -> usize {
        std::mem::size_of::<Self>()
            + self.session_id.capacity()
            + self.activity_id.capacity()
            + self.identity.agent_id.capacity()
            + self.identity.thread_id.as_ref().map_or(0, String::capacity)
            + self.identity.turn_id.as_ref().map_or(0, String::capacity)
            + self.evidence.item_id.as_ref().map_or(0, String::capacity)
            + match &self.evidence.request_id {
                Some(crate::protocol::RpcId::String(text)) => text.capacity(),
                _ => 0,
            }
    }

    pub fn matches(&self, view: &HistoricalView) -> bool {
        view.info.session_id == self.session_id
            && view.selected.event_seq == self.event_seq
            && view.selected.state().is_ok_and(|state| {
                state.observation.activities.iter().any(|activity| {
                    activity.activity_id == self.activity_id
                        && activity.identity == self.identity
                        && activity.scope == self.scope
                        && activity.recent_evidence.contains(&self.evidence)
                })
            })
    }
}

#[derive(Debug)]
pub struct Results {
    pub info: SessionInfo,
    pub query: Query,
    pub hits: Vec<Hit>,
    pub total: u64,
    pub omitted_evidence: u64,
    pub retained_bytes: usize,
    pub uncommitted_tail: bool,
}

pub(crate) fn scan(
    replay: &mut Replay,
    query: Query,
    cancelled: impl FnMut() -> bool,
) -> Result<Results, HistoryError> {
    query.validate()?;
    let mut results = Results {
        info: replay.info.clone(),
        query,
        hits: Vec::new(),
        total: 0,
        omitted_evidence: 0,
        retained_bytes: 0,
        uncommitted_tail: replay.uncommitted_tail,
    };
    let mut through = 0;
    let mut last_record = None;
    replay.visit_records_cancellable(
        |record| {
            if matches!(record.payload, Payload::ReplayEnd(_))
                || last_record == Some(record.event_seq)
            {
                return Ok(());
            }
            last_record = Some(record.event_seq);
            let state = record.state()?;
            let accepted = state.observation.accepted_evidence_count;
            if accepted < through {
                return Err(JournalError::Corrupt);
            }
            // Old evidence repeats in many snapshots. Only the first available record is a hit.
            let mut fresh = Vec::new();
            for activity in &state.observation.activities {
                for evidence in &activity.recent_evidence {
                    if evidence.id == 0 || evidence.id > accepted {
                        return Err(JournalError::Corrupt);
                    }
                    if evidence.id > through {
                        fresh.push((activity, evidence));
                    }
                }
            }
            fresh.sort_by_key(|(_, evidence)| evidence.id);
            if fresh.windows(2).any(|pair| pair[0].1.id == pair[1].1.id) {
                return Err(JournalError::Corrupt);
            }
            results.omitted_evidence = results.omitted_evidence.saturating_add(
                accepted
                    .saturating_sub(through)
                    .saturating_sub(fresh.len() as u64),
            );
            for (activity, evidence) in fresh {
                if !results.query.category.accepts(evidence.kind)
                    || !results.query.thread.is_empty()
                        && activity.identity.thread_id.as_ref() != Some(&results.query.thread)
                    || !results.query.turn.is_empty()
                        && activity.identity.turn_id.as_ref() != Some(&results.query.turn)
                {
                    continue;
                }
                let hit = Hit {
                    session_id: record.session_id.clone(),
                    event_seq: record.event_seq,
                    activity_id: activity.activity_id.clone(),
                    identity: activity.identity.clone(),
                    scope: activity.scope,
                    evidence: evidence.clone(),
                };
                if !results.query.text.is_empty()
                    && !hit.searchable_metadata().contains(&results.query.text)
                {
                    continue;
                }
                results.total = results.total.saturating_add(1);
                let bytes = hit.bytes();
                if results.hits.len() < HIT_LIMIT && results.retained_bytes + bytes <= HIT_BYTES {
                    results.hits.push(hit);
                    results.retained_bytes += bytes;
                }
            }
            through = accepted;
            Ok(())
        },
        cancelled,
    )?;
    Ok(results)
}

fn presence(value: Option<&str>) -> &'static str {
    if value.is_some() {
        "present"
    } else {
        "unavailable"
    }
}

struct Job {
    id: u64,
    session: String,
    query: Query,
}

#[derive(Debug)]
pub struct Status {
    pub id: u64,
    pub result: Result<Arc<Results>, HistoryError>,
}

struct Mailbox {
    job: Mutex<Option<Job>>,
    wake: Condvar,
    epoch: AtomicU64,
    stopping: AtomicBool,
}

pub struct SearchHandle {
    mailbox: Arc<Mailbox>,
    pub status: watch::Receiver<Option<Arc<Status>>>,
    worker: Option<thread::JoinHandle<()>>,
}

impl SearchHandle {
    pub fn start(settings: JournalSettings, cwd: std::path::PathBuf) -> Result<Self, HistoryError> {
        let mailbox = Arc::new(Mailbox {
            job: Mutex::new(None),
            wake: Condvar::new(),
            epoch: AtomicU64::new(0),
            stopping: AtomicBool::new(false),
        });
        let owner = mailbox.clone();
        let (updates, status) = watch::channel(None);
        let worker = thread::Builder::new()
            .name("history-search".into())
            .spawn(move || loop {
                let job = {
                    let Ok(mut slot) = owner.job.lock() else {
                        break;
                    };
                    while slot.is_none() && !owner.stopping.load(Ordering::Acquire) {
                        let Ok(next) = owner.wake.wait(slot) else {
                            return;
                        };
                        slot = next;
                    }
                    if owner.stopping.load(Ordering::Acquire) {
                        break;
                    }
                    slot.take().unwrap()
                };
                let cancelled = || {
                    owner.stopping.load(Ordering::Acquire)
                        || owner.epoch.load(Ordering::Acquire) != job.id
                };
                let result = Replay::open_cancellable(&settings, &cwd, &job.session, 0, cancelled)
                    .map_err(HistoryError::from)
                    .and_then(|mut replay| scan(&mut replay, job.query, cancelled))
                    .map(Arc::new);
                if !cancelled() {
                    updates.send_replace(Some(Arc::new(Status { id: job.id, result })));
                }
            })
            .map_err(|_| HistoryError::Closed)?;
        Ok(Self {
            mailbox,
            status,
            worker: Some(worker),
        })
    }

    pub fn submit(&mut self, session: String, query: Query) -> Result<u64, HistoryError> {
        query.validate()?;
        if session.len() > 160 {
            return Err(HistoryError::Journal(JournalError::Identity));
        }
        if self.mailbox.stopping.load(Ordering::Acquire) || self.status.has_changed().is_err() {
            return Err(HistoryError::Closed);
        }
        let mut slot = self.mailbox.job.lock().map_err(|_| HistoryError::Closed)?;
        let id = self
            .mailbox
            .epoch
            .fetch_add(1, Ordering::AcqRel)
            .checked_add(1)
            .ok_or(HistoryError::Closed)?;
        *slot = Some(Job { id, session, query });
        self.mailbox.wake.notify_one();
        Ok(id)
    }

    pub fn cancel(&mut self) {
        self.mailbox.epoch.fetch_add(1, Ordering::AcqRel);
        if let Ok(mut slot) = self.mailbox.job.lock() {
            *slot = None;
        }
    }

    pub async fn response(&mut self, id: u64) -> Result<Arc<Results>, HistoryError> {
        loop {
            if self.mailbox.epoch.load(Ordering::Acquire) != id {
                return Err(HistoryError::Cancelled);
            }
            if let Some(status) = self
                .status
                .borrow()
                .as_ref()
                .filter(|status| status.id == id)
            {
                return status.result.clone();
            }
            self.status
                .changed()
                .await
                .map_err(|_| HistoryError::Closed)?;
        }
    }

    pub async fn shutdown(&mut self) -> Result<(), HistoryError> {
        self.mailbox.stopping.store(true, Ordering::Release);
        self.cancel();
        self.mailbox.wake.notify_one();
        tokio::time::timeout(Duration::from_secs(1), async {
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
        .unwrap_or(Err(HistoryError::Closed))
    }
}

impl Drop for SearchHandle {
    fn drop(&mut self) {
        self.mailbox.stopping.store(true, Ordering::Release);
        self.cancel();
        self.mailbox.wake.notify_one();
    }
}

#[cfg(test)]
mod tests;
