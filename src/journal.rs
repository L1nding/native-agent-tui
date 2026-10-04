//! 持久脱敏投影；写入线程持有文件，回放只读取已提交前缀。
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::sync::{mpsc, watch, OwnedSemaphorePermit, Semaphore};

use crate::observation::{ActivityScope, ExecutionState, ObservationSnapshot};
use crate::scheduler::{
    BlockedReason, DependencyPolicy, ExternalTurn, FailurePolicy, SchedulerSnapshot, TaskAttempt,
    TaskId, TaskKind, TaskSnapshot, TaskState,
};
use crate::state::{CoreSnapshot, SessionPhase};

pub const SCHEMA_VERSION: u32 = 2;
const RECORD_BYTES: usize = 4 * 1024 * 1024;
const QUEUE_BYTES: usize = 64 * 1024 * 1024;
const QUEUE_RECORDS: usize = 128;
const CURSOR_BYTES: usize = 4096;
const STORE_FILES: usize = 16384;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JournalSettings {
    pub root: Option<PathBuf>,
    pub max_bytes: u64,
    pub retention_days: u32,
}

impl Default for JournalSettings {
    fn default() -> Self {
        Self {
            root: None,
            max_bytes: 500 * 1024 * 1024,
            retention_days: 30,
        }
    }
}

impl JournalSettings {
    pub fn directory(&self) -> Result<PathBuf, JournalError> {
        if let Some(root) = &self.root {
            return Ok(root.clone());
        }
        let base = if cfg!(windows) {
            std::env::var_os("LOCALAPPDATA").map(PathBuf::from)
        } else if cfg!(target_os = "macos") {
            std::env::var_os("HOME")
                .map(|home| PathBuf::from(home).join("Library/Application Support"))
        } else {
            std::env::var_os("XDG_DATA_HOME")
                .map(PathBuf::from)
                .filter(|path| path.is_absolute())
                .or_else(|| {
                    std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/share"))
                })
        }
        .ok_or(JournalError::Directory)?;
        Ok(base.join("native-agent-tui").join("journal"))
    }

    /// Return the side-effect outbox path paired with a journal session.
    ///
    /// The outbox intentionally uses a distinct extension so journal replay
    /// never consumes it as a state record, while retention can still account
    /// for and remove it with the owning session.
    pub fn outbox_path(&self, cwd: &Path, session_id: &str) -> Result<PathBuf, JournalError> {
        validate_session(session_id)?;
        let workspace = workspace_id(cwd)?;
        Ok(self
            .directory()?
            .join(format!("{workspace}_{session_id}.outbox")))
    }
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum JournalError {
    #[error("journal data directory is unavailable; select --journal-dir PATH")]
    Directory,
    #[error("journal file operation failed; execution cannot safely continue")]
    Io,
    #[error("journal store is busy")]
    Busy,
    #[error("journal queue exceeded its memory or record budget")]
    Overloaded,
    #[error("journal storage budget is full; active or uncertain sessions are protected")]
    Budget,
    #[error("journal record exceeds 4 MiB")]
    RecordLimit,
    #[error("journal store has too many files")]
    StoreLimit,
    #[error("journal writer closed before confirming persistence")]
    Closed,
    #[error("journal shutdown did not confirm persistence within five seconds")]
    Shutdown,
    #[error("invalid session identity")]
    Identity,
    #[error("session was not found in this workspace")]
    NotFound,
    #[error("session history was removed; no replay range is available")]
    Removed,
    #[error("unsupported journal schema version")]
    Schema,
    #[error("journal has invalid, incomplete, or mismatched committed records")]
    Corrupt,
    #[error("cursor {requested} is unavailable; valid range is 0..={high}")]
    Cursor { requested: u64, high: u64 },
    #[error("could not write replay output")]
    Output,
    #[error("journal read was cancelled")]
    Cancelled,
}
impl From<io::Error> for JournalError {
    fn from(_: io::Error) -> Self {
        Self::Io
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JournalView {
    pub session_id: String,
    pub submitted_seq: u64,
    pub committed_seq: u64,
    pub committed_version: u64,
    pub error: Option<JournalError>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StoredTask {
    pub id: TaskId,
    pub kind: TaskKind,
    pub state: TaskState,
    pub attempt: u64,
    pub parent: Option<TaskId>,
    pub dependencies: Vec<TaskId>,
    pub external: Option<ExternalTurn>,
    pub pause_requested: bool,
    pub cancel_requested: bool,
    pub pending_requests: usize,
    #[serde(default)]
    pub policy: DependencyPolicy,
    #[serde(default)]
    pub failure: FailurePolicy,
    #[serde(default)]
    pub priority: i32,
    #[serde(default)]
    pub blocked_reason: Option<BlockedReason>,
    #[serde(default)]
    pub wait_targets: Vec<TaskAttempt>,
    #[serde(default)]
    pub root_slot_reserved: bool,
    #[serde(default)]
    pub cancellation_epoch: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PersistenceIssue {
    ExecutionUncertain,
    ExecutionFailed,
    CleanupUncertain,
    JournalUnavailable,
    StartupFailed,
    OutputUnavailable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StoredSnapshot {
    pub phase: SessionPhase,
    pub root_start_requests: u64,
    pub workflow_paused: bool,
    pub workflow_stopping: bool,
    #[serde(default)]
    pub ready_roots: Vec<TaskId>,
    #[serde(default)]
    pub active_root: Option<TaskAttempt>,
    #[serde(default)]
    pub root_slots_reserved: usize,
    #[serde(default)]
    pub native_turns_observed: usize,
    #[serde(default)]
    pub scheduler_disconnected: bool,
    pub tasks: Vec<StoredTask>,
    pub observation: ObservationSnapshot,
    pub execution_result: Option<SessionPhase>,
    pub cleanup_confirmed: Option<bool>,
    pub session_closed: bool,
    pub issue: Option<PersistenceIssue>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_headless_action: Option<crate::interactions::HeadlessAction>,
    /// 只保存服务端确认的 token 计数，不保存对话或工具内容。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<StoredUsageSummary>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_budget: Option<StoredTokenBudgetSnapshot>,
    /// 只持久化技能扫描摘要，不保存名称、路径或服务端错误文本。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skills: Option<crate::skills::StoredSkillsSummary>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StoredUsageSummary {
    pub input_tokens: Option<u64>,
    pub cached_input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub reasoning_tokens: Option<u64>,
    pub total_tokens: Option<u64>,
    pub context_window: Option<u64>,
    pub source: StoredUsageSource,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum StoredUsageSource {
    ServerConfirmed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StoredTokenBudgetSnapshot {
    pub confirmed_total_tokens: Option<u64>,
    pub confirmed_complete: bool,
    pub limit: Option<u64>,
    pub stop_triggered: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub per_agent_limit: Option<u64>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub per_agent_stop_triggered: bool,
}

fn is_false(value: &bool) -> bool {
    !*value
}

impl StoredSnapshot {
    pub fn capture(core: &CoreSnapshot) -> Self {
        Self {
            phase: core.phase,
            root_start_requests: core.root_start_requests,
            workflow_paused: core.scheduler.paused,
            workflow_stopping: core.scheduler.stopping,
            ready_roots: core.scheduler.ready_roots.clone(),
            active_root: core.scheduler.active_root,
            root_slots_reserved: core.scheduler.root_slots_reserved,
            native_turns_observed: core.scheduler.native_turns_observed,
            scheduler_disconnected: core.scheduler.disconnected,
            tasks: core
                .scheduler
                .tasks
                .iter()
                .map(|task| StoredTask {
                    id: task.id,
                    kind: task.kind,
                    state: task.state,
                    attempt: task.attempt,
                    parent: task.parent,
                    dependencies: task.dependencies.clone(),
                    external: task.external.clone(),
                    pause_requested: task.pause_requested,
                    cancel_requested: task.cancel_requested,
                    pending_requests: task.pending_requests,
                    policy: task.policy,
                    failure: task.failure,
                    priority: task.priority,
                    blocked_reason: task.blocked_reason.clone(),
                    wait_targets: task.wait_targets.clone(),
                    root_slot_reserved: task.root_slot_reserved,
                    cancellation_epoch: task.cancellation_epoch,
                })
                .collect(),
            observation: core.observation.clone(),
            execution_result: None,
            cleanup_confirmed: None,
            session_closed: false,
            last_headless_action: core.last_headless_action.clone(),
            usage: (core.usage.source == crate::state::FactSource::ServerConfirmed).then_some(
                StoredUsageSummary {
                    input_tokens: core.usage.input_tokens,
                    cached_input_tokens: core.usage.cached_input_tokens,
                    output_tokens: core.usage.output_tokens,
                    reasoning_tokens: core.usage.reasoning_tokens,
                    total_tokens: core.usage.total_tokens,
                    context_window: core.usage.context_window,
                    source: StoredUsageSource::ServerConfirmed,
                },
            ),
            token_budget: {
                let budget = core.token_budget;
                (budget.confirmed_total_tokens.is_some()
                    || budget.confirmed_complete
                    || budget.limit.is_some()
                    || budget.stop_triggered
                    || budget.per_agent_limit.is_some()
                    || budget.per_agent_stop_triggered)
                    .then_some(StoredTokenBudgetSnapshot {
                        confirmed_total_tokens: budget.confirmed_total_tokens,
                        confirmed_complete: budget.confirmed_complete,
                        limit: budget.limit,
                        stop_triggered: budget.stop_triggered,
                        per_agent_limit: budget.per_agent_limit,
                        per_agent_stop_triggered: budget.per_agent_stop_triggered,
                    })
            },
            skills: (core.skills.freshness != crate::skills::SkillFreshness::Unknown)
                .then_some(crate::skills::StoredSkillsSummary::from(&core.skills)),
            issue: match (&core.last_headless_action, core.phase) {
                (Some(crate::interactions::HeadlessAction::StopForOutput), _) => {
                    Some(PersistenceIssue::OutputUnavailable)
                }
                (_, SessionPhase::Unknown | SessionPhase::Disconnected) => {
                    Some(PersistenceIssue::ExecutionUncertain)
                }
                (_, SessionPhase::Failed) => Some(PersistenceIssue::ExecutionFailed),
                _ => None,
            },
        }
    }

    pub fn close(&mut self, outcome: SessionPhase, cleanup_confirmed: bool) {
        let outcome = crate::state::workflow_outcome(
            outcome,
            self.tasks
                .iter()
                .filter(|task| task.kind == TaskKind::RootTurn)
                .map(|task| task.state),
        );
        self.session_closed = true;
        self.cleanup_confirmed = Some(cleanup_confirmed);
        self.execution_result = Some(
            if matches!(
                outcome,
                SessionPhase::Completed
                    | SessionPhase::Failed
                    | SessionPhase::Interrupted
                    | SessionPhase::Ready
            ) {
                outcome
            } else {
                SessionPhase::Unknown
            },
        );
        if !cleanup_confirmed {
            self.issue = Some(PersistenceIssue::CleanupUncertain);
        } else if self.execution_result == Some(SessionPhase::Unknown) && self.issue.is_none() {
            self.issue = Some(PersistenceIssue::ExecutionUncertain);
        } else if self.execution_result == Some(SessionPhase::Failed) && self.issue.is_none() {
            self.issue = Some(PersistenceIssue::ExecutionFailed);
        }
    }

    /// 判断快照是否仍可能包含未确认的执行结果。
    pub fn needs_recovery(&self) -> bool {
        !self.session_closed
            || self.cleanup_confirmed != Some(true)
            || matches!(
                self.issue,
                Some(
                    PersistenceIssue::JournalUnavailable
                        | PersistenceIssue::OutputUnavailable
                        | PersistenceIssue::CleanupUncertain
                        | PersistenceIssue::ExecutionUncertain
                )
            )
            || !matches!(
                self.execution_result,
                Some(
                    SessionPhase::Completed
                        | SessionPhase::Failed
                        | SessionPhase::Interrupted
                        | SessionPhase::Ready
                )
            )
            || self.observation.activities.iter().any(|activity| {
                matches!(
                    activity.execution_state,
                    ExecutionState::Running
                        | ExecutionState::Starting
                        | ExecutionState::Waiting
                        | ExecutionState::Unknown
                )
            })
    }

    /// 构造脱敏的只读恢复摘要。
    ///
    /// StoredSnapshot 不含任务正文；存在非终态任务时，重新规划前必须取得用户输入。
    /// 历史回放不会恢复副作用，can_resume 固定为 false。
    pub fn recovery_summary(
        &self,
        session_id: impl Into<String>,
        committed_seq: u64,
    ) -> RecoverySummary {
        RecoverySummary::from_snapshot(session_id.into(), committed_seq, self)
    }

    /// Converts the persisted, redacted scheduler projection into the
    /// scheduler's read-only restore seam. Task bodies remain unavailable.
    pub fn scheduler_snapshot(&self) -> SchedulerSnapshot {
        let tasks: Vec<_> = self
            .tasks
            .iter()
            .map(|task| TaskSnapshot {
                id: task.id,
                kind: task.kind,
                state: task.state,
                title: format!("task #{}", task.id.0),
                parent: task.parent,
                attempt: task.attempt,
                dependencies: task.dependencies.clone(),
                policy: task.policy,
                failure: task.failure,
                priority: task.priority,
                blocked_reason: task.blocked_reason.clone(),
                external: task.external.clone(),
                pause_requested: task.pause_requested,
                cancel_requested: task.cancel_requested,
                pending_requests: task.pending_requests,
                wait_targets: task.wait_targets.clone(),
                root_slot_reserved: task.root_slot_reserved,
                cancellation_epoch: task.cancellation_epoch,
            })
            .collect();
        let ready_roots = if self.ready_roots.is_empty() {
            tasks
                .iter()
                .filter(|task| task.kind == TaskKind::RootTurn && task.state == TaskState::Ready)
                .map(|task| task.id)
                .collect()
        } else {
            self.ready_roots.clone()
        };
        let computed_root_slots = tasks.iter().filter(|task| task.root_slot_reserved).count();
        let computed_native_turns = tasks
            .iter()
            .filter(|task| {
                task.kind == TaskKind::NativeChild && task.external.is_some() && task.state.active()
            })
            .count();
        SchedulerSnapshot {
            tasks,
            ready_roots,
            active_root: self.active_root,
            paused: self.workflow_paused,
            stopping: self.workflow_stopping,
            disconnected: self.scheduler_disconnected,
            queued_roots: self
                .tasks
                .iter()
                .filter(|task| {
                    task.kind == TaskKind::RootTurn
                        && matches!(
                            task.state,
                            TaskState::Queued
                                | TaskState::Ready
                                | TaskState::Paused
                                | TaskState::Blocked
                        )
                })
                .count(),
            root_slots_reserved: if self.root_slots_reserved == 0 {
                computed_root_slots
            } else {
                self.root_slots_reserved
            },
            native_turns_observed: if self.native_turns_observed == 0 {
                computed_native_turns
            } else {
                self.native_turns_observed
            },
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecoveryTaskClass {
    Active,
    Unknown,
    Queued,
    Blocked,
    Terminal,
}

impl RecoveryTaskClass {
    fn classify(state: TaskState) -> Self {
        match state {
            TaskState::Starting
            | TaskState::Running
            | TaskState::WaitingChildren
            | TaskState::WaitingApproval
            | TaskState::Cancelling => Self::Active,
            TaskState::Unknown => Self::Unknown,
            TaskState::Queued | TaskState::Ready | TaskState::Paused => Self::Queued,
            TaskState::Blocked => Self::Blocked,
            TaskState::Succeeded | TaskState::Failed | TaskState::Cancelled => Self::Terminal,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecoveryTaskAction {
    UnknownAfterRestart,
    NeedsInput,
    ResolveBlock,
    Terminal,
}

impl RecoveryTaskAction {
    fn from_class(class: RecoveryTaskClass) -> Self {
        match class {
            RecoveryTaskClass::Active | RecoveryTaskClass::Unknown => Self::UnknownAfterRestart,
            RecoveryTaskClass::Queued => Self::NeedsInput,
            RecoveryTaskClass::Blocked => Self::ResolveBlock,
            RecoveryTaskClass::Terminal => Self::Terminal,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecoveryTaskSummary {
    pub id: TaskId,
    pub kind: TaskKind,
    pub state: TaskState,
    pub attempt: u64,
    pub parent: Option<TaskId>,
    pub dependencies: Vec<TaskId>,
    pub external: Option<ExternalTurn>,
    pub pending_requests: usize,
    pub class: RecoveryTaskClass,
    pub action: RecoveryTaskAction,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecoverySummary {
    pub session_id: String,
    pub committed_seq: u64,
    pub uncommitted_tail: bool,
    pub session_closed: bool,
    pub execution_result: Option<SessionPhase>,
    pub needs_recovery: bool,
    pub requires_input: bool,
    pub can_resume: bool,
    pub tasks: Vec<RecoveryTaskSummary>,
}

impl RecoverySummary {
    fn from_snapshot(session_id: String, committed_seq: u64, snapshot: &StoredSnapshot) -> Self {
        let tasks = snapshot
            .tasks
            .iter()
            .map(|task| {
                let class = RecoveryTaskClass::classify(task.state);
                RecoveryTaskSummary {
                    id: task.id,
                    kind: task.kind,
                    state: task.state,
                    attempt: task.attempt,
                    parent: task.parent,
                    dependencies: task.dependencies.clone(),
                    external: task.external.clone(),
                    pending_requests: task.pending_requests,
                    class,
                    action: RecoveryTaskAction::from_class(class),
                }
            })
            .collect::<Vec<_>>();
        let requires_input = snapshot.needs_recovery()
            && tasks
                .iter()
                .any(|task| task.class != RecoveryTaskClass::Terminal);
        Self {
            session_id,
            committed_seq,
            uncommitted_tail: false,
            session_closed: snapshot.session_closed,
            execution_result: snapshot.execution_result,
            needs_recovery: snapshot.needs_recovery(),
            requires_input,
            can_resume: false,
            tasks,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecordKind {
    Snapshot,
    State,
    ReplayEnd,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReplayEnd {
    pub high_watermark: u64,
    pub live_attached: bool,
    pub session_closed: bool,
    pub execution_result: Option<SessionPhase>,
    pub needs_recovery: bool,
    pub uncommitted_tail: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Payload {
    Snapshot(Box<StoredSnapshot>),
    ReplayEnd(ReplayEnd),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Record {
    pub schema_version: u32,
    pub kind: RecordKind,
    pub session_id: String,
    pub attempt_id: Option<TaskAttempt>,
    pub event_seq: u64,
    pub snapshot_version: u64,
    pub recorded_at: Option<u64>,
    pub payload: Payload,
    /// JSONL 输出时标记为 committed；journal 文件本身不保存这个瞬时投影。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub persistence: Option<crate::state::PersistenceState>,
    /// 回放保留历史时钟值；它们不能当作当前进程的新鲜度。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub historical: Option<bool>,
}

impl Record {
    fn snapshot(sequence: u64, snapshot: StoredSnapshot) -> Self {
        let attempt_id = snapshot
            .observation
            .activities
            .iter()
            .find(|activity| {
                activity.scope == ActivityScope::Turn && activity.identity.agent_id == "root"
            })
            .and_then(|activity| {
                Some(TaskAttempt {
                    task: activity.identity.task_id?,
                    attempt: activity.identity.attempt_id?,
                })
            });
        Self {
            schema_version: SCHEMA_VERSION,
            kind: if sequence == 0 {
                RecordKind::Snapshot
            } else {
                RecordKind::State
            },
            session_id: snapshot.observation.session_id.clone(),
            attempt_id,
            event_seq: sequence,
            snapshot_version: snapshot.observation.snapshot_version,
            recorded_at: wall_ms(),
            payload: Payload::Snapshot(Box::new(snapshot)),
            persistence: None,
            historical: None,
        }
    }
    pub(crate) fn state(&self) -> Result<&StoredSnapshot, JournalError> {
        match &self.payload {
            Payload::Snapshot(snapshot) => Ok(snapshot),
            _ => Err(JournalError::Corrupt),
        }
    }
    fn encode(&self) -> Result<Vec<u8>, JournalError> {
        let mut frame = serde_json::to_vec(self).map_err(|_| JournalError::Corrupt)?;
        if frame.len() >= RECORD_BYTES {
            return Err(JournalError::RecordLimit);
        }
        frame.push(b'\n');
        Ok(frame)
    }

    pub(crate) fn into_history(mut self) -> Self {
        self.historical = Some(true);
        if let Payload::Snapshot(snapshot) = &mut self.payload {
            for activity in &mut snapshot.observation.activities {
                if activity.freshness == crate::observation::Freshness::Current {
                    activity.freshness = crate::observation::Freshness::Unknown;
                }
            }
        }
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionInfo {
    pub schema_version: u32,
    pub workspace_id: String,
    pub session_id: String,
    pub committed_seq: u64,
    pub committed_bytes: u64,
    pub snapshot_version: u64,
    pub recorded_at: Option<u64>,
    pub session_closed: bool,
    pub needs_recovery: bool,
    pub execution_result: Option<SessionPhase>,
}

#[derive(Debug, Clone)]
pub struct JournalStatus {
    pub committed_seq: u64,
    pub committed_version: u64,
    pub error: Option<JournalError>,
    pub closed: bool,
}

// Queue only encoded payloads and small commit metadata, not a second parsed snapshot.
struct Commit {
    sequence: u64,
    version: u64,
    recorded_at: Option<u64>,
    session_closed: bool,
    needs_recovery: bool,
    execution_result: Option<SessionPhase>,
}

impl Commit {
    fn capture(record: &Record) -> Result<Self, JournalError> {
        let state = record.state()?;
        Ok(Self {
            sequence: record.event_seq,
            version: record.snapshot_version,
            recorded_at: record.recorded_at,
            session_closed: state.session_closed,
            needs_recovery: state.needs_recovery(),
            execution_result: state.execution_result,
        })
    }
}

struct Queued {
    commit: Commit,
    bytes: Vec<u8>,
    _budget: OwnedSemaphorePermit,
}

pub struct Journal {
    session_id: String,
    next_seq: u64,
    sender: Option<mpsc::Sender<Queued>>,
    budget: Arc<Semaphore>,
    pub status: watch::Receiver<JournalStatus>,
    worker: Option<thread::JoinHandle<()>>,
}

impl Journal {
    pub fn open(
        settings: &JournalSettings,
        cwd: &Path,
        initial: StoredSnapshot,
    ) -> Result<Self, JournalError> {
        let record = Record::snapshot(0, initial);
        let mut store = Store::create(settings.clone(), cwd, &record)?;
        store.persist(&record, &record.encode()?)?;
        let (sender, mut records) = mpsc::channel::<Queued>(QUEUE_RECORDS);
        let (updates, status) = watch::channel(JournalStatus {
            committed_seq: 0,
            committed_version: record.snapshot_version,
            error: None,
            closed: false,
        });
        let session_id = record.session_id.clone();
        let worker = thread::Builder::new()
            .name("journal-writer".into())
            .spawn(move || {
                let mut status = updates.borrow().clone();
                while let Some(record) = records.blocking_recv() {
                    if let Err(error) = store.persist_commit(&record.commit, &record.bytes) {
                        status.error = Some(error);
                        break;
                    }
                    status.committed_seq = record.commit.sequence;
                    status.committed_version = record.commit.version;
                    updates.send_replace(status.clone());
                }
                drop(records);
                drop(store);
                status.closed = true;
                updates.send_replace(status);
            })?;
        Ok(Self {
            session_id,
            next_seq: 1,
            sender: Some(sender),
            budget: Arc::new(Semaphore::new(QUEUE_BYTES)),
            status,
            worker: Some(worker),
        })
    }

    pub fn view(&self) -> JournalView {
        let status = self.status.borrow();
        JournalView {
            session_id: self.session_id.clone(),
            submitted_seq: self.next_seq - 1,
            committed_seq: status.committed_seq,
            committed_version: status.committed_version,
            error: status.error.clone(),
        }
    }

    fn prepare(&self, snapshot: StoredSnapshot) -> Result<(Commit, Vec<u8>), JournalError> {
        let status = self.status.borrow();
        if let Some(error) = &status.error {
            return Err(error.clone());
        }
        if status.closed {
            return Err(JournalError::Closed);
        }
        drop(status);
        if snapshot.observation.session_id != self.session_id {
            return Err(JournalError::Identity);
        }
        let record = Record::snapshot(self.next_seq, snapshot);
        let bytes = record.encode()?;
        Ok((Commit::capture(&record)?, bytes))
    }

    fn queued(&self, snapshot: StoredSnapshot) -> Result<Queued, JournalError> {
        let (commit, bytes) = self.prepare(snapshot)?;
        let budget = self
            .budget
            .clone()
            .try_acquire_many_owned(bytes.len() as u32)
            .map_err(|_| JournalError::Overloaded)?;
        Ok(Queued {
            commit,
            bytes,
            _budget: budget,
        })
    }

    pub fn append(&mut self, snapshot: StoredSnapshot) -> Result<(), JournalError> {
        let queued = self.queued(snapshot)?;
        self.sender
            .as_ref()
            .ok_or(JournalError::Closed)?
            .try_send(queued)
            .map_err(|error| match error {
                mpsc::error::TrySendError::Full(_) => JournalError::Overloaded,
                _ => JournalError::Closed,
            })?;
        self.next_seq += 1;
        Ok(())
    }

    pub async fn finish(
        mut self,
        final_snapshot: StoredSnapshot,
    ) -> Result<JournalView, JournalError> {
        let result = tokio::time::timeout(Duration::from_secs(5), async {
            let (commit, bytes) = self.prepare(final_snapshot)?;
            // Shutdown may drain a full queue, but it still obeys the five-second deadline.
            let budget = self
                .budget
                .clone()
                .acquire_many_owned(bytes.len() as u32)
                .await
                .map_err(|_| JournalError::Closed)?;
            let queued = Queued {
                commit,
                bytes,
                _budget: budget,
            };
            self.sender
                .as_ref()
                .ok_or(JournalError::Closed)?
                .send(queued)
                .await
                .map_err(|_| JournalError::Closed)?;
            self.next_seq += 1;
            self.sender.take();
            while !self.status.borrow().closed {
                self.status
                    .changed()
                    .await
                    .map_err(|_| JournalError::Closed)?;
            }
            if let Some(error) = self.status.borrow().error.clone() {
                return Err(error);
            }
            while self
                .worker
                .as_ref()
                .is_some_and(|worker| !worker.is_finished())
            {
                tokio::task::yield_now().await;
            }
            if let Some(worker) = self.worker.take() {
                worker.join().map_err(|_| JournalError::Closed)?;
            }
            Ok(self.view())
        })
        .await
        .map_err(|_| JournalError::Shutdown)?;
        self.sender.take();
        result
    }
}

struct Store {
    settings: JournalSettings,
    root: PathBuf,
    stem: String,
    log: File,
    _lease: File,
    cursor: SessionInfo,
}

impl Store {
    fn create(
        settings: JournalSettings,
        cwd: &Path,
        record: &Record,
    ) -> Result<Self, JournalError> {
        if settings.max_bytes == 0 || settings.retention_days == 0 {
            return Err(JournalError::Budget);
        }
        validate_session(&record.session_id)?;
        let root = settings.directory()?;
        fs::create_dir_all(&root)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&root, fs::Permissions::from_mode(0o700))?;
        }
        let root = root.canonicalize()?;
        let workspace_id = workspace_id(cwd)?;
        let stem = format!("{workspace_id}_{}", record.session_id);
        let lock = catalog_lock(&root)?;
        lock.try_lock().map_err(|_| JournalError::Busy)?;
        let lease = private_new(&root.join(format!("{stem}.lease")))?;
        lease.try_lock().map_err(|_| JournalError::Busy)?;
        let log = private_new(&root.join(format!("{stem}.jsonl")))?;
        drop(lock);
        Ok(Self {
            settings,
            root,
            stem,
            log,
            _lease: lease,
            cursor: SessionInfo {
                schema_version: SCHEMA_VERSION,
                workspace_id,
                session_id: record.session_id.clone(),
                committed_seq: 0,
                committed_bytes: 0,
                snapshot_version: 0,
                recorded_at: record.recorded_at,
                session_closed: false,
                needs_recovery: true,
                execution_result: None,
            },
        })
    }

    fn persist(&mut self, record: &Record, bytes: &[u8]) -> Result<(), JournalError> {
        self.persist_commit(&Commit::capture(record)?, bytes)
    }

    fn persist_commit(&mut self, commit: &Commit, bytes: &[u8]) -> Result<(), JournalError> {
        let lock = catalog_lock(&self.root)?;
        // Another process must not leave this writer blocked indefinitely.
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        loop {
            match lock.try_lock() {
                Ok(()) => break,
                Err(std::fs::TryLockError::WouldBlock) if std::time::Instant::now() < deadline => {
                    thread::sleep(Duration::from_millis(10));
                }
                Err(std::fs::TryLockError::WouldBlock) => return Err(JournalError::Busy),
                Err(std::fs::TryLockError::Error(_)) => return Err(JournalError::Io),
            }
        }
        reclaim(
            &self.root,
            &self.settings,
            bytes.len() as u64 + (CURSOR_BYTES * 2) as u64,
        )?;
        self.log.write_all(bytes)?;
        self.log.sync_all()?;
        let mut cursor = self.cursor.clone();
        cursor.committed_seq = commit.sequence;
        cursor.committed_bytes += bytes.len() as u64;
        cursor.snapshot_version = commit.version;
        cursor.recorded_at = commit.recorded_at;
        cursor.session_closed = commit.session_closed;
        cursor.needs_recovery = commit.needs_recovery;
        cursor.execution_result = commit.execution_result;
        // 日志先同步，再发布游标；完整但未提交的尾部不能被当作历史事实。
        atomic_metadata(&self.root.join(format!("{}.cursor", self.stem)), &cursor)?;
        self.cursor = cursor;
        drop(lock);
        Ok(())
    }
}

fn private_new(path: &Path) -> Result<File, JournalError> {
    let mut options = OpenOptions::new();
    options.read(true).write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    Ok(options.open(path)?)
}

fn catalog_lock(root: &Path) -> Result<File, JournalError> {
    let path = root.join(".catalog.lock");
    if path.exists() && !fs::symlink_metadata(&path)?.is_file() {
        return Err(JournalError::Corrupt);
    }
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    Ok(options.open(path)?)
}

fn atomic_metadata(path: &Path, info: &SessionInfo) -> Result<(), JournalError> {
    let bytes = serde_json::to_vec(info).map_err(|_| JournalError::Corrupt)?;
    if bytes.len() > CURSOR_BYTES {
        return Err(JournalError::Corrupt);
    }
    let temporary = path.with_extension(format!(
        "{}.new",
        path.extension().unwrap().to_string_lossy()
    ));
    let mut file = private_new(&temporary)?;
    file.write_all(&bytes)?;
    file.sync_all()?;
    drop(file);
    fs::rename(&temporary, path)?;
    #[cfg(unix)]
    {
        File::open(path.parent().unwrap())?.sync_all()?;
    }
    Ok(())
}

fn metadata(path: &Path) -> Result<SessionInfo, JournalError> {
    if !fs::symlink_metadata(path)?.is_file() {
        return Err(JournalError::Corrupt);
    }
    let mut bytes = Vec::new();
    File::open(path)?
        .take((CURSOR_BYTES + 1) as u64)
        .read_to_end(&mut bytes)?;
    if bytes.len() > CURSOR_BYTES {
        return Err(JournalError::Corrupt);
    }
    let value: serde_json::Value =
        serde_json::from_slice(&bytes).map_err(|_| JournalError::Corrupt)?;
    if !supported_schema(value["schema_version"].as_u64()) {
        return Err(JournalError::Schema);
    }
    let info: SessionInfo = serde_json::from_value(value).map_err(|_| JournalError::Corrupt)?;
    validate_session(&info.session_id)?;
    if info.workspace_id.len() != 32
        || !info
            .workspace_id
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(JournalError::Corrupt);
    }
    if path.file_stem().and_then(|stem| stem.to_str())
        != Some(format!("{}_{}", info.workspace_id, info.session_id).as_str())
    {
        return Err(JournalError::Corrupt);
    }
    Ok(info)
}

fn entries(root: &Path) -> Result<Vec<PathBuf>, JournalError> {
    let mut entries = Vec::new();
    for entry in fs::read_dir(root)? {
        if entries.len() >= STORE_FILES {
            return Err(JournalError::StoreLimit);
        }
        entries.push(entry?.path());
    }
    Ok(entries)
}

fn owned(path: &Path) -> bool {
    path.file_stem()
        .and_then(|name| name.to_str())
        .and_then(|name| name.split_once('_'))
        .is_some_and(|(workspace, session)| {
            workspace.len() == 32
                && workspace.bytes().all(|byte| byte.is_ascii_hexdigit())
                && validate_session(session).is_ok()
        })
}

fn reclaim(root: &Path, settings: &JournalSettings, required: u64) -> Result<(), JournalError> {
    let entries = entries(root)?;
    let mut bytes = 0u64;
    let mut candidates = Vec::new();
    for path in &entries {
        if !owned(path) {
            continue;
        }
        if !fs::symlink_metadata(path)?.is_file() {
            return Err(JournalError::Corrupt);
        }
        bytes = bytes.saturating_add(fs::metadata(path)?.len());
        if path.extension().and_then(|extension| extension.to_str()) == Some("cursor") {
            let info = metadata(path)?;
            if info.session_closed && !info.needs_recovery {
                candidates.push((info.recorded_at.unwrap_or(u64::MAX), path.clone(), info));
            }
        }
    }
    candidates.sort_by_key(|(time, _, _)| *time);
    let expiry =
        wall_ms().and_then(|now| now.checked_sub(u64::from(settings.retention_days) * 86400000));
    for (time, path, info) in candidates {
        if bytes.saturating_add(required) <= settings.max_bytes
            && expiry.is_none_or(|expiry| time >= expiry)
        {
            continue;
        }
        let lease_path = path.with_extension("lease");
        let lease = match OpenOptions::new().read(true).write(true).open(&lease_path) {
            Ok(lease) => lease,
            Err(_) => continue,
        };
        if lease.try_lock().is_err() {
            continue;
        }
        let removed = path.with_extension("removed");
        if !removed.exists() {
            atomic_metadata(&removed, &info)?;
        }
        drop(lease);
        for extension in ["jsonl", "cursor", "lease", "outbox"] {
            let path = path.with_extension(extension);
            if path.exists() {
                let length = fs::metadata(&path)?.len();
                fs::remove_file(path)?;
                bytes = bytes.saturating_sub(length);
            }
        }
        bytes = bytes.saturating_add(fs::metadata(removed)?.len());
    }
    if bytes.saturating_add(required) > settings.max_bytes {
        Err(JournalError::Budget)
    } else {
        Ok(())
    }
}

fn validate_session(id: &str) -> Result<(), JournalError> {
    if id.is_empty()
        || id.len() > 160
        || !id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
    {
        Err(JournalError::Identity)
    } else {
        Ok(())
    }
}

fn workspace_id(cwd: &Path) -> Result<String, JournalError> {
    let cwd = cwd.canonicalize()?;
    let bytes = cwd.as_os_str().as_encoded_bytes();
    let hash = |seed: u64| {
        bytes.iter().fold(seed, |hash, byte| {
            (hash ^ u64::from(*byte)).wrapping_mul(0x100000001b3)
        })
    };
    Ok(format!(
        "{:016x}{:016x}",
        hash(0xcbf29ce484222325),
        hash(0x84222325cbf29ce4)
    ))
}

fn wall_ms() -> Option<u64> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|time| time.as_millis().try_into().ok())
}

pub fn sessions(settings: &JournalSettings, cwd: &Path) -> Result<Vec<SessionInfo>, JournalError> {
    let root = settings.directory()?;
    if !root.exists() {
        return Ok(Vec::new());
    }
    let workspace = workspace_id(cwd)?;
    let mut sessions = Vec::new();
    for path in entries(&root)? {
        if owned(&path)
            && path.extension().and_then(|extension| extension.to_str()) == Some("cursor")
        {
            let info = metadata(&path)?;
            if info.workspace_id == workspace && !path.with_extension("removed").exists() {
                sessions.push(info);
            }
        }
    }
    sessions.sort_by_key(|session| std::cmp::Reverse(session.recorded_at));
    Ok(sessions)
}

pub struct Replay {
    reader: BufReader<io::Take<File>>,
    pub info: SessionInfo,
    pub baseline: Record,
    pub latest: Record,
    pub uncommitted_tail: bool,
    since: u64,
}

impl Replay {
    pub fn open(
        settings: &JournalSettings,
        cwd: &Path,
        session_id: &str,
        since: u64,
    ) -> Result<Self, JournalError> {
        Self::open_cancellable(settings, cwd, session_id, since, || false)
    }

    pub(crate) fn open_cancellable(
        settings: &JournalSettings,
        cwd: &Path,
        session_id: &str,
        since: u64,
        mut cancelled: impl FnMut() -> bool,
    ) -> Result<Self, JournalError> {
        if cancelled() {
            return Err(JournalError::Cancelled);
        }
        validate_session(session_id)?;
        let root = settings.directory()?;
        let stem = format!("{}_{}", workspace_id(cwd)?, session_id);
        if root.join(format!("{stem}.removed")).exists() {
            return Err(JournalError::Removed);
        }
        let cursor = root.join(format!("{stem}.cursor"));
        if !cursor.exists() {
            return Err(JournalError::NotFound);
        }
        let info = metadata(&cursor)?;
        if since > info.committed_seq {
            return Err(JournalError::Cursor {
                requested: since,
                high: info.committed_seq,
            });
        }
        let path = root.join(format!("{stem}.jsonl"));
        if !fs::symlink_metadata(&path)?.is_file() {
            return Err(JournalError::Corrupt);
        }
        let file = File::open(path)?;
        let length = file.metadata()?.len();
        if length < info.committed_bytes || info.committed_bytes == 0 {
            return Err(JournalError::Corrupt);
        }
        let mut reader = BufReader::new(file.take(info.committed_bytes));
        let mut sequence = 0u64;
        let mut previous_version = 0u64;
        let mut baseline = None;
        let mut latest = None;
        loop {
            if cancelled() {
                return Err(JournalError::Cancelled);
            }
            let Some(record) = read_record(&mut reader)? else {
                break;
            };
            validate_record(&record, &info, sequence)?;
            if sequence > 0 && record.snapshot_version < previous_version {
                return Err(JournalError::Corrupt);
            }
            previous_version = record.snapshot_version;
            if record.event_seq == since {
                baseline = Some(record.clone());
            }
            latest = Some(record);
            sequence = sequence.checked_add(1).ok_or(JournalError::Corrupt)?;
        }
        let latest = latest.ok_or(JournalError::Corrupt)?;
        if latest.event_seq != info.committed_seq
            || latest.snapshot_version != info.snapshot_version
            || latest.state()?.session_closed != info.session_closed
            || latest.state()?.execution_result != info.execution_result
            || latest.state()?.needs_recovery() != info.needs_recovery
        {
            return Err(JournalError::Corrupt);
        }
        let baseline = baseline.ok_or(JournalError::Corrupt)?;
        reader.get_mut().get_mut().seek(SeekFrom::Start(0))?;
        let file = reader.into_inner().into_inner();
        Ok(Self {
            reader: BufReader::new(file.take(info.committed_bytes)),
            uncommitted_tail: length > info.committed_bytes,
            info,
            baseline,
            latest,
            since,
        })
    }

    pub(crate) fn visit_records(
        &mut self,
        visit: impl FnMut(Record) -> Result<(), JournalError>,
    ) -> Result<(), JournalError> {
        self.visit_records_cancellable(visit, || false)
    }

    pub(crate) fn visit_records_cancellable(
        &mut self,
        mut visit: impl FnMut(Record) -> Result<(), JournalError>,
        mut cancelled: impl FnMut() -> bool,
    ) -> Result<(), JournalError> {
        if cancelled() {
            return Err(JournalError::Cancelled);
        }
        self.reader.get_mut().get_mut().seek(SeekFrom::Start(0))?;
        self.reader = BufReader::new(
            self.reader
                .get_mut()
                .get_mut()
                .try_clone()?
                .take(self.info.committed_bytes),
        );
        let mut baseline = self.baseline.clone();
        baseline.kind = RecordKind::Snapshot;
        baseline = baseline.into_history();
        visit(baseline)?;
        let mut sequence = 0;
        let mut previous_version = 0;
        loop {
            if cancelled() {
                return Err(JournalError::Cancelled);
            }
            let Some(mut record) = read_record(&mut self.reader)? else {
                break;
            };
            validate_record(&record, &self.info, sequence)?;
            if record.snapshot_version < previous_version
                || record.event_seq == self.since && record != self.baseline
                || record.event_seq == self.info.committed_seq && record != self.latest
            {
                return Err(JournalError::Corrupt);
            }
            previous_version = record.snapshot_version;
            sequence = sequence.checked_add(1).ok_or(JournalError::Corrupt)?;
            if record.event_seq > self.since {
                record = record.into_history();
                visit(record)?;
            }
        }
        if sequence
            != self
                .info
                .committed_seq
                .checked_add(1)
                .ok_or(JournalError::Corrupt)?
        {
            return Err(JournalError::Corrupt);
        }
        if cancelled() {
            return Err(JournalError::Cancelled);
        }
        let mut latest = self.latest.clone();
        latest.kind = RecordKind::Snapshot;
        latest = latest.into_history();
        visit(latest.clone())?;
        let end = Record {
            kind: RecordKind::ReplayEnd,
            payload: Payload::ReplayEnd(ReplayEnd {
                high_watermark: self.info.committed_seq,
                live_attached: false,
                session_closed: self.info.session_closed,
                execution_result: self.info.execution_result,
                needs_recovery: self.info.needs_recovery,
                uncommitted_tail: self.uncommitted_tail,
            }),
            ..latest
        };
        visit(end)
    }

    pub fn write_jsonl(mut self, writer: &mut impl Write) -> Result<(), JournalError> {
        self.visit_records(|record| write_record(writer, &record))?;
        writer.flush().map_err(|_| JournalError::Output)
    }

    pub fn latest_state(&self) -> &StoredSnapshot {
        self.latest.state().expect("validated replay")
    }

    /// 返回已提交最新快照的脱敏恢复结论。
    pub fn recovery_summary(&self) -> RecoverySummary {
        let mut summary = self
            .latest_state()
            .recovery_summary(self.info.session_id.clone(), self.info.committed_seq);
        summary.uncommitted_tail = self.uncommitted_tail;
        summary
    }
}

fn validate_record(record: &Record, info: &SessionInfo, sequence: u64) -> Result<(), JournalError> {
    if !supported_schema(Some(u64::from(record.schema_version))) {
        return Err(JournalError::Schema);
    }
    if record.schema_version != info.schema_version
        || record.event_seq != sequence
        || record.event_seq > info.committed_seq
        || record.session_id != info.session_id
        || record.state()?.observation.session_id != info.session_id
        || record.snapshot_version != record.state()?.observation.snapshot_version
        || record.historical.is_some()
        || record.kind
            != if sequence == 0 {
                RecordKind::Snapshot
            } else {
                RecordKind::State
            }
    {
        return Err(JournalError::Corrupt);
    }
    Ok(())
}

fn read_record(reader: &mut impl BufRead) -> Result<Option<Record>, JournalError> {
    let mut bytes = Vec::new();
    loop {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            return if bytes.is_empty() {
                Ok(None)
            } else {
                Err(JournalError::Corrupt)
            };
        }
        let newline = available.iter().position(|byte| *byte == b'\n');
        let length = newline.map_or(available.len(), |index| index + 1);
        if bytes.len() + length > RECORD_BYTES {
            return Err(JournalError::RecordLimit);
        }
        bytes.extend_from_slice(&available[..length]);
        reader.consume(length);
        if newline.is_some() {
            break;
        }
    }
    let version: serde_json::Value =
        serde_json::from_slice(&bytes).map_err(|_| JournalError::Corrupt)?;
    if !supported_schema(version["schema_version"].as_u64()) {
        return Err(JournalError::Schema);
    }
    serde_json::from_value(version)
        .map(Some)
        .map_err(|_| JournalError::Corrupt)
}

fn supported_schema(version: Option<u64>) -> bool {
    matches!(version, Some(1)) || version == Some(u64::from(SCHEMA_VERSION))
}

pub(crate) fn write_record(writer: &mut impl Write, record: &Record) -> Result<(), JournalError> {
    let mut output = record.clone();
    output.persistence = Some(crate::state::PersistenceState::Committed);
    writer
        .write_all(&output.encode()?)
        .map_err(|_| JournalError::Output)
}

/// Sequential reader for one live session. Only committed bytes are exposed.
/// It holds a single record and never waits on the execution owner.
pub(crate) struct CommittedReader {
    cursor: PathBuf,
    file: File,
    offset: u64,
    next_sequence: u64,
    latest: Option<Record>,
}

impl CommittedReader {
    pub(crate) fn open(
        settings: &JournalSettings,
        cwd: &Path,
        session: &str,
    ) -> Result<Self, JournalError> {
        validate_session(session)?;
        let root = settings.directory()?;
        let stem = format!("{}_{}", workspace_id(cwd)?, session);
        let cursor = root.join(format!("{stem}.cursor"));
        let path = root.join(format!("{stem}.jsonl"));
        if !fs::symlink_metadata(&path)?.is_file() {
            return Err(JournalError::Corrupt);
        }
        Ok(Self {
            cursor,
            file: File::open(path)?,
            offset: 0,
            next_sequence: 0,
            latest: None,
        })
    }

    pub(crate) fn drain(
        &mut self,
        mut consume: impl FnMut(&Record) -> Result<(), JournalError>,
    ) -> Result<(), JournalError> {
        let info = metadata(&self.cursor)?;
        if info.committed_bytes < self.offset || self.file.metadata()?.len() < info.committed_bytes
        {
            return Err(JournalError::Corrupt);
        }
        let mut reader = BufReader::new((&mut self.file).take(info.committed_bytes - self.offset));
        while let Some(record) = read_record(&mut reader)? {
            validate_record(&record, &info, self.next_sequence)?;
            if self
                .latest
                .as_ref()
                .is_some_and(|latest| record.snapshot_version < latest.snapshot_version)
            {
                return Err(JournalError::Corrupt);
            }
            consume(&record)?;
            self.next_sequence = self
                .next_sequence
                .checked_add(1)
                .ok_or(JournalError::Corrupt)?;
            self.latest = Some(record);
        }
        self.offset = info.committed_bytes;
        let latest = self.latest.as_ref().ok_or(JournalError::Corrupt)?;
        if latest.event_seq != info.committed_seq
            || latest.snapshot_version != info.snapshot_version
            || latest.state()?.session_closed != info.session_closed
            || latest.state()?.needs_recovery() != info.needs_recovery
            || latest.state()?.execution_result != info.execution_result
        {
            return Err(JournalError::Corrupt);
        }
        Ok(())
    }

    pub(crate) fn latest(&self) -> Option<&Record> {
        self.latest.as_ref()
    }
}

#[cfg(test)]
pub(crate) mod tests;
