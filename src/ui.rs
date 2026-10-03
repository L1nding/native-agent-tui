use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::{self, stdout, Stdout};
use std::time::Duration;

use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Color, Style};
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use ratatui::Terminal;
use thiserror::Error;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use crate::client::{ClientHandle, Command};
use crate::history::HistoryHandle;
use crate::interactions::{ApprovalDecision, RequestKind, RequestRef, RequestView};
use crate::observation::{
    ActivityScope, ActivitySnapshot, AttentionClass, AttentionLevel, ExecutionState,
};
use crate::scheduler::{
    RootTaskSpec, SchedulerCommand, TaskAttempt, TaskId, TaskSnapshot, ROOT_QUEUE_LIMIT,
};
use crate::state::{display_text, CoreSnapshot, MESSAGE_BYTES};

mod history;
mod reminders;
mod requests;

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
struct Editor {
    text: String,
    cursor: usize,
}

impl Editor {
    fn insert(&mut self, text: &str) {
        let text = display_text(text);
        if self.text.len() + text.len() > MESSAGE_BYTES {
            return;
        }
        self.text.insert_str(self.cursor, &text);
        self.cursor += text.len();
    }
    fn left(&mut self) {
        self.cursor = self.text[..self.cursor]
            .grapheme_indices(true)
            .next_back()
            .map_or(0, |(index, _)| index);
    }
    fn right(&mut self) {
        if let Some(grapheme) = self.text[self.cursor..].graphemes(true).next() {
            self.cursor += grapheme.len();
        }
    }
    fn backspace(&mut self) {
        let end = self.cursor;
        self.left();
        self.text.drain(self.cursor..end);
    }
    fn delete(&mut self) {
        let start = self.cursor;
        self.right();
        self.text.drain(start..self.cursor);
        self.cursor = start;
    }
    fn clear(&mut self) {
        self.text.clear();
        self.cursor = 0;
    }
}

#[derive(Default)]
struct LocalState {
    reminders: reminders::Reminders,
    attention_editor: Option<AttentionEditor>,
    evidence: bool,
    evidence_scroll: usize,
    editor: Editor,
    task_draft: Option<Editor>,
    scroll_from_bottom: usize,
    notice: Option<String>,
    help: bool,
    tasks: bool,
    task_id: Option<TaskId>,
    confirm_stop: bool,
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

struct AttentionEditor {
    class_index: usize,
    field: usize,
    quiet: String,
    attention: String,
    notice: Option<String>,
}

impl AttentionEditor {
    fn new(snapshot: &CoreSnapshot, class_index: usize) -> Self {
        let pair = snapshot
            .observation
            .settings
            .get(AttentionClass::ALL[class_index]);
        Self {
            class_index,
            field: 0,
            quiet: pair.quiet_ms.to_string(),
            attention: pair.attention_ms.to_string(),
            notice: None,
        }
    }
    fn input(&mut self) -> &mut String {
        if self.field == 0 {
            &mut self.quiet
        } else {
            &mut self.attention
        }
    }
}

pub async fn run(client: ClientHandle, goal: Option<String>) -> Result<(), UiError> {
    run_tasks(client, goal.into_iter().map(RootTaskSpec::input).collect()).await
}

pub async fn run_tasks(client: ClientHandle, tasks: Vec<RootTaskSpec>) -> Result<(), UiError> {
    run_tasks_inner(client, tasks, None).await
}

pub async fn run_tasks_with_history(
    client: ClientHandle,
    tasks: Vec<RootTaskSpec>,
    history: HistoryHandle,
) -> Result<(), UiError> {
    run_tasks_inner(client, tasks, Some(history)).await
}

async fn run_tasks_inner(
    mut client: ClientHandle,
    tasks: Vec<RootTaskSpec>,
    mut history: Option<HistoryHandle>,
) -> Result<(), UiError> {
    let mut history_panel = history::HistoryPanel::default();
    let mut history_open = history.is_some();
    let result = async {
        let mut terminal = TerminalGuard::enter()?;
        let mut local = LocalState::default();
        if !tasks.is_empty() {
            client.commands.send(Command::QueueRootTasks { tasks }).await
                .map_err(|_| UiError::Shutdown("client is closed".into()))?;
        }
        let mut dirty = true;
        let mut snapshots_open = true;
        let mut tick = tokio::time::interval(Duration::from_millis(25));
        loop {
            if dirty {
                let snapshot = client.snapshots.borrow().clone();
                local.reminders.sync(&snapshot.observation);
                sync_local_requests(&mut local, &snapshot);
                terminal.terminal.draw(|frame| {
                    if history_panel.visible { history_panel.draw(frame, Some(&snapshot)); }
                    else { draw(frame, &snapshot, &local); }
                })?;
                dirty = false;
            }
            tokio::select! {
                change = async {
                    match &mut history { Some(history) => history.status.changed().await, None => std::future::pending().await }
                }, if history_open => {
                    if change.is_ok() { history_panel.updated(history.as_ref().unwrap()); }
                    else { history_open = false; history_panel.notice = Some("History reader closed; return to live execution.".into()); }
                    dirty = true;
                }
                change = client.snapshots.changed(), if snapshots_open => {
                    snapshots_open = change.is_ok();
                    dirty = true;
                }
                _ = tick.tick() => {
                    // poll with zero timeout; the runtime is never blocked waiting for a key.
                    while event::poll(Duration::ZERO)? {
                        match event::read()? {
                            Event::Key(key) if key.kind != KeyEventKind::Release => {
                                let snapshot = client.snapshots.borrow().clone();
                                if history_panel.visible {
                                    if history_panel.key(key, history.as_mut().unwrap(), false) { return Ok(()); }
                                } else if key.code == KeyCode::F(12) {
                                    if let Some(history) = &mut history { history_panel.open(history, None); }
                                    else { local.notice = Some("History browsing is unavailable for this client.".into()); }
                                } else if handle_key(key, &snapshot, &mut local, &client.commands) { return Ok(()); }
                                dirty = true;
                            }
                            Event::Paste(text) => {
                                if history_panel.visible { history_panel.paste(&text); dirty = true; continue; }
                                let snapshot = client.snapshots.borrow().clone();
                                sync_local_requests(&mut local, &snapshot);
                                if let Some(editor) = &mut local.attention_editor {
                                    for digit in text.chars().filter(char::is_ascii_digit).take(10) {
                                        if editor.input().len() < 10 { editor.input().push(digit); }
                                    }
                                } else { local.editor.insert(&text); }
                                dirty = true;
                            }
                            Event::Resize(_, _) => dirty = true,
                            _ => {}
                        }
                    }
                }
            }
        }
    }.await;
    let _ = client.commands.send(Command::Quit).await;
    let report = client
        .join
        .await
        .map_err(|error| UiError::Shutdown(error.to_string()))?;
    let history_result = if let Some(history) = &mut history {
        history.shutdown().await.map_err(UiError::from)
    } else {
        Ok(())
    };
    if let Some(error) = report.cleanup_error {
        return Err(UiError::Shutdown(error));
    }
    if let Some(error) = report.journal_error {
        return Err(UiError::Shutdown(error.to_string()));
    }
    result.and(history_result)
}

/// Offline observation recovery. This runtime has no ClientHandle or execution sender.
pub async fn run_history(
    mut service: HistoryHandle,
    session: Option<String>,
) -> Result<(), UiError> {
    let result = async {
        let mut terminal = TerminalGuard::enter()?;
        let mut panel = history::HistoryPanel::default();
        panel.open(&mut service, session);
        let mut dirty = true;
        let mut reader_open = true;
        let mut tick = tokio::time::interval(Duration::from_millis(25));
        loop {
            if dirty { terminal.terminal.draw(|frame| panel.draw(frame, None))?; dirty = false; }
            tokio::select! {
                changed = service.status.changed(), if reader_open => {
                    if changed.is_ok() { panel.updated(&service); }
                    else { reader_open = false; panel.notice = Some("History reader closed. Ctrl+Q exits.".into()); }
                    dirty = true;
                }
                _ = tick.tick() => {
                    while event::poll(Duration::ZERO)? {
                        match event::read()? {
                            Event::Key(key) if key.kind != KeyEventKind::Release => {
                                if panel.key(key, &mut service, true) { return Ok(()); }
                                dirty = true;
                            }
                            Event::Paste(text) => { panel.paste(&text); dirty = true; }
                            Event::Resize(_, _) => dirty = true,
                            _ => {}
                        }
                    }
                }
            }
        }
    }.await;
    let stopped = service.shutdown().await.map_err(UiError::from);
    result.and(stopped)
}

fn selected_request<'a>(snapshot: &'a CoreSnapshot, local: &LocalState) -> Option<&'a RequestView> {
    match &local.request_selection {
        Some(reference) => snapshot
            .requests
            .iter()
            .find(|request| request.matches(reference)),
        None => snapshot.requests.iter().find(|request| !request.responding),
    }
}

fn sync_local_requests(local: &mut LocalState, snapshot: &CoreSnapshot) {
    if local.request_selection.is_none() {
        local.request_selection = selected_request(snapshot, local).map(RequestView::reference);
    } else if snapshot.requests.is_empty() && !local.request_panel {
        local.request_selection = None;
    }
    sync_questions(local, selected_request(snapshot, local).cloned());
    local.input_drafts.retain(|reference, _| {
        snapshot
            .requests
            .iter()
            .any(|request| request.matches(reference) && !request.responding)
    });
    local.submitted.retain(|reference| {
        snapshot
            .requests
            .iter()
            .any(|request| request.matches(reference))
    });
}

fn request_locked(snapshot: &CoreSnapshot, local: &LocalState, request: &RequestView) -> bool {
    request.responding
        || local.submitted.contains(&request.reference())
        || matches!(
            snapshot.phase,
            crate::state::SessionPhase::Unknown
                | crate::state::SessionPhase::Disconnected
                | crate::state::SessionPhase::Stopping
                | crate::state::SessionPhase::ClosingTransport
                | crate::state::SessionPhase::Stopped
        )
}

fn sync_questions(local: &mut LocalState, request: Option<RequestView>) {
    let id = request
        .filter(|r| matches!(r.kind, RequestKind::UserInput { .. }))
        .map(|r| r.reference());
    if local.answering != id {
        if let Some(reference) = local.answering.take() {
            local.input_drafts.insert(
                reference,
                InputDraft {
                    editor: std::mem::take(&mut local.editor),
                    question_index: local.question_index,
                    answers: std::mem::take(&mut local.answers),
                },
            );
        }
        match (local.task_draft.is_some(), id.is_some()) {
            (false, true) => local.task_draft = Some(std::mem::take(&mut local.editor)),
            (true, false) => local.editor = local.task_draft.take().unwrap_or_default(),
            (true, true) => local.editor.clear(),
            (false, false) => {}
        }
        local.answers.clear();
        local.question_index = 0;
        if let Some(draft) = id
            .as_ref()
            .and_then(|reference| local.input_drafts.remove(reference))
        {
            local.editor = draft.editor;
            local.question_index = draft.question_index;
            local.answers = draft.answers;
        }
        local.answering = id;
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
    if key.code != KeyCode::F(9) {
        local.confirm_stop = false;
    }
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
            KeyCode::Char('y') | KeyCode::Char('n') => {
                if let Some(request) = selected_request(snapshot, local) {
                    let decision = if key.code == KeyCode::Char('y') {
                        ApprovalDecision::Accept
                    } else {
                        ApprovalDecision::Decline
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
            local.tasks = !local.tasks;
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
        KeyCode::F(6) | KeyCode::F(7) | KeyCode::F(8) if local.tasks => {
            if let Some(task) = selected_task(snapshot, local) {
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
        KeyCode::F(9) if local.tasks => {
            if local.confirm_stop {
                send(Command::Schedule(SchedulerCommand::StopWorkflow), tx, local);
                local.confirm_stop = false;
            } else {
                local.confirm_stop = true;
                local.notice = Some("Press F9 again to cancel all workflow tasks. Any other key cancels this action.".into());
            }
        }
        KeyCode::Up | KeyCode::Down if local.tasks => {
            let tasks = &snapshot.scheduler.tasks;
            if !tasks.is_empty() {
                let index = selected_task(snapshot, local)
                    .and_then(|task| tasks.iter().position(|other| other.id == task.id))
                    .unwrap_or(0);
                let next = if key.code == KeyCode::Up {
                    index.saturating_sub(1)
                } else {
                    (index + 1).min(tasks.len() - 1)
                };
                local.task_id = Some(tasks[next].id);
            }
        }
        KeyCode::Char('+') | KeyCode::Char('-') if local.tasks && !control => {
            if let Some(task) = selected_task(snapshot, local) {
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
            local.tasks = false;
            local.evidence = false;
            sync_local_requests(local, snapshot);
        }
        KeyCode::F(3) => {
            let next = local
                .agent_id
                .as_ref()
                .and_then(|id| {
                    snapshot
                        .agents
                        .iter()
                        .position(|agent| &agent.info.id == id)
                })
                .map_or(0, |index| index + 1);
            local.agent_id = snapshot.agents.get(next).map(|agent| agent.info.id.clone());
            local.scroll_from_bottom = 0;
        }
        KeyCode::Esc => {
            if local.request_panel {
                local.request_panel = false;
                return false;
            }
            local.help = false;
            local.tasks = false;
            local.evidence = false;
            local.editor.clear();
        }
        KeyCode::PageUp => local.scroll_from_bottom = local.scroll_from_bottom.saturating_add(8),
        KeyCode::PageDown => local.scroll_from_bottom = local.scroll_from_bottom.saturating_sub(8),
        KeyCode::Home if control => local.scroll_from_bottom = usize::MAX,
        KeyCode::End if control => local.scroll_from_bottom = 0,
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

fn selected_task<'a>(snapshot: &'a CoreSnapshot, local: &LocalState) -> Option<&'a TaskSnapshot> {
    local
        .task_id
        .and_then(|id| snapshot.scheduler.tasks.iter().find(|task| task.id == id))
        .or_else(|| snapshot.scheduler.tasks.first())
}

fn selected_agent_id<'a>(snapshot: &'a CoreSnapshot, local: &LocalState) -> &'a str {
    local
        .agent_id
        .as_ref()
        .and_then(|id| snapshot.agents.iter().find(|agent| &agent.info.id == id))
        .map_or("root", |agent| agent.info.id.as_str())
}

fn wrap(text: &str, width: usize) -> Vec<String> {
    let mut output = Vec::new();
    for source in display_text(text).split('\n') {
        let mut line = String::new();
        let mut cells = 0;
        for grapheme in source.graphemes(true) {
            let count = grapheme.width();
            if cells + count > width.max(1) && !line.is_empty() {
                output.push(std::mem::take(&mut line));
                cells = 0;
            }
            line.push_str(grapheme);
            cells += count;
        }
        output.push(line);
    }
    output
}

fn draw(frame: &mut ratatui::Frame<'_>, snapshot: &CoreSnapshot, local: &LocalState) {
    let area = frame.area();
    if area.width < 24 || area.height < 8 {
        frame.render_widget(
            Paragraph::new(format!(
                "{:?}\nResize terminal\nCtrl+Q quit",
                snapshot.phase
            )),
            area,
        );
        return;
    }
    let request = selected_request(snapshot, local);
    let waiting = snapshot.gate.as_ref().filter(|gate| gate.pending);
    let selected_agent = local
        .agent_id
        .as_ref()
        .and_then(|id| snapshot.agents.iter().find(|agent| &agent.info.id == id));
    let selected_thread = selected_agent
        .map(|agent| agent.info.id.as_str())
        .or(snapshot.thread_id.as_deref())
        .unwrap_or("");
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(if area.height > 16 {
                if snapshot.observation.activities.is_empty() {
                    4
                } else {
                    6
                }
            } else {
                3
            }),
            Constraint::Min(1),
            Constraint::Length(
                if area.height > 16
                    && (request.is_some() || snapshot.last_error.is_some() || waiting.is_some())
                {
                    6
                } else {
                    2
                },
            ),
            Constraint::Length(3),
            Constraint::Length(1),
        ])
        .split(area);
    let actions = snapshot
        .observation
        .activities
        .iter()
        .filter(|activity| activity.attention.requires_action)
        .count();
    let attention = snapshot
        .observation
        .activities
        .iter()
        .filter(|activity| activity.attention.level == AttentionLevel::AttentionNeeded)
        .count();
    let acknowledged = snapshot
        .observation
        .activities
        .iter()
        .filter(|activity| local.reminders.is_acknowledged(activity))
        .count();
    let reminders_status = if acknowledged > 0 {
        format!(" | waiting:{acknowledged}")
    } else {
        String::new()
    };
    let status = format!(
        "{:?} | action:{} attention:{}{} | turns: {} | children: {} | queued: {}{}",
        snapshot.phase,
        actions,
        attention,
        reminders_status,
        snapshot.root_turn_count,
        snapshot.agents.len(),
        snapshot.queued_inputs,
        if snapshot.scheduler.stopping {
            " | stopping"
        } else if snapshot.scheduler.paused {
            " | dispatch paused"
        } else {
            ""
        },
    );
    let settings = format!(
        "{} | {} | {} | {}",
        snapshot.model.as_deref().unwrap_or("model pending"),
        snapshot.cwd,
        snapshot.sandbox,
        snapshot.approval_policy
    );
    let agent_id = selected_agent.map_or("root", |agent| agent.info.id.as_str());
    let selected_activity = focus_activity(snapshot, agent_id);
    let mut header = vec![Line::from(status)];
    if let Some(activity) = selected_activity {
        header.push(Line::from(display_text(&reminder_brief(activity, local))));
        header.push(Line::from(display_text(&evidence_brief(activity, local))));
    }
    header.push(Line::from(display_text(&settings)));
    if area.height <= 16 && selected_activity.is_some() {
        header.remove(0);
    }
    frame.render_widget(
        Paragraph::new(header).block(Block::default().borders(Borders::ALL).title(
            if area.height <= 16 && selected_activity.is_some() {
                format!(
                    " {:?} action:{actions} attention:{attention}{} ",
                    snapshot.phase,
                    if acknowledged > 0 {
                        format!(" wait:{acknowledged}")
                    } else {
                        String::new()
                    }
                )
            } else {
                format!(" Native Agent TUI {} ", env!("CARGO_PKG_VERSION"))
            },
        )),
        chunks[0],
    );

    let conversation_area = if area.width >= 100 && !snapshot.agents.is_empty() {
        let panels = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Length(32), Constraint::Min(1)])
            .split(chunks[1]);
        let mut agents = vec![Line::from(if selected_agent.is_none() {
            "> root"
        } else {
            "  root"
        })];
        for agent in &snapshot.agents {
            let status = if agent.awaiting_turn {
                "starting".into()
            } else {
                agent
                    .outcome
                    .as_ref()
                    .map_or_else(|| "running".into(), |outcome| format!("{outcome:?}"))
            };
            let name = agent
                .info
                .path
                .as_deref()
                .or(agent.info.nickname.as_deref())
                .unwrap_or(&agent.info.id);
            agents.push(Line::from(display_text(&format!(
                "{} {name}",
                if Some(agent.info.id.as_str())
                    == selected_agent.map(|agent| agent.info.id.as_str())
                {
                    ">"
                } else {
                    " "
                }
            ))));
            agents.push(Line::from(format!(
                "    {status} / gen {}",
                agent.generation
            )));
            if let Some(activity) = focus_activity(snapshot, &agent.info.id) {
                agents.push(Line::from(format!(
                    "    {:?} / quiet {}",
                    activity.attention.level,
                    age(activity.silence_ms)
                )));
            }
        }
        frame.render_widget(
            Paragraph::new(agents).block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(" Agents · F3 "),
            ),
            panels[0],
        );
        panels[1]
    } else {
        chunks[1]
    };
    let width = conversation_area.width.saturating_sub(2) as usize;
    let mut transcript = Vec::new();
    for message in snapshot
        .messages
        .iter()
        .filter(|message| message.thread_id == selected_thread)
    {
        transcript.push(format!(
            "{}{}",
            message.role,
            if message.truncated {
                " [truncated]"
            } else {
                ""
            }
        ));
        transcript.extend(wrap(&message.text, width));
        transcript.push(String::new());
    }
    if transcript.is_empty() {
        transcript.push(
            if selected_agent.is_some() {
                "Waiting for child output."
            } else {
                "Type a task below and press Enter."
            }
            .into(),
        );
    }
    if transcript.last().is_some_and(|line| line.is_empty()) {
        transcript.pop();
    }
    let height = conversation_area.height.saturating_sub(2) as usize;
    let max_scroll = transcript.len().saturating_sub(height);
    let scroll = local.scroll_from_bottom.min(max_scroll);
    let start = max_scroll.saturating_sub(scroll);
    let name = selected_agent
        .map(|agent| {
            agent
                .info
                .path
                .as_deref()
                .or(agent.info.nickname.as_deref())
                .unwrap_or(&agent.info.id)
        })
        .unwrap_or("root");
    let title = display_text(&format!(
        " {name} · F3 switch{} ",
        if snapshot.history_truncated {
            " [older content truncated]"
        } else {
            ""
        }
    ));
    let visible: Vec<_> = transcript
        .into_iter()
        .skip(start)
        .take(height)
        .map(Line::from)
        .collect();
    frame.render_widget(
        Paragraph::new(visible).block(Block::default().borders(Borders::ALL).title(title)),
        conversation_area,
    );
    if local.tasks {
        draw_tasks(frame, chunks[1], snapshot, local);
    }
    if local.evidence {
        draw_evidence(frame, chunks[1], snapshot, local, agent_id);
    }

    let notice = local
        .notice
        .as_ref()
        .or(snapshot.notice.as_ref())
        .or(snapshot.last_error.as_ref());
    let activity = if local.help {
        "Enter root task | Ctrl+Enter queue | Shift+Enter newline | Ctrl+C interrupt root | Ctrl+Q quit | Ctrl+W acknowledge/restore silence reminders for selected agent | F2 request | F3 agent | F4 tasks | F5 pause dispatch | F6 pause task | F7 cancel | F8 retry (may repeat effects) | +/- priority | F9 twice stop workflow | F10 thresholds | F11 evidence | F12 history/export".to_owned()
    } else if let Some(notice) = &local.notice {
        notice.clone()
    } else if let Some(request) = request {
        match &request.kind {
            RequestKind::UserInput { questions } => {
                let question = &questions[local.question_index.min(questions.len() - 1)];
                let options = question
                    .options
                    .as_ref()
                    .map(|values| {
                        values
                            .iter()
                            .map(|v| format!("{}: {}", v.label, v.description))
                            .collect::<Vec<_>>()
                            .join("\n")
                    })
                    .unwrap_or_default();
                format!(
                    "{} · {} ({}/{})\n{}\n{}",
                    request.thread_id,
                    question.header,
                    local.question_index + 1,
                    questions.len(),
                    question.question,
                    options
                )
            }
            _ => format!(
                "{} · {}\n{}",
                request.thread_id,
                request.summary,
                requests::actions(snapshot, local, request)
            ),
        }
    } else if let Some(notice) = notice {
        if let Some(gate) = waiting {
            format!("{}\n{notice}", gate_status(snapshot, gate))
        } else {
            notice.clone()
        }
    } else if let Some(gate) = waiting {
        gate_status(snapshot, gate)
    } else if snapshot.queued_inputs > 0 {
        format!(
            "{} root tasks pending; see dependencies and controls in F4.",
            snapshot.queued_inputs
        )
    } else {
        snapshot.tool_activity.clone().unwrap_or_default()
    };
    let lines: Vec<_> = wrap(&activity, chunks[2].width.saturating_sub(2) as usize)
        .into_iter()
        .map(Line::from)
        .collect();
    frame.render_widget(
        Paragraph::new(lines)
            .style(Style::default().fg(if snapshot.last_error.is_some() {
                Color::Red
            } else {
                Color::Yellow
            }))
            .block(
                Block::default()
                    .borders(Borders::LEFT | Borders::RIGHT)
                    .title(if request.is_some() {
                        " Pending request "
                    } else {
                        " Status "
                    }),
            ),
        chunks[2],
    );

    let secret = request.is_some_and(|r| matches!(&r.kind, RequestKind::UserInput { questions } if questions.get(local.question_index).is_some_and(|q| q.is_secret)));
    let prefix = if secret {
        "*".repeat(
            local.editor.text[..local.editor.cursor]
                .graphemes(true)
                .count(),
        )
    } else {
        display_text(&local.editor.text[..local.editor.cursor]).replace('\n', "↵")
    };
    let suffix = if secret {
        "*".repeat(
            local.editor.text[local.editor.cursor..]
                .graphemes(true)
                .count(),
        )
    } else {
        display_text(&local.editor.text[local.editor.cursor..]).replace('\n', "↵")
    };
    let input_width = chunks[3].width.saturating_sub(2) as usize;
    let mut left = prefix;
    while left.width() >= input_width && !left.is_empty() {
        let length = left.graphemes(true).next().unwrap().len();
        left.drain(..length);
    }
    let cursor = left.width();
    let display = format!("{left}{suffix}");
    frame.render_widget(
        Paragraph::new(display).block(Block::default().borders(Borders::ALL).title(
            if request.is_some_and(|r| matches!(r.kind, RequestKind::UserInput { .. })) {
                " Answer "
            } else {
                if waiting.is_some() {
                    " Root task · Enter to queue "
                } else {
                    " Root task "
                }
            },
        )),
        chunks[3],
    );
    frame.set_cursor_position((chunks[3].x + 1 + cursor as u16, chunks[3].y + 1));
    frame.render_widget(
        Paragraph::new(if local.tasks {
            "Up/Down select  F5 workflow  F6 pause  F7 cancel  F8 retry  +/- priority  F9 stop"
        } else if local.evidence {
            "Ctrl+W wait/restore  PgUp/PgDn scroll  F11 close  F3 agent"
        } else {
            "Enter send | Ctrl+C interrupt | Ctrl+Q quit | F3 agent | F4 tasks | F11 evidence | Ctrl+W wait/restore | F12 history"
        }),
        chunks[4],
    );
    if let Some(editor) = &local.attention_editor {
        draw_attention_editor(frame, snapshot, editor);
    }
    if local.request_panel && local.attention_editor.is_none() {
        requests::draw(frame, snapshot, local);
    }
}

fn age(ms: Option<u64>) -> String {
    ms.map_or_else(|| "unknown".into(), |ms| format!("{}s", ms / 1000))
}

fn focus_activity<'a>(snapshot: &'a CoreSnapshot, agent_id: &str) -> Option<&'a ActivitySnapshot> {
    snapshot
        .observation
        .activities
        .iter()
        .filter(|activity| activity.identity.agent_id == agent_id)
        .max_by_key(|activity| {
            let rank = if activity.attention.requires_action {
                5
            } else if activity.attention.level == AttentionLevel::AttentionNeeded {
                4
            } else if matches!(
                activity.execution_state,
                ExecutionState::Starting | ExecutionState::Running | ExecutionState::Waiting
            ) {
                3
            } else if activity.scope == ActivityScope::Turn {
                2
            } else {
                1
            };
            (
                rank,
                activity.scope == ActivityScope::Tool,
                activity.silence_ms.unwrap_or(0),
            )
        })
}

fn activity_brief(activity: &ActivitySnapshot) -> String {
    format!(
        "{:?} {} | quiet {} | {:?}",
        activity.execution_state,
        activity.tool_category.map_or_else(
            || format!("{:?}", activity.kind),
            |category| format!("{category:?}")
        ),
        age(activity.silence_ms),
        activity.attention.level
    )
}

fn reminder_brief(activity: &ActivitySnapshot, local: &LocalState) -> String {
    let mut text = activity_brief(activity);
    if local.reminders.is_acknowledged(activity) {
        text.push_str(" | reminder off locally");
    }
    text
}

fn next_action(activity: &ActivitySnapshot, local: &LocalState) -> &'static str {
    if activity.attention.requires_action {
        "F2 answer request"
    } else if activity.execution_state == ExecutionState::Unknown {
        "inspect unknown outcome"
    } else if local.reminders.is_acknowledged(activity) {
        "continuing to wait; Ctrl+W restore"
    } else if matches!(
        activity.attention.level,
        AttentionLevel::Quiet | AttentionLevel::AttentionNeeded
    ) {
        "F11 inspect; Ctrl+W wait or Ctrl+C"
    } else if activity.attention.level == AttentionLevel::Ended {
        "review result"
    } else {
        "wait; F11 evidence"
    }
}

fn evidence_brief(activity: &ActivitySnapshot, local: &LocalState) -> String {
    format!(
        "Next: {} | Last: {}",
        next_action(activity, local),
        activity.last_evidence.as_ref().map_or_else(
            || "unknown".into(),
            |evidence| format!("{:?} {:?} #{}", evidence.kind, evidence.source, evidence.id)
        )
    )
}

fn draw_evidence(
    frame: &mut ratatui::Frame<'_>,
    area: ratatui::layout::Rect,
    snapshot: &CoreSnapshot,
    local: &LocalState,
    agent_id: &str,
) {
    let mut rows = Vec::new();
    if let Some(journal) = &snapshot.journal {
        rows.push(format!(
            "Session {} / committed {} / submitted {}",
            journal.session_id, journal.committed_seq, journal.submitted_seq
        ));
    }
    for activity in snapshot
        .observation
        .activities
        .iter()
        .filter(|activity| activity.identity.agent_id == agent_id)
    {
        rows.push(format!(
            "{:?} {} | {:?}",
            activity.scope,
            activity.item_id.as_deref().unwrap_or("turn"),
            activity.attention.level
        ));
        rows.push(activity_brief(activity));
        if local.reminders.is_acknowledged(activity) {
            rows.push("Silence reminder: off locally until new evidence; Ctrl+W restore".into());
        }
        rows.push(format!(
            "Last: {}",
            activity.last_evidence.as_ref().map_or_else(
                || "unknown".into(),
                |evidence| format!(
                    "{:?} / {:?} #{}",
                    evidence.kind, evidence.source, evidence.id
                )
            )
        ));
        rows.push(format!("Next: {}", next_action(activity, local)));
        rows.push(format!(
            "Elapsed {} / progress {} / bytes {}",
            age(activity.elapsed_ms),
            activity.progress_seq,
            activity.output_bytes
        ));
        rows.push(format!(
            "Thread {:?} turn {:?} gen {:?} attempt {:?}",
            activity.identity.thread_id,
            activity.identity.turn_id,
            activity.identity.generation,
            activity.identity.attempt_id
        ));
        rows.push(format!(
            "Quiet {:?}ms / attention {:?}ms / source {:?}",
            activity.attention.quiet_after_ms,
            activity.attention.attention_after_ms,
            activity.attention.config_source
        ));
        rows.push("Provider execution: unavailable".into());
        if let Some(reason) = activity.wait_reason {
            rows.push(format!(
                "Wait: {reason:?} / resume: {:?}",
                activity.resume_condition
            ));
        }
        for target in &activity.wait_targets {
            rows.push(format!(
                "Target {} / turn {:?} gen {} / {:?} / quiet {} / {:?}",
                target.thread_id,
                target.turn_id,
                target.generation,
                target.outcome,
                age(target.silence_ms),
                target.attention.as_ref().map(|attention| attention.level)
            ));
        }
        rows.push(String::new());
    }
    if rows.is_empty() {
        rows.push("Activity evidence unavailable.".into());
    }
    let lines: Vec<_> = rows
        .iter()
        .flat_map(|row| wrap(&display_text(row), area.width.saturating_sub(2) as usize))
        .map(Line::from)
        .collect();
    let height = area.height.saturating_sub(2) as usize;
    let start = local
        .evidence_scroll
        .min(lines.len().saturating_sub(height));
    frame.render_widget(
        Paragraph::new(
            lines
                .into_iter()
                .skip(start)
                .take(height)
                .collect::<Vec<_>>(),
        )
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Evidence · F11 · PgUp/PgDn "),
        ),
        area,
    );
}

fn draw_attention_editor(
    frame: &mut ratatui::Frame<'_>,
    snapshot: &CoreSnapshot,
    editor: &AttentionEditor,
) {
    let screen = frame.area();
    let width = screen.width.min(68);
    let height = screen.height.min(12);
    let area = ratatui::layout::Rect::new(
        screen.x + (screen.width - width) / 2,
        screen.y + (screen.height - height) / 2,
        width,
        height,
    );
    let class = AttentionClass::ALL[editor.class_index];
    let effective = snapshot.observation.settings.get(class);
    let rows = [
        format!("{class:?} / effective source {:?}", effective.source),
        format!(
            "{} Quiet ms: {}",
            if editor.field == 0 { ">" } else { " " },
            editor.quiet
        ),
        format!(
            "{} Attention ms: {}",
            if editor.field == 1 { ">" } else { " " },
            editor.attention
        ),
        "Up/Down class · Tab field · Ctrl+U clear".into(),
        "Enter apply · Esc close · session only".into(),
        editor.notice.clone().unwrap_or_default(),
    ];
    let lines: Vec<_> = rows
        .iter()
        .flat_map(|row| wrap(row, width.saturating_sub(2) as usize))
        .map(Line::from)
        .collect();
    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new(lines).block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Attention · F10 "),
        ),
        area,
    );
    frame.set_cursor_position((area.x + 1, area.y + 2 + editor.field as u16));
}

fn draw_tasks(
    frame: &mut ratatui::Frame<'_>,
    area: ratatui::layout::Rect,
    snapshot: &CoreSnapshot,
    local: &LocalState,
) {
    let scheduler = &snapshot.scheduler;
    let selected = selected_task(snapshot, local);
    let height = area.height.saturating_sub(2) as usize;
    let details = if height >= 6 { 3 } else { 0 };
    let capacity = height
        .saturating_sub(details + usize::from(height > 1))
        .max(1);
    let index = selected
        .and_then(|task| scheduler.tasks.iter().position(|other| other.id == task.id))
        .unwrap_or(0);
    let start = index.saturating_sub(capacity.saturating_sub(1));
    let mut lines = Vec::new();
    if height > 1 {
        lines.push(Line::from(format!(
            "Root slots: {} | native turns observed: {}",
            scheduler.root_slots_reserved, scheduler.native_turns_observed
        )));
    }
    for task in scheduler.tasks.iter().skip(start).take(capacity) {
        lines.push(Line::from(display_text(&format!(
            "{} #{} {:?} a{} p{} {}{} {}",
            if selected.is_some_and(|selected| selected.id == task.id) {
                ">"
            } else {
                " "
            },
            task.id.0,
            task.state,
            task.attempt,
            task.priority,
            if task.pause_requested {
                "pause requested "
            } else {
                ""
            },
            if task.cancel_requested {
                "cancel requested "
            } else {
                ""
            },
            task.title
        ))));
    }
    if let Some(task) = selected.filter(|_| details > 0) {
        lines.push(Line::from(format!(
            "Dependencies: {:?} | wait: {:?}",
            task.dependencies, task.wait_targets
        )));
        lines.push(Line::from(format!(
            "Blocked: {:?} | requests: {}",
            task.blocked_reason, task.pending_requests
        )));
        lines.push(Line::from(display_text(
            &task.external.as_ref().map_or_else(
                || "External turn: pending".into(),
                |external| {
                    format!(
                        "External: {} / {} / gen {}",
                        external.thread_id, external.turn_id, external.generation
                    )
                },
            ),
        )));
    }
    frame.render_widget(
        Paragraph::new(lines).block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Tasks · F4 close · F5 pause dispatch "),
        ),
        area,
    );
}

fn gate_status(snapshot: &CoreSnapshot, gate: &crate::state::GateSnapshot) -> String {
    let done = gate
        .targets
        .iter()
        .filter(|target| target.outcome.is_some())
        .count();
    let mut lines = vec![format!(
        "Waiting children: {done}/{} | queued: {} | root starts during wait: {}",
        gate.targets.len(),
        snapshot.queued_inputs,
        snapshot
            .root_start_requests
            .saturating_sub(gate.root_starts_at_enter)
    )];
    for target in gate.targets.iter().take(3) {
        let agent = snapshot
            .agents
            .iter()
            .find(|agent| agent.info.id == target.id);
        let name = agent
            .and_then(|agent| {
                agent
                    .info
                    .path
                    .as_deref()
                    .or(agent.info.nickname.as_deref())
            })
            .unwrap_or(&target.id);
        let status = target.outcome.as_ref().map_or_else(
            || {
                if target.turn_id.is_some() {
                    "running".into()
                } else {
                    "awaiting turn".into()
                }
            },
            |outcome| format!("{outcome:?}"),
        );
        lines.push(format!("{name}: {status} (gen {})", target.generation));
    }
    lines.join("\n")
}

struct TerminalGuard {
    terminal: Terminal<CrosstermBackend<Stdout>>,
}

impl TerminalGuard {
    fn enter() -> Result<Self, UiError> {
        enable_raw_mode()?;
        let mut output = stdout();
        if let Err(error) = execute!(output, EnterAlternateScreen, event::EnableBracketedPaste) {
            let _ = disable_raw_mode();
            let _ = execute!(output, LeaveAlternateScreen, event::DisableBracketedPaste);
            return Err(error.into());
        }
        match Terminal::new(CrosstermBackend::new(output)) {
            Ok(terminal) => Ok(Self { terminal }),
            Err(error) => {
                let _ = disable_raw_mode();
                let _ = execute!(stdout(), LeaveAlternateScreen, event::DisableBracketedPaste);
                Err(error.into())
            }
        }
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(
            self.terminal.backend_mut(),
            LeaveAlternateScreen,
            event::DisableBracketedPaste
        );
        let _ = self.terminal.show_cursor();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::RpcId;

    fn observed_snapshot() -> CoreSnapshot {
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
                    requests: &[],
                    gate: None,
                },
                now,
            )
            .unwrap();
        CoreSnapshot {
            phase: crate::state::SessionPhase::Running,
            observation: observer.snapshot_at(1, now + Duration::from_secs(31)),
            ..Default::default()
        }
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
        let snapshot = observed_snapshot();
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
            next_action(&snapshot.observation.activities[1], &local),
            "F2 answer request"
        );
        assert_eq!(
            next_action(&snapshot.observation.activities[2], &local),
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
        assert!(local.tasks);
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
            assert!(rendered.contains("Tasks · F4"), "{rendered}");
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
        let selected = selected_task(&snapshot, &local).unwrap();
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
        assert!(rx.try_recv().is_err());
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
    fn gate_view_shows_waiting_progress_and_agent_switching_only_changes_the_local_view() {
        use crate::agents::{AgentInfo, AgentSnapshot};
        use crate::gate::WaitTarget;
        use crate::state::GateSnapshot;
        let mut snapshot = CoreSnapshot {
            phase: SessionPhase::GatePending,
            thread_id: Some("root".into()),
            root_start_requests: 1,
            queued_inputs: 2,
            agents: vec![AgentSnapshot {
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
            }],
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
        assert!(local.agent_id.is_none());
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
    fn request_context_is_scrollable_at_all_supported_sizes_and_never_exposes_secret_drafts() {
        let request = RequestView::decode(RpcId::Number(1), "item/commandExecution/requestApproval", &serde_json::json!({"threadId":"root","turnId":"one","itemId":"shell","command":"printf 中文👋\nSECOND_LINE\u{001b}[31m","cwd":"COMMAND_DIRECTORY","startedAtMs":123,"availableDecisions":["accept","decline","acceptForSession"],"additionalPermissions":{"network":{"enabled":true}}})).unwrap();
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
