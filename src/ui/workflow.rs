use std::collections::BTreeMap;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::agents::AgentSnapshot;
use crate::observation::ActivityScope;
use crate::scheduler::{SchedulerCommand, TaskAttempt, TaskId, TaskSnapshot};
use crate::state::CoreSnapshot;

#[derive(Debug, Default)]
pub(super) struct WorkflowPanel {
    pub(super) visible: bool,
    pub(super) selected_id: Option<TaskId>,
    pub(super) scroll: usize,
    pub(super) manual_scroll: bool,
    pub(super) link_cursor: Option<WorkflowLinkCursor>,
    pub(super) confirm_stop: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum WorkflowLinkKind {
    Dependency,
    Gate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct WorkflowLinkCursor {
    pub(super) origin: TaskId,
    pub(super) origin_attempt: u64,
    pub(super) target: TaskId,
    pub(super) kind: WorkflowLinkKind,
    pub(super) index: usize,
    pub(super) captured_attempt: Option<u64>,
}

pub(super) struct WorkflowLinkNavigation {
    pub(super) task_id: Option<TaskId>,
    pub(super) cursor: Option<WorkflowLinkCursor>,
    pub(super) notice: Option<String>,
}

pub(super) enum WorkflowConversation {
    Root,
    Child(String),
    Unavailable(&'static str),
}

pub(super) enum WorkflowAction {
    Select(TaskId),
    Scroll { position: usize, manual: bool },
    OpenConversation(WorkflowConversation),
    ConfirmStop(bool),
}

pub(super) enum WorkflowScheduleRequest {
    Workflow(SchedulerCommand),
    Task {
        attempt: TaskAttempt,
        command: SchedulerCommand,
    },
}

pub(super) struct WorkflowKeyResult {
    pub(super) action: Option<WorkflowAction>,
    pub(super) navigation: Option<WorkflowLinkNavigation>,
    pub(super) notice: Option<String>,
    pub(super) schedule: Option<WorkflowScheduleRequest>,
}

impl WorkflowKeyResult {
    fn handled() -> Self {
        Self {
            action: None,
            navigation: None,
            notice: None,
            schedule: None,
        }
    }
}

pub(super) fn allows_key(key: KeyEvent) -> bool {
    let control = key.modifiers.contains(KeyModifiers::CONTROL);
    let plain = key.modifiers.is_empty();
    control && matches!(key.code, KeyCode::Char('q' | 'd' | 'c'))
        || plain
            && matches!(
                key.code,
                KeyCode::F(2)
                    | KeyCode::F(4)
                    | KeyCode::F(5)
                    | KeyCode::F(6)
                    | KeyCode::F(7)
                    | KeyCode::F(8)
                    | KeyCode::F(9)
                    | KeyCode::Up
                    | KeyCode::Down
                    | KeyCode::PageUp
                    | KeyCode::PageDown
                    | KeyCode::Home
                    | KeyCode::End
                    | KeyCode::Enter
                    | KeyCode::Esc
                    | KeyCode::Char('d' | 'g' | '+' | '-')
            )
}

/// 工作流面板只解析本面板按键并返回意图；状态应用和 Core 命令发送由根路由负责。
pub(super) fn handle_key(
    key: KeyEvent,
    snapshot: &CoreSnapshot,
    panel: &WorkflowPanel,
) -> Option<WorkflowKeyResult> {
    if key.code == KeyCode::F(5) {
        return Some(toggle_workflow(snapshot));
    }
    if !panel.visible {
        return None;
    }
    let plain = key.modifiers.is_empty();
    match key.code {
        KeyCode::PageUp => Some(scroll(panel.scroll.saturating_sub(8))),
        KeyCode::PageDown => Some(scroll(panel.scroll.saturating_add(8))),
        KeyCode::Home => Some(scroll(0)),
        KeyCode::End => Some(scroll(usize::MAX)),
        KeyCode::Char('d') if plain => {
            Some(navigate(snapshot, panel, WorkflowLinkKind::Dependency))
        }
        KeyCode::Char('g') if plain => Some(navigate(snapshot, panel, WorkflowLinkKind::Gate)),
        KeyCode::Enter if !key.modifiers.contains(KeyModifiers::CONTROL) => {
            Some(open_conversation(snapshot, panel))
        }
        KeyCode::F(6) | KeyCode::F(7) | KeyCode::F(8) => {
            Some(task_command(key.code, snapshot, panel))
        }
        KeyCode::F(9) => Some(stop_confirmation(panel.confirm_stop)),
        KeyCode::Up | KeyCode::Down => Some(select_task(key.code, snapshot, panel)),
        KeyCode::Char('+') | KeyCode::Char('-') if plain => {
            Some(change_priority(key.code, snapshot, panel))
        }
        _ => None,
    }
}

fn toggle_workflow(snapshot: &CoreSnapshot) -> WorkflowKeyResult {
    WorkflowKeyResult {
        schedule: Some(WorkflowScheduleRequest::Workflow(
            if snapshot.scheduler.paused {
                SchedulerCommand::ResumeWorkflow
            } else {
                SchedulerCommand::PauseWorkflow
            },
        )),
        ..WorkflowKeyResult::handled()
    }
}

fn scroll(position: usize) -> WorkflowKeyResult {
    WorkflowKeyResult {
        action: Some(WorkflowAction::Scroll {
            position,
            manual: true,
        }),
        ..WorkflowKeyResult::handled()
    }
}

fn navigate(
    snapshot: &CoreSnapshot,
    panel: &WorkflowPanel,
    kind: WorkflowLinkKind,
) -> WorkflowKeyResult {
    WorkflowKeyResult {
        navigation: Some(navigate_workflow_link(
            snapshot,
            panel.selected_id,
            panel.link_cursor,
            kind,
        )),
        ..WorkflowKeyResult::handled()
    }
}

fn open_conversation(snapshot: &CoreSnapshot, panel: &WorkflowPanel) -> WorkflowKeyResult {
    let Some(task) = selected_task(snapshot, panel.selected_id) else {
        return WorkflowKeyResult::handled();
    };
    if stale_gate_link(snapshot, panel.link_cursor, task) {
        return stale_gate_notice("before opening it");
    }
    let target = match workflow_conversation(snapshot, task) {
        ConversationTarget::Root => WorkflowConversation::Root,
        ConversationTarget::Child(agent) => WorkflowConversation::Child(agent.info.id.clone()),
        ConversationTarget::Unavailable(message) => WorkflowConversation::Unavailable(message),
    };
    WorkflowKeyResult {
        action: Some(WorkflowAction::OpenConversation(target)),
        ..WorkflowKeyResult::handled()
    }
}

fn task_command(key: KeyCode, snapshot: &CoreSnapshot, panel: &WorkflowPanel) -> WorkflowKeyResult {
    let Some(task) = selected_task(snapshot, panel.selected_id) else {
        return WorkflowKeyResult::handled();
    };
    if stale_gate_link(snapshot, panel.link_cursor, task) {
        return stale_gate_notice("before changing it");
    }
    let command = match key {
        KeyCode::F(6) if task.pause_requested => SchedulerCommand::Resume(task.id),
        KeyCode::F(6) => SchedulerCommand::Pause(task.id),
        KeyCode::F(7) => SchedulerCommand::Cancel(task.id),
        _ => SchedulerCommand::Retry(task.id),
    };
    WorkflowKeyResult {
        schedule: Some(WorkflowScheduleRequest::Task {
            attempt: TaskAttempt {
                task: task.id,
                attempt: task.attempt,
            },
            command,
        }),
        ..WorkflowKeyResult::handled()
    }
}

fn stop_confirmation(confirmed: bool) -> WorkflowKeyResult {
    if confirmed {
        WorkflowKeyResult {
            action: Some(WorkflowAction::ConfirmStop(false)),
            schedule: Some(WorkflowScheduleRequest::Workflow(
                SchedulerCommand::StopWorkflow,
            )),
            ..WorkflowKeyResult::handled()
        }
    } else {
        WorkflowKeyResult {
            action: Some(WorkflowAction::ConfirmStop(true)),
            notice: Some(
                "Press F9 again to cancel all workflow tasks. Any other key cancels this action."
                    .into(),
            ),
            ..WorkflowKeyResult::handled()
        }
    }
}

fn select_task(key: KeyCode, snapshot: &CoreSnapshot, panel: &WorkflowPanel) -> WorkflowKeyResult {
    let tasks = project_workflow(snapshot);
    if tasks.is_empty() {
        return WorkflowKeyResult::handled();
    }
    let index = selected_task(snapshot, panel.selected_id)
        .and_then(|task| tasks.iter().position(|id| *id == task.id))
        .unwrap_or(0);
    let next = if key == KeyCode::Up {
        index.saturating_sub(1)
    } else {
        (index + 1).min(tasks.len() - 1)
    };
    WorkflowKeyResult {
        action: Some(WorkflowAction::Select(tasks[next])),
        ..WorkflowKeyResult::handled()
    }
}

fn change_priority(
    key: KeyCode,
    snapshot: &CoreSnapshot,
    panel: &WorkflowPanel,
) -> WorkflowKeyResult {
    let Some(task) = selected_task(snapshot, panel.selected_id) else {
        return WorkflowKeyResult::handled();
    };
    if stale_gate_link(snapshot, panel.link_cursor, task) {
        return stale_gate_notice("before changing it");
    }
    let priority = if key == KeyCode::Char('+') {
        task.priority.saturating_add(1)
    } else {
        task.priority.saturating_sub(1)
    };
    WorkflowKeyResult {
        schedule: Some(WorkflowScheduleRequest::Task {
            attempt: TaskAttempt {
                task: task.id,
                attempt: task.attempt,
            },
            command: SchedulerCommand::Reprioritize {
                task_id: task.id,
                priority,
            },
        }),
        ..WorkflowKeyResult::handled()
    }
}

fn stale_gate_notice(action: &str) -> WorkflowKeyResult {
    WorkflowKeyResult {
        notice: Some(format!(
            "The captured Gate attempt is no longer current; use Up/Down to reselect the task {action}."
        )),
        ..WorkflowKeyResult::handled()
    }
}

pub(super) enum ConversationTarget<'a> {
    Root,
    Child(&'a AgentSnapshot),
    Unavailable(&'static str),
}

pub(super) fn selected_task(
    snapshot: &CoreSnapshot,
    selected_id: Option<TaskId>,
) -> Option<&TaskSnapshot> {
    selected_id
        .and_then(|id| snapshot.scheduler.tasks.iter().find(|task| task.id == id))
        .or_else(|| snapshot.scheduler.tasks.first())
}

pub(super) fn project_workflow(snapshot: &CoreSnapshot) -> Vec<TaskId> {
    workflow_order(&snapshot.scheduler.tasks)
}

/// 工作流行序只根据快照中的显式 parent 建树；依赖与 Gate 目标单独呈现。
fn workflow_order(tasks: &[TaskSnapshot]) -> Vec<TaskId> {
    fn visit(
        id: TaskId,
        children: &BTreeMap<TaskId, Vec<TaskId>>,
        seen: &mut std::collections::HashSet<TaskId>,
        output: &mut Vec<TaskId>,
    ) {
        if !seen.insert(id) {
            return;
        }
        output.push(id);
        if let Some(direct) = children.get(&id) {
            for child in direct {
                visit(*child, children, seen, output);
            }
        }
    }
    let known = tasks
        .iter()
        .map(|task| task.id)
        .collect::<std::collections::HashSet<_>>();
    let mut children = BTreeMap::<TaskId, Vec<TaskId>>::new();
    for task in tasks {
        if let Some(parent) = task.parent.filter(|parent| known.contains(parent)) {
            children.entry(parent).or_default().push(task.id);
        }
    }
    for direct in children.values_mut() {
        direct.sort();
    }
    let mut roots = tasks
        .iter()
        .filter(|task| task.parent.is_none() || !known.contains(&task.parent.unwrap()))
        .map(|task| task.id)
        .collect::<Vec<_>>();
    roots.sort();
    let mut seen = std::collections::HashSet::new();
    let mut output = Vec::with_capacity(tasks.len());
    for root in roots {
        visit(root, &children, &mut seen, &mut output);
    }
    for task in tasks {
        visit(task.id, &children, &mut seen, &mut output);
    }
    output
}

pub(super) fn navigate_workflow_link(
    snapshot: &CoreSnapshot,
    selected_id: Option<TaskId>,
    current_cursor: Option<WorkflowLinkCursor>,
    kind: WorkflowLinkKind,
) -> WorkflowLinkNavigation {
    let unchanged = |notice| WorkflowLinkNavigation {
        task_id: None,
        cursor: current_cursor,
        notice,
    };
    let Some(current) = selected_task(snapshot, selected_id) else {
        return unchanged(Some("No workflow task is selected.".into()));
    };
    let cursor = current_cursor.filter(|cursor| {
        cursor.kind == kind
            && cursor.target == current.id
            && snapshot
                .scheduler
                .tasks
                .iter()
                .find(|task| task.id == cursor.origin)
                .is_some_and(|task| task.attempt == cursor.origin_attempt)
    });
    let origin = cursor.map_or(current.id, |cursor| cursor.origin);
    let Some(source) = snapshot
        .scheduler
        .tasks
        .iter()
        .find(|task| task.id == origin)
    else {
        return unchanged(Some("The workflow link source is unavailable.".into()));
    };
    let count = match kind {
        WorkflowLinkKind::Dependency => source.dependencies.len(),
        WorkflowLinkKind::Gate => source.wait_targets.len(),
    };
    if count == 0 {
        return WorkflowLinkNavigation {
            task_id: None,
            cursor: None,
            notice: Some(match kind {
                WorkflowLinkKind::Dependency => "Selected task has no dependencies.".into(),
                WorkflowLinkKind::Gate => "Selected task has no captured Gate wait targets.".into(),
            }),
        };
    }
    let index = cursor.map_or(0, |cursor| (cursor.index + 1) % count);
    let (target, captured_attempt) = match kind {
        WorkflowLinkKind::Dependency => (source.dependencies[index], None),
        WorkflowLinkKind::Gate => {
            let target = source.wait_targets[index];
            (target.task, Some(target.attempt))
        }
    };
    if !snapshot
        .scheduler
        .tasks
        .iter()
        .any(|task| task.id == target)
    {
        return WorkflowLinkNavigation {
            task_id: None,
            cursor: None,
            notice: Some(format!(
                "Workflow link target #{} is unavailable.",
                target.0
            )),
        };
    }
    WorkflowLinkNavigation {
        task_id: Some(target),
        cursor: Some(WorkflowLinkCursor {
            origin,
            origin_attempt: source.attempt,
            target,
            kind,
            index,
            captured_attempt,
        }),
        notice: None,
    }
}

pub(super) fn stale_gate_link(
    snapshot: &CoreSnapshot,
    cursor: Option<WorkflowLinkCursor>,
    task: &TaskSnapshot,
) -> bool {
    let Some(cursor) =
        cursor.filter(|cursor| cursor.kind == WorkflowLinkKind::Gate && cursor.target == task.id)
    else {
        return false;
    };
    let origin_is_current = snapshot
        .scheduler
        .tasks
        .iter()
        .find(|candidate| candidate.id == cursor.origin)
        .is_some_and(|origin| origin.attempt == cursor.origin_attempt);
    !origin_is_current || cursor.captured_attempt != Some(task.attempt)
}

pub(super) fn workflow_conversation<'a>(
    snapshot: &'a CoreSnapshot,
    task: &'a TaskSnapshot,
) -> ConversationTarget<'a> {
    match task.kind {
        crate::scheduler::TaskKind::RootTurn => {
            let active = snapshot.scheduler.active_root
                == Some(TaskAttempt {
                    task: task.id,
                    attempt: task.attempt,
                });
            let current_thread = snapshot.thread_id.as_deref();
            let current_turn = snapshot.turn_id.as_deref();
            let exact_turn = task.external.as_ref().is_some_and(|turn| {
                !turn.turn_id.is_empty()
                    && turn.generation > 0
                    && Some(turn.thread_id.as_str()) == current_thread
                    && Some(turn.turn_id.as_str()) == current_turn
            });
            let observed = task.external.as_ref().is_some_and(|turn| {
                snapshot.observation.activities.iter().any(|activity| {
                    activity.scope == ActivityScope::Turn
                        && activity.identity.agent_id == "root"
                        && activity.identity.task_id == Some(task.id)
                        && activity.identity.attempt_id == Some(task.attempt)
                        && activity.identity.thread_id.as_deref() == Some(turn.thread_id.as_str())
                        && activity.identity.turn_id.as_deref() == Some(turn.turn_id.as_str())
                        && activity.identity.generation == Some(turn.generation)
                })
            });
            if snapshot.phase != crate::state::SessionPhase::StartingTurn
                && exact_turn
                && observed
                && (!task.state.active() || active)
            {
                ConversationTarget::Root
            } else {
                ConversationTarget::Unavailable(
                    "This root task does not match the currently confirmed conversation turn.",
                )
            }
        }
        crate::scheduler::TaskKind::NativeChild => {
            let Some(thread_id) = task.child_thread_id.as_deref() else {
                return ConversationTarget::Unavailable(
                    "This child task has no confirmed thread identity.",
                );
            };
            if task.external.as_ref().is_some_and(|turn| {
                turn.thread_id != thread_id
                    || turn.generation != task.attempt
                    || turn.generation == 0
                    || turn.turn_id.is_empty()
            }) {
                return ConversationTarget::Unavailable(
                    "The child task identity is stale or inconsistent.",
                );
            }
            let Some(agent) = snapshot
                .agents
                .iter()
                .find(|agent| agent.info.id == thread_id)
            else {
                return ConversationTarget::Unavailable(
                    "No observed agent is available for this child thread.",
                );
            };
            if let Some(turn) = &task.external {
                if agent.turn_id.as_deref() == Some(turn.turn_id.as_str())
                    && agent.generation == turn.generation
                {
                    ConversationTarget::Child(agent)
                } else {
                    ConversationTarget::Unavailable(
                        "The child task's observed agent turn is stale or unavailable.",
                    )
                }
            } else if task.attempt == 0
                && agent.generation == 0
                && agent.turn_id.is_none()
                && agent.awaiting_turn
            {
                ConversationTarget::Child(agent)
            } else {
                ConversationTarget::Unavailable(
                    "The child thread has started without a matching confirmed task turn.",
                )
            }
        }
    }
}

pub(super) fn task_reference(id: TaskId, tasks: &BTreeMap<TaskId, &TaskSnapshot>) -> String {
    tasks.get(&id).map_or_else(
        || format!("#{} unavailable", id.0),
        |task| format!("#{} {} [{:?}]", id.0, task.title, task.state),
    )
}
