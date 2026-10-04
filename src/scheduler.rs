use std::collections::{BTreeMap, BTreeSet, VecDeque};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::time::Instant;

use crate::gate::{ChildOutcome, GateEvent, WaitTarget};
use crate::interactions::RequestView;
use crate::state::MESSAGE_BYTES;

pub const ROOT_QUEUE_LIMIT: usize = 8;
const TASK_LIMIT: usize = 256;
pub const WORKFLOW_BYTES: usize = 2 * 1024 * 1024;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkflowPlan {
    pub tasks: Vec<RootTaskSpec>,
}

impl WorkflowPlan {
    pub fn parse(bytes: &[u8]) -> Result<Self, String> {
        if bytes.len() > WORKFLOW_BYTES {
            return Err("Workflow file exceeds 2 MiB.".into());
        }
        let plan: Self = serde_json::from_slice(bytes).map_err(|error| error.to_string())?;
        Scheduler::default()
            .enqueue(plan.tasks.clone())
            .map_err(|error| error.to_string())?;
        Ok(plan)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TaskId(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskAttempt {
    pub task: TaskId,
    pub attempt: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TaskKind {
    RootTurn,
    NativeChild,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TaskState {
    Queued,
    Ready,
    Starting,
    Running,
    WaitingChildren,
    WaitingApproval,
    Cancelling,
    Succeeded,
    Failed,
    Cancelled,
    Unknown,
    Paused,
    Blocked,
}

impl TaskState {
    pub fn active(self) -> bool {
        matches!(
            self,
            Self::Starting
                | Self::Running
                | Self::WaitingChildren
                | Self::WaitingApproval
                | Self::Cancelling
        )
    }
    fn unstarted(self) -> bool {
        matches!(
            self,
            Self::Queued | Self::Ready | Self::Paused | Self::Blocked
        )
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DependencyPolicy {
    #[default]
    AllRequired,
    Any,
    Quorum(usize),
    CollectAll,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum FailurePolicy {
    #[default]
    FailFast,
    ContinueWithErrors,
    Optional,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct RootTaskSpec {
    #[serde(default)]
    pub id: Option<TaskId>,
    pub text: String,
    #[serde(default)]
    pub dependencies: Vec<TaskId>,
    #[serde(default)]
    pub policy: DependencyPolicy,
    #[serde(default)]
    pub failure: FailurePolicy,
    #[serde(default)]
    pub priority: i32,
}

impl RootTaskSpec {
    pub fn input(text: String) -> Self {
        Self {
            id: None,
            text,
            dependencies: Vec::new(),
            policy: DependencyPolicy::AllRequired,
            failure: FailurePolicy::FailFast,
            priority: 0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum BlockedReason {
    DependencyFailed(TaskId),
    DependencyUnknown(TaskId),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExternalTurn {
    pub thread_id: String,
    pub turn_id: String,
    pub generation: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskSnapshot {
    pub id: TaskId,
    pub kind: TaskKind,
    pub state: TaskState,
    pub title: String,
    pub parent: Option<TaskId>,
    pub attempt: u64,
    pub dependencies: Vec<TaskId>,
    pub policy: DependencyPolicy,
    pub failure: FailurePolicy,
    pub priority: i32,
    pub blocked_reason: Option<BlockedReason>,
    pub external: Option<ExternalTurn>,
    pub pause_requested: bool,
    pub cancel_requested: bool,
    pub pending_requests: usize,
    pub wait_targets: Vec<TaskAttempt>,
    pub root_slot_reserved: bool,
    pub cancellation_epoch: u64,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct SchedulerSnapshot {
    pub tasks: Vec<TaskSnapshot>,
    pub ready_roots: Vec<TaskId>,
    pub active_root: Option<TaskAttempt>,
    pub paused: bool,
    pub stopping: bool,
    pub disconnected: bool,
    pub queued_roots: usize,
    pub root_slots_reserved: usize,
    /// Native turns are observed; this count is not a client-enforced capacity.
    pub native_turns_observed: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SchedulerCommand {
    PauseWorkflow,
    ResumeWorkflow,
    StopWorkflow,
    Pause(TaskId),
    Resume(TaskId),
    Cancel(TaskId),
    Retry(TaskId),
    Reprioritize { task_id: TaskId, priority: i32 },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InterruptEffect {
    pub attempt: TaskAttempt,
    pub external: Option<ExternalTurn>,
    pub kind: TaskKind,
}

pub struct RootDispatch {
    pub attempt: TaskAttempt,
    pub text: String,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum SchedulerError {
    #[error("task queue is full (8 pending root tasks)")]
    QueueFull,
    #[error("task history limit reached (256 tasks); start a new session")]
    Limit,
    #[error("task text must contain 1-32768 UTF-8 bytes")]
    InvalidText,
    #[error("invalid or duplicate task id")]
    InvalidId,
    #[error("unknown dependency or task: {0:?}")]
    UnknownTask(TaskId),
    #[error("task dependencies contain a cycle")]
    Cycle,
    #[error("invalid dependency policy or priority")]
    InvalidPolicy,
    #[error("this task state does not allow that command")]
    InvalidState,
    #[error("workflow is stopped; start a new session")]
    Stopped,
    #[error("execution owner is disconnected; start a new session")]
    Disconnected,
    #[error("task attempt or external identity changed")]
    Stale,
    #[error("native child retries are controlled by the root agent")]
    NativeRetryUnsupported,
    #[error("invalid scheduler snapshot: {0}")]
    InvalidSnapshot(&'static str),
}

struct Task {
    view: TaskSnapshot,
    text: Option<String>,
    queued_at: Instant,
    ready_seq: u64,
    waiting_children: bool,
}

/// Core owns this module. It returns dispatch/interrupt effects and never performs I/O.
pub struct Scheduler {
    tasks: BTreeMap<TaskId, Task>,
    dependents: BTreeMap<TaskId, BTreeSet<TaskId>>,
    children: BTreeMap<String, TaskId>,
    next_id: u64,
    next_seq: u64,
    active_root: Option<TaskAttempt>,
    paused: bool,
    stopping: bool,
    disconnected: bool,
}

impl Default for Scheduler {
    fn default() -> Self {
        Self {
            tasks: BTreeMap::new(),
            dependents: BTreeMap::new(),
            children: BTreeMap::new(),
            next_id: 1,
            next_seq: 0,
            active_root: None,
            paused: false,
            stopping: false,
            disconnected: false,
        }
    }
}

impl Scheduler {
    /// Rebuilds the scheduler's read-only state from a persisted snapshot.
    ///
    /// Task bodies are intentionally unavailable in persisted snapshots, so a
    /// restored scheduler can validate dependencies and lifecycle facts but
    /// cannot dispatch work until the caller obtains fresh input.
    pub fn restore(snapshot: &SchedulerSnapshot) -> Result<Self, SchedulerError> {
        if snapshot.tasks.len() > TASK_LIMIT {
            return Err(SchedulerError::InvalidSnapshot("task limit exceeded"));
        }
        if snapshot.stopping && !snapshot.paused {
            return Err(SchedulerError::InvalidSnapshot(
                "stopping workflow must be paused",
            ));
        }

        let mut tasks = BTreeMap::new();
        let mut children = BTreeMap::new();
        let mut next_id = 1u64;
        let mut next_seq = 0u64;
        for (index, view) in snapshot.tasks.iter().enumerate() {
            if view.id.0 == 0 || view.id.0 == u64::MAX || tasks.contains_key(&view.id) {
                return Err(SchedulerError::InvalidSnapshot(
                    "task ids must be unique and non-zero",
                ));
            }
            next_id = next_id.max(
                view.id
                    .0
                    .checked_add(1)
                    .ok_or(SchedulerError::InvalidSnapshot("task id overflow"))?,
            );
            next_seq = next_seq.max((index as u64).saturating_add(1));
            if view.kind == TaskKind::RootTurn && view.parent.is_some() {
                return Err(SchedulerError::InvalidSnapshot(
                    "root tasks cannot have a parent",
                ));
            }
            if view.kind == TaskKind::NativeChild && !view.dependencies.is_empty() {
                return Err(SchedulerError::InvalidSnapshot(
                    "native child tasks cannot have dependencies",
                ));
            }
            if view.root_slot_reserved && (view.kind != TaskKind::RootTurn || !view.state.active())
            {
                return Err(SchedulerError::InvalidSnapshot(
                    "only active root tasks can reserve root slots",
                ));
            }
            if view.priority.abs_diff(0) > 1000 {
                return Err(SchedulerError::InvalidSnapshot("priority is out of range"));
            }
            if matches!(view.policy, DependencyPolicy::Any) && view.dependencies.is_empty()
                || matches!(view.policy, DependencyPolicy::Quorum(n) if n == 0 || n > view.dependencies.iter().collect::<BTreeSet<_>>().len())
            {
                return Err(SchedulerError::InvalidSnapshot(
                    "dependency policy is invalid",
                ));
            }
            if let Some(parent) = view.parent {
                if parent == view.id
                    || !snapshot
                        .tasks
                        .iter()
                        .any(|candidate| candidate.id == parent)
                {
                    return Err(SchedulerError::UnknownTask(parent));
                }
            }
            if view.kind == TaskKind::NativeChild {
                if let Some(external) = &view.external {
                    if external.thread_id.is_empty()
                        || children
                            .insert(external.thread_id.clone(), view.id)
                            .is_some()
                    {
                        return Err(SchedulerError::InvalidSnapshot(
                            "native child thread identities must be unique",
                        ));
                    }
                }
            }
            tasks.insert(
                view.id,
                Task {
                    view: view.clone(),
                    text: None,
                    queued_at: Instant::now(),
                    ready_seq: next_seq,
                    waiting_children: view.state == TaskState::WaitingChildren,
                },
            );
        }

        for view in &snapshot.tasks {
            if let Some(parent) = view.parent {
                let parent_task = tasks
                    .get(&parent)
                    .ok_or(SchedulerError::UnknownTask(parent))?;
                if parent_task.view.kind != TaskKind::RootTurn {
                    return Err(SchedulerError::InvalidSnapshot(
                        "task parents must target root tasks",
                    ));
                }
            }
        }

        for view in &snapshot.tasks {
            if view.kind == TaskKind::NativeChild && view.blocked_reason.is_some() {
                return Err(SchedulerError::InvalidSnapshot(
                    "native child tasks cannot have blocked reasons",
                ));
            }
            for target in &view.wait_targets {
                if target.attempt == 0 {
                    return Err(SchedulerError::InvalidSnapshot(
                        "wait target attempts must be positive",
                    ));
                }
                let target_task = tasks
                    .get(&target.task)
                    .ok_or(SchedulerError::UnknownTask(target.task))?;
                if target_task.view.kind != TaskKind::NativeChild {
                    return Err(SchedulerError::InvalidSnapshot(
                        "wait targets must reference native children",
                    ));
                }
            }
            if let Some(reason) = &view.blocked_reason {
                let dependency = match reason {
                    BlockedReason::DependencyFailed(id) | BlockedReason::DependencyUnknown(id) => {
                        *id
                    }
                };
                tasks
                    .get(&dependency)
                    .ok_or(SchedulerError::UnknownTask(dependency))?;
                if !view.dependencies.contains(&dependency) {
                    return Err(SchedulerError::InvalidSnapshot(
                        "blocked reason must reference a direct dependency",
                    ));
                }
                if !matches!(
                    view.state,
                    TaskState::Blocked | TaskState::Paused | TaskState::Cancelled
                ) {
                    return Err(SchedulerError::InvalidSnapshot(
                        "blocked reason requires a blocked, paused, or cancelled task",
                    ));
                }
                let dependency_task = tasks.get(&dependency).unwrap();
                let matches_dependency_state = match reason {
                    BlockedReason::DependencyFailed(_) => {
                        matches!(
                            dependency_task.view.state,
                            TaskState::Failed | TaskState::Cancelled
                        ) || matches!(dependency_task.view.state, TaskState::Blocked)
                            && matches!(
                                dependency_task.view.blocked_reason,
                                Some(BlockedReason::DependencyFailed(_))
                            )
                    }
                    BlockedReason::DependencyUnknown(_) => {
                        dependency_task.view.state == TaskState::Unknown
                            || matches!(dependency_task.view.state, TaskState::Blocked)
                                && matches!(
                                    dependency_task.view.blocked_reason,
                                    Some(BlockedReason::DependencyUnknown(_))
                                )
                    }
                };
                if !matches_dependency_state {
                    return Err(SchedulerError::InvalidSnapshot(
                        "blocked reason does not match dependency state",
                    ));
                }
            }
        }

        let mut dependents = BTreeMap::<TaskId, BTreeSet<TaskId>>::new();
        let mut graph = BTreeMap::<TaskId, Vec<TaskId>>::new();
        for view in &snapshot.tasks {
            graph.insert(view.id, view.dependencies.clone());
            let mut unique = BTreeSet::new();
            for dependency in &view.dependencies {
                if !unique.insert(*dependency) {
                    return Err(SchedulerError::InvalidSnapshot(
                        "task dependencies must be unique",
                    ));
                }
                let dependency_task = tasks
                    .get(dependency)
                    .ok_or(SchedulerError::UnknownTask(*dependency))?;
                if dependency_task.view.kind == TaskKind::NativeChild {
                    return Err(SchedulerError::InvalidSnapshot(
                        "root dependencies cannot target native children",
                    ));
                }
                dependents.entry(*dependency).or_default().insert(view.id);
            }
        }
        fn visit(
            id: TaskId,
            graph: &BTreeMap<TaskId, Vec<TaskId>>,
            visiting: &mut BTreeSet<TaskId>,
            done: &mut BTreeSet<TaskId>,
        ) -> bool {
            if done.contains(&id) {
                return true;
            }
            if !visiting.insert(id) {
                return false;
            }
            for dependency in &graph[&id] {
                if !visit(*dependency, graph, visiting, done) {
                    return false;
                }
            }
            visiting.remove(&id);
            done.insert(id);
            true
        }
        let mut done = BTreeSet::new();
        for id in graph.keys() {
            if !visit(*id, &graph, &mut BTreeSet::new(), &mut done) {
                return Err(SchedulerError::Cycle);
            }
        }

        let expected_ready: BTreeSet<_> = tasks
            .values()
            .filter(|task| {
                task.view.kind == TaskKind::RootTurn && task.view.state == TaskState::Ready
            })
            .map(|task| task.view.id)
            .collect();
        let mut actual_ready = BTreeSet::new();
        for id in &snapshot.ready_roots {
            if !actual_ready.insert(*id) {
                return Err(SchedulerError::InvalidSnapshot(
                    "ready root ids must be unique",
                ));
            }
            let task = tasks.get(id).ok_or(SchedulerError::UnknownTask(*id))?;
            if task.view.kind != TaskKind::RootTurn || task.view.state != TaskState::Ready {
                return Err(SchedulerError::InvalidSnapshot(
                    "ready root list does not match task state",
                ));
            }
        }
        if actual_ready != expected_ready {
            return Err(SchedulerError::InvalidSnapshot(
                "ready root list does not match task state",
            ));
        }
        let active_root = if let Some(attempt) = snapshot.active_root {
            let task = tasks
                .get(&attempt.task)
                .ok_or(SchedulerError::UnknownTask(attempt.task))?;
            if task.view.kind != TaskKind::RootTurn
                || task.view.attempt != attempt.attempt
                || !task.view.state.active()
            {
                return Err(SchedulerError::InvalidSnapshot("active root is invalid"));
            }
            Some(attempt)
        } else {
            None
        };
        let active_roots: Vec<_> = tasks
            .values()
            .filter(|task| task.view.kind == TaskKind::RootTurn && task.view.state.active())
            .map(|task| TaskAttempt {
                task: task.view.id,
                attempt: task.view.attempt,
            })
            .collect();
        if active_roots.len() > 1 {
            return Err(SchedulerError::InvalidSnapshot(
                "multiple root tasks are active",
            ));
        }
        if active_root.is_none() && !active_roots.is_empty()
            || active_root.is_some() && active_roots != vec![active_root.unwrap()]
        {
            return Err(SchedulerError::InvalidSnapshot(
                "active root does not match root task states",
            ));
        }
        if snapshot.disconnected && active_root.is_some() {
            return Err(SchedulerError::InvalidSnapshot(
                "disconnected workflow cannot have an active root",
            ));
        }
        let root_slots_reserved = tasks
            .values()
            .filter(|task| task.view.root_slot_reserved)
            .count();
        if root_slots_reserved != snapshot.root_slots_reserved {
            return Err(SchedulerError::InvalidSnapshot(
                "root slot count does not match task state",
            ));
        }
        let native_turns_observed = tasks
            .values()
            .filter(|task| {
                task.view.kind == TaskKind::NativeChild
                    && task.view.external.is_some()
                    && task.view.state.active()
            })
            .count();
        if native_turns_observed != snapshot.native_turns_observed {
            return Err(SchedulerError::InvalidSnapshot(
                "native turn count does not match task state",
            ));
        }
        let queued_roots = tasks
            .values()
            .filter(|task| task.view.kind == TaskKind::RootTurn && task.view.state.unstarted())
            .count();
        if queued_roots != snapshot.queued_roots {
            return Err(SchedulerError::InvalidSnapshot(
                "queued root count does not match task state",
            ));
        }

        Ok(Self {
            tasks,
            dependents,
            children,
            next_id,
            next_seq,
            active_root,
            paused: snapshot.paused,
            stopping: snapshot.stopping,
            disconnected: snapshot.disconnected,
        })
    }

    pub fn task(&self, id: TaskId) -> Option<&TaskSnapshot> {
        self.tasks.get(&id).map(|task| &task.view)
    }
    pub fn active_root(&self) -> Option<TaskAttempt> {
        self.active_root
    }
    pub fn child_task(&self, thread: &str) -> Option<&TaskSnapshot> {
        self.children.get(thread).and_then(|id| self.task(*id))
    }

    /// Atomic batch creation permits forward references within a plan, with no partial dispatch.
    pub fn enqueue(&mut self, specs: Vec<RootTaskSpec>) -> Result<Vec<TaskId>, SchedulerError> {
        if self.disconnected {
            return Err(SchedulerError::Disconnected);
        }
        if self.stopping {
            return Err(SchedulerError::Stopped);
        }
        if specs.is_empty() || self.pending_count() + specs.len() > ROOT_QUEUE_LIMIT {
            return Err(SchedulerError::QueueFull);
        }
        if self.tasks.len() + specs.len() > TASK_LIMIT {
            return Err(SchedulerError::Limit);
        }
        let mut next = self.next_id;
        let mut ids = BTreeSet::new();
        for spec in &specs {
            if let Some(id) = spec.id {
                if id.0 == 0 || id.0 == u64::MAX || self.tasks.contains_key(&id) || !ids.insert(id)
                {
                    return Err(SchedulerError::InvalidId);
                }
                next = next.max(id.0 + 1);
            }
        }
        let mut proposed = Vec::new();
        for spec in specs {
            if spec.text.trim().is_empty() || spec.text.len() > MESSAGE_BYTES {
                return Err(SchedulerError::InvalidText);
            }
            if spec.priority.abs_diff(0) > 1000 || spec.dependencies.len() > TASK_LIMIT {
                return Err(SchedulerError::InvalidPolicy);
            }
            if matches!(spec.policy, DependencyPolicy::Any) && spec.dependencies.is_empty()
                || matches!(spec.policy, DependencyPolicy::Quorum(n) if n == 0 || n > spec.dependencies.iter().collect::<BTreeSet<_>>().len())
            {
                return Err(SchedulerError::InvalidPolicy);
            }
            let id = if let Some(id) = spec.id {
                id
            } else {
                while self.tasks.contains_key(&TaskId(next)) || ids.contains(&TaskId(next)) {
                    next = next.checked_add(1).ok_or(SchedulerError::InvalidId)?;
                }
                let id = TaskId(next);
                next = next.checked_add(1).ok_or(SchedulerError::InvalidId)?;
                ids.insert(id);
                id
            };
            proposed.push((id, spec));
        }
        let mut graph: BTreeMap<TaskId, Vec<TaskId>> = self
            .tasks
            .iter()
            .map(|(id, task)| (*id, task.view.dependencies.clone()))
            .collect();
        for (id, spec) in &proposed {
            graph.insert(*id, spec.dependencies.clone());
        }
        for (_, spec) in &proposed {
            for dependency in &spec.dependencies {
                if !graph.contains_key(dependency) {
                    return Err(SchedulerError::UnknownTask(*dependency));
                }
                if self
                    .task(*dependency)
                    .is_some_and(|task| task.kind == TaskKind::NativeChild)
                {
                    // Native generations are captured by Gate, never by mutable DAG edges.
                    return Err(SchedulerError::InvalidPolicy);
                }
            }
        }
        fn visit(
            id: TaskId,
            graph: &BTreeMap<TaskId, Vec<TaskId>>,
            visiting: &mut BTreeSet<TaskId>,
            done: &mut BTreeSet<TaskId>,
        ) -> bool {
            if done.contains(&id) {
                return true;
            }
            if !visiting.insert(id) {
                return false;
            }
            for dependency in &graph[&id] {
                if !visit(*dependency, graph, visiting, done) {
                    return false;
                }
            }
            visiting.remove(&id);
            done.insert(id);
            true
        }
        let mut done = BTreeSet::new();
        for id in graph.keys() {
            if !visit(*id, &graph, &mut BTreeSet::new(), &mut done) {
                return Err(SchedulerError::Cycle);
            }
        }
        let assigned: Vec<_> = proposed.iter().map(|(id, _)| *id).collect();
        self.next_id = next;
        for (id, spec) in proposed {
            self.next_seq += 1;
            let dependencies: Vec<_> = spec
                .dependencies
                .into_iter()
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect();
            for dependency in &dependencies {
                self.dependents.entry(*dependency).or_default().insert(id);
            }
            let title = spec
                .text
                .lines()
                .next()
                .unwrap_or("")
                .chars()
                .take(60)
                .collect();
            self.tasks.insert(
                id,
                Task {
                    view: TaskSnapshot {
                        id,
                        kind: TaskKind::RootTurn,
                        state: TaskState::Queued,
                        title,
                        parent: None,
                        attempt: 1,
                        dependencies,
                        policy: spec.policy,
                        failure: spec.failure,
                        priority: spec.priority,
                        blocked_reason: None,
                        external: None,
                        pause_requested: false,
                        cancel_requested: false,
                        pending_requests: 0,
                        wait_targets: Vec::new(),
                        root_slot_reserved: false,
                        cancellation_epoch: 0,
                    },
                    text: Some(spec.text),
                    queued_at: Instant::now(),
                    ready_seq: self.next_seq,
                    waiting_children: false,
                },
            );
        }
        self.refresh(assigned.clone());
        Ok(assigned)
    }

    pub fn pending_count(&self) -> usize {
        self.tasks
            .values()
            .filter(|task| task.view.kind == TaskKind::RootTurn && task.view.state.unstarted())
            .count()
    }

    pub fn last_pending_root(&self) -> Option<TaskId> {
        self.tasks
            .values()
            .filter(|task| task.view.kind == TaskKind::RootTurn && task.view.state.unstarted())
            .max_by_key(|task| task.ready_seq)
            .map(|task| task.view.id)
    }

    fn ordered_ready(&self) -> Vec<TaskId> {
        let now = Instant::now();
        let mut ready: Vec<_> = self
            .tasks
            .values()
            .filter(|task| {
                task.view.kind == TaskKind::RootTurn && task.view.state == TaskState::Ready
            })
            .collect();
        let score = |task: &Task| {
            i64::from(task.view.priority).saturating_add(
                i64::try_from(now.saturating_duration_since(task.queued_at).as_secs() / 30)
                    .unwrap_or(i64::MAX),
            )
        };
        ready.sort_by(|left, right| {
            score(right)
                .cmp(&score(left))
                .then(left.ready_seq.cmp(&right.ready_seq))
        });
        ready.into_iter().map(|task| task.view.id).collect()
    }

    pub fn dispatch(&mut self) -> Option<RootDispatch> {
        if self.active_root.is_some() || self.paused || self.stopping || self.disconnected {
            return None;
        }
        let id = self.ordered_ready().first().copied()?;
        let task = self.tasks.get_mut(&id)?;
        let text = task.text.clone()?;
        task.view.state = TaskState::Starting;
        task.view.root_slot_reserved = true;
        let attempt = TaskAttempt {
            task: id,
            attempt: task.view.attempt,
        };
        self.active_root = Some(attempt);
        Some(RootDispatch { attempt, text })
    }

    pub fn started_root(
        &mut self,
        attempt: TaskAttempt,
        external: ExternalTurn,
    ) -> Result<(), SchedulerError> {
        if self.active_root != Some(attempt) {
            return Err(SchedulerError::Stale);
        }
        let task = self
            .tasks
            .get_mut(&attempt.task)
            .ok_or(SchedulerError::Stale)?;
        if task.view.attempt != attempt.attempt
            || task
                .view
                .external
                .as_ref()
                .is_some_and(|old| old != &external)
        {
            return Err(SchedulerError::Stale);
        }
        task.view.external = Some(external);
        if !task.view.cancel_requested {
            task.view.state = TaskState::Running;
        }
        Ok(())
    }

    pub fn finish(&mut self, attempt: TaskAttempt, outcome: ChildOutcome) -> bool {
        let Some(task) = self.tasks.get_mut(&attempt.task) else {
            return false;
        };
        if task.view.attempt != attempt.attempt || !task.view.state.active() {
            return false;
        }
        task.view.state = match outcome {
            ChildOutcome::Completed => TaskState::Succeeded,
            ChildOutcome::Failed => TaskState::Failed,
            ChildOutcome::Interrupted => TaskState::Cancelled,
        };
        task.view.root_slot_reserved = false;
        task.view.pending_requests = 0;
        task.waiting_children = false;
        if outcome == ChildOutcome::Completed {
            task.text = None;
        }
        if self.active_root == Some(attempt) {
            self.active_root = None;
        }
        self.refresh(
            self.dependents
                .get(&attempt.task)
                .map(|tasks| tasks.iter().copied().collect())
                .unwrap_or_default(),
        );
        true
    }

    pub fn register_child(
        &mut self,
        thread: &str,
        parent: Option<TaskAttempt>,
        title: &str,
    ) -> Result<TaskId, SchedulerError> {
        if self.disconnected {
            return Err(SchedulerError::Disconnected);
        }
        if let Some(id) = self.children.get(thread).copied() {
            self.tasks.get_mut(&id).unwrap().view.title = title.chars().take(60).collect();
            if let Some(parent) = parent {
                self.tasks.get_mut(&id).unwrap().view.parent = Some(parent.task);
            }
            return Ok(id);
        }
        if self.tasks.len() >= TASK_LIMIT {
            return Err(SchedulerError::Limit);
        }
        if thread.is_empty() || thread.len() > 1024 {
            return Err(SchedulerError::InvalidId);
        }
        let id = TaskId(self.next_id);
        self.next_id = self
            .next_id
            .checked_add(1)
            .ok_or(SchedulerError::InvalidId)?;
        self.children.insert(thread.into(), id);
        self.next_seq += 1;
        self.tasks.insert(
            id,
            Task {
                view: TaskSnapshot {
                    id,
                    kind: TaskKind::NativeChild,
                    state: TaskState::Starting,
                    title: title.chars().take(60).collect(),
                    parent: parent.map(|parent| parent.task),
                    attempt: 0,
                    dependencies: Vec::new(),
                    policy: DependencyPolicy::AllRequired,
                    failure: FailurePolicy::FailFast,
                    priority: 0,
                    blocked_reason: None,
                    external: None,
                    pause_requested: false,
                    cancel_requested: self.stopping,
                    pending_requests: 0,
                    wait_targets: Vec::new(),
                    root_slot_reserved: false,
                    cancellation_epoch: 0,
                },
                text: None,
                queued_at: Instant::now(),
                ready_seq: self.next_seq,
                waiting_children: false,
            },
        );
        Ok(id)
    }

    pub fn child_event(&mut self, event: &GateEvent) -> Result<(), SchedulerError> {
        if self.disconnected {
            return Err(SchedulerError::Disconnected);
        }
        let id = *self
            .children
            .get(&event.target)
            .ok_or(SchedulerError::InvalidId)?;
        let task = self.tasks.get_mut(&id).unwrap();
        if event.outcome.is_none() {
            if event.generation <= task.view.attempt {
                return Ok(());
            }
            let deferred_cancel = task.view.cancel_requested && task.view.external.is_none();
            task.view.attempt = event.generation;
            task.view.external = Some(ExternalTurn {
                thread_id: event.target.clone(),
                turn_id: event.turn_id.clone(),
                generation: event.generation,
            });
            task.view.cancel_requested = deferred_cancel || self.stopping;
            task.view.state = if task.view.cancel_requested {
                TaskState::Cancelling
            } else {
                TaskState::Running
            };
            task.view.pending_requests = 0;
            self.refresh(
                self.dependents
                    .get(&id)
                    .map(|tasks| tasks.iter().copied().collect())
                    .unwrap_or_default(),
            );
        } else if task.view.attempt == event.generation
            && task
                .view
                .external
                .as_ref()
                .is_some_and(|external| external.turn_id == event.turn_id)
        {
            self.finish(
                TaskAttempt {
                    task: id,
                    attempt: event.generation,
                },
                event.outcome.clone().unwrap(),
            );
        }
        Ok(())
    }

    pub fn wait_for_children(
        &mut self,
        attempt: TaskAttempt,
        targets: &[WaitTarget],
    ) -> Result<(), SchedulerError> {
        if self.active_root != Some(attempt) {
            return Err(SchedulerError::Stale);
        }
        let mut refs = Vec::new();
        for target in targets {
            refs.push(TaskAttempt {
                task: *self
                    .children
                    .get(&target.id)
                    .ok_or(SchedulerError::InvalidId)?,
                attempt: target.generation,
            });
        }
        let task = self.tasks.get_mut(&attempt.task).unwrap();
        task.view.wait_targets = refs;
        task.waiting_children = true;
        task.view.state = TaskState::WaitingChildren;
        task.view.root_slot_reserved = false;
        Ok(())
    }

    pub fn can_release_wait(&self, attempt: TaskAttempt) -> bool {
        !self.paused
            && !self.stopping
            && self.active_root == Some(attempt)
            && self.tasks.get(&attempt.task).is_some_and(|task| {
                task.view.attempt == attempt.attempt
                    && !task.view.pause_requested
                    && !task.view.cancel_requested
            })
    }

    pub fn released_wait(&mut self, attempt: TaskAttempt) -> Result<(), SchedulerError> {
        if self.active_root != Some(attempt) {
            return Err(SchedulerError::Stale);
        }
        let task = self.tasks.get_mut(&attempt.task).unwrap();
        task.waiting_children = false;
        task.view.state = if task.view.pending_requests == 0 {
            TaskState::Running
        } else {
            TaskState::WaitingApproval
        };
        task.view.root_slot_reserved = task.view.pending_requests == 0;
        Ok(())
    }

    pub fn interactions(&mut self, requests: &[RequestView]) {
        for task in self
            .tasks
            .values_mut()
            .filter(|task| task.view.state.active())
        {
            task.view.pending_requests = task.view.external.as_ref().map_or(0, |external| {
                requests
                    .iter()
                    .filter(|request| {
                        request.thread_id == external.thread_id
                            && request.turn_id == external.turn_id
                    })
                    .count()
            });
            if task.view.cancel_requested {
                task.view.state = TaskState::Cancelling;
            } else if task.waiting_children {
                task.view.state = TaskState::WaitingChildren;
            } else if task.view.external.is_some() {
                task.view.state = if task.view.pending_requests > 0 {
                    TaskState::WaitingApproval
                } else {
                    TaskState::Running
                };
            }
            if task.view.kind == TaskKind::RootTurn {
                task.view.root_slot_reserved =
                    !task.waiting_children && task.view.pending_requests == 0;
            }
        }
    }

    pub fn command(
        &mut self,
        command: SchedulerCommand,
    ) -> Result<Vec<InterruptEffect>, SchedulerError> {
        if self.disconnected {
            return Err(SchedulerError::Disconnected);
        }
        if self.stopping && command != SchedulerCommand::StopWorkflow {
            return Err(SchedulerError::Stopped);
        }
        match command {
            SchedulerCommand::PauseWorkflow => {
                self.paused = true;
                Ok(Vec::new())
            }
            SchedulerCommand::ResumeWorkflow => {
                self.paused = false;
                Ok(Vec::new())
            }
            SchedulerCommand::StopWorkflow => {
                self.stopping = true;
                self.paused = true;
                let ids: Vec<_> = self.tasks.keys().copied().collect();
                let mut effects = Vec::new();
                for id in ids {
                    if self.tasks[&id].view.state.active() || self.tasks[&id].view.state.unstarted()
                    {
                        effects.extend(self.cancel(id)?);
                    }
                }
                Ok(effects)
            }
            SchedulerCommand::Cancel(id) => self.cancel(id),
            SchedulerCommand::Pause(id) | SchedulerCommand::Resume(id) => {
                let paused = matches!(command, SchedulerCommand::Pause(_));
                let task = self
                    .tasks
                    .get_mut(&id)
                    .ok_or(SchedulerError::UnknownTask(id))?;
                if task.view.kind != TaskKind::RootTurn
                    || !task.view.state.active() && !task.view.state.unstarted()
                {
                    return Err(SchedulerError::InvalidState);
                }
                task.view.pause_requested = paused;
                self.refresh(vec![id]);
                Ok(Vec::new())
            }
            SchedulerCommand::Retry(id) => {
                if self.pending_count() >= ROOT_QUEUE_LIMIT {
                    return Err(SchedulerError::QueueFull);
                }
                let task = self
                    .tasks
                    .get_mut(&id)
                    .ok_or(SchedulerError::UnknownTask(id))?;
                if task.view.kind != TaskKind::RootTurn {
                    return Err(SchedulerError::NativeRetryUnsupported);
                }
                if !matches!(task.view.state, TaskState::Failed | TaskState::Cancelled)
                    || task.text.is_none()
                {
                    return Err(SchedulerError::InvalidState);
                }
                task.view.attempt = task
                    .view
                    .attempt
                    .checked_add(1)
                    .ok_or(SchedulerError::InvalidId)?;
                task.view.external = None;
                task.view.state = TaskState::Queued;
                task.view.pause_requested = false;
                task.view.cancel_requested = false;
                task.view.wait_targets.clear();
                task.queued_at = Instant::now();
                self.next_seq += 1;
                task.ready_seq = self.next_seq;
                self.refresh(vec![id]);
                Ok(Vec::new())
            }
            SchedulerCommand::Reprioritize { task_id, priority } => {
                if priority.abs_diff(0) > 1000 {
                    return Err(SchedulerError::InvalidPolicy);
                }
                let task = self
                    .tasks
                    .get_mut(&task_id)
                    .ok_or(SchedulerError::UnknownTask(task_id))?;
                if task.view.kind != TaskKind::RootTurn || !task.view.state.unstarted() {
                    return Err(SchedulerError::InvalidState);
                }
                task.view.priority = priority;
                Ok(Vec::new())
            }
        }
    }

    fn cancel(&mut self, id: TaskId) -> Result<Vec<InterruptEffect>, SchedulerError> {
        let task = self
            .tasks
            .get_mut(&id)
            .ok_or(SchedulerError::UnknownTask(id))?;
        if task.view.cancel_requested {
            return Ok(Vec::new());
        }
        if task.view.state.unstarted() && task.view.kind == TaskKind::RootTurn {
            task.view.state = TaskState::Cancelled;
            task.view.cancellation_epoch += 1;
            self.refresh(
                self.dependents
                    .get(&id)
                    .map(|tasks| tasks.iter().copied().collect())
                    .unwrap_or_default(),
            );
            return Ok(Vec::new());
        }
        if !task.view.state.active() {
            return Err(SchedulerError::InvalidState);
        }
        task.view.cancellation_epoch += 1;
        task.view.cancel_requested = true;
        task.view.state = TaskState::Cancelling;
        Ok(vec![InterruptEffect {
            attempt: TaskAttempt {
                task: id,
                attempt: task.view.attempt,
            },
            external: task.view.external.clone(),
            kind: task.view.kind,
        }])
    }

    pub fn interrupt_rejected(&mut self, attempt: TaskAttempt) {
        if let Some(task) = self
            .tasks
            .get_mut(&attempt.task)
            .filter(|task| task.view.attempt == attempt.attempt && task.view.state.active())
        {
            task.view.cancel_requested = false;
            task.view.state = if task.waiting_children {
                TaskState::WaitingChildren
            } else if task.view.pending_requests > 0 {
                TaskState::WaitingApproval
            } else if task.view.external.is_some() {
                TaskState::Running
            } else {
                TaskState::Starting
            };
        }
    }

    pub fn disconnected(&mut self) {
        self.disconnected = true;
        self.paused = true;
        self.active_root = None;
        let mut changed = Vec::new();
        for task in self
            .tasks
            .values_mut()
            .filter(|task| task.view.state.active())
        {
            task.view.state = TaskState::Unknown;
            task.view.root_slot_reserved = false;
            changed.push(task.view.id);
        }
        let affected = changed
            .iter()
            .flat_map(|id| self.dependents.get(id).into_iter().flatten().copied())
            .collect();
        self.refresh(affected);
    }

    pub fn snapshot(&self) -> SchedulerSnapshot {
        SchedulerSnapshot {
            tasks: self.tasks.values().map(|task| task.view.clone()).collect(),
            ready_roots: self.ordered_ready(),
            active_root: self.active_root,
            paused: self.paused,
            stopping: self.stopping,
            disconnected: self.disconnected,
            queued_roots: self.pending_count(),
            root_slots_reserved: self
                .tasks
                .values()
                .filter(|task| task.view.root_slot_reserved)
                .count(),
            native_turns_observed: self
                .tasks
                .values()
                .filter(|task| {
                    task.view.kind == TaskKind::NativeChild
                        && task.view.external.is_some()
                        && task.view.state.active()
                })
                .count(),
        }
    }

    fn refresh(&mut self, ids: Vec<TaskId>) {
        let mut queue: VecDeque<_> = ids.into();
        while let Some(id) = queue.pop_front() {
            let task = &self.tasks[&id].view;
            if task.kind != TaskKind::RootTurn || !task.state.unstarted() {
                continue;
            }
            let mut success = 0;
            let mut terminal = 0;
            let mut failed = None;
            let mut unknown = None;
            let mut unreachable = 0;
            for dependency in &task.dependencies {
                let other = &self.tasks[dependency].view;
                match other.state {
                    TaskState::Succeeded => {
                        success += 1;
                        terminal += 1;
                    }
                    TaskState::Failed | TaskState::Cancelled => {
                        failed = Some(*dependency);
                        terminal += 1;
                    }
                    TaskState::Unknown => unknown = Some(*dependency),
                    TaskState::Blocked => match other.blocked_reason {
                        Some(BlockedReason::DependencyFailed(_)) => {
                            failed = Some(*dependency);
                            unreachable += 1;
                        }
                        Some(BlockedReason::DependencyUnknown(_)) => unknown = Some(*dependency),
                        None => {}
                    },
                    _ => {}
                }
            }
            let count = task.dependencies.len();
            let satisfied = match task.policy {
                DependencyPolicy::AllRequired => {
                    if task.failure == FailurePolicy::FailFast {
                        success == count
                    } else {
                        terminal == count
                    }
                }
                DependencyPolicy::Any => terminal > 0,
                DependencyPolicy::Quorum(n) => {
                    success >= n || task.failure == FailurePolicy::Optional && terminal >= n
                }
                DependencyPolicy::CollectAll => terminal == count,
            };
            let blocked = if satisfied {
                None
            } else if let Some(id) = unknown {
                Some(BlockedReason::DependencyUnknown(id))
            } else if failed.is_some()
                && (matches!(
                    task.policy,
                    DependencyPolicy::AllRequired | DependencyPolicy::CollectAll
                ) && unreachable > 0
                    || task.failure == FailurePolicy::FailFast
                        && task.policy == DependencyPolicy::AllRequired
                    || matches!(task.policy, DependencyPolicy::Quorum(n) if (if task.failure == FailurePolicy::Optional {count - unreachable} else {success + count - terminal - unreachable}) < n)
                    || terminal + unreachable == count)
            {
                failed.map(BlockedReason::DependencyFailed)
            } else {
                None
            };
            let state = if task.pause_requested {
                TaskState::Paused
            } else if satisfied {
                TaskState::Ready
            } else if blocked.is_some() {
                TaskState::Blocked
            } else {
                TaskState::Queued
            };
            let changed = task.state != state || task.blocked_reason != blocked;
            let task = self.tasks.get_mut(&id).unwrap();
            task.view.state = state;
            task.view.blocked_reason = blocked;
            if changed {
                if let Some(dependents) = self.dependents.get(&id) {
                    queue.extend(dependents.iter().copied());
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn task(
        id: u64,
        deps: &[u64],
        policy: DependencyPolicy,
        failure: FailurePolicy,
    ) -> RootTaskSpec {
        RootTaskSpec {
            id: Some(TaskId(id)),
            text: format!("task {id}"),
            dependencies: deps.iter().map(|id| TaskId(*id)).collect(),
            policy,
            failure,
            priority: 0,
        }
    }
    fn simple(id: u64, deps: &[u64]) -> RootTaskSpec {
        task(
            id,
            deps,
            DependencyPolicy::AllRequired,
            FailurePolicy::FailFast,
        )
    }

    #[test]
    fn batch_creation_rejects_unknown_edges_and_cycles_atomically() {
        let mut s = Scheduler::default();
        assert_eq!(
            s.enqueue(vec![simple(1, &[2]), simple(2, &[1])]),
            Err(SchedulerError::Cycle)
        );
        assert!(s.snapshot().tasks.is_empty());
        assert_eq!(
            s.enqueue(vec![simple(1, &[99])]),
            Err(SchedulerError::UnknownTask(TaskId(99)))
        );
        assert!(s.snapshot().tasks.is_empty());
        s.enqueue(vec![simple(2, &[1]), simple(1, &[])]).unwrap();
        assert_eq!(s.dispatch().unwrap().attempt.task, TaskId(1));
        assert!(s.dispatch().is_none());
        assert!(s.finish(
            TaskAttempt {
                task: TaskId(1),
                attempt: 1
            },
            ChildOutcome::Completed
        ));
        assert_eq!(s.dispatch().unwrap().attempt.task, TaskId(2));
    }

    #[test]
    fn failure_policies_collect_results_and_keep_failfast_dependents_blocked() {
        let mut s = Scheduler::default();
        s.enqueue(vec![
            simple(1, &[]),
            simple(2, &[1]),
            task(
                3,
                &[1],
                DependencyPolicy::CollectAll,
                FailurePolicy::ContinueWithErrors,
            ),
            task(4, &[1], DependencyPolicy::Any, FailurePolicy::Optional),
        ])
        .unwrap();
        let root = s.dispatch().unwrap().attempt;
        s.finish(root, ChildOutcome::Failed);
        assert_eq!(
            s.task(TaskId(2)).unwrap().blocked_reason,
            Some(BlockedReason::DependencyFailed(TaskId(1)))
        );
        assert_eq!(s.snapshot().ready_roots, vec![TaskId(3), TaskId(4)]);
        s.command(SchedulerCommand::Retry(TaskId(1))).unwrap();
        assert_eq!(s.task(TaskId(1)).unwrap().attempt, 2);
        assert!(!s.finish(root, ChildOutcome::Completed));
        assert_eq!(s.task(TaskId(2)).unwrap().state, TaskState::Queued);
    }

    #[tokio::test(start_paused = true)]
    async fn priority_aging_and_fifo_choose_the_next_ready_task_without_skipping_dependencies() {
        let mut s = Scheduler::default();
        s.enqueue(vec![simple(1, &[])]).unwrap();
        tokio::time::advance(std::time::Duration::from_secs(60)).await;
        let mut new = simple(2, &[]);
        new.priority = 1;
        let mut blocked = simple(3, &[2]);
        blocked.priority = 1000;
        s.enqueue(vec![new, blocked]).unwrap();
        assert_eq!(s.dispatch().unwrap().attempt.task, TaskId(1));
        s.finish(
            TaskAttempt {
                task: TaskId(1),
                attempt: 1,
            },
            ChildOutcome::Completed,
        );
        assert_eq!(s.dispatch().unwrap().attempt.task, TaskId(2));
    }

    #[test]
    fn gate_releases_the_root_reservation_but_never_dispatches_another_root() {
        let mut s = Scheduler::default();
        s.enqueue(vec![simple(1, &[]), simple(2, &[])]).unwrap();
        let root = s.dispatch().unwrap().attempt;
        s.started_root(
            root,
            ExternalTurn {
                thread_id: "root".into(),
                turn_id: "t".into(),
                generation: 1,
            },
        )
        .unwrap();
        s.register_child("a", Some(root), "a").unwrap();
        s.wait_for_children(
            root,
            &[WaitTarget {
                id: "a".into(),
                generation: 1,
                turn_id: None,
                outcome: None,
            }],
        )
        .unwrap();
        assert_eq!(s.snapshot().root_slots_reserved, 0);
        assert!(s.dispatch().is_none());
        s.command(SchedulerCommand::PauseWorkflow).unwrap();
        assert!(!s.can_release_wait(root));
        s.command(SchedulerCommand::ResumeWorkflow).unwrap();
        assert!(s.can_release_wait(root));
        s.released_wait(root).unwrap();
        assert_eq!(s.snapshot().root_slots_reserved, 1);
        s.finish(root, ChildOutcome::Completed);
        let next = s.dispatch().unwrap().attempt;
        assert!(!s.finish(root, ChildOutcome::Interrupted));
        assert_eq!(s.snapshot().active_root, Some(next));
        assert_eq!(s.snapshot().root_slots_reserved, 1);
    }

    #[test]
    fn cancellation_ack_does_not_complete_a_turn_and_disconnect_never_fabricates_success() {
        let mut s = Scheduler::default();
        s.enqueue(vec![simple(1, &[]), simple(2, &[1])]).unwrap();
        let root = s.dispatch().unwrap().attempt;
        let effects = s.command(SchedulerCommand::Cancel(root.task)).unwrap();
        assert_eq!(effects.len(), 1);
        assert_eq!(s.task(root.task).unwrap().state, TaskState::Cancelling);
        assert!(s
            .command(SchedulerCommand::Cancel(root.task))
            .unwrap()
            .is_empty());
        s.interrupt_rejected(root);
        assert_eq!(s.task(root.task).unwrap().state, TaskState::Starting);
        s.disconnected();
        assert_eq!(s.task(root.task).unwrap().state, TaskState::Unknown);
        assert_eq!(
            s.task(TaskId(2)).unwrap().blocked_reason,
            Some(BlockedReason::DependencyUnknown(root.task))
        );
        assert!(s.dispatch().is_none());
        assert_eq!(
            s.command(SchedulerCommand::Retry(root.task)),
            Err(SchedulerError::Disconnected)
        );
    }

    #[test]
    fn impossible_quorum_and_blocked_successors_propagate_without_becoming_ready() {
        let mut s = Scheduler::default();
        s.enqueue(vec![
            simple(1, &[]),
            simple(2, &[1]),
            task(
                3,
                &[2],
                DependencyPolicy::CollectAll,
                FailurePolicy::ContinueWithErrors,
            ),
            simple(4, &[]),
            task(
                5,
                &[1, 4],
                DependencyPolicy::Quorum(2),
                FailurePolicy::FailFast,
            ),
            task(6, &[1, 4], DependencyPolicy::Any, FailurePolicy::FailFast),
        ])
        .unwrap();
        let first = s.dispatch().unwrap().attempt;
        s.finish(first, ChildOutcome::Failed);
        for id in [2, 3, 5] {
            assert_eq!(s.task(TaskId(id)).unwrap().state, TaskState::Blocked);
        }
        assert_eq!(s.task(TaskId(6)).unwrap().state, TaskState::Ready);
        s.command(SchedulerCommand::Retry(TaskId(1))).unwrap();
        for id in [2, 3, 5, 6] {
            assert_eq!(s.task(TaskId(id)).unwrap().state, TaskState::Queued);
        }
    }

    #[test]
    fn plans_are_validated_before_execution_and_native_generations_cannot_be_dag_dependencies() {
        for bytes in [
            br#"{"tasks":[]}"#.as_slice(),
            br#"{"tasks":[{"id":1,"text":"a","dependencies":[1]}]}"#,
            br#"{"tasks":[{"text":"a","policy":{"quorum":0}}]}"#,
            br#"{"tasks":[{"text":"a","timeout":10}]}"#,
        ] {
            assert!(WorkflowPlan::parse(bytes).is_err());
        }
        assert!(WorkflowPlan::parse(
            br#"{"tasks":[{"id":2,"text":"b","dependencies":[1]},{"id":1,"text":"a"}]}"#
        )
        .is_ok());
        let mut s = Scheduler::default();
        let child = s.register_child("a", None, "a").unwrap();
        let mut root = simple(2, &[]);
        root.dependencies.push(child);
        assert_eq!(s.enqueue(vec![root]), Err(SchedulerError::InvalidPolicy));
    }

    #[test]
    fn restore_accepts_a_valid_dag_but_never_dispatches_without_task_text() {
        let mut original = Scheduler::default();
        original
            .enqueue(vec![simple(1, &[]), simple(2, &[1])])
            .unwrap();
        let mut restored = Scheduler::restore(&original.snapshot()).unwrap();
        assert_eq!(restored.snapshot().tasks.len(), 2);
        assert!(restored.dispatch().is_none());
    }

    #[test]
    fn restore_rejects_missing_dependencies_duplicate_ids_and_invalid_active_root() {
        let mut original = Scheduler::default();
        original.enqueue(vec![simple(1, &[])]).unwrap();

        let mut missing = original.snapshot();
        missing.tasks[0].dependencies = vec![TaskId(99)];
        assert!(matches!(
            Scheduler::restore(&missing),
            Err(SchedulerError::UnknownTask(TaskId(99)))
        ));

        let mut duplicate = original.snapshot();
        duplicate.tasks.push(duplicate.tasks[0].clone());
        duplicate.queued_roots = 2;
        assert!(matches!(
            Scheduler::restore(&duplicate),
            Err(SchedulerError::InvalidSnapshot(_))
        ));

        let mut invalid_active = original.snapshot();
        invalid_active.active_root = Some(TaskAttempt {
            task: TaskId(99),
            attempt: 1,
        });
        assert!(matches!(
            Scheduler::restore(&invalid_active),
            Err(SchedulerError::UnknownTask(TaskId(99)))
        ));
    }

    #[test]
    fn restore_rejects_inconsistent_parent_and_child_slot_facts() {
        let mut original = Scheduler::default();
        original.enqueue(vec![simple(1, &[])]).unwrap();
        let first_child = original.register_child("child-1", None, "child").unwrap();
        let second_child = original.register_child("child-2", None, "child").unwrap();

        let mut wrong_parent = original.snapshot();
        wrong_parent
            .tasks
            .iter_mut()
            .find(|task| task.id == second_child)
            .unwrap()
            .parent = Some(first_child);
        assert!(matches!(
            Scheduler::restore(&wrong_parent),
            Err(SchedulerError::InvalidSnapshot(_))
        ));

        let mut reserved_child = original.snapshot();
        reserved_child
            .tasks
            .iter_mut()
            .find(|task| task.id == first_child)
            .unwrap()
            .root_slot_reserved = true;
        assert!(matches!(
            Scheduler::restore(&reserved_child),
            Err(SchedulerError::InvalidSnapshot(_))
        ));
    }

    #[test]
    fn restore_rejects_invalid_wait_targets_and_blocked_reasons() {
        let mut original = Scheduler::default();
        original
            .enqueue(vec![simple(1, &[]), simple(2, &[1]), simple(3, &[])])
            .unwrap();
        let child = original.register_child("child", None, "child").unwrap();

        let mut zero_attempt = original.snapshot();
        zero_attempt.tasks[0].wait_targets = vec![TaskAttempt {
            task: child,
            attempt: 0,
        }];
        assert!(matches!(
            Scheduler::restore(&zero_attempt),
            Err(SchedulerError::InvalidSnapshot(_))
        ));

        let mut wrong_wait_target_kind = original.snapshot();
        wrong_wait_target_kind.tasks[0].wait_targets = vec![TaskAttempt {
            task: TaskId(1),
            attempt: 1,
        }];
        assert!(matches!(
            Scheduler::restore(&wrong_wait_target_kind),
            Err(SchedulerError::InvalidSnapshot(_))
        ));

        let mut wrong_blocked_reason = original.snapshot();
        wrong_blocked_reason.tasks[1].blocked_reason =
            Some(BlockedReason::DependencyFailed(TaskId(3)));
        assert!(matches!(
            Scheduler::restore(&wrong_blocked_reason),
            Err(SchedulerError::InvalidSnapshot(_))
        ));
    }

    #[test]
    fn restore_requires_active_root_to_match_active_root_task_states() {
        let mut original = Scheduler::default();
        original
            .enqueue(vec![simple(1, &[]), simple(2, &[])])
            .unwrap();
        let active = original.dispatch().unwrap().attempt;

        let mut missing_active_root = original.snapshot();
        missing_active_root.active_root = None;
        assert!(matches!(
            Scheduler::restore(&missing_active_root),
            Err(SchedulerError::InvalidSnapshot(_))
        ));

        let mut multiple_active_roots = original.snapshot();
        multiple_active_roots.tasks[1].state = TaskState::Running;
        assert!(matches!(
            Scheduler::restore(&multiple_active_roots),
            Err(SchedulerError::InvalidSnapshot(_))
        ));

        let mut valid = original.snapshot();
        assert_eq!(
            Scheduler::restore(&valid).unwrap().active_root(),
            Some(active)
        );
        valid.tasks[0].state = TaskState::Ready;
        assert!(matches!(
            Scheduler::restore(&valid),
            Err(SchedulerError::InvalidSnapshot(_))
        ));
    }

    #[test]
    fn restore_requires_consistent_root_slots_and_blocked_reasons() {
        let mut queued = Scheduler::default();
        queued.enqueue(vec![simple(1, &[])]).unwrap();
        let mut reserved = queued.snapshot();
        reserved.tasks[0].root_slot_reserved = true;
        assert!(matches!(
            Scheduler::restore(&reserved),
            Err(SchedulerError::InvalidSnapshot(_))
        ));

        let mut failed = Scheduler::default();
        failed
            .enqueue(vec![simple(1, &[]), simple(2, &[1])])
            .unwrap();
        let attempt = failed.dispatch().unwrap().attempt;
        failed.finish(attempt, ChildOutcome::Failed);
        let valid_failed = failed.snapshot();
        assert!(Scheduler::restore(&valid_failed).is_ok());

        let mut paused_blocked = failed;
        paused_blocked
            .command(SchedulerCommand::Pause(TaskId(2)))
            .unwrap();
        assert_eq!(
            paused_blocked.task(TaskId(2)).unwrap().state,
            TaskState::Paused
        );
        assert_eq!(
            paused_blocked.task(TaskId(2)).unwrap().blocked_reason,
            Some(BlockedReason::DependencyFailed(TaskId(1)))
        );
        assert!(Scheduler::restore(&paused_blocked.snapshot()).is_ok());

        let mut wrong_task_state = valid_failed.clone();
        wrong_task_state.tasks[1].state = TaskState::Queued;
        assert!(matches!(
            Scheduler::restore(&wrong_task_state),
            Err(SchedulerError::InvalidSnapshot(_))
        ));

        let mut wrong_dependency_state = valid_failed.clone();
        wrong_dependency_state.tasks[0].state = TaskState::Succeeded;
        assert!(matches!(
            Scheduler::restore(&wrong_dependency_state),
            Err(SchedulerError::InvalidSnapshot(_))
        ));

        let mut unknown = Scheduler::default();
        unknown
            .enqueue(vec![simple(1, &[]), simple(2, &[1])])
            .unwrap();
        unknown.dispatch().unwrap();
        unknown.disconnected();
        let valid_unknown = unknown.snapshot();
        assert!(Scheduler::restore(&valid_unknown).is_ok());
        let mut wrong_unknown_state = valid_unknown.clone();
        wrong_unknown_state.tasks[1].state = TaskState::Queued;
        assert!(matches!(
            Scheduler::restore(&wrong_unknown_state),
            Err(SchedulerError::InvalidSnapshot(_))
        ));

        let mut child = Scheduler::default();
        child.enqueue(vec![simple(1, &[])]).unwrap();
        let child_id = child.register_child("child", None, "child").unwrap();
        let mut child_reason = child.snapshot();
        child_reason
            .tasks
            .iter_mut()
            .find(|task| task.id == child_id)
            .unwrap()
            .blocked_reason = Some(BlockedReason::DependencyFailed(TaskId(1)));
        assert!(matches!(
            Scheduler::restore(&child_reason),
            Err(SchedulerError::InvalidSnapshot(_))
        ));
    }
}
