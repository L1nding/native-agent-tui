//! Core-owned evidence and attention. This module never sends execution effects.
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::time::Instant;

use crate::agents::AgentSnapshot;
use crate::gate::{ChildOutcome, WaitTarget};
use crate::interactions::{RequestKind, RequestView};
use crate::protocol::{RpcId, ToolCategory};
use crate::scheduler::{TaskId, TaskSnapshot, TaskState};
use crate::state::{GateSnapshot, SessionPhase};

const ACTIVITY_LIMIT: usize = 1024;
const RECENT_EVIDENCE: usize = 8;
const MAX_THRESHOLD_MS: u64 = 7 * 24 * 60 * 60 * 1000;
static NEXT_SESSION: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AttentionClass {
    Model,
    Tool,
    Children,
    Transport,
}

impl AttentionClass {
    pub const ALL: [Self; 4] = [Self::Model, Self::Tool, Self::Children, Self::Transport];
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ConfigSource {
    Default,
    Global,
    Cli,
    Tui,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Thresholds {
    pub quiet_ms: u64,
    pub attention_ms: u64,
    pub source: ConfigSource,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttentionSettings {
    pub model: Thresholds,
    pub tool: Thresholds,
    pub children: Thresholds,
    pub transport: Thresholds,
}

impl Default for AttentionSettings {
    fn default() -> Self {
        let pair = |quiet_ms, attention_ms| Thresholds {
            quiet_ms,
            attention_ms,
            source: ConfigSource::Default,
        };
        Self {
            model: pair(15000, 30000),
            tool: pair(30000, 60000),
            children: pair(60000, 120000),
            transport: pair(15000, 30000),
        }
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ObservationError {
    #[error("attention thresholds require 0 < quiet < attention <= 604800000 milliseconds")]
    Thresholds,
    #[error("observation identity limit reached (1024); evidence cannot be dropped")]
    Limit,
}

impl AttentionSettings {
    pub fn get(&self, class: AttentionClass) -> Thresholds {
        match class {
            AttentionClass::Model => self.model,
            AttentionClass::Tool => self.tool,
            AttentionClass::Children => self.children,
            AttentionClass::Transport => self.transport,
        }
    }
    pub fn set(
        &mut self,
        class: AttentionClass,
        quiet_ms: u64,
        attention_ms: u64,
        source: ConfigSource,
    ) -> Result<(), ObservationError> {
        if quiet_ms == 0 || quiet_ms >= attention_ms || attention_ms > MAX_THRESHOLD_MS {
            return Err(ObservationError::Thresholds);
        }
        let pair = Thresholds {
            quiet_ms,
            attention_ms,
            source,
        };
        match class {
            AttentionClass::Model => self.model = pair,
            AttentionClass::Tool => self.tool = pair,
            AttentionClass::Children => self.children = pair,
            AttentionClass::Transport => self.transport = pair,
        }
        Ok(())
    }
    pub fn validate(&self) -> Result<(), ObservationError> {
        for class in AttentionClass::ALL {
            let t = self.get(class);
            if t.quiet_ms == 0 || t.quiet_ms >= t.attention_ms || t.attention_ms > MAX_THRESHOLD_MS
            {
                return Err(ObservationError::Thresholds);
            }
        }
        Ok(())
    }
    pub fn apply_json(&mut self, bytes: &[u8], source: ConfigSource) -> Result<(), String> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Pair {
            quiet_ms: u64,
            attention_ms: u64,
        }
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Overrides {
            model: Option<Pair>,
            tool: Option<Pair>,
            children: Option<Pair>,
            transport: Option<Pair>,
        }
        if bytes.len() > 8192 {
            return Err("Attention configuration exceeds 8 KiB.".into());
        }
        let changes: Overrides = serde_json::from_slice(bytes).map_err(|_| {
            "Invalid attention configuration; expected threshold pairs in milliseconds.".to_owned()
        })?;
        let mut proposed = self.clone();
        for (class, pair) in [
            (AttentionClass::Model, changes.model),
            (AttentionClass::Tool, changes.tool),
            (AttentionClass::Children, changes.children),
            (AttentionClass::Transport, changes.transport),
        ] {
            if let Some(pair) = pair {
                proposed
                    .set(class, pair.quiet_ms, pair.attention_ms, source)
                    .map_err(|e| e.to_string())?;
            }
        }
        *self = proposed;
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ActivityKind {
    Starting,
    ModelRequest,
    ModelStreaming,
    ToolRunning,
    WaitingApproval,
    WaitingUserInput,
    WaitingChildren,
    WaitingTransport,
    Completed,
    Failed,
    Unknown,
}

impl ActivityKind {
    fn class(self) -> Option<AttentionClass> {
        match self {
            Self::ModelRequest | Self::ModelStreaming => Some(AttentionClass::Model),
            Self::ToolRunning => Some(AttentionClass::Tool),
            Self::WaitingChildren => Some(AttentionClass::Children),
            Self::WaitingTransport => Some(AttentionClass::Transport),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ExecutionState {
    Idle,
    Starting,
    Running,
    Waiting,
    Completed,
    Failed,
    Interrupted,
    Unknown,
}
impl ExecutionState {
    fn active(self) -> bool {
        matches!(self, Self::Starting | Self::Running | Self::Waiting)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum EvidenceKind {
    StartupChanged,
    TurnSubmitted,
    TurnStarted,
    StateChanged,
    Output,
    MessageFinalized,
    ToolStarted,
    ToolCompleted,
    RequestCreated,
    RequestAnswered,
    RequestResolved,
    RequestExpired,
    GateEntered,
    GateReleased,
    ChildTurnBound,
    ChildTerminal,
    TurnCompleted,
    TurnFailed,
    TurnInterrupted,
    ExecutionUnknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum EvidenceSource {
    Core,
    AppServer,
    ClientWriter,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Evidence {
    pub id: u64,
    pub kind: EvidenceKind,
    pub source: EvidenceSource,
    pub recorded_at_ms: Option<u64>,
    pub item_id: Option<String>,
    pub request_id: Option<RpcId>,
    pub output_bytes: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AttentionLevel {
    Active,
    Quiet,
    AttentionNeeded,
    RequiresAction,
    Unknown,
    Ended,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AttentionReason {
    RecentEvidence,
    NoRecentEvidence,
    UserDecision,
    AwaitingResolution,
    ThresholdUnavailable,
    ClockUncertain,
    ExecutionEnded,
    ExecutionUnknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Attention {
    pub level: AttentionLevel,
    pub reason: AttentionReason,
    pub requires_action: bool,
    pub quiet_after_ms: Option<u64>,
    pub attention_after_ms: Option<u64>,
    pub config_source: Option<ConfigSource>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Freshness {
    Current,
    Final,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ActivityScope {
    Startup,
    Turn,
    Tool,
    Interaction,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum InteractionState {
    Pending,
    Responding,
    Resolved,
    Expired,
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActivityIdentity {
    pub agent_id: String,
    pub task_id: Option<TaskId>,
    pub attempt_id: Option<u64>,
    pub thread_id: Option<String>,
    pub turn_id: Option<String>,
    pub generation: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WaitTargetSnapshot {
    pub thread_id: String,
    pub turn_id: Option<String>,
    pub generation: u64,
    pub outcome: Option<ChildOutcome>,
    pub last_evidence: Option<Evidence>,
    pub silence_ms: Option<u64>,
    pub attention: Option<Attention>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActivitySnapshot {
    pub session_id: String,
    pub clock_epoch: String,
    pub activity_id: String,
    pub identity: ActivityIdentity,
    pub scope: ActivityScope,
    pub kind: ActivityKind,
    pub execution_state: ExecutionState,
    pub item_id: Option<String>,
    pub request_id: Option<RpcId>,
    pub interaction_state: Option<InteractionState>,
    pub tool_category: Option<ToolCategory>,
    pub started_at_ms: Option<u64>,
    pub last_evidence_at_ms: Option<u64>,
    pub elapsed_ms: Option<u64>,
    pub silence_ms: Option<u64>,
    pub freshness: Freshness,
    pub last_evidence: Option<Evidence>,
    pub recent_evidence: Vec<Evidence>,
    pub progress_seq: u64,
    pub output_bytes: u64,
    pub transition_count: u64,
    pub child_terminal_count: u64,
    pub wait_reason: Option<WaitReason>,
    pub resume_condition: Option<ResumeCondition>,
    pub wait_targets: Vec<WaitTargetSnapshot>,
    pub attention: Attention,
    /// A turn/started event does not prove a provider is currently computing.
    pub provider_state: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum WaitReason {
    Children,
    Approval,
    UserInput,
    ServerResolution,
    TurnIdentity,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ResumeCondition {
    AllChildrenCompleteOrAnyUnsuccessful,
    AnswerApproval,
    AnswerQuestions,
    ServerRequestResolved,
    TurnStarted,
}

#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ObservationSnapshot {
    pub session_id: String,
    pub clock_epoch: String,
    pub snapshot_version: u64,
    pub raw_message_count: u64,
    pub accepted_evidence_count: u64,
    pub monotonic_ms: u64,
    pub settings: AttentionSettings,
    pub activities: Vec<ActivitySnapshot>,
}

pub struct ChildFact<'a> {
    pub agent: &'a AgentSnapshot,
    pub task: Option<&'a TaskSnapshot>,
}
pub struct ObservationFacts<'a> {
    pub phase: SessionPhase,
    pub root_thread: Option<&'a str>,
    pub root_generation: u64,
    pub root_task: Option<&'a TaskSnapshot>,
    pub children: Vec<ChildFact<'a>>,
    pub requests: &'a [RequestView],
    pub gate: Option<&'a GateSnapshot>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum AgentKey {
    Root,
    Child(String),
}
impl AgentKey {
    fn label(&self) -> String {
        match self {
            Self::Root => "root".into(),
            Self::Child(id) => id.clone(),
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum Slot {
    Main,
    Tool(String),
    Request(String),
}
type Key = (AgentKey, Slot);

struct Activity {
    identity: ActivityIdentity,
    scope: ActivityScope,
    kind: ActivityKind,
    execution: ExecutionState,
    model_kind: ActivityKind,
    started: Instant,
    last: Option<Instant>,
    ended: Option<Instant>,
    started_wall: Option<u64>,
    evidence: VecDeque<Evidence>,
    progress_seq: u64,
    output_bytes: u64,
    transitions: u64,
    item_id: Option<String>,
    request_id: Option<RpcId>,
    interaction: Option<InteractionState>,
    targets: Vec<WaitTarget>,
    children_terminal: u64,
    phase: Option<SessionPhase>,
    seen_children: BTreeMap<String, u64>,
    start_known: bool,
    tool_category: Option<ToolCategory>,
}

pub struct Observer {
    session_id: String,
    epoch: String,
    origin: Instant,
    origin_wall: Option<u64>,
    settings: AttentionSettings,
    activities: BTreeMap<Key, Activity>,
    finalized_items: BTreeSet<(AgentKey, String)>,
    raw_messages: u64,
    next_evidence: u64,
    active_gate: Option<(String, u64)>,
}

impl Observer {
    pub fn new(settings: AttentionSettings) -> Self {
        let wall = SystemTime::now().duration_since(UNIX_EPOCH).ok();
        let session = format!(
            "{}-{}-{}",
            std::process::id(),
            wall.map_or(0, |time| time.as_nanos()),
            NEXT_SESSION.fetch_add(1, Ordering::Relaxed)
        );
        Self::new_at(
            session,
            settings,
            Instant::now(),
            wall.and_then(|time| u64::try_from(time.as_millis()).ok()),
        )
    }
    pub fn new_at(
        session: String,
        settings: AttentionSettings,
        now: Instant,
        wall_ms: Option<u64>,
    ) -> Self {
        Self {
            epoch: format!("{session}:clock-1"),
            session_id: session,
            origin: now,
            origin_wall: wall_ms,
            settings,
            activities: BTreeMap::new(),
            finalized_items: BTreeSet::new(),
            raw_messages: 0,
            next_evidence: 0,
            active_gate: None,
        }
    }
    pub fn raw_message(&mut self) {
        self.raw_messages = self.raw_messages.saturating_add(1);
    }
    pub fn configure(
        &mut self,
        class: AttentionClass,
        quiet_ms: u64,
        attention_ms: u64,
    ) -> Result<(), ObservationError> {
        self.settings
            .set(class, quiet_ms, attention_ms, ConfigSource::Tui)
    }
    fn wall(&self, now: Instant) -> Option<u64> {
        self.origin_wall?
            .checked_add(millis(now.checked_duration_since(self.origin)?))
    }
    fn create(
        &mut self,
        key: Key,
        identity: ActivityIdentity,
        scope: ActivityScope,
        kind: ActivityKind,
        execution: ExecutionState,
        now: Instant,
    ) -> Result<(), ObservationError> {
        if self.activities.len() + self.finalized_items.len() >= ACTIVITY_LIMIT {
            return Err(ObservationError::Limit);
        }
        let item_id = if let Slot::Tool(id) = &key.1 {
            Some(id.clone())
        } else {
            None
        };
        let started_wall = self.wall(now);
        self.activities.insert(
            key,
            Activity {
                identity,
                scope,
                kind,
                execution,
                model_kind: ActivityKind::ModelRequest,
                started: now,
                last: None,
                ended: None,
                started_wall,
                evidence: VecDeque::new(),
                progress_seq: 0,
                output_bytes: 0,
                transitions: 0,
                item_id,
                request_id: None,
                interaction: None,
                targets: Vec::new(),
                children_terminal: 0,
                phase: None,
                seen_children: BTreeMap::new(),
                start_known: true,
                tool_category: None,
            },
        );
        Ok(())
    }
    fn evidence(
        &mut self,
        key: &Key,
        kind: EvidenceKind,
        source: EvidenceSource,
        now: Instant,
        item_id: Option<String>,
        bytes: u64,
    ) {
        let wall = self.wall(now);
        let Some(activity) = self.activities.get_mut(key) else {
            return;
        };
        self.next_evidence = self.next_evidence.saturating_add(1);
        let evidence = Evidence {
            id: self.next_evidence,
            kind,
            source,
            recorded_at_ms: wall,
            item_id,
            request_id: activity.request_id.clone(),
            output_bytes: bytes,
        };
        activity.last = Some(now);
        activity.progress_seq = activity.progress_seq.saturating_add(1);
        activity.output_bytes = activity.output_bytes.saturating_add(bytes);
        if kind != EvidenceKind::Output {
            activity.transitions = activity.transitions.saturating_add(1);
        }
        activity.evidence.push_back(evidence);
        while activity.evidence.len() > RECENT_EVIDENCE {
            activity.evidence.pop_front();
        }
    }
    fn finish(
        &mut self,
        key: &Key,
        execution: ExecutionState,
        now: Instant,
        kind: EvidenceKind,
        source: EvidenceSource,
    ) {
        if let Some(activity) = self.activities.get_mut(key) {
            if !activity.execution.active() {
                return;
            }
            activity.execution = execution;
            activity.kind = match execution {
                ExecutionState::Failed => ActivityKind::Failed,
                ExecutionState::Unknown => ActivityKind::Unknown,
                _ => ActivityKind::Completed,
            };
            activity.ended = Some(now);
            self.evidence(key, kind, source, now, None, 0);
        }
    }
    fn agent_for_turn(&self, thread: &str, turn: &str) -> Option<AgentKey> {
        self.activities
            .iter()
            .find_map(|((agent, slot), activity)| {
                (slot == &Slot::Main
                    && activity.identity.thread_id.as_deref() == Some(thread)
                    && activity.identity.turn_id.as_deref() == Some(turn))
                .then(|| agent.clone())
            })
    }
    pub fn output(
        &mut self,
        thread: &str,
        turn: &str,
        item: &str,
        bytes: usize,
        finalized: bool,
        now: Instant,
    ) -> Result<(), ObservationError> {
        if bytes == 0 && !finalized {
            return Ok(());
        }
        let Some(agent) = self.agent_for_turn(thread, turn) else {
            return Ok(());
        };
        let finalized_key = (agent.clone(), item.to_owned());
        if self.finalized_items.contains(&finalized_key) {
            return Ok(());
        }
        if finalized {
            if self.activities.len() + self.finalized_items.len() >= ACTIVITY_LIMIT {
                return Err(ObservationError::Limit);
            }
            self.finalized_items.insert(finalized_key);
        }
        let key = (agent, Slot::Main);
        let activity = self.activities.get_mut(&key).unwrap();
        if !activity.execution.active() && !finalized {
            return Ok(());
        }
        if activity.execution.active() {
            activity.model_kind = ActivityKind::ModelStreaming;
            if activity.kind != ActivityKind::WaitingChildren {
                activity.kind = ActivityKind::ModelStreaming;
            }
        }
        self.evidence(
            &key,
            if finalized {
                EvidenceKind::MessageFinalized
            } else {
                EvidenceKind::Output
            },
            EvidenceSource::AppServer,
            now,
            Some(item.into()),
            bytes as u64,
        );
        Ok(())
    }
    pub fn tool(
        &mut self,
        notice: &crate::protocol::ObservedTool,
        now: Instant,
    ) -> Result<(), ObservationError> {
        let Some(agent) = self.agent_for_turn(&notice.thread_id, &notice.turn_id) else {
            return Ok(());
        };
        let main = (agent.clone(), Slot::Main);
        if !self.activities[&main].execution.active() {
            return Ok(());
        }
        let key = (agent, Slot::Tool(notice.item_id.clone()));
        if !self.activities.contains_key(&key) {
            self.create(
                key.clone(),
                self.activities[&main].identity.clone(),
                ActivityScope::Tool,
                ActivityKind::ToolRunning,
                ExecutionState::Running,
                now,
            )?;
            let activity = self.activities.get_mut(&key).unwrap();
            activity.tool_category = Some(notice.category);
            if notice.outcome.is_some() {
                activity.start_known = false;
                activity.started_wall = None;
            } else {
                self.evidence(
                    &key,
                    EvidenceKind::ToolStarted,
                    EvidenceSource::AppServer,
                    now,
                    Some(notice.item_id.clone()),
                    0,
                );
                self.evidence(
                    &main,
                    EvidenceKind::ToolStarted,
                    EvidenceSource::AppServer,
                    now,
                    Some(notice.item_id.clone()),
                    0,
                );
            }
        }
        if notice.outcome.is_none()
            && self.activities[&key].execution.active()
            && !self.activities[&key].start_known
        {
            let wall = self.wall(now);
            let activity = self.activities.get_mut(&key).unwrap();
            activity.start_known = true;
            activity.started = now;
            activity.started_wall = wall;
            self.evidence(
                &key,
                EvidenceKind::ToolStarted,
                EvidenceSource::AppServer,
                now,
                Some(notice.item_id.clone()),
                0,
            );
            self.evidence(
                &main,
                EvidenceKind::ToolStarted,
                EvidenceSource::AppServer,
                now,
                Some(notice.item_id.clone()),
                0,
            );
        }
        if let Some(outcome) = notice.outcome {
            if !self.activities[&key].execution.active() {
                return Ok(());
            }
            let execution = match outcome {
                crate::protocol::ObservedToolOutcome::Completed => ExecutionState::Completed,
                crate::protocol::ObservedToolOutcome::Failed => ExecutionState::Failed,
                crate::protocol::ObservedToolOutcome::Interrupted => ExecutionState::Interrupted,
                crate::protocol::ObservedToolOutcome::Unknown => ExecutionState::Unknown,
            };
            self.finish(
                &key,
                execution,
                now,
                EvidenceKind::ToolCompleted,
                EvidenceSource::AppServer,
            );
            self.evidence(
                &main,
                EvidenceKind::ToolCompleted,
                EvidenceSource::AppServer,
                now,
                Some(notice.item_id.clone()),
                0,
            );
        }
        Ok(())
    }
    pub fn tool_output(
        &mut self,
        thread: &str,
        turn: &str,
        item: &str,
        bytes: usize,
        category: ToolCategory,
        now: Instant,
    ) -> Result<(), ObservationError> {
        if bytes == 0 {
            return Ok(());
        }
        let Some(agent) = self.agent_for_turn(thread, turn) else {
            return Ok(());
        };
        let main = (agent.clone(), Slot::Main);
        if !self.activities[&main].execution.active() {
            return Ok(());
        }
        let key = (agent.clone(), Slot::Tool(item.into()));
        if !self.activities.contains_key(&key) {
            self.create(
                key.clone(),
                self.activities[&main].identity.clone(),
                ActivityScope::Tool,
                ActivityKind::ToolRunning,
                ExecutionState::Running,
                now,
            )?;
            let activity = self.activities.get_mut(&key).unwrap();
            activity.start_known = false;
            activity.started_wall = None;
            activity.tool_category = Some(category);
        }
        if !self
            .activities
            .get(&key)
            .is_some_and(|activity| activity.execution.active())
        {
            return Ok(());
        }
        self.evidence(
            &key,
            EvidenceKind::Output,
            EvidenceSource::AppServer,
            now,
            Some(item.into()),
            bytes as u64,
        );
        self.evidence(
            &(agent, Slot::Main),
            EvidenceKind::Output,
            EvidenceSource::AppServer,
            now,
            Some(item.into()),
            bytes as u64,
        );
        Ok(())
    }
    pub fn request_resolved(&mut self, thread: &str, id: &RpcId, now: Instant) {
        let key = self.activities.iter().find_map(|(key, activity)| {
            (activity.identity.thread_id.as_deref() == Some(thread)
                && activity.request_id.as_ref() == Some(id)
                && activity.execution.active())
            .then(|| key.clone())
        });
        if let Some(key) = key {
            self.activities.get_mut(&key).unwrap().interaction = Some(InteractionState::Resolved);
            self.finish(
                &key,
                ExecutionState::Completed,
                now,
                EvidenceKind::RequestResolved,
                EvidenceSource::AppServer,
            );
            self.evidence(
                &(key.0, Slot::Main),
                EvidenceKind::RequestResolved,
                EvidenceSource::AppServer,
                now,
                None,
                0,
            );
        }
    }

    pub fn reconcile(
        &mut self,
        facts: ObservationFacts<'_>,
        now: Instant,
    ) -> Result<(), ObservationError> {
        let unavailable = matches!(
            facts.phase,
            SessionPhase::Unknown
                | SessionPhase::Disconnected
                | SessionPhase::Stopping
                | SessionPhase::Stopped
        );
        if unavailable && !self.activities.is_empty() {
            self.execution_unavailable(now);
            return Ok(());
        }
        let root = &facts.root_task;
        let identity = ActivityIdentity {
            agent_id: "root".into(),
            task_id: root.map(|task| task.id),
            attempt_id: root.map(|task| task.attempt),
            thread_id: facts.root_thread.map(str::to_owned),
            turn_id: root.and_then(|task| {
                task.external
                    .as_ref()
                    .map(|external| external.turn_id.clone())
            }),
            generation: root.map(|_| facts.root_generation),
        };
        let root_key = (AgentKey::Root, Slot::Main);
        let reset = self.activities.get(&root_key).is_none_or(|activity| {
            activity.identity.task_id != identity.task_id
                || activity.identity.attempt_id != identity.attempt_id
        });
        if reset {
            self.activities
                .retain(|(agent, _), _| agent != &AgentKey::Root);
            self.finalized_items
                .retain(|(agent, _)| agent != &AgentKey::Root);
            self.active_gate = None;
            let mut unbound = identity.clone();
            unbound.turn_id = None;
            self.create(
                root_key.clone(),
                unbound,
                if root.is_some() {
                    ActivityScope::Turn
                } else {
                    ActivityScope::Startup
                },
                ActivityKind::Starting,
                ExecutionState::Starting,
                now,
            )?;
            self.evidence(
                &root_key,
                if root.is_some() {
                    EvidenceKind::TurnSubmitted
                } else {
                    EvidenceKind::StartupChanged
                },
                EvidenceSource::Core,
                now,
                None,
                0,
            );
        }
        if let Some(task) = root {
            self.reconcile_turn(
                &root_key,
                identity,
                execution(task.state, task.external.is_some()),
                now,
            );
        } else {
            let activity = self.activities.get_mut(&root_key).unwrap();
            activity.identity = identity;
            if activity.phase != Some(facts.phase) {
                activity.phase = Some(facts.phase);
                match facts.phase {
                    SessionPhase::Ready => {
                        activity.execution = ExecutionState::Idle;
                        activity.ended = Some(now);
                        activity.kind = ActivityKind::Completed;
                    }
                    SessionPhase::Failed => {
                        activity.execution = ExecutionState::Failed;
                        activity.ended = Some(now);
                        activity.kind = ActivityKind::Failed;
                    }
                    _ => {}
                }
                self.evidence(
                    &root_key,
                    EvidenceKind::StartupChanged,
                    EvidenceSource::Core,
                    now,
                    None,
                    0,
                );
            }
        }

        for child in facts.children {
            let agent = child.agent;
            let key = (AgentKey::Child(agent.info.id.clone()), Slot::Main);
            let identity = ActivityIdentity {
                agent_id: agent.info.id.clone(),
                task_id: child.task.map(|task| task.id),
                attempt_id: (agent.generation > 0).then_some(agent.generation),
                thread_id: Some(agent.info.id.clone()),
                turn_id: agent.turn_id.clone(),
                generation: (agent.generation > 0).then_some(agent.generation),
            };
            let reset = self
                .activities
                .get(&key)
                .is_none_or(|activity| activity.identity.generation != identity.generation);
            if reset {
                self.activities.retain(|(owner, _), _| owner != &key.0);
                self.finalized_items.retain(|(owner, _)| owner != &key.0);
                let mut unbound = identity.clone();
                unbound.turn_id = None;
                self.create(
                    key.clone(),
                    unbound,
                    ActivityScope::Turn,
                    ActivityKind::Starting,
                    ExecutionState::Starting,
                    now,
                )?;
            }
            let execution = if let Some(outcome) = &agent.outcome {
                match outcome {
                    ChildOutcome::Completed => ExecutionState::Completed,
                    ChildOutcome::Failed => ExecutionState::Failed,
                    ChildOutcome::Interrupted => ExecutionState::Interrupted,
                }
            } else if agent.turn_id.is_some() {
                ExecutionState::Running
            } else {
                ExecutionState::Starting
            };
            self.reconcile_turn(&key, identity, execution, now);
        }

        if let Some(gate) = facts.gate {
            let pending = gate.pending && self.activities[&root_key].execution.active();
            if pending && self.active_gate.is_none() {
                self.active_gate = Some((
                    self.activities[&root_key]
                        .identity
                        .turn_id
                        .clone()
                        .unwrap_or_default(),
                    facts.root_generation,
                ));
                let activity = self.activities.get_mut(&root_key).unwrap();
                activity.kind = ActivityKind::WaitingChildren;
                activity.execution = ExecutionState::Waiting;
                activity.targets = gate.targets.clone();
                self.evidence(
                    &root_key,
                    EvidenceKind::GateEntered,
                    EvidenceSource::Core,
                    now,
                    None,
                    0,
                );
            }
            if self.active_gate.is_some() {
                let previous = self.activities[&root_key].targets.clone();
                for target in &gate.targets {
                    if let Some(old) = previous
                        .iter()
                        .find(|old| old.id == target.id && old.generation == target.generation)
                    {
                        if old.turn_id.is_none() && target.turn_id.is_some() {
                            self.evidence(
                                &root_key,
                                EvidenceKind::ChildTurnBound,
                                EvidenceSource::AppServer,
                                now,
                                None,
                                0,
                            );
                        }
                    }
                    if target.outcome.is_some()
                        && self.activities[&root_key]
                            .seen_children
                            .get(&target.id)
                            .is_none_or(|generation| *generation < target.generation)
                    {
                        let activity = self.activities.get_mut(&root_key).unwrap();
                        activity
                            .seen_children
                            .insert(target.id.clone(), target.generation);
                        activity.children_terminal = activity.children_terminal.saturating_add(1);
                        self.evidence(
                            &root_key,
                            EvidenceKind::ChildTerminal,
                            EvidenceSource::AppServer,
                            now,
                            None,
                            0,
                        );
                    }
                }
                self.activities.get_mut(&root_key).unwrap().targets = gate.targets.clone();
                if !pending {
                    self.active_gate = None;
                    if gate.root_starts_at_release.is_some()
                        && self.activities[&root_key].execution.active()
                    {
                        let activity = self.activities.get_mut(&root_key).unwrap();
                        activity.kind = activity.model_kind;
                        self.evidence(
                            &root_key,
                            EvidenceKind::GateReleased,
                            EvidenceSource::ClientWriter,
                            now,
                            None,
                            0,
                        );
                    }
                }
            }
        }

        let mut present = BTreeSet::new();
        for request in facts.requests {
            let Some(agent) = self.agent_for_turn(&request.thread_id, &request.turn_id) else {
                continue;
            };
            let key = (
                agent.clone(),
                Slot::Request(serde_json::to_string(&request.id).expect("RPC ids serialize")),
            );
            present.insert(key.clone());
            if !self.activities.contains_key(&key) {
                let kind = if matches!(request.kind, RequestKind::UserInput { .. }) {
                    ActivityKind::WaitingUserInput
                } else {
                    ActivityKind::WaitingApproval
                };
                self.create(
                    key.clone(),
                    self.activities[&(agent.clone(), Slot::Main)]
                        .identity
                        .clone(),
                    ActivityScope::Interaction,
                    kind,
                    ExecutionState::Waiting,
                    now,
                )?;
                let activity = self.activities.get_mut(&key).unwrap();
                activity.request_id = Some(request.id.clone());
                activity.interaction = Some(InteractionState::Pending);
                self.evidence(
                    &key,
                    EvidenceKind::RequestCreated,
                    EvidenceSource::AppServer,
                    now,
                    None,
                    0,
                );
                self.evidence(
                    &(agent.clone(), Slot::Main),
                    EvidenceKind::RequestCreated,
                    EvidenceSource::AppServer,
                    now,
                    None,
                    0,
                );
            }
            let activity = self.activities.get_mut(&key).unwrap();
            if request.responding && activity.interaction == Some(InteractionState::Pending) {
                activity.interaction = Some(InteractionState::Responding);
                activity.kind = ActivityKind::WaitingTransport;
                self.evidence(
                    &key,
                    EvidenceKind::RequestAnswered,
                    EvidenceSource::ClientWriter,
                    now,
                    None,
                    0,
                );
                self.evidence(
                    &(agent, Slot::Main),
                    EvidenceKind::RequestAnswered,
                    EvidenceSource::ClientWriter,
                    now,
                    None,
                    0,
                );
            }
        }
        let expired: Vec<_> = self
            .activities
            .iter()
            .filter(|(key, activity)| {
                matches!(key.1, Slot::Request(_))
                    && activity.execution.active()
                    && !present.contains(*key)
            })
            .map(|(key, _)| key.clone())
            .collect();
        for key in expired {
            self.activities.get_mut(&key).unwrap().interaction = Some(InteractionState::Expired);
            self.finish(
                &key,
                ExecutionState::Completed,
                now,
                EvidenceKind::RequestExpired,
                EvidenceSource::Core,
            );
        }
        if unavailable {
            self.execution_unavailable(now);
        }
        Ok(())
    }

    /// Core has explicitly lost execution certainty or closed its owner.
    pub fn execution_unavailable(&mut self, now: Instant) {
        let live: Vec<_> = self
            .activities
            .iter()
            .filter(|(_, activity)| activity.execution.active())
            .map(|(key, _)| key.clone())
            .collect();
        for key in live {
            if self.activities[&key].interaction.is_some() {
                self.activities.get_mut(&key).unwrap().interaction =
                    Some(InteractionState::Unavailable);
            }
            self.finish(
                &key,
                ExecutionState::Unknown,
                now,
                EvidenceKind::ExecutionUnknown,
                EvidenceSource::Core,
            );
        }
    }

    fn reconcile_turn(
        &mut self,
        key: &Key,
        identity: ActivityIdentity,
        execution: ExecutionState,
        now: Instant,
    ) {
        let activity = self.activities.get_mut(key).unwrap();
        let newly_bound = activity.identity.turn_id.is_none() && identity.turn_id.is_some();
        activity.identity = identity;
        if newly_bound {
            activity.kind = ActivityKind::ModelRequest;
            activity.execution = ExecutionState::Running;
            self.evidence(
                key,
                EvidenceKind::TurnStarted,
                EvidenceSource::AppServer,
                now,
                None,
                0,
            );
        }
        if execution.active() {
            let activity = self.activities.get_mut(key).unwrap();
            if !activity.execution.active() {
                return;
            }
            if activity.execution != execution {
                activity.execution = execution;
                self.evidence(
                    key,
                    EvidenceKind::StateChanged,
                    EvidenceSource::Core,
                    now,
                    None,
                    0,
                );
            }
        } else {
            let kind = match execution {
                ExecutionState::Completed => EvidenceKind::TurnCompleted,
                ExecutionState::Failed => EvidenceKind::TurnFailed,
                ExecutionState::Interrupted => EvidenceKind::TurnInterrupted,
                _ => EvidenceKind::ExecutionUnknown,
            };
            self.finish(
                key,
                execution,
                now,
                kind,
                if execution == ExecutionState::Unknown {
                    EvidenceSource::Core
                } else {
                    EvidenceSource::AppServer
                },
            );
            // Tools cannot be assumed successful because their owning turn ended.
            let open: Vec<_> = self
                .activities
                .iter()
                .filter(|((agent, slot), activity)| {
                    agent == &key.0 && matches!(slot, Slot::Tool(_)) && activity.execution.active()
                })
                .map(|(key, _)| key.clone())
                .collect();
            for key in open {
                self.finish(
                    &key,
                    ExecutionState::Unknown,
                    now,
                    EvidenceKind::ExecutionUnknown,
                    EvidenceSource::Core,
                );
            }
        }
    }

    pub fn has_timed_activity(&self) -> bool {
        self.activities
            .values()
            .any(|activity| activity.execution.active() && activity.kind.class().is_some())
    }

    pub fn snapshot_at(&self, version: u64, now: Instant) -> ObservationSnapshot {
        let activities = self
            .activities
            .iter()
            .map(|(key, activity)| {
                let mut snapshot = self.project(key, activity, now);
                snapshot.wait_targets = activity
                    .targets
                    .iter()
                    .map(|target| {
                        let child_key = (AgentKey::Child(target.id.clone()), Slot::Main);
                        let child = self
                            .activities
                            .get(&child_key)
                            .filter(|child| {
                                child.identity.generation == Some(target.generation)
                                    && child.identity.turn_id == target.turn_id
                            })
                            .map(|child| self.project(&child_key, child, now));
                        WaitTargetSnapshot {
                            thread_id: target.id.clone(),
                            turn_id: target.turn_id.clone(),
                            generation: target.generation,
                            outcome: target.outcome.clone(),
                            last_evidence: child
                                .as_ref()
                                .and_then(|child| child.last_evidence.clone()),
                            silence_ms: child.as_ref().and_then(|child| child.silence_ms),
                            attention: child.map(|child| child.attention),
                        }
                    })
                    .collect();
                snapshot
            })
            .collect();
        ObservationSnapshot {
            session_id: self.session_id.clone(),
            clock_epoch: self.epoch.clone(),
            snapshot_version: version,
            raw_message_count: self.raw_messages,
            accepted_evidence_count: self.next_evidence,
            monotonic_ms: millis(now.saturating_duration_since(self.origin)),
            settings: self.settings.clone(),
            activities,
        }
    }

    fn project(&self, key: &Key, activity: &Activity, now: Instant) -> ActivitySnapshot {
        let stop = activity.ended.unwrap_or(now);
        let valid_clock = stop >= activity.started && activity.last.is_none_or(|last| now >= last);
        let elapsed = (valid_clock && activity.start_known)
            .then(|| millis(stop.saturating_duration_since(activity.started)));
        let silence = if activity.execution.active() && valid_clock {
            activity
                .last
                .map(|last| millis(now.saturating_duration_since(last)))
        } else {
            None
        };
        let requires_action =
            activity.interaction == Some(InteractionState::Pending) && activity.execution.active();
        let thresholds = activity.kind.class().map(|class| self.settings.get(class));
        let (level, reason) = if requires_action {
            (
                AttentionLevel::RequiresAction,
                AttentionReason::UserDecision,
            )
        } else if !valid_clock {
            (AttentionLevel::Unknown, AttentionReason::ClockUncertain)
        } else if activity.execution == ExecutionState::Unknown {
            (AttentionLevel::Unknown, AttentionReason::ExecutionUnknown)
        } else if !activity.execution.active() {
            (AttentionLevel::Ended, AttentionReason::ExecutionEnded)
        } else if let Some(thresholds) = thresholds {
            match silence {
                Some(ms) if ms >= thresholds.attention_ms => (
                    AttentionLevel::AttentionNeeded,
                    AttentionReason::NoRecentEvidence,
                ),
                Some(ms) if ms >= thresholds.quiet_ms => {
                    (AttentionLevel::Quiet, AttentionReason::NoRecentEvidence)
                }
                Some(_) => (
                    AttentionLevel::Active,
                    if activity.interaction == Some(InteractionState::Responding) {
                        AttentionReason::AwaitingResolution
                    } else {
                        AttentionReason::RecentEvidence
                    },
                ),
                None => (
                    AttentionLevel::Unknown,
                    AttentionReason::ThresholdUnavailable,
                ),
            }
        } else {
            (
                AttentionLevel::Unknown,
                AttentionReason::ThresholdUnavailable,
            )
        };
        let (wait_reason, resume_condition) = match activity.kind {
            ActivityKind::WaitingChildren => (
                Some(WaitReason::Children),
                Some(ResumeCondition::AllChildrenCompleteOrAnyUnsuccessful),
            ),
            ActivityKind::WaitingApproval => (
                Some(WaitReason::Approval),
                Some(ResumeCondition::AnswerApproval),
            ),
            ActivityKind::WaitingUserInput => (
                Some(WaitReason::UserInput),
                Some(ResumeCondition::AnswerQuestions),
            ),
            ActivityKind::WaitingTransport => (
                Some(WaitReason::ServerResolution),
                Some(ResumeCondition::ServerRequestResolved),
            ),
            ActivityKind::Starting if activity.scope == ActivityScope::Turn => (
                Some(WaitReason::TurnIdentity),
                Some(ResumeCondition::TurnStarted),
            ),
            _ => (None, None),
        };
        ActivitySnapshot {
            session_id: self.session_id.clone(),
            clock_epoch: self.epoch.clone(),
            activity_id: format!(
                "{}:{:?}:{}:{}",
                key.0.label(),
                key.1,
                activity.identity.task_id.map_or(0, |task| task.0),
                activity.identity.attempt_id.unwrap_or(0)
            ),
            identity: activity.identity.clone(),
            scope: activity.scope,
            kind: activity.kind,
            execution_state: activity.execution,
            item_id: activity.item_id.clone(),
            request_id: activity.request_id.clone(),
            interaction_state: activity.interaction,
            tool_category: activity.tool_category,
            started_at_ms: activity.started_wall,
            last_evidence_at_ms: activity.last.and_then(|last| self.wall(last)),
            elapsed_ms: elapsed,
            silence_ms: silence,
            freshness: if !valid_clock || activity.execution == ExecutionState::Unknown {
                Freshness::Unknown
            } else if activity.execution.active() {
                Freshness::Current
            } else {
                Freshness::Final
            },
            last_evidence: activity.evidence.back().cloned(),
            recent_evidence: activity.evidence.iter().cloned().collect(),
            progress_seq: activity.progress_seq,
            output_bytes: activity.output_bytes,
            transition_count: activity.transitions,
            child_terminal_count: activity.children_terminal,
            wait_reason,
            resume_condition,
            wait_targets: Vec::new(),
            attention: Attention {
                level,
                reason,
                requires_action,
                quiet_after_ms: thresholds.map(|t| t.quiet_ms),
                attention_after_ms: thresholds.map(|t| t.attention_ms),
                config_source: thresholds.map(|t| t.source),
            },
            provider_state: None,
        }
    }
}

fn execution(state: TaskState, bound: bool) -> ExecutionState {
    match state {
        TaskState::Succeeded => ExecutionState::Completed,
        TaskState::Failed => ExecutionState::Failed,
        TaskState::Cancelled => ExecutionState::Interrupted,
        TaskState::Unknown => ExecutionState::Unknown,
        TaskState::WaitingChildren | TaskState::WaitingApproval => ExecutionState::Waiting,
        TaskState::Starting => ExecutionState::Starting,
        TaskState::Cancelling if !bound => ExecutionState::Starting,
        _ => ExecutionState::Running,
    }
}

fn millis(duration: std::time::Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests;
