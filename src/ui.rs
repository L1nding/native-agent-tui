use std::collections::{BTreeMap, HashMap, HashSet};
use std::io;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::{Constraint, Direction, Layout};
use thiserror::Error;

use crate::client::Command;
use crate::interactions::{ApprovalDecision, RequestKind, RequestRef, RequestView};
use crate::observation::AttentionClass;
use crate::scheduler::{RootTaskSpec, SchedulerCommand, TaskAttempt, ROOT_QUEUE_LIMIT};
use crate::state::CoreSnapshot;

mod activity;
mod attention;
mod commands;
mod context;
mod editor;
mod history;
mod input;
mod layout;
mod overlays;
mod reminders;
mod render;
mod request_state;
mod requests;
mod runtime;
mod scope;
mod search;
mod skills;
mod terminal;
mod timeline;
mod tool_detail;
mod tool_search;
mod tool_trace;
mod workflow;
mod workflow_view;

use activity::compaction_fact_rows;
use attention::AttentionEditor;
use commands::PaletteEvent;
use editor::Editor;
use layout::{conversation_content_size, main_layout, wrap};
use overlays::open_palette_action;
use render::draw;
use request_state::{request_locked, selected_request, sync_local_requests};
use skills::skills_panel_entries_capacity;
use workflow::{
    navigate_workflow_link, project_workflow, selected_task, stale_gate_link,
    workflow_conversation, ConversationTarget, WorkflowLinkKind, WorkflowLinkNavigation,
};

pub use runtime::{run_history, run_tasks_with_history};

#[cfg(test)]
use crate::observation::{ActivityScope, AttentionLevel, ExecutionState};
#[cfg(test)]
use crate::scheduler::{TaskId, TaskSnapshot};
#[cfg(test)]
use crate::state::FactSource;
#[cfg(test)]
use activity::truncate_display_label;
#[cfg(test)]
use activity::{activity_brief, format_token_budget_evidence, token_budget_brief};
#[cfg(test)]
use input::InputEvent;
#[cfg(test)]
use ratatui::Terminal;
#[cfg(test)]
use request_state::sync_questions;
#[cfg(test)]
use std::time::Duration;
#[cfg(test)]
use unicode_width::UnicodeWidthStr;
#[cfg(test)]
use workflow::WorkflowLinkCursor;

const PASTE_REJECTED: &str = "Paste exceeds 32 KiB; the entire paste was discarded.";

#[derive(Debug, Error)]
pub enum UiError {
    #[error(transparent)]
    History(#[from] crate::history::HistoryError),
    #[error("terminal I/O failed: {0}")]
    Io(#[from] io::Error),
    #[error("Core shutdown failed: {0}")]
    Shutdown(String),
}

#[derive(Default)]
struct LocalState {
    viewport: ratatui::layout::Rect,
    search: search::SearchPanel,
    timeline: timeline::TimelinePanel,
    conversation_focus: Option<search::Focus>,
    reminders: reminders::Reminders,
    attention_editor: Option<AttentionEditor>,
    evidence: bool,
    evidence_scroll: usize,
    skills: bool,
    skills_scroll: usize,
    editor: Editor,
    task_draft: Option<Editor>,
    scroll_from_bottom: usize,
    notice: Option<String>,
    help: bool,
    workflow: workflow::WorkflowPanel,
    context: context::ContextPanel,
    palette: commands::PaletteState,
    request_selection: Option<RequestRef>,
    request_panel: bool,
    request_scroll: usize,
    submitted: HashSet<RequestRef>,
    input_drafts: HashMap<RequestRef, InputDraft>,
    agent_id: Option<String>,
    answering: Option<RequestRef>,
    question_index: usize,
    answers: BTreeMap<String, Vec<String>>,
}

#[derive(Default)]
struct InputDraft {
    editor: Editor,
    question_index: usize,
    answers: BTreeMap<String, Vec<String>>,
}

fn handle_paste(text: &str, snapshot: &CoreSnapshot, local: &mut LocalState) {
    if local.palette.visible {
        local.palette.paste(text);
        return;
    }
    if local.workflow.visible {
        return;
    }
    sync_local_requests(local, snapshot);
    if local.search.visible {
        local.search.paste(text);
        return;
    }
    if local.timeline.visible {
        local.timeline.paste(text);
        return;
    }
    if let Some(editor) = &mut local.attention_editor {
        for digit in text.chars().filter(char::is_ascii_digit).take(10) {
            if editor.input().len() < 10 {
                editor.input().push(digit);
            }
        }
    } else {
        local.editor.insert(text);
    }
}

fn reject_paste(local: &mut LocalState) {
    if local.search.visible {
        local.search.reject_paste();
    } else if local.timeline.visible {
        local.timeline.reject_paste();
    } else if let Some(editor) = &mut local.attention_editor {
        editor.notice = Some(PASTE_REJECTED.into());
    } else {
        local.notice = Some(PASTE_REJECTED.into());
    }
}

fn send(command: Command, tx: &tokio::sync::mpsc::Sender<Command>, local: &mut LocalState) -> bool {
    match tx.try_send(command) {
        Ok(()) => {
            local.notice = None;
            true
        }
        Err(error) => {
            local.notice = Some(format!("Could not submit: {error}"));
            false
        }
    }
}

fn handle_key(
    key: KeyEvent,
    snapshot: &CoreSnapshot,
    local: &mut LocalState,
    tx: &tokio::sync::mpsc::Sender<Command>,
) -> bool {
    local.reminders.sync(&snapshot.observation);
    sync_local_requests(local, snapshot);
    let control = key.modifiers.contains(KeyModifiers::CONTROL);
    if control && key.code == KeyCode::Char('p') {
        local.palette.toggle();
        return false;
    }
    if local.palette.visible {
        match local.palette.key(key) {
            PaletteEvent::Action(action) => open_palette_action(action, snapshot, local),
            PaletteEvent::Close | PaletteEvent::Consumed => {}
        }
        return false;
    }
    if local.context.visible {
        if control && matches!(key.code, KeyCode::Char('q') | KeyCode::Char('d')) {
            return true;
        }
        if control && key.code == KeyCode::Char('c') {
            send(Command::Interrupt, tx, local);
            return false;
        }
        local.context.handle_key(key, 8);
        return false;
    }
    if control && key.code == KeyCode::Char('g') {
        local.search.close();
        local.timeline.close();
        local.skills = false;
        local.request_panel = false;
        local.evidence = false;
        local.workflow.visible = false;
        local.attention_editor = None;
        local.context.toggle();
        return false;
    }
    if local.skills {
        match key.code {
            KeyCode::Char('q') if control => return true,
            KeyCode::Char('k') if control => local.skills = false,
            KeyCode::Esc => local.skills = false,
            KeyCode::Char('c') if control => {
                send(Command::Interrupt, tx, local);
                local.skills = false;
            }
            KeyCode::F(2) => {
                local.skills = false;
                let current = selected_request(snapshot, local)
                    .filter(|request| !request.responding)
                    .or_else(|| snapshot.requests.iter().find(|request| !request.responding));
                if let Some(request) = current {
                    local.request_selection = Some(request.reference());
                    local.request_panel = true;
                    local.request_scroll = 0;
                    local.workflow.visible = false;
                    local.evidence = false;
                    sync_local_requests(local, snapshot);
                    local.notice = None;
                } else {
                    local.request_panel = false;
                    local.notice = Some("No pending requests.".into());
                }
            }
            KeyCode::Enter => {
                send(Command::RefreshSkills, tx, local);
            }
            KeyCode::Up | KeyCode::PageUp => {
                let step = skills_panel_entries_capacity(local.viewport).max(1);
                local.skills_scroll = local.skills_scroll.saturating_sub(step);
            }
            KeyCode::Down | KeyCode::PageDown => {
                let visible = skills_panel_entries_capacity(local.viewport);
                local.skills_scroll = local
                    .skills_scroll
                    .saturating_add(visible.max(1))
                    .min(snapshot.skills.entries.len().saturating_sub(visible));
            }
            KeyCode::Home => local.skills_scroll = 0,
            KeyCode::End => {
                let visible = skills_panel_entries_capacity(local.viewport);
                local.skills_scroll = snapshot.skills.entries.len().saturating_sub(visible);
            }
            _ => {}
        }
        return false;
    }
    if local.search.visible {
        if control && matches!(key.code, KeyCode::Char('q') | KeyCode::Char('d')) {
            return true;
        }
        if control && key.code == KeyCode::Char('c') {
            send(Command::Interrupt, tx, local);
            return false;
        }
        if matches!(
            key.code,
            KeyCode::F(2) | KeyCode::F(3) | KeyCode::F(4) | KeyCode::F(10) | KeyCode::F(11)
        ) {
            local.search.close();
        } else {
            if let Some(focus) = local.search.key(key, snapshot) {
                local.agent_id = if snapshot.thread_id.as_deref() == Some(focus.thread()) {
                    None
                } else {
                    Some(focus.thread().into())
                };
                local.conversation_focus = Some(focus);
                local.workflow.visible = false;
                local.evidence = false;
                local.request_panel = false;
                local.notice = None;
            }
            return false;
        }
    }
    if local.timeline.visible {
        if control && matches!(key.code, KeyCode::Char('q') | KeyCode::Char('d')) {
            return true;
        }
        if control && key.code == KeyCode::Char('c') {
            send(Command::Interrupt, tx, local);
            return false;
        }
        if matches!(
            key.code,
            KeyCode::F(2) | KeyCode::F(3) | KeyCode::F(4) | KeyCode::F(10) | KeyCode::F(11)
        ) {
            local.timeline.close();
        } else {
            match local.timeline.key(key, snapshot) {
                Some(timeline::Locate::Message(focus)) => {
                    local.agent_id = if snapshot.thread_id.as_deref() == Some(focus.thread()) {
                        None
                    } else {
                        Some(focus.thread().into())
                    };
                    local.conversation_focus = Some(focus);
                    local.workflow.visible = false;
                    local.evidence = false;
                    local.request_panel = false;
                    local.notice = None;
                }
                Some(timeline::Locate::Request(reference)) => {
                    local.request_selection = Some(reference);
                    local.request_panel = true;
                    local.request_scroll = 0;
                    local.workflow.visible = false;
                    local.evidence = false;
                    sync_local_requests(local, snapshot);
                    local.notice = None;
                }
                Some(timeline::Locate::ToolSearch(thread)) => {
                    local.timeline.close();
                    local.search.open_tools(thread);
                    local.workflow.visible = false;
                    local.evidence = false;
                    local.request_panel = false;
                    local.notice = None;
                }
                None => {}
            }
            return false;
        }
    }
    if control && key.code == KeyCode::Char('t') && local.attention_editor.is_none() {
        let thread = local
            .agent_id
            .clone()
            .or_else(|| snapshot.thread_id.clone())
            .unwrap_or_default();
        local.timeline.open(thread, snapshot);
        return false;
    }
    if control && key.code == KeyCode::Char('f') && local.attention_editor.is_none() {
        let thread = local
            .agent_id
            .clone()
            .or_else(|| snapshot.thread_id.clone())
            .unwrap_or_default();
        local.search.open(thread);
        return false;
    }
    if matches!(key.code, KeyCode::PageUp | KeyCode::PageDown)
        || control && matches!(key.code, KeyCode::Home | KeyCode::End)
    {
        if let Some(focus) = &local.conversation_focus {
            let chunks = main_layout(
                local.viewport,
                !snapshot.observation.activities.is_empty(),
                selected_request(snapshot, local).is_some()
                    || snapshot.last_error.is_some()
                    || snapshot.gate.as_ref().is_some_and(|gate| gate.pending),
            );
            let mut area = chunks[1];
            if local.viewport.width >= 100 && !snapshot.agents.is_empty() {
                area = Layout::default()
                    .direction(Direction::Horizontal)
                    .constraints([Constraint::Length(32), Constraint::Min(1)])
                    .split(area)[1];
            }
            let (width, height) = conversation_content_size(area);
            let mut rows = 0;
            let mut target = None;
            for message in snapshot
                .messages
                .iter()
                .filter(|m| m.thread_id == focus.thread())
            {
                rows += 1;
                if focus.matches(message) {
                    target = Some(rows + focus.row(width));
                }
                rows += wrap(&message.text, width).len() + 1;
            }
            let max_scroll = rows.saturating_sub(1).saturating_sub(height);
            if let Some(row) = target {
                local.scroll_from_bottom =
                    max_scroll.saturating_sub(row.saturating_sub(height / 3).min(max_scroll));
            }
        }
    }
    if let Some(editor) = &mut local.attention_editor {
        match key.code {
            KeyCode::Char('q') | KeyCode::Char('d') if control => return true,
            KeyCode::Char('c') if control => {
                send(Command::Interrupt, tx, local);
            }
            KeyCode::Esc | KeyCode::F(10) => local.attention_editor = None,
            KeyCode::Up | KeyCode::Down => {
                let index = if key.code == KeyCode::Up {
                    (editor.class_index + 3) % 4
                } else {
                    (editor.class_index + 1) % 4
                };
                local.attention_editor = Some(AttentionEditor::new(snapshot, index));
            }
            KeyCode::Tab | KeyCode::BackTab => editor.field = 1 - editor.field,
            KeyCode::Backspace => {
                editor.input().pop();
            }
            KeyCode::Char('u') if control => editor.input().clear(),
            KeyCode::Char(digit) if !control && digit.is_ascii_digit() => {
                if editor.input().len() < 10 {
                    editor.input().push(digit);
                }
            }
            KeyCode::Enter => {
                let values = editor
                    .quiet
                    .parse::<u64>()
                    .ok()
                    .zip(editor.attention.parse::<u64>().ok());
                let valid = values.filter(|(quiet, attention)| {
                    *quiet > 0 && quiet < attention && *attention <= 604800000
                });
                if let Some((quiet_ms, attention_ms)) = valid {
                    let class = AttentionClass::ALL[editor.class_index];
                    if send(
                        Command::ConfigureAttention {
                            class,
                            quiet_ms,
                            attention_ms,
                        },
                        tx,
                        local,
                    ) {
                        local.attention_editor = None;
                    }
                } else {
                    editor.notice = Some("Require 0 < quiet < attention <= 604800000".into());
                }
            }
            _ => {}
        }
        return false;
    }
    if local.workflow.visible {
        let plain = key.modifiers.is_empty();
        let allowed = control && matches!(key.code, KeyCode::Char('q' | 'd' | 'c'))
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
                );
        if !allowed {
            return false;
        }
    }
    if key.code != KeyCode::F(9) {
        local.workflow.confirm_stop = false;
    }
    // These control letters remain distinct when a host drops Enter modifiers.
    let key = if control && key.code == KeyCode::Char('s') {
        KeyEvent::new(KeyCode::Enter, KeyModifiers::CONTROL)
    } else if control && key.code == KeyCode::Char('o') {
        KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT)
    } else {
        key
    };
    if control {
        match key.code {
            KeyCode::Char('q') | KeyCode::Char('d') => return true,
            KeyCode::Char('c') => {
                send(Command::Interrupt, tx, local);
                return false;
            }
            KeyCode::Char('u') => {
                local.editor.clear();
                return false;
            }
            KeyCode::Char('w') => {
                let agent = selected_agent_id(snapshot, local);
                local.notice = if local
                    .reminders
                    .toggle_for_agent(&snapshot.observation, agent)
                    .is_some()
                {
                    None
                } else {
                    Some("No silence reminders for the selected agent. Pending requests still require an answer.".into())
                };
                return false;
            }
            KeyCode::Char('y') | KeyCode::Char('n') | KeyCode::Char('b') => {
                if let Some(request) = selected_request(snapshot, local) {
                    let decision = match key.code {
                        KeyCode::Char('y') => ApprovalDecision::Accept,
                        KeyCode::Char('n') => ApprovalDecision::Decline,
                        _ => ApprovalDecision::Cancel,
                    };
                    if request_locked(snapshot, local, request) {
                        local.notice = Some(
                            "Request is submitted or unavailable; wait for resolution.".into(),
                        );
                    } else if let Err(error) = request.approval_result(decision) {
                        local.notice = Some(error.to_string());
                    } else if send(
                        Command::AnswerApproval {
                            request: request.reference(),
                            decision,
                        },
                        tx,
                        local,
                    ) {
                        local.submitted.insert(request.reference());
                    }
                } else {
                    local.notice = Some(
                        "Selected request expired; press F2 to select a current request.".into(),
                    );
                }
                return false;
            }
            _ => {}
        }
    }
    match key.code {
        KeyCode::Char('k') if control => {
            local.skills = !local.skills;
            local.skills_scroll = 0;
        }
        KeyCode::PageUp if local.request_panel => {
            local.request_scroll = local.request_scroll.saturating_sub(1)
        }
        KeyCode::PageDown if local.request_panel => {
            local.request_scroll = local.request_scroll.saturating_add(1)
        }
        KeyCode::Home if local.request_panel && control => local.request_scroll = 0,
        KeyCode::End if local.request_panel && control => local.request_scroll = usize::MAX,
        KeyCode::F(10) => local.attention_editor = Some(AttentionEditor::new(snapshot, 0)),
        KeyCode::F(11) => {
            local.request_panel = false;
            local.evidence = !local.evidence;
            local.evidence_scroll = 0;
        }
        KeyCode::PageUp if local.evidence => {
            local.evidence_scroll = local.evidence_scroll.saturating_sub(8)
        }
        KeyCode::PageDown if local.evidence => {
            local.evidence_scroll = local.evidence_scroll.saturating_add(8)
        }
        KeyCode::F(4) => {
            local.request_panel = false;
            local.workflow.visible = !local.workflow.visible;
            local.workflow.scroll = 0;
            local.workflow.manual_scroll = false;
            local.workflow.link_cursor = None;
        }
        KeyCode::PageUp if local.workflow.visible => {
            local.workflow.manual_scroll = true;
            local.workflow.scroll = local.workflow.scroll.saturating_sub(8);
        }
        KeyCode::PageDown if local.workflow.visible => {
            local.workflow.manual_scroll = true;
            local.workflow.scroll = local.workflow.scroll.saturating_add(8);
        }
        KeyCode::Home if local.workflow.visible => {
            local.workflow.manual_scroll = true;
            local.workflow.scroll = 0;
        }
        KeyCode::End if local.workflow.visible => {
            local.workflow.manual_scroll = true;
            local.workflow.scroll = usize::MAX;
        }
        KeyCode::Char('d') if local.workflow.visible && key.modifiers.is_empty() => {
            apply_workflow_navigation(
                navigate_workflow_link(
                    snapshot,
                    local.workflow.selected_id,
                    local.workflow.link_cursor,
                    WorkflowLinkKind::Dependency,
                ),
                local,
            );
        }
        KeyCode::Char('g') if local.workflow.visible && key.modifiers.is_empty() => {
            apply_workflow_navigation(
                navigate_workflow_link(
                    snapshot,
                    local.workflow.selected_id,
                    local.workflow.link_cursor,
                    WorkflowLinkKind::Gate,
                ),
                local,
            );
        }
        KeyCode::Enter if local.workflow.visible && !control => {
            if let Some(task) = selected_task(snapshot, local.workflow.selected_id) {
                if stale_gate_link(snapshot, local.workflow.link_cursor, task) {
                    local.notice = Some(
                        "The captured Gate attempt is no longer current; use Up/Down to reselect the task before opening it.".into(),
                    );
                } else {
                    match workflow_conversation(snapshot, task) {
                        ConversationTarget::Root => {
                            local.notice = None;
                            local.agent_id = None;
                            local.conversation_focus = None;
                            local.scroll_from_bottom = 0;
                            local.workflow.visible = false;
                        }
                        ConversationTarget::Child(agent) => {
                            local.notice = None;
                            local.agent_id = Some(agent.info.id.clone());
                            local.conversation_focus = None;
                            local.scroll_from_bottom = 0;
                            local.workflow.visible = false;
                        }
                        ConversationTarget::Unavailable(message) => {
                            local.notice = Some(message.into());
                        }
                    }
                }
            }
        }
        KeyCode::F(5) => {
            send(
                Command::Schedule(if snapshot.scheduler.paused {
                    SchedulerCommand::ResumeWorkflow
                } else {
                    SchedulerCommand::PauseWorkflow
                }),
                tx,
                local,
            );
        }
        KeyCode::F(6) | KeyCode::F(7) | KeyCode::F(8) if local.workflow.visible => {
            if let Some(task) = selected_task(snapshot, local.workflow.selected_id) {
                if stale_gate_link(snapshot, local.workflow.link_cursor, task) {
                    local.notice = Some(
                        "The captured Gate attempt is no longer current; use Up/Down to reselect the task before changing it.".into(),
                    );
                } else {
                    let command = match key.code {
                        KeyCode::F(6) if task.pause_requested => SchedulerCommand::Resume(task.id),
                        KeyCode::F(6) => SchedulerCommand::Pause(task.id),
                        KeyCode::F(7) => SchedulerCommand::Cancel(task.id),
                        _ => SchedulerCommand::Retry(task.id),
                    };
                    let attempt = TaskAttempt {
                        task: task.id,
                        attempt: task.attempt,
                    };
                    send(Command::ScheduleTask { attempt, command }, tx, local);
                }
            }
        }
        KeyCode::F(9) if local.workflow.visible => {
            if local.workflow.confirm_stop {
                send(Command::Schedule(SchedulerCommand::StopWorkflow), tx, local);
                local.workflow.confirm_stop = false;
            } else {
                local.workflow.confirm_stop = true;
                local.notice = Some("Press F9 again to cancel all workflow tasks. Any other key cancels this action.".into());
            }
        }
        KeyCode::Up | KeyCode::Down if local.workflow.visible => {
            let tasks = project_workflow(snapshot);
            if !tasks.is_empty() {
                let index = selected_task(snapshot, local.workflow.selected_id)
                    .and_then(|task| tasks.iter().position(|id| *id == task.id))
                    .unwrap_or(0);
                let next = if key.code == KeyCode::Up {
                    index.saturating_sub(1)
                } else {
                    (index + 1).min(tasks.len() - 1)
                };
                local.workflow.selected_id = Some(tasks[next]);
                local.workflow.scroll = 0;
                local.workflow.manual_scroll = false;
                local.workflow.link_cursor = None;
            }
        }
        KeyCode::Char('+') | KeyCode::Char('-')
            if local.workflow.visible && key.modifiers.is_empty() =>
        {
            if let Some(task) = selected_task(snapshot, local.workflow.selected_id) {
                if stale_gate_link(snapshot, local.workflow.link_cursor, task) {
                    local.notice = Some(
                        "The captured Gate attempt is no longer current; use Up/Down to reselect the task before changing it.".into(),
                    );
                } else {
                    let priority = task.priority
                        + if key.code == KeyCode::Char('+') {
                            1
                        } else {
                            -1
                        };
                    send(
                        Command::ScheduleTask {
                            attempt: TaskAttempt {
                                task: task.id,
                                attempt: task.attempt,
                            },
                            command: SchedulerCommand::Reprioritize {
                                task_id: task.id,
                                priority,
                            },
                        },
                        tx,
                        local,
                    );
                }
            }
        }
        KeyCode::F(1) => local.help = !local.help,
        KeyCode::F(2) => {
            if local.request_panel || selected_request(snapshot, local).is_none() {
                let next = selected_request(snapshot, local)
                    .and_then(|request| {
                        snapshot
                            .requests
                            .iter()
                            .position(|r| r.matches(&request.reference()))
                    })
                    .map_or(0, |index| (index + 1) % snapshot.requests.len().max(1));
                local.request_selection = snapshot.requests.get(next).map(RequestView::reference);
            }
            local.request_panel = true;
            local.request_scroll = 0;
            local.notice = None;
            local.workflow.visible = false;
            local.evidence = false;
            sync_local_requests(local, snapshot);
        }
        KeyCode::F(3) => {
            local.conversation_focus = None;
            local.agent_id = workflow_view::next_agent_id(snapshot, local.agent_id.as_deref());
            local.scroll_from_bottom = 0;
        }
        KeyCode::Esc => {
            if local.request_panel {
                local.request_panel = false;
                return false;
            }
            if local.workflow.visible {
                local.workflow.visible = false;
                local.workflow.link_cursor = None;
                local.workflow.scroll = 0;
                local.workflow.manual_scroll = false;
                return false;
            }
            local.help = false;
            local.workflow.visible = false;
            local.evidence = false;
            local.editor.clear();
        }
        KeyCode::PageUp => {
            local.conversation_focus = None;
            local.scroll_from_bottom = local.scroll_from_bottom.saturating_add(8);
        }
        KeyCode::PageDown => {
            local.conversation_focus = None;
            local.scroll_from_bottom = local.scroll_from_bottom.saturating_sub(8);
        }
        KeyCode::Home if control => {
            local.conversation_focus = None;
            local.scroll_from_bottom = usize::MAX;
        }
        KeyCode::End if control => {
            local.conversation_focus = None;
            local.scroll_from_bottom = 0;
        }
        KeyCode::Left => local.editor.left(),
        KeyCode::Right => local.editor.right(),
        KeyCode::Home => local.editor.cursor = 0,
        KeyCode::End => local.editor.cursor = local.editor.text.len(),
        KeyCode::Backspace => local.editor.backspace(),
        KeyCode::Delete => local.editor.delete(),
        KeyCode::Enter if key.modifiers.contains(KeyModifiers::SHIFT) => local.editor.insert("\n"),
        KeyCode::Enter if !local.editor.text.trim().is_empty() => {
            let text = local.editor.text.clone();
            if let Some(request) = selected_request(snapshot, local) {
                if let RequestKind::UserInput { questions } = &request.kind {
                    if request_locked(snapshot, local, request) {
                        local.notice = Some(
                            "Request is submitted or unavailable; wait for resolution.".into(),
                        );
                        return false;
                    }
                    local
                        .answers
                        .insert(questions[local.question_index].id.clone(), vec![text]);
                    if let Err(error) = request.validate_input_answers(
                        &local.answers,
                        local.question_index + 1 == questions.len(),
                    ) {
                        local.answers.remove(&questions[local.question_index].id);
                        local.notice = Some(error.to_string());
                        return false;
                    }
                    if local.question_index + 1 < questions.len() {
                        local.question_index += 1;
                        local.editor.clear();
                    } else if send(
                        Command::AnswerUserInput {
                            request: request.reference(),
                            answers: local.answers.clone(),
                        },
                        tx,
                        local,
                    ) {
                        local.submitted.insert(request.reference());
                        local.editor.clear();
                        local.answers.clear();
                    }
                    return false;
                }
            }
            if local.request_panel
                || local.request_selection.is_some() && selected_request(snapshot, local).is_none()
            {
                local.notice =
                    Some("Close request details with Esc before submitting a root task.".into());
                return false;
            }
            let explicit_queue = control && key.code == KeyCode::Enter;
            if !(snapshot.phase.can_submit()
                || snapshot.phase == crate::state::SessionPhase::GatePending
                || explicit_queue)
                || snapshot.thread_id.is_none()
            {
                local.notice =
                    Some("Wait for the current turn, or use Ctrl+C to interrupt.".into());
            } else if snapshot.queued_inputs >= ROOT_QUEUE_LIMIT {
                local.notice =
                    Some("The task queue is full (8 tasks); your draft is retained.".into());
            } else if send(
                if explicit_queue {
                    let mut task = RootTaskSpec::input(text);
                    if let Some(active) = snapshot.scheduler.active_root {
                        task.dependencies.push(active.task);
                    }
                    Command::QueueRootTasks { tasks: vec![task] }
                } else {
                    Command::SubmitRootInput { text }
                },
                tx,
                local,
            ) {
                local.editor.clear();
                local.scroll_from_bottom = 0;
            }
        }
        KeyCode::Char(c) if !control && !key.modifiers.contains(KeyModifiers::ALT) => {
            local.editor.insert(&c.to_string())
        }
        _ => {}
    }
    false
}

fn selected_agent_id<'a>(snapshot: &'a CoreSnapshot, local: &LocalState) -> &'a str {
    local
        .agent_id
        .as_ref()
        .and_then(|id| snapshot.agents.iter().find(|agent| &agent.info.id == id))
        .map_or("root", |agent| agent.info.id.as_str())
}

fn apply_workflow_navigation(result: WorkflowLinkNavigation, local: &mut LocalState) {
    if let Some(task_id) = result.task_id {
        local.workflow.selected_id = Some(task_id);
        local.workflow.scroll = 0;
        local.workflow.manual_scroll = false;
    }
    local.workflow.link_cursor = result.cursor;
    local.notice = result.notice;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::RpcId;

    fn observed_snapshot() -> CoreSnapshot {
        observed_snapshot_with_requests(&[])
    }

    #[test]
    fn command_palette_is_modal_and_opens_search_without_commands() {
        let snapshot = observed_snapshot();
        let (tx, mut rx) = tokio::sync::mpsc::channel(4);
        let mut local = LocalState::default();
        local.editor.insert("TASK_DRAFT");
        local.task_draft = Some(Editor {
            text: "SECRET_TASK_DRAFT".into(),
            cursor: "SECRET_TASK_DRAFT".len(),
        });
        local
            .answers
            .insert("secret".into(), vec!["SECRET_ANSWER".into()]);

        handle_key(
            KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL),
            &snapshot,
            &mut local,
            &tx,
        );
        assert!(local.palette.visible);
        handle_key(
            KeyEvent::new(KeyCode::Char('c'), KeyModifiers::NONE),
            &snapshot,
            &mut local,
            &tx,
        );
        handle_paste("中文🙂\n", &snapshot, &mut local);
        assert!(local.palette.visible);
        assert!(!local.context.visible);
        assert_eq!(local.editor.text, "TASK_DRAFT");
        assert!(rx.try_recv().is_err());
        handle_key(
            KeyEvent::new(KeyCode::F(12), KeyModifiers::NONE),
            &snapshot,
            &mut local,
            &tx,
        );
        assert!(local.palette.visible);
        assert!(!local.search.visible);
        assert!(!local.timeline.visible);
        assert!(rx.try_recv().is_err());

        handle_key(
            KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
            &snapshot,
            &mut local,
            &tx,
        );
        assert!(!local.palette.visible);
        assert_eq!(local.editor.text, "TASK_DRAFT");

        handle_key(
            KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL),
            &snapshot,
            &mut local,
            &tx,
        );
        local.palette.paste("search");
        handle_key(
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            &snapshot,
            &mut local,
            &tx,
        );
        assert!(!local.palette.visible);
        assert!(local.search.visible);
        assert_eq!(local.editor.text, "TASK_DRAFT");
        assert_eq!(
            local.task_draft.as_ref().map(|draft| draft.text.as_str()),
            Some("SECRET_TASK_DRAFT")
        );
        assert_eq!(
            local.answers.get("secret"),
            Some(&vec!["SECRET_ANSWER".to_string()])
        );
        assert!(rx.try_recv().is_err());

        handle_key(
            KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL),
            &snapshot,
            &mut local,
            &tx,
        );
        assert!(local.palette.visible);
        assert!(local.search.visible);
        handle_key(
            KeyEvent::new(KeyCode::F(12), KeyModifiers::NONE),
            &snapshot,
            &mut local,
            &tx,
        );
        assert!(local.palette.visible);
        assert!(local.search.visible);
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn command_palette_action_closes_mutual_overlay_and_renders_no_results() {
        use ratatui::backend::TestBackend;

        let snapshot = observed_snapshot();
        let (tx, mut rx) = tokio::sync::mpsc::channel(4);
        let mut local = LocalState::default();
        local.timeline.open("root-thread".into(), &snapshot);
        handle_key(
            KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL),
            &snapshot,
            &mut local,
            &tx,
        );
        assert!(local.palette.visible);
        local.palette.paste("zzzz");
        let mut terminal = Terminal::new(TestBackend::new(30, 10)).unwrap();
        terminal
            .draw(|frame| draw(frame, &snapshot, &local))
            .unwrap();
        let screen: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(screen.contains("No matching commands."), "{screen}");
        local.palette.close();
        handle_key(
            KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL),
            &snapshot,
            &mut local,
            &tx,
        );
        local.palette.paste("context");
        handle_key(
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            &snapshot,
            &mut local,
            &tx,
        );
        assert!(!local.search.visible);
        assert!(!local.timeline.visible);
        assert!(local.context.visible);
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn opening_skills_panel_is_local_and_enter_requests_a_refresh() {
        let snapshot = observed_snapshot();
        let (tx, mut rx) = tokio::sync::mpsc::channel(4);
        let mut local = LocalState::default();
        handle_key(
            KeyEvent::new(KeyCode::Char('k'), KeyModifiers::CONTROL),
            &snapshot,
            &mut local,
            &tx,
        );
        assert!(local.skills);
        assert!(rx.try_recv().is_err());
        handle_key(
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            &snapshot,
            &mut local,
            &tx,
        );
        assert_eq!(rx.try_recv().unwrap(), Command::RefreshSkills);
        handle_key(
            KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
            &snapshot,
            &mut local,
            &tx,
        );
        assert!(!local.skills);
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn context_panel_routes_locally_and_scrolls_or_closes_without_commands() {
        let snapshot = observed_snapshot();
        let (tx, mut rx) = tokio::sync::mpsc::channel(4);
        let mut local = LocalState::default();
        handle_key(
            KeyEvent::new(KeyCode::Char('c'), KeyModifiers::NONE),
            &snapshot,
            &mut local,
            &tx,
        );
        assert_eq!(local.editor.text, "c");
        local.attention_editor = Some(AttentionEditor::new(&snapshot, 0));
        local.request_panel = true;

        handle_key(
            KeyEvent::new(KeyCode::Char('g'), KeyModifiers::CONTROL),
            &snapshot,
            &mut local,
            &tx,
        );
        assert!(local.context.visible);
        assert!(local.attention_editor.is_none());
        assert!(!local.request_panel);
        assert_eq!(local.editor.text, "c");
        handle_key(
            KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE),
            &snapshot,
            &mut local,
            &tx,
        );
        assert_eq!(local.context.scroll, 8);
        handle_key(
            KeyEvent::new(KeyCode::PageUp, KeyModifiers::NONE),
            &snapshot,
            &mut local,
            &tx,
        );
        assert_eq!(local.context.scroll, 0);
        handle_key(
            KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE),
            &snapshot,
            &mut local,
            &tx,
        );
        handle_key(
            KeyEvent::new(KeyCode::Home, KeyModifiers::CONTROL),
            &snapshot,
            &mut local,
            &tx,
        );
        assert_eq!(local.context.scroll, 0);
        handle_key(
            KeyEvent::new(KeyCode::Home, KeyModifiers::NONE),
            &snapshot,
            &mut local,
            &tx,
        );
        assert_eq!(local.context.scroll, 0);
        handle_key(
            KeyEvent::new(KeyCode::End, KeyModifiers::CONTROL),
            &snapshot,
            &mut local,
            &tx,
        );
        assert_eq!(local.context.scroll, 0);
        handle_key(
            KeyEvent::new(KeyCode::PageUp, KeyModifiers::NONE),
            &snapshot,
            &mut local,
            &tx,
        );
        assert_eq!(local.context.scroll, 8);
        handle_key(
            KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE),
            &snapshot,
            &mut local,
            &tx,
        );
        assert_eq!(local.context.scroll, 0);
        handle_key(
            KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
            &snapshot,
            &mut local,
            &tx,
        );
        assert!(!local.context.visible);
        assert_eq!(local.context.scroll, 0);
        handle_key(
            KeyEvent::new(KeyCode::Char('g'), KeyModifiers::CONTROL),
            &snapshot,
            &mut local,
            &tx,
        );
        handle_key(
            KeyEvent::new(KeyCode::Char('g'), KeyModifiers::CONTROL),
            &snapshot,
            &mut local,
            &tx,
        );
        assert!(!local.context.visible);
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn context_panel_uses_selected_agent_usage_and_keeps_budget_session_wide() {
        use crate::agents::{AgentInfo, AgentSnapshot};
        use crate::observation::{CompactionFact, CompactionFactStatus};

        let mut snapshot = observed_snapshot();
        snapshot.usage = crate::state::UsageSummary {
            total_tokens: Some(900),
            input_tokens: Some(800),
            source: FactSource::ServerConfirmed,
            ..Default::default()
        };
        snapshot.token_budget = crate::state::TokenBudgetSnapshot {
            confirmed_total_tokens: Some(900),
            confirmed_complete: true,
            limit: Some(1_000),
            stop_triggered: false,
            per_agent_limit: Some(300),
            per_agent_stop_triggered: true,
        };
        let child = AgentSnapshot {
            info: AgentInfo {
                id: "child-thread".into(),
                parent_id: "root-thread".into(),
                path: Some("/root/worker".into()),
                nickname: None,
                role: None,
                model: None,
                confirmed: true,
            },
            generation: 1,
            turn_id: Some("child-turn".into()),
            outcome: None,
            awaiting_turn: false,
            usage: crate::state::UsageSummary {
                total_tokens: Some(12),
                input_tokens: Some(7),
                cached_input_tokens: Some(3),
                output_tokens: Some(4),
                reasoning_tokens: Some(2),
                context_window: Some(80),
                source: FactSource::ServerConfirmed,
            },
        };
        snapshot.agents.push(child.clone());
        snapshot.usage_facts = vec![
            crate::state::UsageFact {
                summary: snapshot.usage,
                identity: crate::state::UsageIdentity {
                    thread_id: Some("root-thread".into()),
                    turn_id: Some("turn".into()),
                    generation: Some(1),
                },
            },
            crate::state::UsageFact {
                summary: child.usage,
                identity: crate::state::UsageIdentity {
                    thread_id: Some("child-thread".into()),
                    turn_id: Some("child-turn".into()),
                    generation: Some(1),
                },
            },
        ];
        snapshot.observation.compactions.push(CompactionFact {
            thread_id: "root-thread".into(),
            turn_id: "root-turn".into(),
            item_id: "root-compaction".into(),
            status: CompactionFactStatus::Completed,
            started_at_ms: Some(1),
            completed_at_ms: Some(2),
            input_tokens: Some(900),
            cached_input_tokens: Some(100),
            output_tokens: Some(80),
            total_tokens: Some(980),
            context_window: Some(1_000),
        });
        snapshot.observation.compactions.push(CompactionFact {
            thread_id: "child-thread".into(),
            turn_id: "child-turn".into(),
            item_id: "child-compaction".into(),
            status: CompactionFactStatus::Unknown,
            started_at_ms: None,
            completed_at_ms: None,
            input_tokens: Some(8),
            cached_input_tokens: None,
            output_tokens: None,
            total_tokens: Some(9),
            context_window: None,
        });

        for selected in [None, Some(&child)] {
            let mut context_panel = context::ContextPanel::default();
            context_panel.visible = true;
            let local = LocalState {
                context: context_panel,
                agent_id: selected.map(|agent| agent.info.id.clone()),
                ..Default::default()
            };
            let mut terminal = Terminal::new(TestBackend::new(160, 20)).unwrap();
            terminal
                .draw(|frame| draw(frame, &snapshot, &local))
                .unwrap();
            let screen = terminal
                .backend()
                .buffer()
                .content()
                .iter()
                .map(|cell| cell.symbol())
                .collect::<String>();
            assert!(screen.contains("Session-wide token budget"), "{screen}");
            assert!(screen.contains("total 900"), "{screen}");
            assert!(screen.contains("limit 1000"), "{screen}");
            assert!(screen.contains("completeness complete"), "{screen}");
            assert!(screen.contains("per-agent limit 300"), "{screen}");
            if selected.is_some() {
                assert!(
                    screen.contains("Usage (child child-thread) source: server confirmed"),
                    "{screen}"
                );
                assert!(screen.contains("total 12 | input 7"), "{screen}");
                assert!(screen.contains("Compaction #1"), "{screen}");
                assert!(screen.contains("status Unknown"), "{screen}");
                assert!(screen.contains("cached input unavailable"), "{screen}");
                assert!(screen.contains("Usage identity child · thread child-thread · turn child-turn · generation 1"), "{screen}");
                assert!(
                    screen.contains(
                        "most recent server-confirmed value for this thread/turn/generation"
                    ),
                    "{screen}"
                );
                assert!(!screen.contains("root-compaction"), "{screen}");
                assert!(!screen.contains("total 900 | input 800"), "{screen}");
            } else {
                assert!(
                    screen.contains("Usage (root) source: server confirmed"),
                    "{screen}"
                );
                assert!(
                    screen.contains(
                        "Usage identity root · thread root-thread · turn turn · generation 1"
                    ),
                    "{screen}"
                );
                assert!(
                    screen.contains(
                        "most recent server-confirmed value for this thread/turn/generation"
                    ),
                    "{screen}"
                );
                assert!(screen.contains("total 900 | input 800"), "{screen}");
                assert!(screen.contains("root-compaction"), "{screen}");
                assert!(!screen.contains("child-compaction"), "{screen}");
            }
        }

        let unknown = CoreSnapshot::default();
        let mut terminal = Terminal::new(TestBackend::new(120, 16)).unwrap();
        let mut context_panel = context::ContextPanel::default();
        context_panel.visible = true;
        terminal
            .draw(|frame| context_panel.draw(frame, frame.area(), &unknown, None))
            .unwrap();
        let screen = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(screen.contains("source: unknown"), "{screen}");
        assert!(screen.contains("Usage identity root · thread unavailable · turn unavailable · generation unavailable"), "{screen}");
        assert!(
            screen.contains("current thread/turn/generation usage unavailable"),
            "{screen}"
        );
        assert!(screen.contains("total unavailable"), "{screen}");
        assert!(screen.contains("limit unavailable"), "{screen}");
        assert!(screen.contains("completeness unavailable"), "{screen}");
        assert!(screen.contains("Compaction facts: unavailable"), "{screen}");

        let mut narrow = Terminal::new(TestBackend::new(32, 8)).unwrap();
        context_panel.handle_key(KeyEvent::new(KeyCode::End, KeyModifiers::NONE), 8);
        narrow
            .draw(|frame| context_panel.draw(frame, frame.area(), &unknown, None))
            .unwrap();
    }

    #[test]
    fn help_view_lists_the_context_panel_shortcut() {
        let snapshot = observed_snapshot();
        let local = LocalState {
            help: true,
            ..Default::default()
        };
        let mut terminal = Terminal::new(TestBackend::new(160, 30)).unwrap();
        terminal
            .draw(|frame| draw(frame, &snapshot, &local))
            .unwrap();
        let screen = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(screen.contains("Ctrl+G context"), "{screen}");
    }

    #[test]
    fn skills_panel_f2_opens_selected_request_without_losing_its_reference_or_draft() {
        let request = RequestView::decode(
            RpcId::Number(93),
            "item/commandExecution/requestApproval",
            &serde_json::json!({"threadId":"root-thread","turnId":"turn","command":"safe"}),
        )
        .unwrap();
        let reference = request.reference();
        let snapshot = observed_snapshot_with_requests(&[request]);
        let (tx, mut rx) = tokio::sync::mpsc::channel(4);
        let mut local = LocalState {
            skills: true,
            request_selection: Some(reference.clone()),
            ..Default::default()
        };
        local.editor.insert("TASK_DRAFT");
        handle_key(
            KeyEvent::new(KeyCode::F(2), KeyModifiers::NONE),
            &snapshot,
            &mut local,
            &tx,
        );
        assert!(!local.skills);
        assert!(local.request_panel);
        assert_eq!(
            selected_request(&snapshot, &local).unwrap().reference(),
            reference
        );
        assert_eq!(local.editor.text, "TASK_DRAFT");
        assert!(rx.try_recv().is_err());

        handle_key(
            KeyEvent::new(KeyCode::Char('y'), KeyModifiers::CONTROL),
            &snapshot,
            &mut local,
            &tx,
        );
        assert_eq!(
            rx.try_recv().unwrap(),
            Command::AnswerApproval {
                request: reference,
                decision: ApprovalDecision::Accept,
            }
        );
    }

    #[test]
    fn narrow_skills_panel_renders_and_scrolls_a_long_list() {
        use ratatui::backend::TestBackend;

        let mut snapshot = observed_snapshot();
        snapshot.skills.availability = crate::skills::SkillAvailability::Available;
        snapshot.skills.freshness = crate::skills::SkillFreshness::Current;
        snapshot.skills.skill_count = 20;
        snapshot.skills.enabled_count = 20;
        snapshot.skills.entries = (0..20)
            .map(|index| crate::skills::SkillEntry {
                name: format!("skill-{index}"),
                path: format!("/workspace/skill-{index}/SKILL.md"),
                scope: "repo".into(),
                enabled: true,
            })
            .collect();
        let mut local = LocalState {
            skills: true,
            viewport: ratatui::layout::Rect::new(0, 0, 30, 10),
            ..Default::default()
        };
        let (tx, _rx) = tokio::sync::mpsc::channel(4);
        let mut terminal = Terminal::new(TestBackend::new(30, 10)).unwrap();
        terminal
            .draw(|frame| draw(frame, &snapshot, &local))
            .unwrap();
        let initial: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(initial.contains("Skills inventory"));
        assert!(initial.contains("Source: AppServer"));
        assert!(initial.contains("skill-0"));
        let page_size = skills_panel_entries_capacity(local.viewport).max(1);

        handle_key(
            KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE),
            &snapshot,
            &mut local,
            &tx,
        );
        terminal
            .draw(|frame| draw(frame, &snapshot, &local))
            .unwrap();
        let scrolled: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(scrolled.contains(&format!("skill-{page_size}")));
        assert!(!scrolled.contains("skill-0"));

        snapshot.skills.availability = crate::skills::SkillAvailability::Unsupported;
        snapshot.skills.freshness = crate::skills::SkillFreshness::Stale;
        for (width, height) in [(30, 10), (40, 12)] {
            local.viewport = ratatui::layout::Rect::new(0, 0, width, height);
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            terminal
                .draw(|frame| draw(frame, &snapshot, &local))
                .unwrap();
            let unavailable: String = terminal
                .backend()
                .buffer()
                .content()
                .iter()
                .map(|cell| cell.symbol())
                .collect();
            assert!(
                unavailable.contains("Unsupported / Stale"),
                "status clipped at {width}x{height}"
            );
            assert!(
                unavailable.contains("Unavailable: Unsupported"),
                "unavailable source clipped at {width}x{height}"
            );
            assert!(
                unavailable.contains("Running req 0"),
                "phase/request count clipped at {width}x{height}"
            );
        }
        snapshot.skills.availability = crate::skills::SkillAvailability::Available;
        snapshot.skills.freshness = crate::skills::SkillFreshness::Current;

        for (width, height) in [(40, 12), (80, 24)] {
            local.viewport = ratatui::layout::Rect::new(0, 0, width, height);
            local.skills_scroll = 0;
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            terminal
                .draw(|frame| draw(frame, &snapshot, &local))
                .unwrap();
            local.skills_scroll = snapshot
                .skills
                .entries
                .len()
                .saturating_sub(skills_panel_entries_capacity(local.viewport));
            terminal
                .draw(|frame| draw(frame, &snapshot, &local))
                .unwrap();
            let tail: String = terminal
                .backend()
                .buffer()
                .content()
                .iter()
                .map(|cell| cell.symbol())
                .collect();
            assert!(
                tail.contains("skill-19"),
                "last entry clipped at {width}x{height}"
            );
        }
    }

    #[test]
    fn skills_panel_renders_refresh_source_and_legacy_unavailable_value() {
        use ratatui::backend::TestBackend;

        let mut snapshot = observed_snapshot();
        snapshot.skills.entries.clear();
        snapshot.skills.skill_count = 0;
        snapshot.skills.enabled_count = 0;
        snapshot.skills.availability = crate::skills::SkillAvailability::Available;
        snapshot.skills.freshness = crate::skills::SkillFreshness::Current;
        let mut local = LocalState {
            skills: true,
            ..Default::default()
        };

        for (source, label) in [
            (Some(crate::skills::SkillRefreshSource::Initial), "Initial"),
            (Some(crate::skills::SkillRefreshSource::Changed), "Changed"),
            (Some(crate::skills::SkillRefreshSource::Manual), "Manual"),
            (None, "unavailable"),
        ] {
            snapshot.skills.refresh_source = source;
            for (width, height) in [(30, 10), (80, 24)] {
                local.viewport = ratatui::layout::Rect::new(0, 0, width, height);
                let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
                terminal
                    .draw(|frame| draw(frame, &snapshot, &local))
                    .unwrap();
                let text: String = terminal
                    .backend()
                    .buffer()
                    .content()
                    .iter()
                    .map(|cell| cell.symbol())
                    .collect();
                let refresh_line = if height < 20 {
                    format!("Refresh: {label}")
                } else {
                    format!("Refresh source: {label}")
                };
                assert!(
                    text.contains(&refresh_line),
                    "refresh source clipped at {width}x{height}"
                );
                assert!(!text.contains("PRIVATE_PROMPT"));
            }
        }
    }

    fn observed_snapshot_with_requests(requests: &[RequestView]) -> CoreSnapshot {
        use crate::observation::{ObservationFacts, Observer};
        use crate::scheduler::{ExternalTurn, Scheduler};
        let now = tokio::time::Instant::now();
        let mut scheduler = Scheduler::default();
        scheduler
            .enqueue(vec![RootTaskSpec::input("private task".into())])
            .unwrap();
        let attempt = scheduler.dispatch().unwrap().attempt;
        scheduler
            .started_root(
                attempt,
                ExternalTurn {
                    thread_id: "root-thread".into(),
                    turn_id: "turn".into(),
                    generation: 1,
                },
            )
            .unwrap();
        let mut observer = Observer::new_at("ui-fixture".into(), Default::default(), now, None);
        observer
            .reconcile(
                ObservationFacts {
                    phase: crate::state::SessionPhase::Running,
                    root_thread: Some("root-thread"),
                    root_generation: 1,
                    root_task: scheduler.task(attempt.task),
                    children: vec![],
                    requests,
                    gate: None,
                },
                now,
            )
            .unwrap();
        CoreSnapshot {
            phase: crate::state::SessionPhase::Running,
            observation: observer.snapshot_at(1, now + Duration::from_secs(31)),
            timeline: observer.timeline_snapshot(),
            requests: requests.to_vec(),
            thread_id: Some("root-thread".into()),
            turn_id: Some("turn".into()),
            ..Default::default()
        }
    }

    #[test]
    fn timeline_browsing_preserves_secret_and_task_drafts_and_never_sends_commands() {
        let mut request = RequestView::decode(
            RpcId::Number(7),
            "item/tool/requestUserInput",
            &serde_json::json!({"threadId":"root-thread","turnId":"turn","questions":[{
                "id":"secret","header":"Secret","question":"Value?","isSecret":true}]}),
        )
        .unwrap();
        request.received_seq = 10;
        let snapshot = observed_snapshot_with_requests(&[request.clone()]);
        let (tx, mut rx) = tokio::sync::mpsc::channel(32);
        let mut local = LocalState::default();
        local.editor.insert("PRIVATE_TASK_DRAFT");
        sync_local_requests(&mut local, &snapshot);
        local.editor.insert("PRIVATE_SECRET_DRAFT中文👋");
        let open = KeyEvent::new(KeyCode::Char('t'), KeyModifiers::CONTROL);
        assert!(!handle_key(open, &snapshot, &mut local, &tx));
        assert!(local.timeline.visible);
        for (code, modifiers) in [
            (KeyCode::Char('y'), KeyModifiers::CONTROL),
            (KeyCode::Char('n'), KeyModifiers::CONTROL),
            (KeyCode::Char('b'), KeyModifiers::CONTROL),
            (KeyCode::Char('s'), KeyModifiers::CONTROL),
            (KeyCode::Char('o'), KeyModifiers::CONTROL),
            (KeyCode::F(6), KeyModifiers::NONE),
            (KeyCode::F(7), KeyModifiers::NONE),
            (KeyCode::F(8), KeyModifiers::NONE),
            (KeyCode::Char('/'), KeyModifiers::NONE),
        ] {
            assert!(!handle_key(
                KeyEvent::new(code, modifiers),
                &snapshot,
                &mut local,
                &tx
            ));
        }
        handle_paste("bB中文\nmetadata", &snapshot, &mut local);
        for (width, height) in [
            (30, 10),
            (60, 20),
            (80, 24),
            (100, 30),
            (120, 40),
            (160, 50),
        ] {
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            terminal
                .draw(|frame| draw(frame, &snapshot, &local))
                .unwrap();
            let text = terminal
                .backend()
                .buffer()
                .content
                .iter()
                .map(|cell| cell.symbol())
                .collect::<String>();
            assert!(text.contains("Timeline"));
            assert!(text.contains("action:1"));
            assert!(!text.contains("PRIVATE_SECRET") && !text.contains("PRIVATE_TASK"));
        }
        assert!(rx.try_recv().is_err());
        assert_eq!(local.editor.text, "PRIVATE_SECRET_DRAFT中文👋");
        assert_eq!(
            local.task_draft.as_ref().unwrap().text,
            "PRIVATE_TASK_DRAFT"
        );
        handle_key(
            KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
            &snapshot,
            &mut local,
            &tx,
        );
        handle_key(
            KeyEvent::new(KeyCode::Char('f'), KeyModifiers::CONTROL),
            &snapshot,
            &mut local,
            &tx,
        );
        handle_key(open, &snapshot, &mut local, &tx); // Search keeps Ctrl+T as its own turn field.
        assert!(local.search.visible && !local.timeline.visible);
        handle_key(
            KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
            &snapshot,
            &mut local,
            &tx,
        );
        assert!(rx.try_recv().is_err());
        assert_eq!(local.editor.text, "PRIVATE_SECRET_DRAFT中文👋");
    }

    #[test]
    fn portable_newline_and_submit_keys_use_the_existing_typed_command_paths() {
        use crate::scheduler::{ExternalTurn, Scheduler};
        let mut scheduler = Scheduler::default();
        scheduler
            .enqueue(vec![RootTaskSpec::input("active root".into())])
            .unwrap();
        let attempt = scheduler.dispatch().unwrap().attempt;
        scheduler
            .started_root(
                attempt,
                ExternalTurn {
                    thread_id: "root".into(),
                    turn_id: "one".into(),
                    generation: 1,
                },
            )
            .unwrap();
        let snapshot = CoreSnapshot {
            phase: SessionPhase::Running,
            thread_id: Some("root".into()),
            scheduler: scheduler.snapshot(),
            ..Default::default()
        };
        let (tx, mut rx) = tokio::sync::mpsc::channel(8);
        let mut local = LocalState::default();
        local.editor.insert("FIRST中文👋");
        for event in input::fixture_events("\x0f") {
            let InputEvent::Key(key) = event else {
                panic!("expected a key");
            };
            assert!(!handle_key(key, &snapshot, &mut local, &tx));
        }
        local.editor.insert("SECOND");
        assert_eq!(local.editor.text, "FIRST中文👋\nSECOND");
        assert!(rx.try_recv().is_err());
        for event in input::fixture_events("\x13") {
            let InputEvent::Key(key) = event else {
                panic!("expected a key");
            };
            assert!(!handle_key(key, &snapshot, &mut local, &tx));
        }
        let Command::QueueRootTasks { tasks } = rx.try_recv().unwrap() else {
            panic!("expected typed queue command");
        };
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].text, "FIRST中文👋\nSECOND");
        assert_eq!(tasks[0].dependencies, vec![attempt.task]);
        assert!(local.editor.text.is_empty());
        assert!(rx.try_recv().is_err());
        local.editor.insert("TASK_DRAFT");
        handle_key(
            KeyEvent::new(KeyCode::Char('f'), KeyModifiers::CONTROL),
            &snapshot,
            &mut local,
            &tx,
        );
        for event in input::fixture_events("\x0f\x13") {
            let InputEvent::Key(key) = event else {
                panic!("expected a key");
            };
            handle_key(key, &snapshot, &mut local, &tx);
        }
        assert!(rx.try_recv().is_err());
        assert_eq!(local.editor.text, "TASK_DRAFT");
        local.search.close();
        let request = RequestView::decode(RpcId::Number(1), "item/tool/requestUserInput", &serde_json::json!({"threadId":"root","turnId":"one","questions":[{"id":"secret","header":"Secret","question":"Value?","isSecret":true}]})).unwrap();
        let snapshot = CoreSnapshot {
            requests: vec![request],
            ..snapshot
        };
        handle_paste("ANSWER中文👋", &snapshot, &mut local);
        handle_key(
            KeyEvent::new(KeyCode::Char('o'), KeyModifiers::CONTROL),
            &snapshot,
            &mut local,
            &tx,
        );
        handle_paste("SECOND", &snapshot, &mut local);
        handle_key(
            KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL),
            &snapshot,
            &mut local,
            &tx,
        );
        let Command::AnswerUserInput { request, answers } = rx.try_recv().unwrap() else {
            panic!("expected typed answer command");
        };
        assert_eq!(request, snapshot.requests[0].reference());
        assert_eq!(answers["secret"], vec!["ANSWER中文👋\nSECOND"]);
        handle_key(
            KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL),
            &snapshot,
            &mut local,
            &tx,
        );
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn terminal_paste_retains_secret_and_task_drafts_and_never_sends_commands() {
        let request = RequestView::decode(RpcId::Number(1), "item/tool/requestUserInput", &serde_json::json!({"threadId":"root","turnId":"one","questions":[{"id":"secret","header":"Secret","question":"Value?","isSecret":true}]})).unwrap();
        let snapshot = CoreSnapshot {
            phase: SessionPhase::Running,
            thread_id: Some("root".into()),
            root_start_requests: 1,
            requests: vec![request],
            ..Default::default()
        };
        let (tx, mut rx) = tokio::sync::mpsc::channel(8);
        let mut local = LocalState::default();
        local.editor.insert("SAVED_TASK");
        let secret = "SECRET_PASTE中文👋\r\nSECRET_LINE\x19\x0e\x03\x11";
        let events = input::fixture_events(&format!("\x1b[200~{secret}\x1b[201~"));
        assert_eq!(events.len(), 1);
        for event in events {
            match event {
                InputEvent::Paste(text) => handle_paste(&text, &snapshot, &mut local),
                InputEvent::Key(key) => {
                    assert!(!handle_key(key, &snapshot, &mut local, &tx));
                }
                _ => panic!("unexpected input event"),
            }
        }
        assert_eq!(local.editor.text, "SECRET_PASTE中文👋\nSECRET_LINE");
        assert_eq!(local.task_draft.as_ref().unwrap().text, "SAVED_TASK");
        assert!(rx.try_recv().is_err());
        assert!(local.submitted.is_empty());
        assert!(!handle_key(
            KeyEvent::new(KeyCode::Char('f'), KeyModifiers::CONTROL),
            &snapshot,
            &mut local,
            &tx
        ));
        let events =
            input::fixture_events("\x1b[200~QUERY中文👋\rSECOND\x19\x0e\x03\x11\x1b[24~\x1b[201~");
        assert_eq!(events.len(), 1);
        let InputEvent::Paste(text) = &events[0] else {
            panic!("expected a paste");
        };
        handle_paste(text, &snapshot, &mut local);
        let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
        terminal
            .draw(|frame| draw(frame, &snapshot, &local))
            .unwrap();
        let rendered: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        // TestBackend retains padding cells for wide glyphs.
        assert!(rendered.replace(' ', "").contains("QUERY中文👋SECOND"));
        assert!(!rendered.contains("SECRET_PASTE"));
        assert!(!rendered.contains("SECRET_LINE"));
        assert!(!rendered.contains("SAVED_TASK"));
        assert!(rx.try_recv().is_err());
        assert_eq!(snapshot.root_start_requests, 1);
        assert_eq!(local.editor.text, "SECRET_PASTE中文👋\nSECRET_LINE");
        assert!(!handle_key(
            KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
            &snapshot,
            &mut local,
            &tx
        ));
        assert_eq!(local.editor.text, "SECRET_PASTE中文👋\nSECRET_LINE");
    }

    #[test]
    fn rejected_terminal_paste_leaves_the_editor_unchanged_and_shows_a_notice_in_search() {
        let snapshot = CoreSnapshot {
            phase: SessionPhase::Ready,
            thread_id: Some("root".into()),
            ..Default::default()
        };
        let (tx, mut rx) = tokio::sync::mpsc::channel(8);
        let mut local = LocalState::default();
        local.editor.insert("SAVED_TASK");
        let events = input::fixture_events(&format!(
            "\x1b[200~{}\x11\x19\n\x1b[201~",
            "a".repeat(input::PASTE_BYTES + 1)
        ));
        assert_eq!(events, vec![InputEvent::PasteRejected]);
        reject_paste(&mut local);
        assert_eq!(local.editor.text, "SAVED_TASK");
        assert_eq!(local.notice.as_deref(), Some(PASTE_REJECTED));
        handle_key(
            KeyEvent::new(KeyCode::Char('f'), KeyModifiers::CONTROL),
            &snapshot,
            &mut local,
            &tx,
        );
        handle_paste("QUERY", &snapshot, &mut local);
        reject_paste(&mut local);
        let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
        terminal
            .draw(|frame| draw(frame, &snapshot, &local))
            .unwrap();
        let rendered: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(rendered.contains("32 KiB"));
        assert!(rendered.contains("QUERY"));
        assert_eq!(local.editor.text, "SAVED_TASK");
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn threshold_editor_keeps_task_and_secret_buffers_separate_and_validates_before_sending() {
        let mut snapshot = observed_snapshot();
        snapshot.requests.push(RequestView::decode(RpcId::Number(1), "item/tool/requestUserInput", &serde_json::json!({"threadId":"root-thread","turnId":"turn","questions":[{"id":"secret","header":"Secret","question":"Value?","isSecret":true}]})).unwrap());
        let (tx, mut rx) = tokio::sync::mpsc::channel(8);
        let mut local = LocalState::default();
        local.editor.insert("TASK_DRAFT");
        sync_questions(&mut local, snapshot.requests.first().cloned());
        local.editor.insert("SECRET_ANSWER");
        let key = |code, modifiers| KeyEvent::new(code, modifiers);
        handle_key(
            key(KeyCode::F(10), KeyModifiers::NONE),
            &snapshot,
            &mut local,
            &tx,
        );
        handle_key(
            key(KeyCode::Char('u'), KeyModifiers::CONTROL),
            &snapshot,
            &mut local,
            &tx,
        );
        handle_key(
            key(KeyCode::Enter, KeyModifiers::NONE),
            &snapshot,
            &mut local,
            &tx,
        );
        assert!(rx.try_recv().is_err());
        assert!(local.attention_editor.as_ref().unwrap().notice.is_some());
        handle_key(
            key(KeyCode::Char('5'), KeyModifiers::NONE),
            &snapshot,
            &mut local,
            &tx,
        );
        handle_key(
            key(KeyCode::Tab, KeyModifiers::NONE),
            &snapshot,
            &mut local,
            &tx,
        );
        handle_key(
            key(KeyCode::Char('u'), KeyModifiers::CONTROL),
            &snapshot,
            &mut local,
            &tx,
        );
        for digit in ['1', '0'] {
            handle_key(
                key(KeyCode::Char(digit), KeyModifiers::NONE),
                &snapshot,
                &mut local,
                &tx,
            );
        }
        handle_key(
            key(KeyCode::Enter, KeyModifiers::NONE),
            &snapshot,
            &mut local,
            &tx,
        );
        assert_eq!(
            rx.try_recv().unwrap(),
            Command::ConfigureAttention {
                class: AttentionClass::Model,
                quiet_ms: 5,
                attention_ms: 10
            }
        );
        assert_eq!(local.editor.text, "SECRET_ANSWER");
        assert_eq!(local.task_draft.as_ref().unwrap().text, "TASK_DRAFT");
        handle_key(
            key(KeyCode::F(10), KeyModifiers::NONE),
            &snapshot,
            &mut local,
            &tx,
        );
        snapshot.requests.clear();
        handle_key(
            key(KeyCode::Esc, KeyModifiers::NONE),
            &snapshot,
            &mut local,
            &tx,
        );
        assert_eq!(local.editor.text, "TASK_DRAFT");
        assert!(local.attention_editor.is_none());
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn evidence_and_threshold_views_render_at_supported_sizes_without_leaking_drafts() {
        let mut snapshot = observed_snapshot();
        snapshot.usage.total_tokens = Some(4);
        snapshot.token_budget.confirmed_total_tokens = Some(4);
        snapshot.token_budget.confirmed_complete = true;
        snapshot.token_budget.limit = Some(10);
        snapshot.diagnostics.transport_bytes_in = 123;
        snapshot.diagnostics.transport_bytes_out = 456;
        snapshot.diagnostics.control_events = 7;
        snapshot.diagnostics.telemetry_events = 8;
        snapshot.journal = Some(crate::journal::JournalView {
            session_id: "session".into(),
            submitted_seq: 2,
            committed_seq: 1,
            committed_version: 1,
            error: None,
        });
        snapshot.persistence = crate::state::PersistenceState::Submitted;
        for (width, height) in [(40, 12), (80, 24), (160, 45)] {
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            let mut local = LocalState::default();
            local.editor.insert("draft 中文 👋");
            terminal
                .draw(|frame| draw(frame, &snapshot, &local))
                .unwrap();
            let rendered: String = terminal
                .backend()
                .buffer()
                .content()
                .iter()
                .map(|cell| cell.symbol())
                .collect();
            assert!(rendered.contains("quiet 31s"), "{rendered}");
            assert!(rendered.contains("attention:1"), "{rendered}");
            if width >= 100 {
                assert!(rendered.contains("tokens:4/10"), "{rendered}");
            }
            local.evidence = true;
            terminal
                .draw(|frame| draw(frame, &snapshot, &local))
                .unwrap();
            let rendered: String = terminal
                .backend()
                .buffer()
                .content()
                .iter()
                .map(|cell| cell.symbol())
                .collect();
            assert!(rendered.contains("Evidence"), "{rendered}");
            assert!(rendered.contains("Transport bytes"), "{rendered}");
            if height > 16 {
                assert!(rendered.contains("tokens:4/10"), "{rendered}");
                assert!(rendered.contains("Session token budget"), "{rendered}");
            }
            if width >= 100 && height > 16 {
                assert!(rendered.contains("persistence Submitted"), "{rendered}");
            }
            local.attention_editor = Some(AttentionEditor::new(&snapshot, 0));
            terminal
                .draw(|frame| draw(frame, &snapshot, &local))
                .unwrap();
            let rendered: String = terminal
                .backend()
                .buffer()
                .content()
                .iter()
                .map(|cell| cell.symbol())
                .collect();
            assert!(rendered.contains("Quiet ms: 15000"), "{rendered}");
            assert!(rendered.contains("Attention ms: 30000"), "{rendered}");
            assert_eq!(local.editor.text, "draft 中文 👋");
        }
        let mut interrupted = snapshot.clone();
        interrupted.observation.activities[0].execution_state = ExecutionState::Interrupted;
        interrupted.observation.activities[0].kind = crate::observation::ActivityKind::Completed;
        assert!(activity_brief(&interrupted.observation.activities[0]).starts_with("Interrupted"));
    }

    #[test]
    fn f11_evidence_uses_selected_child_usage_and_keeps_budget_session_wide() {
        use crate::agents::{AgentInfo, AgentSnapshot};

        let mut snapshot = observed_snapshot();
        snapshot.usage = crate::state::UsageSummary {
            total_tokens: Some(900),
            input_tokens: Some(901),
            source: FactSource::ServerConfirmed,
            ..Default::default()
        };
        snapshot.token_budget.confirmed_total_tokens = Some(900);
        snapshot.token_budget.confirmed_complete = true;
        snapshot.token_budget.limit = Some(1_000);
        let child = AgentSnapshot {
            info: AgentInfo {
                id: "child-thread".into(),
                parent_id: "root-thread".into(),
                path: Some("/root/worker".into()),
                nickname: None,
                role: None,
                model: None,
                confirmed: true,
            },
            generation: 1,
            turn_id: Some("child-turn".into()),
            outcome: None,
            awaiting_turn: false,
            usage: crate::state::UsageSummary {
                total_tokens: Some(12),
                input_tokens: Some(7),
                cached_input_tokens: Some(3),
                output_tokens: Some(4),
                reasoning_tokens: Some(2),
                context_window: Some(80),
                source: FactSource::ServerConfirmed,
            },
        };
        snapshot.agents.push(child.clone());
        snapshot.usage_facts = vec![crate::state::UsageFact {
            summary: child.usage,
            identity: crate::state::UsageIdentity {
                thread_id: Some("child-thread".into()),
                turn_id: Some("child-turn".into()),
                generation: Some(1),
            },
        }];

        let mut local = LocalState {
            agent_id: Some("child-thread".into()),
            ..LocalState::default()
        };
        let (tx, _rx) = tokio::sync::mpsc::channel(4);
        assert!(!handle_key(
            KeyEvent::new(KeyCode::F(11), KeyModifiers::NONE),
            &snapshot,
            &mut local,
            &tx,
        ));
        assert!(local.evidence);

        let mut terminal = Terminal::new(TestBackend::new(160, 45)).unwrap();
        terminal
            .draw(|frame| draw(frame, &snapshot, &local))
            .unwrap();
        let rendered: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(
            rendered.contains("Usage (child child-thread) source: server confirmed"),
            "{rendered}"
        );
        assert!(
            rendered.contains(
                "total 12 | input 7 | cached 3 | output 4 | reasoning 2 | context window 80"
            ),
            "{rendered}"
        );
        assert!(!rendered.contains("total 900 | input 901"), "{rendered}");
        assert!(
            rendered.contains("Session token budget: tokens:900/1000"),
            "{rendered}"
        );
    }

    #[test]
    fn usage_evidence_preserves_unknown_source_and_missing_fields() {
        use crate::agents::{AgentInfo, AgentSnapshot};

        let snapshot = observed_snapshot();
        let root = activity::format_usage_evidence(&snapshot, None);
        assert_eq!(
            root,
            "Usage (root) source: unknown | total unavailable | input unavailable | cached unavailable | output unavailable | reasoning unavailable | context window unavailable | identity thread unavailable turn unavailable generation unavailable"
        );
        let estimated_snapshot = CoreSnapshot {
            usage: crate::state::UsageSummary {
                source: FactSource::LocalEstimate,
                ..Default::default()
            },
            usage_facts: vec![crate::state::UsageFact {
                summary: crate::state::UsageSummary {
                    source: FactSource::LocalEstimate,
                    total_tokens: Some(1),
                    ..Default::default()
                },
                identity: crate::state::UsageIdentity {
                    thread_id: Some("root-thread".into()),
                    turn_id: Some("turn".into()),
                    generation: Some(1),
                },
            }],
            thread_id: Some("root-thread".into()),
            turn_id: Some("turn".into()),
            ..snapshot.clone()
        };
        assert!(activity::format_usage_evidence(&estimated_snapshot, None)
            .contains("Usage (root) source: local estimate"));

        let agent = AgentSnapshot {
            info: AgentInfo {
                id: "child-thread".into(),
                parent_id: "root-thread".into(),
                path: None,
                nickname: None,
                role: None,
                model: None,
                confirmed: true,
            },
            generation: 1,
            turn_id: None,
            outcome: None,
            awaiting_turn: false,
            usage: Default::default(),
        };
        let child = activity::format_usage_evidence(&snapshot, Some(&agent));
        assert_eq!(
            child,
            "Usage (child child-thread) source: unknown | total unavailable | input unavailable | cached unavailable | output unavailable | reasoning unavailable | context window unavailable | identity thread unavailable turn unavailable generation unavailable"
        );
    }

    #[test]
    fn evidence_panel_labels_retained_compaction_facts_and_schema_gaps() {
        use crate::observation::{
            ActivityScope, CompactionFact, CompactionFactStatus, EvidenceKind, EvidenceSource,
        };
        use crate::protocol::ToolCategory;

        let mut snapshot = observed_snapshot();
        let mut compaction = snapshot.observation.activities[0].clone();
        compaction.scope = ActivityScope::Tool;
        compaction.tool_category = Some(ToolCategory::Compaction);
        compaction.item_id = Some("compact-item".into());
        let mut evidence = compaction.last_evidence.clone().unwrap();
        evidence.kind = EvidenceKind::ToolCompleted;
        evidence.source = EvidenceSource::AppServer;
        evidence.item_id = compaction.item_id.clone();
        compaction.last_evidence = Some(evidence.clone());
        compaction.recent_evidence = vec![evidence];
        compaction.execution_state = ExecutionState::Completed;
        snapshot.observation.activities.push(compaction);
        snapshot.observation.compactions.push(CompactionFact {
            thread_id: "root-thread".into(),
            turn_id: "turn".into(),
            item_id: "compact-item".into(),
            status: CompactionFactStatus::Completed,
            started_at_ms: Some(10),
            completed_at_ms: Some(20),
            input_tokens: Some(120),
            cached_input_tokens: Some(30),
            output_tokens: Some(20),
            total_tokens: Some(140),
            context_window: Some(200),
        });

        let mut local = LocalState {
            evidence: true,
            ..Default::default()
        };
        let mut terminal = Terminal::new(TestBackend::new(160, 50)).unwrap();
        terminal
            .draw(|frame| draw(frame, &snapshot, &local))
            .unwrap();
        let screen = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(screen.contains("Compactions retained: 1"), "{screen}");
        assert!(screen.contains("Compaction status: Completed"), "{screen}");
        assert!(screen.contains("AppServer"), "{screen}");
        assert!(screen.contains("input 120"), "{screen}");
        assert!(screen.contains("cached input 30"), "{screen}");
        assert!(screen.contains("context window 200"), "{screen}");

        local.evidence = true;
        local.evidence_scroll = 6;
        let mut narrow = Terminal::new(TestBackend::new(80, 24)).unwrap();
        narrow.draw(|frame| draw(frame, &snapshot, &local)).unwrap();
        let narrow_screen = narrow
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(narrow_screen.contains("Compaction"), "{narrow_screen}");
        assert!(narrow_screen.contains("AppServer"), "{narrow_screen}");
    }

    #[test]
    fn compaction_fact_rows_show_unknown_and_missing_values_without_private_content() {
        use crate::observation::{CompactionFact, CompactionFactStatus};

        let fact = CompactionFact {
            thread_id: "thread".into(),
            turn_id: "turn".into(),
            item_id: "item".into(),
            status: CompactionFactStatus::Unknown,
            started_at_ms: None,
            completed_at_ms: None,
            input_tokens: Some(8),
            cached_input_tokens: None,
            output_tokens: None,
            total_tokens: Some(9),
            context_window: None,
        };
        let rows = compaction_fact_rows(Some(&fact)).join("\n");
        assert!(rows.contains("Compaction status: Unknown"));
        assert!(rows.contains("input 8"));
        assert!(rows.contains("cached input unavailable"));
        assert!(rows.contains("output unavailable"));
        assert!(rows.contains("total 9"));
        assert!(rows.contains("context window unavailable"));
        let missing = compaction_fact_rows(None).join("\n");
        assert!(missing.contains("Compaction status: unavailable"));
        assert!(missing.matches("unavailable").count() >= 6);
    }

    #[test]
    fn token_budget_view_keeps_missing_usage_and_limit_unavailable() {
        let mut snapshot = CoreSnapshot::default();
        assert_eq!(
            token_budget_brief(&snapshot),
            "tokens:unavailable/unavailable"
        );

        snapshot.token_budget.limit = Some(10);
        assert_eq!(token_budget_brief(&snapshot), "tokens:unavailable/10");

        snapshot.token_budget.confirmed_total_tokens = Some(4);
        assert_eq!(token_budget_brief(&snapshot), "tokens:partial 4/10");
        snapshot.token_budget.confirmed_complete = true;
        assert_eq!(token_budget_brief(&snapshot), "tokens:4/10");
        snapshot.token_budget.stop_triggered = true;
        assert_eq!(
            format_token_budget_evidence(&snapshot),
            "Session token budget: tokens:4/10 | stop triggered: yes | Per-agent token budget: not set"
        );

        snapshot.token_budget.per_agent_limit = Some(6);
        assert_eq!(
            format_token_budget_evidence(&snapshot),
            "Session token budget: tokens:4/10 | stop triggered: yes | Per-agent token budget: limit 6 | stop triggered: no"
        );
        snapshot.token_budget.per_agent_stop_triggered = true;
        assert_eq!(
            format_token_budget_evidence(&snapshot),
            "Session token budget: tokens:4/10 | stop triggered: yes | Per-agent token budget: limit 6 | stop triggered: yes"
        );

        snapshot.token_budget.limit = None;
        assert_eq!(token_budget_brief(&snapshot), "tokens:4/unavailable");
    }

    #[test]
    fn acknowledging_silence_is_local_preserves_requests_and_drafts_and_can_be_restored() {
        let mut snapshot = observed_snapshot();
        let mut request = snapshot.observation.activities[0].clone();
        request.activity_id = "request".into();
        request.scope = ActivityScope::Interaction;
        request.attention.level = AttentionLevel::RequiresAction;
        request.attention.requires_action = true;
        snapshot.observation.activities.push(request);
        let mut unknown = snapshot.observation.activities[0].clone();
        unknown.activity_id = "unknown".into();
        unknown.execution_state = ExecutionState::Unknown;
        snapshot.observation.activities.push(unknown);
        snapshot.requests.push(RequestView::decode(RpcId::Number(1), "item/tool/requestUserInput", &serde_json::json!({"threadId":"root-thread","turnId":"turn","questions":[{"id":"secret","header":"Secret","question":"Value?","isSecret":true}]})).unwrap());
        let before = snapshot.observation.clone();
        let mut local = LocalState::default();
        local.editor.insert("TASK_DRAFT");
        sync_questions(&mut local, snapshot.requests.first().cloned());
        local.editor.insert("SECRET_DRAFT");
        let (tx, mut rx) = tokio::sync::mpsc::channel(8);
        let wait = KeyEvent::new(KeyCode::Char('w'), KeyModifiers::CONTROL);
        handle_key(wait, &snapshot, &mut local, &tx);
        assert!(local
            .reminders
            .is_acknowledged(&snapshot.observation.activities[0]));
        assert!(!local
            .reminders
            .is_acknowledged(&snapshot.observation.activities[1]));
        assert!(!local
            .reminders
            .is_acknowledged(&snapshot.observation.activities[2]));
        assert_eq!(
            activity::next_action(&snapshot.observation.activities[1], &local.reminders),
            "F2 answer request"
        );
        assert_eq!(
            activity::next_action(&snapshot.observation.activities[2], &local.reminders),
            "inspect unknown outcome"
        );
        assert_eq!(snapshot.observation, before);
        assert_eq!(snapshot.requests.len(), 1);
        assert_eq!(local.editor.text, "SECRET_DRAFT");
        assert_eq!(local.task_draft.as_ref().unwrap().text, "TASK_DRAFT");
        assert!(rx.try_recv().is_err());
        handle_key(wait, &snapshot, &mut local, &tx);
        assert!(!local
            .reminders
            .is_acknowledged(&snapshot.observation.activities[0]));
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn acknowledgement_survives_redraw_but_new_evidence_only_rearms_its_own_activity() {
        let mut snapshot = observed_snapshot();
        let mut tool = snapshot.observation.activities[0].clone();
        tool.activity_id = "tool".into();
        tool.item_id = Some("tool".into());
        tool.scope = ActivityScope::Tool;
        let mut child = tool.clone();
        child.activity_id = "child-tool".into();
        child.identity.agent_id = "child".into();
        snapshot.observation.activities.extend([tool, child]);
        let mut local = LocalState::default();
        assert_eq!(
            local
                .reminders
                .toggle_for_agent(&snapshot.observation, "root"),
            Some(true)
        );
        assert!(!local
            .reminders
            .is_acknowledged(&snapshot.observation.activities[2]));
        snapshot.observation.snapshot_version += 1;
        snapshot.observation.activities[0].silence_ms = Some(40000);
        local.reminders.sync(&snapshot.observation);
        assert!(local
            .reminders
            .is_acknowledged(&snapshot.observation.activities[0]));
        assert!(local
            .reminders
            .is_acknowledged(&snapshot.observation.activities[1]));
        snapshot.observation.activities[0].progress_seq += 1;
        local.reminders.sync(&snapshot.observation);
        assert!(!local
            .reminders
            .is_acknowledged(&snapshot.observation.activities[0]));
        assert!(local
            .reminders
            .is_acknowledged(&snapshot.observation.activities[1]));
        assert_eq!(
            local
                .reminders
                .toggle_for_agent(&snapshot.observation, "root"),
            Some(true)
        );
        snapshot.observation.activities[1].identity.generation = Some(2);
        local.reminders.sync(&snapshot.observation);
        assert!(local
            .reminders
            .is_acknowledged(&snapshot.observation.activities[0]));
        assert!(!local
            .reminders
            .is_acknowledged(&snapshot.observation.activities[1]));
        snapshot.observation.activities.clear();
        local.reminders.sync(&snapshot.observation);
        assert_eq!(
            local
                .reminders
                .toggle_for_agent(&snapshot.observation, "root"),
            None
        );
    }

    #[test]
    fn silence_acknowledgement_rearms_for_severity_identity_or_clock_changes() {
        let original = observed_snapshot().observation;
        let mut changes = Vec::new();
        let mut changed = original.clone();
        changed.activities[0].attention.level = AttentionLevel::Quiet;
        changes.push(changed);
        let mut changed = original.clone();
        changed.activities[0].clock_epoch.push_str("new");
        changes.push(changed);
        let mut changed = original.clone();
        changed.activities[0].session_id.push_str("new");
        changes.push(changed);
        let mut changed = original.clone();
        changed.activities[0].identity.turn_id = Some("new-turn".into());
        changes.push(changed);
        let mut changed = original.clone();
        changed.activities[0].identity.attempt_id = Some(2);
        changes.push(changed);
        for changed in changes {
            let mut local = LocalState::default();
            local.reminders.toggle_for_agent(&original, "root");
            assert!(local.reminders.is_acknowledged(&original.activities[0]));
            assert!(!local.reminders.is_acknowledged(&changed.activities[0]));
            local.reminders.sync(&changed);
            assert!(!local.reminders.is_acknowledged(&original.activities[0]));
        }
        // A Quiet reminder cannot suppress a later AttentionNeeded escalation.
        let mut quiet = original.clone();
        quiet.activities[0].attention.level = AttentionLevel::Quiet;
        let mut local = LocalState::default();
        local.reminders.toggle_for_agent(&quiet, "root");
        assert!(!local.reminders.is_acknowledged(&original.activities[0]));
    }

    #[test]
    fn acknowledged_reminders_show_core_attention_in_compact_and_wide_views() {
        let snapshot = observed_snapshot();
        let mut local = LocalState::default();
        local
            .reminders
            .toggle_for_agent(&snapshot.observation, "root");
        for (width, height) in [(40, 12), (80, 24), (160, 45)] {
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            terminal
                .draw(|frame| draw(frame, &snapshot, &local))
                .unwrap();
            let rendered: String = terminal
                .backend()
                .buffer()
                .content()
                .iter()
                .map(|cell| cell.symbol())
                .collect();
            assert!(rendered.contains("attention:1"), "{rendered}");
            assert!(
                rendered.contains(if height <= 16 { "wait:1" } else { "waiting:1" }),
                "{rendered}"
            );
            local.evidence = true;
            terminal
                .draw(|frame| draw(frame, &snapshot, &local))
                .unwrap();
            let rendered: String = terminal
                .backend()
                .buffer()
                .content()
                .iter()
                .map(|cell| cell.symbol())
                .collect();
            assert!(rendered.contains("Ctrl+W wait/restore"), "{rendered}");
            local.evidence = false;
        }
    }
    use crate::state::{ConversationItem, SessionPhase};
    use ratatui::backend::TestBackend;

    #[test]
    fn task_controls_use_captured_attempts_and_render_without_mutating_core_facts() {
        use crate::scheduler::{Scheduler, TaskState};
        let mut scheduler = Scheduler::default();
        scheduler
            .enqueue(vec![
                RootTaskSpec::input("任务一 中文".into()),
                RootTaskSpec::input("queued task".into()),
            ])
            .unwrap();
        let first = scheduler.dispatch().unwrap().attempt;
        scheduler
            .started_root(
                first,
                crate::scheduler::ExternalTurn {
                    thread_id: "root".into(),
                    turn_id: "one".into(),
                    generation: 1,
                },
            )
            .unwrap();
        let snapshot = CoreSnapshot {
            phase: SessionPhase::Running,
            thread_id: Some("root".into()),
            scheduler: scheduler.snapshot(),
            ..Default::default()
        };
        let original = snapshot.clone();
        let (tx, mut rx) = tokio::sync::mpsc::channel(8);
        let mut local = LocalState::default();
        handle_key(
            KeyEvent::new(KeyCode::F(4), KeyModifiers::NONE),
            &snapshot,
            &mut local,
            &tx,
        );
        assert!(local.workflow.visible);
        assert!(rx.try_recv().is_err());
        for (width, height) in [(40, 12), (80, 24), (160, 45)] {
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            terminal
                .draw(|frame| draw(frame, &snapshot, &local))
                .unwrap();
            let rendered: String = terminal
                .backend()
                .buffer()
                .content()
                .iter()
                .map(|cell| cell.symbol())
                .collect();
            assert!(rendered.contains("Workflow"), "{rendered}");
            assert!(rendered.contains("#1 Running"), "{rendered}");
            if width >= 80 {
                assert!(rendered.replace(' ', "").contains("任务一中文"));
            }
        }
        handle_key(
            KeyEvent::new(KeyCode::Down, KeyModifiers::NONE),
            &snapshot,
            &mut local,
            &tx,
        );
        let selected = selected_task(&snapshot, local.workflow.selected_id).unwrap();
        assert_eq!(selected.state, TaskState::Ready);
        let attempt = TaskAttempt {
            task: selected.id,
            attempt: selected.attempt,
        };
        handle_key(
            KeyEvent::new(KeyCode::F(7), KeyModifiers::NONE),
            &snapshot,
            &mut local,
            &tx,
        );
        assert_eq!(
            rx.try_recv().unwrap(),
            Command::ScheduleTask {
                attempt,
                command: SchedulerCommand::Cancel(attempt.task)
            }
        );
        assert_eq!(snapshot, original);
        handle_key(
            KeyEvent::new(KeyCode::F(9), KeyModifiers::NONE),
            &snapshot,
            &mut local,
            &tx,
        );
        assert!(local.workflow.confirm_stop);
        handle_key(
            KeyEvent::new(KeyCode::Down, KeyModifiers::NONE),
            &snapshot,
            &mut local,
            &tx,
        );
        assert!(!local.workflow.confirm_stop);
        handle_key(
            KeyEvent::new(KeyCode::F(9), KeyModifiers::NONE),
            &snapshot,
            &mut local,
            &tx,
        );
        assert!(rx.try_recv().is_err());
        assert!(local.workflow.confirm_stop);
        handle_key(
            KeyEvent::new(KeyCode::F(9), KeyModifiers::NONE),
            &snapshot,
            &mut local,
            &tx,
        );
        assert_eq!(
            rx.try_recv().unwrap(),
            Command::Schedule(SchedulerCommand::StopWorkflow)
        );
        local.editor.insert("explicit followup");
        handle_key(
            KeyEvent::new(KeyCode::F(4), KeyModifiers::NONE),
            &snapshot,
            &mut local,
            &tx,
        );
        handle_key(
            KeyEvent::new(KeyCode::Enter, KeyModifiers::CONTROL),
            &snapshot,
            &mut local,
            &tx,
        );
        let Command::QueueRootTasks { tasks } = rx.try_recv().unwrap() else {
            panic!()
        };
        assert_eq!(tasks[0].dependencies, vec![first.task]);
        assert!(local.editor.text.is_empty());
        let failed = CoreSnapshot {
            last_error: Some("old turn failed".into()),
            notice: Some("Scheduler command rejected: attempt changed".into()),
            ..snapshot
        };
        local.notice = None;
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal.draw(|frame| draw(frame, &failed, &local)).unwrap();
        let rendered: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(rendered.contains("attempt changed"));
    }

    #[test]
    fn tasks_panel_shows_native_reservations_separately_from_observed_turns() {
        use crate::scheduler::{SchedulerSnapshot, TaskKind, TaskState};

        let root = TaskSnapshot {
            id: TaskId(1),
            kind: TaskKind::RootTurn,
            state: TaskState::Running,
            title: "root task".into(),
            parent: None,
            attempt: 1,
            dependencies: Vec::new(),
            policy: Default::default(),
            failure: Default::default(),
            priority: 0,
            blocked_reason: None,
            external: None,
            child_thread_id: None,
            pause_requested: false,
            cancel_requested: false,
            pending_requests: 0,
            wait_targets: Vec::new(),
            root_slot_reserved: true,
            native_slot_reserved: false,
            cancellation_epoch: 0,
        };
        let native_reserved = TaskSnapshot {
            id: TaskId(2),
            kind: TaskKind::NativeChild,
            state: TaskState::Running,
            title: "reserved child".into(),
            parent: Some(root.id),
            attempt: 1,
            dependencies: Vec::new(),
            policy: Default::default(),
            failure: Default::default(),
            priority: 0,
            blocked_reason: None,
            external: Some(crate::scheduler::ExternalTurn {
                thread_id: "child-reserved".into(),
                turn_id: "turn-1".into(),
                generation: 1,
            }),
            child_thread_id: Some("child-reserved".into()),
            pause_requested: false,
            cancel_requested: false,
            pending_requests: 0,
            wait_targets: Vec::new(),
            root_slot_reserved: false,
            native_slot_reserved: true,
            cancellation_epoch: 0,
        };
        let native_free = TaskSnapshot {
            id: TaskId(3),
            kind: TaskKind::NativeChild,
            state: TaskState::Starting,
            title: "free child".into(),
            parent: Some(root.id),
            attempt: 0,
            dependencies: Vec::new(),
            policy: Default::default(),
            failure: Default::default(),
            priority: 0,
            blocked_reason: None,
            external: None,
            child_thread_id: Some("child-free".into()),
            pause_requested: false,
            cancel_requested: false,
            pending_requests: 0,
            wait_targets: Vec::new(),
            root_slot_reserved: false,
            native_slot_reserved: false,
            cancellation_epoch: 0,
        };
        let snapshot = CoreSnapshot {
            phase: SessionPhase::Running,
            thread_id: Some("root".into()),
            scheduler: SchedulerSnapshot {
                tasks: vec![root, native_reserved, native_free],
                root_slots_reserved: 12,
                native_slots_reserved: 12,
                native_slot_capacity: Some(12),
                native_turns_observed: 12,
                ..Default::default()
            },
            ..Default::default()
        };
        let local = LocalState {
            workflow: workflow::WorkflowPanel {
                visible: true,
                ..Default::default()
            },
            ..Default::default()
        };
        let render = |width, height| {
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            terminal
                .draw(|frame| {
                    workflow_view::draw_workflow(
                        frame,
                        frame.area(),
                        &snapshot,
                        local.workflow.selected_id,
                        local.workflow.link_cursor,
                        local.workflow.scroll,
                        local.workflow.manual_scroll,
                    )
                })
                .unwrap();
            let width = terminal.backend().buffer().area().width as usize;
            terminal
                .backend()
                .buffer()
                .content()
                .chunks(width)
                .map(|cells| cells.iter().map(|cell| cell.symbol()).collect::<String>())
                .collect::<Vec<_>>()
        };
        let rows = render(120, 20);
        let rendered = rows.join("\n");
        assert!(rendered.contains("native reserved: 12/12"), "{rendered}");
        assert!(rendered.contains("native turns observed: 12"), "{rendered}");
        assert!(rendered.contains("slot reserved"), "{rendered}");
        assert!(rendered.contains("slot free"), "{rendered}");
        let root_row = rows
            .iter()
            .find(|row| row.contains("#1") && row.contains("root task"))
            .expect("root task row");
        assert!(!root_row.contains("slot"), "{root_row}");

        let medium = render(66, 12).join("\n");
        assert!(medium.contains("native reserved: 12/12"), "{medium}");
        assert!(medium.contains("Native turns observed: 12"), "{medium}");

        let narrow = render(34, 12).join("\n");
        assert!(narrow.contains("native reserved: 12/12"), "{narrow}");
        assert!(narrow.contains("native observed: 12"), "{narrow}");

        let compact = render(24, 8).join("\n");
        assert!(compact.contains("native reserved: 12/12"), "{compact}");
        assert!(compact.contains("native observed: 12"), "{compact}");

        let unavailable = CoreSnapshot {
            scheduler: SchedulerSnapshot {
                native_slot_capacity: None,
                ..snapshot.scheduler.clone()
            },
            ..snapshot.clone()
        };
        let mut terminal = Terminal::new(TestBackend::new(80, 12)).unwrap();
        terminal
            .draw(|frame| {
                workflow_view::draw_workflow(
                    frame,
                    frame.area(),
                    &unavailable,
                    local.workflow.selected_id,
                    local.workflow.link_cursor,
                    local.workflow.scroll,
                    local.workflow.manual_scroll,
                )
            })
            .unwrap();
        let unavailable_rendered: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(unavailable_rendered.contains("native reserved: 12/unavailable"));
    }

    #[test]
    fn workflow_enter_opens_only_the_exact_child_turn_and_keeps_the_draft() {
        use crate::agents::{AgentInfo, AgentSnapshot};
        use crate::scheduler::{ExternalTurn, SchedulerSnapshot, TaskKind, TaskState};

        let task = TaskSnapshot {
            id: TaskId(2),
            kind: TaskKind::NativeChild,
            state: TaskState::Running,
            title: "child".into(),
            parent: None,
            attempt: 3,
            dependencies: Vec::new(),
            policy: Default::default(),
            failure: Default::default(),
            priority: 0,
            blocked_reason: None,
            external: Some(ExternalTurn {
                thread_id: "child-thread".into(),
                turn_id: "turn-3".into(),
                generation: 3,
            }),
            child_thread_id: Some("child-thread".into()),
            pause_requested: false,
            cancel_requested: false,
            pending_requests: 0,
            wait_targets: Vec::new(),
            root_slot_reserved: false,
            native_slot_reserved: true,
            cancellation_epoch: 0,
        };
        let agent = AgentSnapshot {
            info: AgentInfo {
                id: "child-thread".into(),
                parent_id: "root-thread".into(),
                path: None,
                nickname: None,
                role: None,
                model: None,
                confirmed: true,
            },
            generation: 3,
            turn_id: Some("turn-3".into()),
            outcome: None,
            awaiting_turn: false,
            usage: Default::default(),
        };
        let snapshot = CoreSnapshot {
            agents: vec![agent.clone()],
            scheduler: SchedulerSnapshot {
                tasks: vec![task.clone()],
                native_slots_reserved: 1,
                ..Default::default()
            },
            ..Default::default()
        };
        let (tx, mut rx) = tokio::sync::mpsc::channel(8);
        let mut local = LocalState::default();
        local.editor.insert("unsent draft");
        local.workflow.visible = true;
        handle_key(
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            &snapshot,
            &mut local,
            &tx,
        );
        assert!(!local.workflow.visible);
        assert_eq!(local.agent_id.as_deref(), Some("child-thread"));
        assert_eq!(local.editor.text, "unsent draft");
        assert!(rx.try_recv().is_err());

        let mut stale = snapshot.clone();
        stale.agents[0].turn_id = Some("newer-turn".into());
        local.workflow.visible = true;
        local.agent_id = None;
        local.notice = None;
        handle_key(
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            &stale,
            &mut local,
            &tx,
        );
        assert!(local.workflow.visible);
        assert!(local.agent_id.is_none());
        assert!(local.notice.as_deref().unwrap().contains("stale"));

        let mut mismatch = snapshot.clone();
        mismatch.scheduler.tasks[0].child_thread_id = Some("other-thread".into());
        local.notice = None;
        handle_key(
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            &mismatch,
            &mut local,
            &tx,
        );
        assert!(local.workflow.visible);
        assert!(local.agent_id.is_none());
        assert!(local.notice.as_deref().unwrap().contains("inconsistent"));

        let mut unbound_started = snapshot;
        unbound_started.scheduler.tasks[0].external = None;
        unbound_started.scheduler.tasks[0].attempt = 1;
        local.notice = None;
        handle_key(
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            &unbound_started,
            &mut local,
            &tx,
        );
        assert!(local.workflow.visible);
        assert!(local.agent_id.is_none());
        assert!(local
            .notice
            .as_deref()
            .unwrap()
            .contains("without a matching"));
    }

    #[test]
    fn workflow_panel_is_modal_and_preserves_task_and_secret_drafts() {
        let approval = RequestView::decode(
            RpcId::Number(911),
            "item/commandExecution/requestApproval",
            &serde_json::json!({
                "threadId":"root-thread",
                "turnId":"turn",
                "command":"review",
                "availableDecisions":["accept", "decline", "cancel"]
            }),
        )
        .unwrap();
        let approval_reference = approval.reference();
        let snapshot = observed_snapshot_with_requests(&[approval]);
        let (tx, mut rx) = tokio::sync::mpsc::channel(8);
        let mut local = LocalState {
            workflow: workflow::WorkflowPanel {
                visible: true,
                ..Default::default()
            },
            answers: BTreeMap::from([("secret-question".into(), vec!["SECRET_DRAFT".into()])]),
            ..Default::default()
        };
        local.editor.insert("TASK_DRAFT");
        for key in [
            KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE),
            KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE),
            KeyEvent::new(KeyCode::Delete, KeyModifiers::NONE),
            KeyEvent::new(KeyCode::Enter, KeyModifiers::CONTROL),
            KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL),
            KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT),
            KeyEvent::new(KeyCode::Char('y'), KeyModifiers::CONTROL),
            KeyEvent::new(KeyCode::Char('n'), KeyModifiers::CONTROL),
            KeyEvent::new(KeyCode::Char('b'), KeyModifiers::CONTROL),
            KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL),
            KeyEvent::new(KeyCode::Char('d'), KeyModifiers::ALT),
            KeyEvent::new(KeyCode::F(3), KeyModifiers::NONE),
        ] {
            handle_key(key, &snapshot, &mut local, &tx);
        }
        handle_paste("SECRET_PASTE", &snapshot, &mut local);
        assert_eq!(local.editor.text, "TASK_DRAFT");
        assert_eq!(
            local.answers.get("secret-question").unwrap(),
            &["SECRET_DRAFT".to_string()]
        );
        assert!(rx.try_recv().is_err());

        handle_key(
            KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
            &snapshot,
            &mut local,
            &tx,
        );
        assert!(!local.workflow.visible);
        assert_eq!(local.editor.text, "TASK_DRAFT");
        assert_eq!(
            local.answers.get("secret-question").unwrap(),
            &["SECRET_DRAFT".to_string()]
        );
        assert!(rx.try_recv().is_err());

        // F2 explicitly opens the request context; approval shortcuts work there.
        handle_key(
            KeyEvent::new(KeyCode::F(2), KeyModifiers::NONE),
            &snapshot,
            &mut local,
            &tx,
        );
        assert!(!local.workflow.visible);
        assert!(local.request_panel);
        handle_key(
            KeyEvent::new(KeyCode::Char('y'), KeyModifiers::CONTROL),
            &snapshot,
            &mut local,
            &tx,
        );
        assert_eq!(
            rx.try_recv().unwrap(),
            Command::AnswerApproval {
                request: approval_reference,
                decision: ApprovalDecision::Accept,
            }
        );

        local.workflow.visible = true;
        local.request_panel = false;
        handle_key(
            KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
            &snapshot,
            &mut local,
            &tx,
        );
        assert_eq!(rx.try_recv().unwrap(), Command::Interrupt);
        assert!(handle_key(
            KeyEvent::new(KeyCode::Char('q'), KeyModifiers::CONTROL),
            &snapshot,
            &mut local,
            &tx,
        ));
    }

    #[test]
    fn stale_gate_link_blocks_task_actions_until_explicit_reselection() {
        use crate::agents::{AgentInfo, AgentSnapshot};
        use crate::scheduler::{ExternalTurn, SchedulerSnapshot, TaskKind, TaskState};

        let origin = TaskSnapshot {
            id: TaskId(1),
            kind: TaskKind::RootTurn,
            state: TaskState::Succeeded,
            title: "gate owner".into(),
            parent: None,
            attempt: 2,
            dependencies: Vec::new(),
            policy: Default::default(),
            failure: Default::default(),
            priority: 0,
            blocked_reason: None,
            external: None,
            child_thread_id: None,
            pause_requested: false,
            cancel_requested: false,
            pending_requests: 0,
            wait_targets: vec![TaskAttempt {
                task: TaskId(2),
                attempt: 1,
            }],
            root_slot_reserved: false,
            native_slot_reserved: false,
            cancellation_epoch: 0,
        };
        let target = TaskSnapshot {
            id: TaskId(2),
            kind: TaskKind::NativeChild,
            state: TaskState::Running,
            title: "current child attempt".into(),
            parent: Some(TaskId(1)),
            attempt: 2,
            dependencies: Vec::new(),
            policy: Default::default(),
            failure: Default::default(),
            priority: 0,
            blocked_reason: None,
            external: Some(ExternalTurn {
                thread_id: "child-thread".into(),
                turn_id: "turn-2".into(),
                generation: 2,
            }),
            child_thread_id: Some("child-thread".into()),
            pause_requested: false,
            cancel_requested: false,
            pending_requests: 0,
            wait_targets: Vec::new(),
            root_slot_reserved: false,
            native_slot_reserved: true,
            cancellation_epoch: 0,
        };
        let snapshot = CoreSnapshot {
            agents: vec![AgentSnapshot {
                info: AgentInfo {
                    id: "child-thread".into(),
                    parent_id: "root-thread".into(),
                    path: None,
                    nickname: None,
                    role: None,
                    model: None,
                    confirmed: true,
                },
                generation: 2,
                turn_id: Some("turn-2".into()),
                outcome: None,
                awaiting_turn: false,
                usage: Default::default(),
            }],
            scheduler: SchedulerSnapshot {
                tasks: vec![origin, target],
                ..Default::default()
            },
            ..Default::default()
        };
        let (tx, mut rx) = tokio::sync::mpsc::channel(16);
        let stale_cursor = WorkflowLinkCursor {
            origin: TaskId(1),
            origin_attempt: 2,
            target: TaskId(2),
            kind: WorkflowLinkKind::Gate,
            index: 0,
            captured_attempt: Some(1),
        };

        for key in [
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            KeyEvent::new(KeyCode::F(6), KeyModifiers::NONE),
            KeyEvent::new(KeyCode::F(7), KeyModifiers::NONE),
            KeyEvent::new(KeyCode::F(8), KeyModifiers::NONE),
            KeyEvent::new(KeyCode::Char('+'), KeyModifiers::NONE),
            KeyEvent::new(KeyCode::Char('-'), KeyModifiers::NONE),
        ] {
            let mut local = LocalState {
                workflow: workflow::WorkflowPanel {
                    visible: true,
                    selected_id: Some(TaskId(2)),
                    link_cursor: Some(stale_cursor),
                    ..Default::default()
                },
                ..Default::default()
            };
            handle_key(key, &snapshot, &mut local, &tx);
            assert!(
                local.workflow.visible,
                "stale Gate action {:?} left F4",
                key.code
            );
            assert_eq!(local.workflow.selected_id, Some(TaskId(2)));
            assert!(local
                .notice
                .as_deref()
                .unwrap()
                .contains("captured Gate attempt"));
            assert!(
                rx.try_recv().is_err(),
                "stale Gate action {:?} sent a command",
                key.code
            );
        }

        let mut local = LocalState {
            workflow: workflow::WorkflowPanel {
                visible: true,
                selected_id: Some(TaskId(2)),
                link_cursor: Some(stale_cursor),
                ..Default::default()
            },
            ..Default::default()
        };
        handle_key(
            KeyEvent::new(KeyCode::Up, KeyModifiers::NONE),
            &snapshot,
            &mut local,
            &tx,
        );
        assert_eq!(local.workflow.selected_id, Some(TaskId(1)));
        assert!(local.workflow.link_cursor.is_none());
        handle_key(
            KeyEvent::new(KeyCode::Down, KeyModifiers::NONE),
            &snapshot,
            &mut local,
            &tx,
        );
        assert_eq!(local.workflow.selected_id, Some(TaskId(2)));
        assert!(local.workflow.link_cursor.is_none());
        handle_key(
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            &snapshot,
            &mut local,
            &tx,
        );
        assert!(!local.workflow.visible);
        assert_eq!(local.agent_id.as_deref(), Some("child-thread"));
        assert!(rx.try_recv().is_err());

        // A cursor from an earlier source attempt is also invalid even if its
        // captured target attempt happens to match the current task attempt.
        let mut source_stale = LocalState {
            workflow: workflow::WorkflowPanel {
                visible: true,
                selected_id: Some(TaskId(2)),
                link_cursor: Some(WorkflowLinkCursor {
                    origin_attempt: 1,
                    captured_attempt: Some(2),
                    ..stale_cursor
                }),
                ..Default::default()
            },
            ..Default::default()
        };
        handle_key(
            KeyEvent::new(KeyCode::F(6), KeyModifiers::NONE),
            &snapshot,
            &mut source_stale,
            &tx,
        );
        assert!(source_stale
            .notice
            .as_deref()
            .unwrap()
            .contains("captured Gate attempt"));
        assert!(rx.try_recv().is_err());
        handle_key(
            KeyEvent::new(KeyCode::Up, KeyModifiers::NONE),
            &snapshot,
            &mut source_stale,
            &tx,
        );
        handle_key(
            KeyEvent::new(KeyCode::Down, KeyModifiers::NONE),
            &snapshot,
            &mut source_stale,
            &tx,
        );
        assert_eq!(source_stale.workflow.selected_id, Some(TaskId(2)));
        assert!(source_stale.workflow.link_cursor.is_none());
        handle_key(
            KeyEvent::new(KeyCode::F(6), KeyModifiers::NONE),
            &snapshot,
            &mut source_stale,
            &tx,
        );
        assert_eq!(
            rx.try_recv().unwrap(),
            Command::ScheduleTask {
                attempt: TaskAttempt {
                    task: TaskId(2),
                    attempt: 2,
                },
                command: SchedulerCommand::Pause(TaskId(2)),
            }
        );
    }

    #[test]
    fn root_conversation_link_uses_current_activity_generation_not_task_attempt() {
        use crate::scheduler::{ExternalTurn, SchedulerSnapshot, TaskKind, TaskState};
        let root = |id: u64, state: TaskState, generation: u64, turn: &str| TaskSnapshot {
            id: TaskId(id),
            kind: TaskKind::RootTurn,
            state,
            title: format!("root-{id}"),
            parent: None,
            attempt: 1,
            dependencies: Vec::new(),
            policy: Default::default(),
            failure: Default::default(),
            priority: 0,
            blocked_reason: None,
            external: Some(ExternalTurn {
                thread_id: "root-thread".into(),
                turn_id: turn.into(),
                generation,
            }),
            child_thread_id: None,
            pause_requested: false,
            cancel_requested: false,
            pending_requests: 0,
            wait_targets: Vec::new(),
            root_slot_reserved: state.active(),
            native_slot_reserved: false,
            cancellation_epoch: 0,
        };
        let mut snapshot = observed_snapshot();
        snapshot.scheduler = SchedulerSnapshot {
            tasks: vec![
                root(1, TaskState::Succeeded, 2, "old-turn"),
                root(2, TaskState::Running, 3, "current-turn"),
            ],
            active_root: Some(TaskAttempt {
                task: TaskId(2),
                attempt: 1,
            }),
            root_slots_reserved: 1,
            ..Default::default()
        };
        snapshot.turn_id = Some("current-turn".into());
        snapshot.observation.activities[0].identity = crate::observation::ActivityIdentity {
            agent_id: "root".into(),
            task_id: Some(TaskId(2)),
            attempt_id: Some(1),
            thread_id: Some("root-thread".into()),
            turn_id: Some("current-turn".into()),
            generation: Some(3),
        };
        assert!(matches!(
            workflow_conversation(&snapshot, &snapshot.scheduler.tasks[1]),
            ConversationTarget::Root
        ));
        assert!(matches!(
            workflow_conversation(&snapshot, &snapshot.scheduler.tasks[0]),
            ConversationTarget::Unavailable(_)
        ));

        snapshot.phase = SessionPhase::StartingTurn;
        snapshot.turn_id = None;
        snapshot.scheduler.active_root = Some(TaskAttempt {
            task: TaskId(1),
            attempt: 1,
        });
        assert!(matches!(
            workflow_conversation(&snapshot, &snapshot.scheduler.tasks[0]),
            ConversationTarget::Unavailable(_)
        ));
    }

    #[test]
    fn workflow_links_cycle_and_keep_gate_capture_separate_from_current_attempt() {
        use crate::scheduler::{SchedulerSnapshot, TaskAttempt, TaskKind, TaskState};
        let task = |id, attempt, dependencies, wait_targets| TaskSnapshot {
            id: TaskId(id),
            kind: TaskKind::NativeChild,
            state: TaskState::Running,
            title: format!("child-{id}"),
            parent: None,
            attempt,
            dependencies,
            policy: Default::default(),
            failure: Default::default(),
            priority: 0,
            blocked_reason: None,
            external: None,
            child_thread_id: Some(format!("thread-{id}")),
            pause_requested: false,
            cancel_requested: false,
            pending_requests: 0,
            wait_targets,
            root_slot_reserved: false,
            native_slot_reserved: false,
            cancellation_epoch: 0,
        };
        let snapshot = CoreSnapshot {
            scheduler: SchedulerSnapshot {
                tasks: vec![
                    task(1, 2, Vec::new(), Vec::new()),
                    task(2, 2, Vec::new(), Vec::new()),
                    task(
                        3,
                        1,
                        vec![TaskId(1), TaskId(2)],
                        vec![TaskAttempt {
                            task: TaskId(2),
                            attempt: 1,
                        }],
                    ),
                ],
                ..Default::default()
            },
            ..Default::default()
        };
        let (tx, mut rx) = tokio::sync::mpsc::channel(8);
        let mut local = LocalState {
            workflow: workflow::WorkflowPanel {
                visible: true,
                selected_id: Some(TaskId(3)),
                ..Default::default()
            },
            ..Default::default()
        };
        handle_key(
            KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE),
            &snapshot,
            &mut local,
            &tx,
        );
        assert_eq!(local.workflow.selected_id, Some(TaskId(1)));
        handle_key(
            KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE),
            &snapshot,
            &mut local,
            &tx,
        );
        assert_eq!(local.workflow.selected_id, Some(TaskId(2)));

        local.workflow.selected_id = Some(TaskId(3));
        local.workflow.link_cursor = None;
        handle_key(
            KeyEvent::new(KeyCode::Char('g'), KeyModifiers::NONE),
            &snapshot,
            &mut local,
            &tx,
        );
        assert_eq!(local.workflow.selected_id, Some(TaskId(2)));
        assert_eq!(
            local.workflow.link_cursor.unwrap().captured_attempt,
            Some(1)
        );
        let mut terminal = Terminal::new(TestBackend::new(100, 24)).unwrap();
        terminal
            .draw(|frame| {
                workflow_view::draw_workflow(
                    frame,
                    frame.area(),
                    &snapshot,
                    local.workflow.selected_id,
                    local.workflow.link_cursor,
                    local.workflow.scroll,
                    local.workflow.manual_scroll,
                )
            })
            .unwrap();
        let rendered: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(rendered.contains("captured attempt 1"), "{rendered}");
        assert!(rendered.contains("current task attempt is 2"), "{rendered}");
        assert!(rx.try_recv().is_err());

        local.workflow.selected_id = Some(TaskId(1));
        local.workflow.link_cursor = None;
        handle_key(
            KeyEvent::new(KeyCode::Char('g'), KeyModifiers::NONE),
            &snapshot,
            &mut local,
            &tx,
        );
        assert!(local
            .notice
            .as_deref()
            .unwrap()
            .contains("no captured Gate wait targets"));
    }

    #[test]
    fn workflow_end_scroll_reaches_the_selected_task_relationship_details() {
        use crate::scheduler::{SchedulerSnapshot, TaskKind, TaskState};
        let task = TaskSnapshot {
            id: TaskId(1),
            kind: TaskKind::RootTurn,
            state: TaskState::Queued,
            title: "a root task with relationship details".into(),
            parent: None,
            attempt: 1,
            dependencies: Vec::new(),
            policy: Default::default(),
            failure: Default::default(),
            priority: 0,
            blocked_reason: None,
            external: None,
            child_thread_id: None,
            pause_requested: false,
            cancel_requested: false,
            pending_requests: 0,
            wait_targets: Vec::new(),
            root_slot_reserved: false,
            native_slot_reserved: false,
            cancellation_epoch: 0,
        };
        let snapshot = CoreSnapshot {
            scheduler: SchedulerSnapshot {
                tasks: vec![task],
                ..Default::default()
            },
            ..Default::default()
        };
        let mut local = LocalState {
            workflow: workflow::WorkflowPanel {
                visible: true,
                manual_scroll: true,
                scroll: usize::MAX,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut terminal = Terminal::new(TestBackend::new(48, 8)).unwrap();
        terminal
            .draw(|frame| {
                workflow_view::draw_workflow(
                    frame,
                    frame.area(),
                    &snapshot,
                    local.workflow.selected_id,
                    local.workflow.link_cursor,
                    local.workflow.scroll,
                    local.workflow.manual_scroll,
                )
            })
            .unwrap();
        let rendered: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(rendered.contains("Conversation:"), "{rendered}");

        let parent_local = LocalState {
            workflow: workflow::WorkflowPanel {
                visible: true,
                selected_id: Some(TaskId(1)),
                manual_scroll: true,
                scroll: 0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut parent_terminal = Terminal::new(TestBackend::new(48, 24)).unwrap();
        parent_terminal
            .draw(|frame| {
                workflow_view::draw_workflow(
                    frame,
                    frame.area(),
                    &snapshot,
                    parent_local.workflow.selected_id,
                    parent_local.workflow.link_cursor,
                    parent_local.workflow.scroll,
                    parent_local.workflow.manual_scroll,
                )
            })
            .unwrap();
        let parent_rendered: String = parent_terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(
            parent_rendered.contains("Parent: none"),
            "{parent_rendered}"
        );

        let mut child_snapshot = snapshot.clone();
        child_snapshot.scheduler.tasks[0].kind = TaskKind::NativeChild;
        let mut child_terminal = Terminal::new(TestBackend::new(48, 24)).unwrap();
        child_terminal
            .draw(|frame| {
                workflow_view::draw_workflow(
                    frame,
                    frame.area(),
                    &child_snapshot,
                    parent_local.workflow.selected_id,
                    parent_local.workflow.link_cursor,
                    parent_local.workflow.scroll,
                    parent_local.workflow.manual_scroll,
                )
            })
            .unwrap();
        let child_rendered: String = child_terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(
            child_rendered.contains("Parent: unconfirmed / unavailable"),
            "{child_rendered}"
        );
        assert!(!child_rendered.contains("Parent: none"), "{child_rendered}");

        local.workflow.scroll = 0;
        local.workflow.manual_scroll = true;
        handle_key(
            KeyEvent::new(KeyCode::End, KeyModifiers::NONE),
            &snapshot,
            &mut local,
            &tokio::sync::mpsc::channel(1).0,
        );
        assert_eq!(local.workflow.scroll, usize::MAX);
    }

    #[test]
    fn gate_view_shows_waiting_progress_and_agent_switching_only_changes_the_local_view() {
        use crate::agents::{AgentInfo, AgentSnapshot};
        use crate::gate::WaitTarget;
        use crate::state::GateSnapshot;
        let mut snapshot = CoreSnapshot {
            phase: SessionPhase::GatePending,
            thread_id: Some("root".into()),
            root_start_requests: 1,
            queued_inputs: 2,
            agents: vec![
                AgentSnapshot {
                    info: AgentInfo {
                        id: "a".into(),
                        parent_id: "root".into(),
                        path: Some("/root/a".into()),
                        nickname: None,
                        role: None,
                        model: None,
                        confirmed: true,
                    },
                    generation: 1,
                    turn_id: Some("a-1".into()),
                    outcome: None,
                    awaiting_turn: false,
                    usage: crate::state::UsageSummary {
                        total_tokens: Some(123),
                        source: FactSource::ServerConfirmed,
                        ..Default::default()
                    },
                },
                AgentSnapshot {
                    info: AgentInfo {
                        id: "b".into(),
                        parent_id: "a".into(),
                        path: Some("/root/a/b".into()),
                        nickname: None,
                        role: None,
                        model: None,
                        confirmed: true,
                    },
                    generation: 2,
                    turn_id: Some("b-1".into()),
                    outcome: None,
                    awaiting_turn: false,
                    usage: Default::default(),
                },
            ],
            gate: Some(GateSnapshot {
                targets: vec![WaitTarget {
                    id: "a".into(),
                    generation: 1,
                    turn_id: Some("a-1".into()),
                    outcome: None,
                }],
                pending: true,
                root_starts_at_enter: 1,
                root_starts_at_release: None,
            }),
            ..Default::default()
        };
        snapshot.usage_facts.push(crate::state::UsageFact {
            summary: snapshot.agents[0].usage,
            identity: crate::state::UsageIdentity {
                thread_id: Some("a".into()),
                turn_id: Some("a-1".into()),
                generation: Some(1),
            },
        });
        for (thread, text) in [("root", "ROOT_OUTPUT"), ("a", "CHILD_OUTPUT 中文")] {
            snapshot.messages.push(ConversationItem {
                id: "shared".into(),
                thread_id: thread.into(),
                turn_id: "one".into(),
                role: "Agent".into(),
                text: text.into(),
                complete: true,
                truncated: false,
            });
        }
        let (tx, mut rx) = tokio::sync::mpsc::channel(8);
        let mut local = LocalState::default();
        handle_key(
            KeyEvent::new(KeyCode::F(3), KeyModifiers::NONE),
            &snapshot,
            &mut local,
            &tx,
        );
        assert_eq!(local.agent_id.as_deref(), Some("a"));
        assert!(rx.try_recv().is_err());
        for (width, height) in [(80, 24), (160, 45), (40, 12)] {
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            terminal
                .draw(|frame| draw(frame, &snapshot, &local))
                .unwrap();
            let rendered: String = terminal
                .backend()
                .buffer()
                .content()
                .iter()
                .map(|cell| cell.symbol())
                .collect();
            assert!(rendered.contains("CHILD_OUTPUT"), "{rendered}");
            assert!(!rendered.contains("ROOT_OUTPUT"));
            assert!(rendered.contains("Waiting children: 0/1"));
            if width >= 80 {
                assert!(rendered.contains("queued: 2"));
            }
            if width >= 100 {
                assert!(rendered.contains("Agents · F3"));
                assert!(rendered.contains("+- /root/a/b"), "{rendered}");
                assert!(rendered.contains("tokens:123"), "{rendered}");
                assert!(rendered.contains("tokens: unavailable"), "{rendered}");
            }
        }
        local.editor.insert("root task");
        handle_key(
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            &snapshot,
            &mut local,
            &tx,
        );
        assert_eq!(
            rx.try_recv().unwrap(),
            Command::SubmitRootInput {
                text: "root task".into()
            }
        );
        snapshot.queued_inputs = 8;
        local.editor.insert("retained draft");
        handle_key(
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            &snapshot,
            &mut local,
            &tx,
        );
        assert_eq!(local.editor.text, "retained draft");
        assert!(rx.try_recv().is_err());
        handle_key(
            KeyEvent::new(KeyCode::F(3), KeyModifiers::NONE),
            &snapshot,
            &mut local,
            &tx,
        );
        handle_key(
            KeyEvent::new(KeyCode::F(3), KeyModifiers::NONE),
            &snapshot,
            &mut local,
            &tx,
        );
        assert!(local.agent_id.is_none());
    }

    #[test]
    fn agent_panel_uses_stable_dfs_and_marks_missing_parent_without_draft_leakage() {
        use crate::agents::{AgentInfo, AgentSnapshot};

        fn agent(id: &str, parent_id: &str, path: &str) -> AgentSnapshot {
            AgentSnapshot {
                info: AgentInfo {
                    id: id.into(),
                    parent_id: parent_id.into(),
                    path: Some(path.into()),
                    nickname: None,
                    role: None,
                    model: None,
                    confirmed: true,
                },
                generation: 1,
                turn_id: Some(format!("{id}-turn")),
                outcome: None,
                awaiting_turn: false,
                usage: Default::default(),
            }
        }

        let first = vec![
            agent("c", "b", "/root/b/c"),
            agent("orphan", "missing-parent", "orphan"),
            agent("b", "root", "/root/b"),
            agent("a", "root", "/root/a"),
        ];
        let ids = |agents: &[AgentSnapshot]| {
            workflow_view::project_agent_tree(agents, Some("root"))
                .into_iter()
                .map(|row| agents[row.index].info.id.clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(ids(&first), ["orphan", "b", "c", "a"]);
        let projected = workflow_view::project_agent_tree(&first, Some("root"));
        assert_eq!(
            projected.iter().map(|row| row.depth).collect::<Vec<_>>(),
            [1, 1, 2, 1]
        );
        assert!(projected[0].parent_missing);

        let root_uuid = "thread-root-uuid";
        let mut uuid_agents = first.clone();
        for agent in &mut uuid_agents {
            if agent.info.parent_id == "root" {
                agent.info.parent_id = root_uuid.into();
            }
        }
        let uuid_projection = workflow_view::project_agent_tree(&uuid_agents, Some(root_uuid));
        assert_eq!(
            uuid_projection
                .iter()
                .map(|row| uuid_agents[row.index].info.id.as_str())
                .collect::<Vec<_>>(),
            ["orphan", "b", "c", "a"]
        );
        assert!(uuid_projection
            .iter()
            .filter(|row| uuid_agents[row.index].info.id != "orphan")
            .all(|row| !row.parent_missing));
        assert!(uuid_projection[0].parent_missing);

        let mut tentative = first.clone();
        tentative[2].info.confirmed = false;
        let tentative_projection = workflow_view::project_agent_tree(&tentative, Some("root"));
        assert!(
            tentative_projection
                .iter()
                .find(|row| tentative[row.index].info.id == "b")
                .unwrap()
                .relationship_unknown
        );
        assert!(
            !tentative_projection
                .iter()
                .find(|row| tentative[row.index].info.id == "c")
                .unwrap()
                .relationship_unknown
        );

        let cycle = vec![agent("a", "b", "a"), agent("b", "a", "b")];
        let cycle_projection = workflow_view::project_agent_tree(&cycle, Some("root"));
        assert_eq!(
            cycle_projection
                .iter()
                .map(|row| cycle[row.index].info.id.as_str())
                .collect::<Vec<_>>(),
            ["a", "b"]
        );
        assert!(cycle_projection.iter().all(|row| row.relationship_unknown));

        let mut deep = Vec::new();
        for index in 0..10 {
            let id = format!("deep-{index}");
            let parent = if index == 0 {
                "root".to_owned()
            } else {
                format!("deep-{}", index - 1)
            };
            deep.push(agent(&id, &parent, &format!("/root/{id}")));
        }
        let deep_projection = workflow_view::project_agent_tree(&deep, Some("root"));
        assert_eq!(deep_projection.len(), deep.len());
        assert_eq!(
            deep_projection
                .iter()
                .map(|row| row.depth)
                .collect::<Vec<_>>(),
            [1, 2, 3, 4, 5, 6, 7, 8, 8, 8]
        );
        assert!(deep_projection[..8].iter().all(|row| !row.depth_truncated));
        assert!(deep_projection[8..].iter().all(|row| row.depth_truncated));

        let deep_snapshot = CoreSnapshot {
            phase: SessionPhase::Running,
            thread_id: Some("root".into()),
            agents: deep,
            ..Default::default()
        };
        let mut deep_terminal = Terminal::new(TestBackend::new(120, 60)).unwrap();
        deep_terminal
            .draw(|frame| draw(frame, &deep_snapshot, &LocalState::default()))
            .unwrap();
        let deep_rendered: String = deep_terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(deep_rendered.contains("/root/deep-8"), "{deep_rendered}");
        assert!(deep_rendered.contains("/root/deep-9"), "{deep_rendered}");
        assert_eq!(deep_rendered.matches("[depth truncated]").count(), 2);

        let siblings = (0..10)
            .map(|index| {
                let id = format!("sibling-{index}");
                agent(&id, "root", &id)
            })
            .collect::<Vec<_>>();
        let sibling_snapshot = CoreSnapshot {
            phase: SessionPhase::Running,
            thread_id: Some("root".into()),
            agents: siblings,
            ..Default::default()
        };
        let (tx, mut rx) = tokio::sync::mpsc::channel(8);
        let mut sibling_local = LocalState::default();
        for _ in 0..10 {
            handle_key(
                KeyEvent::new(KeyCode::F(3), KeyModifiers::NONE),
                &sibling_snapshot,
                &mut sibling_local,
                &tx,
            );
        }
        assert_eq!(sibling_local.agent_id.as_deref(), Some("sibling-9"));
        assert!(rx.try_recv().is_err());
        let mut sibling_terminal = Terminal::new(TestBackend::new(120, 24)).unwrap();
        sibling_terminal
            .draw(|frame| draw(frame, &sibling_snapshot, &sibling_local))
            .unwrap();
        let sibling_rows = sibling_terminal
            .backend()
            .buffer()
            .content()
            .chunks(120)
            .map(|cells| cells.iter().map(|cell| cell.symbol()).collect::<String>())
            .collect::<Vec<_>>();
        let sibling_rendered = sibling_rows.join("\n");
        assert!(
            sibling_rendered.contains("> sibling-9"),
            "{sibling_rendered}"
        );
        let selected_row = sibling_rows
            .iter()
            .position(|row| row.contains("> sibling-9"))
            .expect("selected sibling visible");
        assert!(sibling_rows[selected_row + 1].contains("running / gen 1"));
        assert!(sibling_rows[selected_row + 2].contains("tokens: unavailable"));
        for (width, height) in [
            (100, 12),
            (120, 12),
            (100, 16),
            (120, 16),
            (100, 24),
            (120, 24),
        ] {
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            terminal
                .draw(|frame| draw(frame, &sibling_snapshot, &sibling_local))
                .unwrap();
            let panel = terminal
                .backend()
                .buffer()
                .content()
                .chunks(width as usize)
                .map(|cells| {
                    cells
                        .iter()
                        .take(32)
                        .map(|cell| cell.symbol())
                        .collect::<String>()
                })
                .collect::<Vec<_>>()
                .join("\n");
            assert!(panel.contains("sibling-9"), "{width}x{height}: {panel}");
            assert!(
                panel.contains("running / gen 1"),
                "{width}x{height}: {panel}"
            );
            assert!(
                panel.contains("tokens: unavailable"),
                "{width}x{height}: {panel}"
            );
        }
        let long_name = truncate_display_label("C:/very/long/代理路径/worker-9", 25);
        assert!(long_name.ends_with('…'));
        assert!(UnicodeWidthStr::width(long_name.as_str()) <= 25);

        handle_key(
            KeyEvent::new(KeyCode::F(3), KeyModifiers::NONE),
            &sibling_snapshot,
            &mut sibling_local,
            &tx,
        );
        assert!(sibling_local.agent_id.is_none());
        let mut root_terminal = Terminal::new(TestBackend::new(100, 12)).unwrap();
        root_terminal
            .draw(|frame| draw(frame, &sibling_snapshot, &sibling_local))
            .unwrap();
        let root_panel = root_terminal
            .backend()
            .buffer()
            .content()
            .chunks(100)
            .map(|cells| {
                cells
                    .iter()
                    .take(32)
                    .map(|cell| cell.symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(root_panel.contains("Agent: root"), "{root_panel}");
        assert!(root_panel.contains("gen unavailable"), "{root_panel}");
        assert!(root_panel.contains("tokens: unavailable"), "{root_panel}");

        let snapshot = CoreSnapshot {
            phase: SessionPhase::Running,
            thread_id: Some("root".into()),
            agents: first,
            ..Default::default()
        };
        let mut local = LocalState::default();
        local.editor.insert("SECRET_DRAFT");
        for (width, height) in [(120, 30), (100, 30), (80, 18), (40, 12)] {
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            terminal
                .draw(|frame| draw(frame, &snapshot, &local))
                .unwrap();
            let rows = terminal
                .backend()
                .buffer()
                .content()
                .chunks(width as usize)
                .map(|cells| cells.iter().map(|cell| cell.symbol()).collect::<String>())
                .collect::<Vec<_>>();
            let rendered = rows.join("\n");
            assert!(rendered.contains("SECRET_DRAFT"), "{rendered}");
            if width >= 100 {
                let panel_start = rows
                    .iter()
                    .position(|row| row.contains("Agents · F3"))
                    .expect("agent panel title");
                let panel_end = rows[panel_start..]
                    .iter()
                    .position(|row| row.contains("└──────────────────────────────┘"))
                    .map_or(rows.len() - panel_start - 1, |offset| offset);
                let panel = rows[panel_start..=panel_start + panel_end]
                    .iter()
                    .map(|row| row.chars().take(32).collect::<String>())
                    .collect::<Vec<_>>()
                    .join("\n");
                assert!(panel.contains("Agents · F3"), "{panel}");
                assert!(panel.contains("[parent unavailable] orphan"), "{panel}");
                assert!(!panel.contains("SECRET_DRAFT"), "{panel}");
                let a = panel.find("/root/a").expect("a in panel");
                let b = panel.find("/root/b").expect("b in panel");
                let c = panel.find("/root/b/c").expect("c in panel");
                assert!(b < c && c < a, "{panel}");
            }
        }

        let uuid_snapshot = CoreSnapshot {
            phase: SessionPhase::Running,
            thread_id: Some(root_uuid.into()),
            agents: uuid_agents,
            ..Default::default()
        };
        let mut terminal = Terminal::new(TestBackend::new(120, 30)).unwrap();
        terminal
            .draw(|frame| draw(frame, &uuid_snapshot, &LocalState::default()))
            .unwrap();
        let rows = terminal
            .backend()
            .buffer()
            .content()
            .chunks(120)
            .map(|cells| cells.iter().map(|cell| cell.symbol()).collect::<String>())
            .collect::<Vec<_>>();
        let rendered = rows.join("\n");
        let a_line = rows
            .iter()
            .find(|line| line.contains("/root/a"))
            .expect("uuid root child in panel");
        let b_line = rows
            .iter()
            .find(|line| line.contains("/root/b"))
            .expect("uuid root child in panel");
        assert!(!a_line.contains("parent unavailable"), "{rendered}");
        assert!(!b_line.contains("parent unavailable"), "{rendered}");
    }

    #[test]
    fn editor_deletes_whole_combining_characters_and_emoji_and_retains_utf8_cursor() {
        let mut editor = Editor::default();
        editor.insert("中e\u{301}👨‍👩‍👧‍👦");
        editor.backspace();
        assert_eq!(editor.text, "中e\u{301}");
        editor.left();
        editor.delete();
        assert_eq!(editor.text, "中");
        editor.backspace();
        assert!(editor.text.is_empty());
    }

    #[test]
    fn renders_real_response_and_status_in_compact_and_wide_terminals() {
        let mut snapshot = CoreSnapshot {
            phase: SessionPhase::Completed,
            ..Default::default()
        };
        snapshot.messages.push(ConversationItem {
            id: "a".into(),
            thread_id: String::new(),
            turn_id: "t".into(),
            role: "Agent".into(),
            text: "READY 中文".into(),
            complete: true,
            truncated: false,
        });
        for (width, height) in [(80, 24), (160, 45), (40, 12)] {
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            terminal
                .draw(|frame| draw(frame, &snapshot, &LocalState::default()))
                .unwrap();
            let rendered: String = terminal
                .backend()
                .buffer()
                .content()
                .iter()
                .map(|cell| cell.symbol())
                .collect();
            assert!(rendered.contains("READY"));
            assert!(rendered.contains("Completed"));
            assert!(!rendered.contains("65%"));
        }
    }

    #[test]
    fn enter_during_active_turn_preserves_draft_and_does_not_start_another_turn() {
        let (tx, mut rx) = tokio::sync::mpsc::channel(8);
        let snapshot = CoreSnapshot {
            phase: SessionPhase::Running,
            ..Default::default()
        };
        let mut local = LocalState::default();
        local.editor.insert("draft");
        handle_key(
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            &snapshot,
            &mut local,
            &tx,
        );
        assert_eq!(local.editor.text, "draft");
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn approval_selection_is_pinned_and_each_submission_locks_before_core_redraw() {
        let make = |id, seq, decisions| {
            let mut request = RequestView::decode(RpcId::Number(id), "item/commandExecution/requestApproval", &serde_json::json!({"threadId":"root","turnId":"one","command":"review","availableDecisions":decisions})).unwrap();
            request.received_seq = seq;
            request
        };
        let mut snapshot = CoreSnapshot {
            phase: SessionPhase::Running,
            requests: vec![
                make(1, 1, vec!["decline"]),
                make(2, 2, vec!["accept", "decline"]),
            ],
            ..Default::default()
        };
        let mut local = LocalState::default();
        local.editor.insert("TASK_DRAFT");
        let (tx, mut rx) = tokio::sync::mpsc::channel(8);
        let key = |code, modifiers| KeyEvent::new(code, modifiers);
        handle_key(
            key(KeyCode::F(2), KeyModifiers::NONE),
            &snapshot,
            &mut local,
            &tx,
        );
        assert_eq!(
            local.request_selection,
            Some(snapshot.requests[0].reference())
        );
        handle_key(
            key(KeyCode::Char('y'), KeyModifiers::CONTROL),
            &snapshot,
            &mut local,
            &tx,
        );
        assert!(rx.try_recv().is_err());
        handle_key(
            key(KeyCode::Char('n'), KeyModifiers::CONTROL),
            &snapshot,
            &mut local,
            &tx,
        );
        assert_eq!(
            rx.try_recv().unwrap(),
            Command::AnswerApproval {
                request: snapshot.requests[0].reference(),
                decision: ApprovalDecision::Decline
            }
        );
        handle_key(
            key(KeyCode::Char('n'), KeyModifiers::CONTROL),
            &snapshot,
            &mut local,
            &tx,
        );
        assert!(rx.try_recv().is_err());
        handle_key(
            key(KeyCode::F(2), KeyModifiers::NONE),
            &snapshot,
            &mut local,
            &tx,
        );
        handle_key(
            key(KeyCode::Char('y'), KeyModifiers::CONTROL),
            &snapshot,
            &mut local,
            &tx,
        );
        assert_eq!(
            rx.try_recv().unwrap(),
            Command::AnswerApproval {
                request: snapshot.requests[1].reference(),
                decision: ApprovalDecision::Accept
            }
        );
        handle_key(
            key(KeyCode::F(2), KeyModifiers::NONE),
            &snapshot,
            &mut local,
            &tx,
        );
        handle_key(
            key(KeyCode::Char('n'), KeyModifiers::CONTROL),
            &snapshot,
            &mut local,
            &tx,
        );
        assert!(rx.try_recv().is_err());
        snapshot.requests[0] = make(1, 3, vec!["accept", "decline"]);
        handle_key(
            key(KeyCode::Char('y'), KeyModifiers::CONTROL),
            &snapshot,
            &mut local,
            &tx,
        );
        assert!(rx.try_recv().is_err());
        assert!(selected_request(&snapshot, &local).is_none());
        handle_key(
            key(KeyCode::Esc, KeyModifiers::NONE),
            &snapshot,
            &mut local,
            &tx,
        );
        assert_eq!(local.editor.text, "TASK_DRAFT");
        assert!(!local.request_panel);
        handle_key(
            key(KeyCode::F(2), KeyModifiers::NONE),
            &snapshot,
            &mut local,
            &tx,
        );
        handle_key(
            key(KeyCode::Char('y'), KeyModifiers::CONTROL),
            &snapshot,
            &mut local,
            &tx,
        );
        assert_eq!(
            rx.try_recv().unwrap(),
            Command::AnswerApproval {
                request: snapshot.requests[0].reference(),
                decision: ApprovalDecision::Accept
            }
        );
    }

    #[test]
    fn input_drafts_follow_request_deliveries_and_esc_only_closes_details() {
        let make = |id, seq| {
            let mut request = RequestView::decode(RpcId::Number(id), "item/tool/requestUserInput", &serde_json::json!({"threadId":"root","turnId":"one","questions":[{"id":"secret","header":"Secret","question":"Value?","isSecret":true}]})).unwrap();
            request.received_seq = seq;
            request
        };
        let mut snapshot = CoreSnapshot {
            phase: SessionPhase::Running,
            requests: vec![make(1, 1), make(2, 2)],
            ..Default::default()
        };
        let mut local = LocalState::default();
        local.editor.insert("TASK_DRAFT");
        let (tx, mut rx) = tokio::sync::mpsc::channel(8);
        let f2 = KeyEvent::new(KeyCode::F(2), KeyModifiers::NONE);
        handle_key(f2, &snapshot, &mut local, &tx);
        local.editor.insert("SECRET_A中文👋");
        handle_key(f2, &snapshot, &mut local, &tx);
        assert!(local.editor.text.is_empty());
        local.editor.insert("SECRET_B");
        handle_key(
            KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
            &snapshot,
            &mut local,
            &tx,
        );
        assert_eq!(local.editor.text, "SECRET_B");
        handle_key(f2, &snapshot, &mut local, &tx);
        handle_key(f2, &snapshot, &mut local, &tx);
        assert_eq!(local.editor.text, "SECRET_A中文👋");
        assert_eq!(local.task_draft.as_ref().unwrap().text, "TASK_DRAFT");
        snapshot.requests[0] = make(1, 3);
        handle_key(
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            &snapshot,
            &mut local,
            &tx,
        );
        assert!(rx.try_recv().is_err());
        assert_eq!(local.editor.text, "TASK_DRAFT");
        handle_key(f2, &snapshot, &mut local, &tx);
        assert!(local.editor.text.is_empty());
        assert!(!local
            .input_drafts
            .keys()
            .any(|reference| reference.received_seq == 1));
    }

    #[test]
    fn cancel_targets_the_selected_owner_once_and_never_substitutes_for_decline() {
        let request = RequestView::decode(RpcId::Number(7), "item/commandExecution/requestApproval",
            &serde_json::json!({"threadId":"child","turnId":"child-turn","availableDecisions":["accept","cancel"]})).unwrap();
        let mut snapshot = CoreSnapshot {
            phase: SessionPhase::Running,
            requests: vec![request],
            ..Default::default()
        };
        let mut local = LocalState::default();
        local.editor.insert("TASK_DRAFT中文👋");
        let (tx, mut rx) = tokio::sync::mpsc::channel(8);
        handle_key(
            KeyEvent::new(KeyCode::F(2), KeyModifiers::NONE),
            &snapshot,
            &mut local,
            &tx,
        );
        let ctrl = |code| KeyEvent::new(KeyCode::Char(code), KeyModifiers::CONTROL);
        handle_key(ctrl('n'), &snapshot, &mut local, &tx);
        assert!(rx.try_recv().is_err());
        handle_key(ctrl('b'), &snapshot, &mut local, &tx);
        assert_eq!(
            rx.try_recv().unwrap(),
            Command::AnswerApproval {
                request: snapshot.requests[0].reference(),
                decision: ApprovalDecision::Cancel
            }
        );
        for code in ['b', 'y', 'n'] {
            handle_key(ctrl(code), &snapshot, &mut local, &tx);
        }
        assert!(rx.try_recv().is_err());
        assert_eq!(local.editor.text, "TASK_DRAFT中文👋");
        snapshot.requests[0].received_seq += 1;
        handle_key(ctrl('b'), &snapshot, &mut local, &tx);
        assert!(rx.try_recv().is_err());
        // Explicit selection of a replacement restores only that request's controls.
        handle_key(
            KeyEvent::new(KeyCode::F(2), KeyModifiers::NONE),
            &snapshot,
            &mut local,
            &tx,
        );
        handle_key(ctrl('b'), &snapshot, &mut local, &tx);
        assert_eq!(
            rx.try_recv().unwrap(),
            Command::AnswerApproval {
                request: snapshot.requests[0].reference(),
                decision: ApprovalDecision::Cancel
            }
        );
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn input_hints_and_disabled_cancel_do_not_send_commands_or_expose_answers() {
        let mut snapshot = observed_snapshot();
        snapshot.requests = vec![RequestView::decode(RpcId::Number(2), "item/tool/requestUserInput",
            &serde_json::json!({"threadId":"root-thread","turnId":"turn","isBlocking":false,"autoResolutionMs":42,
                "questions":[{"id":"secret","header":"Secret","question":"Q","isSecret":true}]})).unwrap()];
        let observation = snapshot.observation.clone();
        let mut local = LocalState {
            request_panel: true,
            ..Default::default()
        };
        sync_local_requests(&mut local, &snapshot);
        local.editor.insert("PRIVATE_ANSWER中文👋");
        let (tx, mut rx) = tokio::sync::mpsc::channel(8);
        handle_key(
            KeyEvent::new(KeyCode::Char('b'), KeyModifiers::CONTROL),
            &snapshot,
            &mut local,
            &tx,
        );
        assert!(rx.try_recv().is_err());
        assert_eq!(local.editor.text, "PRIVATE_ANSWER中文👋");
        local.notice = None;
        let mut seen = String::new();
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        for scroll in 0..70 {
            local.request_scroll = scroll;
            terminal
                .draw(|frame| draw(frame, &snapshot, &local))
                .unwrap();
            seen.extend(
                terminal
                    .backend()
                    .buffer()
                    .content()
                    .iter()
                    .map(|cell| cell.symbol()),
            );
        }
        assert!(seen.contains("Server blocking hint: false"));
        assert!(seen.contains("42 ms (informational)"));
        assert!(!seen.contains("PRIVATE_ANSWER"));
        assert!(!seen.contains("Ctrl+B"));
        assert_eq!(snapshot.observation, observation);
        assert!(rx.try_recv().is_err());
        snapshot.requests = vec![RequestView::decode(RpcId::Number(3), "item/fileChange/requestApproval",
            &serde_json::json!({"threadId":"root-thread","turnId":"turn","availableDecisions":["decline"]})).unwrap()];
        local.request_selection = None;
        sync_local_requests(&mut local, &snapshot);
        handle_key(
            KeyEvent::new(KeyCode::Char('b'), KeyModifiers::CONTROL),
            &snapshot,
            &mut local,
            &tx,
        );
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn request_context_is_scrollable_at_all_supported_sizes_and_never_exposes_secret_drafts() {
        let request = RequestView::decode(RpcId::Number(1), "item/commandExecution/requestApproval", &serde_json::json!({"threadId":"root","turnId":"one","itemId":"shell","command":"printf 中文👋\nSECOND_LINE\u{001b}[31m","cwd":"COMMAND_DIRECTORY","startedAtMs":123,"availableDecisions":["accept","decline","cancel","acceptForSession"],"additionalPermissions":{"network":{"enabled":true}}})).unwrap();
        let mut snapshot = CoreSnapshot {
            phase: SessionPhase::Running,
            thread_id: Some("root".into()),
            sandbox: "workspace-write".into(),
            approval_policy: "never".into(),
            cwd: "SESSION_DIRECTORY".into(),
            requests: vec![request],
            ..Default::default()
        };
        for (width, height) in [
            (30, 10),
            (60, 20),
            (80, 24),
            (100, 30),
            (120, 40),
            (160, 50),
        ] {
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            let mut local = LocalState {
                request_panel: true,
                ..Default::default()
            };
            sync_local_requests(&mut local, &snapshot);
            let mut seen = String::new();
            for scroll in 0..120 {
                local.request_scroll = scroll;
                terminal
                    .draw(|frame| draw(frame, &snapshot, &local))
                    .unwrap();
                seen.extend(
                    terminal
                        .backend()
                        .buffer()
                        .content()
                        .iter()
                        .map(|cell| cell.symbol()),
                );
            }
            for value in [
                "SECOND_LINE",
                "COMMAND_DIRECTORY",
                "never",
                "acceptForSession",
                "not applied",
                "Ctrl+Y",
                "Ctrl+N",
                "Ctrl+B",
            ] {
                assert!(seen.contains(value), "missing {value} at {width}x{height}");
            }
            assert!(!seen.contains('\u{001b}'));
        }
        snapshot.requests = vec![RequestView::decode(RpcId::Number(2),"item/tool/requestUserInput",&serde_json::json!({"threadId":"root","turnId":"one","questions":[{"id":"secret","header":"Secret","question":"Long question 中文👋","isSecret":true,"options":[{"label":"Choice","description":"OPTION_DETAIL"}]}]})).unwrap()];
        let mut local = LocalState {
            request_panel: true,
            ..Default::default()
        };
        sync_local_requests(&mut local, &snapshot);
        local.editor.insert("PRIVATE_ANSWER");
        for scroll in 0..60 {
            let mut terminal = Terminal::new(TestBackend::new(60, 20)).unwrap();
            local.request_scroll = scroll;
            terminal
                .draw(|frame| draw(frame, &snapshot, &local))
                .unwrap();
            let rendered: String = terminal
                .backend()
                .buffer()
                .content()
                .iter()
                .map(|cell| cell.symbol())
                .collect();
            assert!(!rendered.contains("PRIVATE_ANSWER"));
        }
    }

    #[test]
    fn oversized_multi_question_answers_keep_the_draft_and_can_be_corrected() {
        let request = RequestView::decode(RpcId::Number(1), "item/tool/requestUserInput", &serde_json::json!({"threadId":"root","turnId":"one","questions":[{"id":"a","header":"A","question":"First"},{"id":"b","header":"B","question":"Second"}]})).unwrap();
        let snapshot = CoreSnapshot {
            phase: SessionPhase::Running,
            requests: vec![request],
            ..Default::default()
        };
        let mut local = LocalState::default();
        let (tx, mut rx) = tokio::sync::mpsc::channel(8);
        sync_local_requests(&mut local, &snapshot);
        local.editor.insert(&"a".repeat(20000));
        handle_key(
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            &snapshot,
            &mut local,
            &tx,
        );
        local.editor.insert(&"b".repeat(20000));
        handle_key(
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            &snapshot,
            &mut local,
            &tx,
        );
        assert!(rx.try_recv().is_err());
        assert_eq!(local.editor.text.len(), 20000);
        assert!(local.notice.as_ref().unwrap().contains("32 KiB"));
        local.editor.clear();
        local.editor.insert("short answer");
        handle_key(
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            &snapshot,
            &mut local,
            &tx,
        );
        assert!(matches!(
            rx.try_recv().unwrap(),
            Command::AnswerUserInput { .. }
        ));
        assert!(local.submitted.contains(&snapshot.requests[0].reference()));
    }

    #[test]
    fn resolved_secret_question_cannot_submit_its_answer_as_a_root_task() {
        let request = RequestView::decode(RpcId::Number(1), "item/tool/requestUserInput", &serde_json::json!({"threadId":"root","turnId":"one","questions":[{"id":"secret","header":"Secret","question":"Value?","isSecret":true}]})).unwrap();
        let mut snapshot = CoreSnapshot {
            phase: SessionPhase::Running,
            thread_id: Some("root".into()),
            requests: vec![request],
            ..Default::default()
        };
        let (tx, mut rx) = tokio::sync::mpsc::channel(8);
        let mut local = LocalState::default();
        local.editor.insert("saved draft");
        handle_key(
            KeyEvent::new(KeyCode::Char('s'), KeyModifiers::NONE),
            &snapshot,
            &mut local,
            &tx,
        );
        assert_eq!(local.editor.text, "s");
        local.editor.insert("ecret answer");
        snapshot.requests.clear();
        snapshot.phase = SessionPhase::Completed;
        handle_key(
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            &snapshot,
            &mut local,
            &tx,
        );
        assert_eq!(
            rx.try_recv().unwrap(),
            Command::SubmitRootInput {
                text: "saved draft".into()
            }
        );
        assert!(local.editor.text.is_empty());
        assert!(local.answers.is_empty());
    }
}
