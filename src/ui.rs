use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::{self, stdout, Stdout};
use std::time::Duration;

use crossterm::event::{self, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
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

use crate::agents::AgentSnapshot;
use crate::client::{ClientHandle, Command};
use crate::history::HistoryHandle;
use crate::interactions::{ApprovalDecision, RequestKind, RequestRef, RequestView};
use crate::observation::{
    ActivityScope, ActivitySnapshot, AttentionClass, AttentionLevel, CompactionFact,
    CompactionFactStatus, ExecutionState,
};
use crate::scheduler::{
    RootTaskSpec, SchedulerCommand, TaskAttempt, TaskId, TaskSnapshot, ROOT_QUEUE_LIMIT,
};
use crate::state::{display_text, CoreSnapshot, FactSource, MESSAGE_BYTES};

mod history;
mod input;
mod reminders;
mod requests;
mod scope;
mod search;
mod timeline;
mod tool_detail;
mod tool_search;

use input::{InputEvent, TerminalInput};

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
    tasks: bool,
    task_id: Option<TaskId>,
    task_scroll: usize,
    task_manual_scroll: bool,
    task_link_cursor: Option<WorkflowLinkCursor>,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WorkflowLinkKind {
    Dependency,
    Gate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct WorkflowLinkCursor {
    origin: TaskId,
    origin_attempt: u64,
    target: TaskId,
    kind: WorkflowLinkKind,
    index: usize,
    captured_attempt: Option<u64>,
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

pub async fn run_tasks_with_history(
    mut client: ClientHandle,
    tasks: Vec<RootTaskSpec>,
    mut history: HistoryHandle,
) -> Result<(), UiError> {
    let mut history_panel = history::HistoryPanel::default();
    let mut history_open = true;
    let mut search = search::SearchHandle::spawn();
    let mut search_open = true;
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
                let size = terminal.terminal.size()?;
                local.viewport = ratatui::layout::Rect::new(0, 0, size.width, size.height);
                let snapshot = client.snapshots.borrow().clone();
                local.reminders.sync(&snapshot.observation);
                sync_local_requests(&mut local, &snapshot);
                local.search.dispatch(snapshot.clone(), &mut search);
                local.timeline.sync(&snapshot);
                terminal.terminal.draw(|frame| {
                    if history_panel.visible { history_panel.draw(frame, Some(&snapshot)); }
                    else { draw(frame, &snapshot, &local); }
                })?;
                dirty = false;
            }
            tokio::select! {
                change = search.changed(), if search_open => {
                    search_open = change.is_ok();
                    if search_open { local.search.updated(&search); }
                    else { local.search.unavailable(); }
                    dirty = true;
                }
                change = async {
                    tokio::select! {
                        result = history.status.changed() => (false, result),
                        result = history.search.status.changed() => (true, result),
                    }
                }, if history_open => {
                    if let (is_search, Ok(())) = change {
                        if is_search {
                            history_panel.search_updated(&history);
                        } else {
                            history_panel.updated(&mut history);
                        }
                    } else {
                        history_open = false;
                        history_panel.notice = Some("History reader closed; return to live execution.".into());
                    }
                    dirty = true;
                }
                change = client.snapshots.changed(), if snapshots_open => {
                    snapshots_open = change.is_ok();
                    dirty = true;
                }
                _ = tick.tick() => {
                    // Bound each input batch so snapshots and redraw remain responsive.
                    for _ in 0..128 {
                        let Some(event) = terminal.input.read_ready()? else { break; };
                        match event {
                            InputEvent::Key(key) if key.kind != KeyEventKind::Release => {
                                let snapshot = client.snapshots.borrow().clone();
                                if history_panel.visible {
                                    if history_panel.key(key, &mut history, false) { return Ok(()); }
                                } else if key.code == KeyCode::F(12) {
                                    local.search.close();
                                    local.timeline.close();
                                    history_panel.open(&mut history, None);
                                } else if handle_key(key, &snapshot, &mut local, &client.commands) { return Ok(()); }
                                dirty = true;
                            }
                            InputEvent::Paste(text) => {
                                if history_panel.visible { history_panel.paste(&text); dirty = true; continue; }
                                let snapshot = client.snapshots.borrow().clone();
                                handle_paste(&text, &snapshot, &mut local);
                                dirty = true;
                            }
                            InputEvent::PasteRejected => {
                                if history_panel.visible { history_panel.notice = Some(PASTE_REJECTED.into()); }
                                else { reject_paste(&mut local); }
                                dirty = true;
                            }
                            InputEvent::Resize(_, _) => dirty = true,
                            _ => {}
                        }
                    }
                }
            }
        }
    }.await;
    search.shutdown().await;
    let _ = client.commands.send(Command::Quit).await;
    let report = client
        .join
        .await
        .map_err(|error| UiError::Shutdown(error.to_string()))?;
    let history_result = history.shutdown().await.map_err(UiError::from);
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
            if dirty {
                terminal.terminal.draw(|frame| panel.draw(frame, None))?;
                dirty = false;
            }
            tokio::select! {
                changed = async {
                    tokio::select! {
                        result = service.status.changed() => (false, result),
                        result = service.search.status.changed() => (true, result),
                    }
                }, if reader_open => {
                    if let (is_search, Ok(())) = changed {
                        if is_search { panel.search_updated(&service); }
                        else { panel.updated(&mut service); }
                    } else {
                        reader_open = false;
                        panel.notice = Some("History reader closed. Ctrl+Q exits.".into());
                    }
                    dirty = true;
                }
                _ = tick.tick() => {
                    for _ in 0..128 {
                        let Some(event) = terminal.input.read_ready()? else { break; };
                        match event {
                            InputEvent::Key(key) if key.kind != KeyEventKind::Release => {
                                if panel.key(key, &mut service, true) { return Ok(()); }
                                dirty = true;
                            }
                            InputEvent::Paste(text) => { panel.paste(&text); dirty = true; }
                            InputEvent::PasteRejected => {
                                panel.notice = Some(PASTE_REJECTED.into());
                                dirty = true;
                            }
                            InputEvent::Resize(_, _) => dirty = true,
                            _ => {}
                        }
                    }
                }
            }
        }
    }
    .await;
    let stopped = service.shutdown().await.map_err(UiError::from);
    result.and(stopped)
}

fn handle_paste(text: &str, snapshot: &CoreSnapshot, local: &mut LocalState) {
    if local.tasks {
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
                    local.tasks = false;
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
                local.tasks = false;
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
                    local.tasks = false;
                    local.evidence = false;
                    local.request_panel = false;
                    local.notice = None;
                }
                Some(timeline::Locate::Request(reference)) => {
                    local.request_selection = Some(reference);
                    local.request_panel = true;
                    local.request_scroll = 0;
                    local.tasks = false;
                    local.evidence = false;
                    sync_local_requests(local, snapshot);
                    local.notice = None;
                }
                Some(timeline::Locate::ToolSearch(thread)) => {
                    local.timeline.close();
                    local.search.open_tools(thread);
                    local.tasks = false;
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
            let chunks = main_layout(local.viewport, snapshot, local);
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
    if local.tasks {
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
        local.confirm_stop = false;
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
            local.tasks = !local.tasks;
            local.task_scroll = 0;
            local.task_manual_scroll = false;
            local.task_link_cursor = None;
        }
        KeyCode::PageUp if local.tasks => {
            local.task_manual_scroll = true;
            local.task_scroll = local.task_scroll.saturating_sub(8);
        }
        KeyCode::PageDown if local.tasks => {
            local.task_manual_scroll = true;
            local.task_scroll = local.task_scroll.saturating_add(8);
        }
        KeyCode::Home if local.tasks => {
            local.task_manual_scroll = true;
            local.task_scroll = 0;
        }
        KeyCode::End if local.tasks => {
            local.task_manual_scroll = true;
            local.task_scroll = usize::MAX;
        }
        KeyCode::Char('d') if local.tasks && key.modifiers.is_empty() => {
            navigate_workflow_link(snapshot, local, WorkflowLinkKind::Dependency);
        }
        KeyCode::Char('g') if local.tasks && key.modifiers.is_empty() => {
            navigate_workflow_link(snapshot, local, WorkflowLinkKind::Gate);
        }
        KeyCode::Enter if local.tasks && !control => {
            if let Some(task) = selected_task(snapshot, local) {
                if stale_gate_link(snapshot, local, task) {
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
                            local.tasks = false;
                        }
                        ConversationTarget::Child(agent) => {
                            local.notice = None;
                            local.agent_id = Some(agent.info.id.clone());
                            local.conversation_focus = None;
                            local.scroll_from_bottom = 0;
                            local.tasks = false;
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
        KeyCode::F(6) | KeyCode::F(7) | KeyCode::F(8) if local.tasks => {
            if let Some(task) = selected_task(snapshot, local) {
                if stale_gate_link(snapshot, local, task) {
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
            let tasks = project_workflow(snapshot).order;
            if !tasks.is_empty() {
                let index = selected_task(snapshot, local)
                    .and_then(|task| tasks.iter().position(|id| *id == task.id))
                    .unwrap_or(0);
                let next = if key.code == KeyCode::Up {
                    index.saturating_sub(1)
                } else {
                    (index + 1).min(tasks.len() - 1)
                };
                local.task_id = Some(tasks[next]);
                local.task_scroll = 0;
                local.task_manual_scroll = false;
                local.task_link_cursor = None;
            }
        }
        KeyCode::Char('+') | KeyCode::Char('-') if local.tasks && key.modifiers.is_empty() => {
            if let Some(task) = selected_task(snapshot, local) {
                if stale_gate_link(snapshot, local, task) {
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
            local.tasks = false;
            local.evidence = false;
            sync_local_requests(local, snapshot);
        }
        KeyCode::F(3) => {
            local.conversation_focus = None;
            let tree = project_agent_tree(&snapshot.agents, snapshot.thread_id.as_deref());
            let next = local
                .agent_id
                .as_ref()
                .and_then(|id| {
                    tree.iter()
                        .position(|row| snapshot.agents[row.index].info.id.as_str() == id)
                })
                .map_or(0, |index| index + 1);
            local.agent_id = tree
                .get(next)
                .map(|row| snapshot.agents[row.index].info.id.clone());
            local.scroll_from_bottom = 0;
        }
        KeyCode::Esc => {
            if local.request_panel {
                local.request_panel = false;
                return false;
            }
            if local.tasks {
                local.tasks = false;
                local.task_link_cursor = None;
                local.task_scroll = 0;
                local.task_manual_scroll = false;
                return false;
            }
            local.help = false;
            local.tasks = false;
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

fn selected_task<'a>(snapshot: &'a CoreSnapshot, local: &LocalState) -> Option<&'a TaskSnapshot> {
    local
        .task_id
        .and_then(|id| snapshot.scheduler.tasks.iter().find(|task| task.id == id))
        .or_else(|| snapshot.scheduler.tasks.first())
}

/// 工作流行序只根据快照中的显式 parent 建树；依赖与 Gate 目标单独呈现。
fn workflow_order(tasks: &[TaskSnapshot]) -> Vec<TaskId> {
    fn visit(
        id: TaskId,
        children: &BTreeMap<TaskId, Vec<TaskId>>,
        seen: &mut HashSet<TaskId>,
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
    let known = tasks.iter().map(|task| task.id).collect::<HashSet<_>>();
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
    let mut seen = HashSet::new();
    let mut output = Vec::with_capacity(tasks.len());
    for root in roots {
        visit(root, &children, &mut seen, &mut output);
    }
    for task in tasks {
        visit(task.id, &children, &mut seen, &mut output);
    }
    output
}

struct WorkflowProjection {
    order: Vec<TaskId>,
    agents: HashMap<String, AgentTreeRow>,
}

/// 键盘导航和 F4 共用任务序；关联 Agent 的顺序/层级沿用既有 DFS 投影。
fn project_workflow(snapshot: &CoreSnapshot) -> WorkflowProjection {
    let agents = project_agent_tree(&snapshot.agents, snapshot.thread_id.as_deref())
        .into_iter()
        .filter_map(|row| {
            snapshot
                .agents
                .get(row.index)
                .map(|agent| (agent.info.id.clone(), row))
        })
        .collect();
    WorkflowProjection {
        order: workflow_order(&snapshot.scheduler.tasks),
        agents,
    }
}

fn navigate_workflow_link(snapshot: &CoreSnapshot, local: &mut LocalState, kind: WorkflowLinkKind) {
    let Some(current) = selected_task(snapshot, local) else {
        local.notice = Some("No workflow task is selected.".into());
        return;
    };
    let cursor = local.task_link_cursor.filter(|cursor| {
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
        local.notice = Some("The workflow link source is unavailable.".into());
        return;
    };
    let count = match kind {
        WorkflowLinkKind::Dependency => source.dependencies.len(),
        WorkflowLinkKind::Gate => source.wait_targets.len(),
    };
    if count == 0 {
        local.notice = Some(match kind {
            WorkflowLinkKind::Dependency => "Selected task has no dependencies.".into(),
            WorkflowLinkKind::Gate => "Selected task has no captured Gate wait targets.".into(),
        });
        local.task_link_cursor = None;
        return;
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
        local.notice = Some(format!(
            "Workflow link target #{} is unavailable.",
            target.0
        ));
        local.task_link_cursor = None;
        return;
    }
    local.task_id = Some(target);
    local.task_scroll = 0;
    local.task_manual_scroll = false;
    local.task_link_cursor = Some(WorkflowLinkCursor {
        origin,
        origin_attempt: source.attempt,
        target,
        kind,
        index,
        captured_attempt,
    });
    local.notice = None;
}

fn stale_gate_link(snapshot: &CoreSnapshot, local: &LocalState, task: &TaskSnapshot) -> bool {
    let Some(cursor) = local
        .task_link_cursor
        .filter(|cursor| cursor.kind == WorkflowLinkKind::Gate && cursor.target == task.id)
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

enum ConversationTarget<'a> {
    Root,
    Child(&'a AgentSnapshot),
    Unavailable(&'static str),
}

fn workflow_conversation<'a>(
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

fn main_layout(
    area: ratatui::layout::Rect,
    snapshot: &CoreSnapshot,
    local: &LocalState,
) -> std::rc::Rc<[ratatui::layout::Rect]> {
    let request = selected_request(snapshot, local).is_some();
    let waiting = snapshot.gate.as_ref().is_some_and(|gate| gate.pending);
    Layout::default()
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
                if area.height > 16 && (request || snapshot.last_error.is_some() || waiting) {
                    6
                } else {
                    2
                },
            ),
            Constraint::Length(3),
            Constraint::Length(1),
        ])
        .split(area)
}

fn conversation_content_size(area: ratatui::layout::Rect) -> (usize, usize) {
    let border = if area.height < 3 { 0 } else { 2 };
    (
        area.width.saturating_sub(border) as usize,
        area.height.saturating_sub(border) as usize,
    )
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
    let chunks = main_layout(area, snapshot, local);
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
    let usage = usage_status(snapshot);
    let status = format!(
        "{:?} | action:{} attention:{}{} | turns: {} | children: {} | queued: {} | {}{}",
        snapshot.phase,
        actions,
        attention,
        reminders_status,
        snapshot.root_turn_count,
        snapshot.agents.len(),
        snapshot.queued_inputs,
        usage,
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

    if local.search.visible {
        local.search.draw(
            frame,
            ratatui::layout::Rect {
                y: chunks[0].y + chunks[0].height,
                height: area.height.saturating_sub(chunks[0].height),
                ..area
            },
            snapshot,
        );
        return;
    }

    if local.timeline.visible {
        local.timeline.draw(
            frame,
            ratatui::layout::Rect {
                y: chunks[0].y + chunks[0].height,
                height: area.height.saturating_sub(chunks[0].height),
                ..area
            },
            snapshot,
        );
        return;
    }

    if local.skills {
        draw_skills(frame, area, snapshot, local);
        return;
    }

    let conversation_area = if area.width >= 100 && !snapshot.agents.is_empty() {
        let panels = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Length(32), Constraint::Min(1)])
            .split(chunks[1]);
        if area.height <= 16 {
            let (name, status, generation, usage) = selected_agent.map_or_else(
                || {
                    let usage = if snapshot.usage.source == FactSource::ServerConfirmed {
                        snapshot.usage.total_tokens.map_or_else(
                            || "tokens: unavailable".into(),
                            |tokens| format!("tokens:{tokens}"),
                        )
                    } else {
                        "tokens: unavailable".into()
                    };
                    (
                        "root",
                        format!("{:?}", snapshot.phase),
                        "unavailable".to_owned(),
                        usage,
                    )
                },
                |agent| {
                    let status = if agent.awaiting_turn {
                        "starting".to_owned()
                    } else {
                        agent
                            .outcome
                            .as_ref()
                            .map_or_else(|| "running".to_owned(), |outcome| format!("{outcome:?}"))
                    };
                    let name = agent
                        .info
                        .path
                        .as_deref()
                        .or(agent.info.nickname.as_deref())
                        .unwrap_or(&agent.info.id);
                    (
                        name,
                        status,
                        agent.generation.to_string(),
                        agent_usage_brief(agent),
                    )
                },
            );
            frame.render_widget(
                Paragraph::new(vec![
                    Line::from(format!("Agent: {}", truncate_display_label(name, 25))),
                    Line::from(format!("State: {status} / gen {generation}")),
                    Line::from(usage),
                ]),
                panels[0],
            );
            panels[1]
        } else {
            let mut agents = vec![Line::from(if selected_agent.is_none() {
                "> root"
            } else {
                "  root"
            })];
            let mut selected_line = None;
            for row in project_agent_tree(&snapshot.agents, snapshot.thread_id.as_deref()) {
                let agent = &snapshot.agents[row.index];
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
                let tree_prefix = if row.depth == 1 {
                    String::new()
                } else {
                    format!("{}+- ", "  ".repeat(row.depth - 2))
                };
                let parent_label = if row.parent_missing {
                    "[parent unavailable] "
                } else if row.relationship_unknown {
                    "[parent unresolved] "
                } else {
                    ""
                };
                if Some(agent.info.id.as_str())
                    == selected_agent.map(|agent| agent.info.id.as_str())
                {
                    selected_line = Some(agents.len());
                }
                agents.push(Line::from(display_text(&format!(
                    "{} {}{}{}",
                    if Some(agent.info.id.as_str())
                        == selected_agent.map(|agent| agent.info.id.as_str())
                    {
                        ">"
                    } else {
                        " "
                    },
                    tree_prefix,
                    parent_label,
                    name,
                ))));
                if row.depth_truncated {
                    agents.push(Line::from("[depth truncated]"));
                }
                agents.push(Line::from(format!(
                    "    {}{status} / gen {}",
                    "  ".repeat(row.depth.saturating_sub(1)),
                    agent.generation,
                )));
                agents.push(Line::from(format!(
                    "    {}{}",
                    "  ".repeat(row.depth.saturating_sub(1)),
                    agent_usage_brief(agent)
                )));
                if let Some(activity) = focus_activity(snapshot, &agent.info.id) {
                    agents.push(Line::from(format!(
                        "    {}{:?} / quiet {}",
                        "  ".repeat(row.depth.saturating_sub(1)),
                        activity.attention.level,
                        age(activity.silence_ms)
                    )));
                }
            }
            let visible_height = panels[0].height.saturating_sub(2) as usize;
            let scroll = selected_line
                .unwrap_or(0)
                .saturating_sub(visible_height / 3)
                .min(agents.len().saturating_sub(visible_height))
                .min(u16::MAX as usize) as u16;
            frame.render_widget(
                Paragraph::new(agents)
                    .block(
                        Block::default()
                            .borders(Borders::ALL)
                            .title(" Agents · F3 "),
                    )
                    .scroll((scroll, 0)),
                panels[0],
            );
            panels[1]
        }
    } else {
        chunks[1]
    };
    let (width, height) = conversation_content_size(conversation_area);
    let mut transcript = Vec::new();
    let mut focused_row = None;
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
        if let Some(focus) = local
            .conversation_focus
            .as_ref()
            .filter(|focus| focus.matches(message))
        {
            focused_row = Some(transcript.len() + focus.row(width));
        }
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
    let max_scroll = transcript.len().saturating_sub(height);
    let scroll = local.scroll_from_bottom.min(max_scroll);
    let start = focused_row.map_or_else(
        || max_scroll.saturating_sub(scroll),
        |row| row.saturating_sub(height / 3).min(max_scroll),
    );
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
        " {name} · F3 switch{}{} ",
        if snapshot.history_truncated {
            " [older content truncated]"
        } else {
            ""
        },
        if local.conversation_focus.is_some() && focused_row.is_none() {
            " [search content changed/evicted]"
        } else {
            ""
        }
    ));
    let visible: Vec<_> = transcript
        .into_iter()
        .enumerate()
        .skip(start)
        .take(height)
        .map(|(row, text)| {
            if Some(row) == focused_row {
                Line::from(text).style(Style::default().bg(Color::DarkGray).fg(Color::Yellow))
            } else {
                Line::from(text)
            }
        })
        .collect();
    frame.render_widget(
        Paragraph::new(visible).block(if conversation_area.height < 3 {
            Block::default()
        } else {
            Block::default().borders(Borders::ALL).title(title)
        }),
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
        "Ctrl+T evidence timeline | Ctrl+F retained conversation search | Ctrl+K skills inventory | Enter task/answer | Ctrl+S queue/answer (Ctrl+Enter) | Ctrl+O newline (Shift+Enter) | Ctrl+C interrupt root | Ctrl+Q quit | Ctrl+W acknowledge/restore silence reminders for selected agent | F2 request | F3 agent | F4 tasks | F5 pause dispatch | F6 pause task | F7 cancel | F8 retry (may repeat effects) | +/- priority | F9 twice stop workflow | F10 thresholds | F11 evidence | F12 history/export".to_owned()
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
            "Enter send | Ctrl+T timeline | Ctrl+F search | Ctrl+Q quit | F1 help"
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

const SKILLS_PANEL_FULL_HEADER_LINES: u16 = 7;
const SKILLS_PANEL_COMPACT_HEADER_LINES: u16 = 5;

fn skill_refresh_source_label(source: Option<crate::skills::SkillRefreshSource>) -> &'static str {
    match source {
        Some(crate::skills::SkillRefreshSource::Initial) => "Initial",
        Some(crate::skills::SkillRefreshSource::Changed) => "Changed",
        Some(crate::skills::SkillRefreshSource::Manual) => "Manual",
        None => "unavailable",
    }
}

fn skills_panel_rect(area: ratatui::layout::Rect) -> ratatui::layout::Rect {
    let width = area.width.saturating_sub(4).clamp(1, 100);
    let height = area.height.saturating_sub(2).max(1);
    ratatui::layout::Rect {
        x: area.x + area.width.saturating_sub(width) / 2,
        y: area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    }
}

fn skills_panel_entries_capacity(area: ratatui::layout::Rect) -> usize {
    let inner = Block::default()
        .borders(Borders::ALL)
        .inner(skills_panel_rect(area));
    let header = if inner.height < 10 {
        SKILLS_PANEL_COMPACT_HEADER_LINES
    } else {
        SKILLS_PANEL_FULL_HEADER_LINES
    };
    inner.height.saturating_sub(header) as usize
}

fn draw_skills(
    frame: &mut ratatui::Frame<'_>,
    area: ratatui::layout::Rect,
    snapshot: &CoreSnapshot,
    local: &LocalState,
) {
    let rect = skills_panel_rect(area);
    let inner = Block::default().borders(Borders::ALL).inner(rect);
    let compact = inner.height < 10;
    frame.render_widget(Clear, rect);
    let skills = &snapshot.skills;
    let directory = Line::from(format!("Directory: {}", display_text(&snapshot.cwd)));
    let status = Line::from(
        if matches!(
            skills.availability,
            crate::skills::SkillAvailability::Available | crate::skills::SkillAvailability::Partial
        ) {
            format!(
                "Status: {:?} / {:?} | {} skills observed, {} enabled | scan errors: {}{}",
                skills.availability,
                skills.freshness,
                skills.skill_count,
                skills.enabled_count,
                skills.scan_error_count,
                if skills.truncated { " | truncated" } else { "" },
            )
        } else {
            format!(
                "Status: {:?} / {:?} | skill counts unavailable",
                skills.availability, skills.freshness
            )
        },
    );
    let session = Line::from(format!(
        "Session: {:?} · pending requests: {}",
        snapshot.phase,
        snapshot.requests.len()
    ));
    let refresh_source = if compact {
        Line::from(format!(
            "Refresh: {}",
            skill_refresh_source_label(skills.refresh_source)
        ))
    } else {
        Line::from(format!(
            "Refresh source: {}",
            skill_refresh_source_label(skills.refresh_source)
        ))
    };
    let source = if matches!(
        skills.availability,
        crate::skills::SkillAvailability::Available | crate::skills::SkillAvailability::Partial
    ) {
        Line::from("Source: server-confirmed skills/list directory scan")
    } else {
        Line::from(format!(
            "Inventory source unavailable: {:?}",
            skills.availability
        ))
    };
    let mut lines = if compact {
        let compact_status = Line::from(format!(
            "{:?} / {:?}",
            skills.availability, skills.freshness
        ));
        let compact_source = if matches!(
            skills.availability,
            crate::skills::SkillAvailability::Available | crate::skills::SkillAvailability::Partial
        ) {
            Line::from("Source: AppServer")
        } else {
            Line::from(format!("Unavailable: {:?}", skills.availability))
        };
        vec![
            directory,
            compact_status,
            compact_source,
            refresh_source,
            Line::from(format!(
                "{:?} req {} · Enter/F2",
                snapshot.phase,
                snapshot.requests.len()
            )),
        ]
    } else {
        vec![
            directory,
            status,
            session,
            source,
            refresh_source,
            Line::from("Listed entries do not confirm loaded, invoked, completed, or failed."),
            Line::from("Enter refresh · Esc/Ctrl+K close · ↑/↓ scroll · F2 requests"),
        ]
    };
    let visible = inner.height.saturating_sub(lines.len() as u16) as usize;
    let start = local
        .skills_scroll
        .min(skills.entries.len().saturating_sub(visible));
    lines.extend(
        skills
            .entries
            .iter()
            .skip(start)
            .take(visible)
            .map(|entry| {
                Line::from(display_text(&format!(
                    "{} [{}] {} · {}",
                    if entry.enabled { "on" } else { "off" },
                    entry.scope,
                    entry.name,
                    entry.path,
                )))
            }),
    );
    frame.render_widget(
        Paragraph::new(lines).block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Skills inventory · Ctrl+K "),
        ),
        rect,
    );
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
    let diagnostics = &snapshot.diagnostics;
    rows.push(format!(
        "Transport bytes in/out {} / {} | control events {} | telemetry events {}",
        diagnostics.transport_bytes_in,
        diagnostics.transport_bytes_out,
        diagnostics.control_events,
        diagnostics.telemetry_events
    ));
    rows.push(format_usage_evidence(snapshot));
    rows.push(format_token_budget_evidence(snapshot));
    if let Some(journal) = &snapshot.journal {
        rows.push(format!(
            "Session {} / committed {} / submitted {} / persistence {:?}",
            journal.session_id, journal.committed_seq, journal.submitted_seq, snapshot.persistence
        ));
    }
    let compactions = snapshot
        .observation
        .activities
        .iter()
        .filter(|activity| {
            activity.identity.agent_id == agent_id
                && activity.tool_category == Some(crate::protocol::ToolCategory::Compaction)
        })
        .count();
    rows.push(format!(
        "Compactions retained: {compactions} | lifetime total unavailable"
    ));
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
        if activity.tool_category == Some(crate::protocol::ToolCategory::Compaction) {
            rows.push(format!(
                "Compaction source {:?}",
                activity
                    .last_evidence
                    .as_ref()
                    .map(|evidence| evidence.source)
            ));
            rows.extend(compaction_fact_rows(compaction_fact_for_activity(
                snapshot, activity,
            )));
        }
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

fn compaction_fact_for_activity<'a>(
    snapshot: &'a CoreSnapshot,
    activity: &ActivitySnapshot,
) -> Option<&'a CompactionFact> {
    let thread_id = activity.identity.thread_id.as_ref()?;
    let turn_id = activity.identity.turn_id.as_ref()?;
    let item_id = activity.item_id.as_ref()?;
    snapshot.observation.compactions.iter().find(|fact| {
        fact.thread_id == *thread_id && fact.turn_id == *turn_id && fact.item_id == *item_id
    })
}

pub(super) fn compaction_fact_rows(fact: Option<&CompactionFact>) -> Vec<String> {
    let status = fact.map_or("unavailable", |fact| match fact.status {
        CompactionFactStatus::Started => "Started",
        CompactionFactStatus::Completed => "Completed",
        CompactionFactStatus::Unknown => "Unknown",
    });
    vec![
        format!("Compaction status: {status}"),
        format!(
            "Compaction usage: input {} | cached input {} | output {} | total {} | context window {}",
            usage_value(fact.and_then(|fact| fact.input_tokens)),
            usage_value(fact.and_then(|fact| fact.cached_input_tokens)),
            usage_value(fact.and_then(|fact| fact.output_tokens)),
            usage_value(fact.and_then(|fact| fact.total_tokens)),
            usage_value(fact.and_then(|fact| fact.context_window)),
        ),
    ]
}

fn usage_status(snapshot: &CoreSnapshot) -> String {
    let usage = snapshot.usage;
    let base = token_budget_brief(snapshot);
    if usage.input_tokens.is_none()
        && usage.cached_input_tokens.is_none()
        && usage.output_tokens.is_none()
        && usage.reasoning_tokens.is_none()
    {
        return base;
    }
    format!(
        "{base} in:{} cached:{} out:{} reasoning:{}",
        usage_value(usage.input_tokens),
        usage_value(usage.cached_input_tokens),
        usage_value(usage.output_tokens),
        usage_value(usage.reasoning_tokens)
    )
}

fn token_budget_brief(snapshot: &CoreSnapshot) -> String {
    let total = snapshot.token_budget.confirmed_total_tokens.map_or_else(
        || "unavailable".to_owned(),
        |total| {
            if snapshot.token_budget.confirmed_complete {
                total.to_string()
            } else {
                format!("partial {total}")
            }
        },
    );
    format!(
        "tokens:{}/{}",
        total,
        usage_value(snapshot.token_budget.limit)
    )
}

fn format_token_budget_evidence(snapshot: &CoreSnapshot) -> String {
    let budget = snapshot.token_budget;
    format!(
        "Session token budget: {} | stop triggered: {} | Per-agent token budget: {}",
        token_budget_brief(snapshot),
        if budget.stop_triggered { "yes" } else { "no" },
        per_agent_budget_brief(budget)
    )
}

fn per_agent_budget_brief(budget: crate::state::TokenBudgetSnapshot) -> String {
    let Some(limit) = budget.per_agent_limit else {
        return "not set".to_owned();
    };
    format!(
        "limit {limit} | stop triggered: {}",
        if budget.per_agent_stop_triggered {
            "yes"
        } else {
            "no"
        }
    )
}

fn agent_usage_brief(agent: &crate::agents::AgentSnapshot) -> String {
    match (agent.usage.source, agent.usage.total_tokens) {
        (FactSource::ServerConfirmed, Some(tokens)) => format!("tokens:{tokens}"),
        _ => "tokens: unavailable".to_owned(),
    }
}

fn truncate_display_label(text: &str, max_width: usize) -> String {
    let text = display_text(text);
    if UnicodeWidthStr::width(text.as_str()) <= max_width {
        return text;
    }
    if max_width == 0 {
        return String::new();
    }
    let mut output = String::new();
    let mut width = 0;
    for grapheme in text.graphemes(true) {
        let grapheme_width = UnicodeWidthStr::width(grapheme);
        if width + grapheme_width + 1 > max_width {
            break;
        }
        output.push_str(grapheme);
        width += grapheme_width;
    }
    output.push('…');
    output
}

fn format_usage_evidence(snapshot: &CoreSnapshot) -> String {
    let usage = snapshot.usage;
    let total = usage.total_tokens;
    format!(
        "Usage source: {} | total {} | input {} | cached {} | output {} | reasoning {} | context window {}",
        source_label(usage.source),
        usage_value(total),
        usage_value(usage.input_tokens),
        usage_value(usage.cached_input_tokens),
        usage_value(usage.output_tokens),
        usage_value(usage.reasoning_tokens),
        usage_value(usage.context_window),
    )
}

fn usage_value(value: Option<u64>) -> String {
    value.map_or_else(|| "unavailable".to_owned(), |value| value.to_string())
}

fn source_label(source: FactSource) -> &'static str {
    match source {
        FactSource::ServerConfirmed => "server confirmed",
        FactSource::LocalEstimate => "local estimate",
        FactSource::Unknown => "unknown",
    }
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
    let projection = project_workflow(snapshot);
    let width = area.width.saturating_sub(2).max(1) as usize;
    let viewport = area.height.saturating_sub(2) as usize;
    let by_id = scheduler
        .tasks
        .iter()
        .map(|task| (task.id, task))
        .collect::<BTreeMap<_, _>>();
    let ready = if scheduler.ready_roots.is_empty() {
        "none".into()
    } else {
        scheduler
            .ready_roots
            .iter()
            .map(|id| format!("#{}", id.0))
            .collect::<Vec<_>>()
            .join(",")
    };
    let mut raw = vec![format!(
        "Workflow: {}{}{} | ready roots: {ready}",
        if scheduler.paused {
            "paused"
        } else {
            "dispatch active"
        },
        if scheduler.stopping {
            " · stopping"
        } else {
            ""
        },
        if scheduler.disconnected {
            " · disconnected"
        } else {
            ""
        },
    )];
    let native_reserved = scheduler
        .native_slot_capacity
        .map(|capacity| format!("{}/{}", scheduler.native_slots_reserved, capacity))
        .unwrap_or_else(|| format!("{}/unavailable", scheduler.native_slots_reserved));
    let summaries = [
        vec![format!(
            "Root slots: {} | native reserved: {} | native turns observed: {}",
            scheduler.root_slots_reserved, native_reserved, scheduler.native_turns_observed
        )],
        vec![
            format!(
                "Root slots: {} | native reserved: {}",
                scheduler.root_slots_reserved, native_reserved
            ),
            format!("Native turns observed: {}", scheduler.native_turns_observed),
        ],
        vec![
            format!("native reserved: {native_reserved}"),
            format!("native observed: {}", scheduler.native_turns_observed),
        ],
        vec![
            format!("N reserved: {native_reserved}"),
            format!("N observed: {}", scheduler.native_turns_observed),
        ],
    ];
    raw.extend(
        summaries
            .into_iter()
            .find(|candidate| candidate.iter().all(|line| line.width() <= width))
            .unwrap_or_else(|| {
                vec![
                    format!("N:{native_reserved}"),
                    format!("O:{}", scheduler.native_turns_observed),
                ]
            }),
    );
    let mut selected_visual = None;
    for id in &projection.order {
        let Some(task) = by_id.get(id).copied() else {
            continue;
        };
        let selected_row = selected.is_some_and(|selected| selected.id == task.id);
        let marker = if selected_row { ">" } else { " " };
        let tree = if task.parent.is_some() {
            "  └─ "
        } else {
            ""
        };
        let slot = match task.kind {
            crate::scheduler::TaskKind::NativeChild if task.native_slot_reserved => {
                " · slot reserved"
            }
            crate::scheduler::TaskKind::NativeChild => " · slot free",
            crate::scheduler::TaskKind::RootTurn => "",
        };
        let flags = format!(
            "{}{}",
            if task.pause_requested {
                " · pause requested"
            } else {
                ""
            },
            if task.cancel_requested {
                " · cancel requested"
            } else {
                ""
            }
        );
        let text = format!(
            "{marker}{tree}#{} {:?} · attempt {} · priority {}{slot}{flags} · {}",
            task.id.0, task.state, task.attempt, task.priority, task.title
        );
        if selected_row {
            selected_visual = Some(
                raw.iter()
                    .map(|line| wrap(line, width).len())
                    .sum::<usize>(),
            );
        }
        raw.push(text);
    }
    if let Some(task) = selected {
        raw.push(format!(
            "Task #{}: {} · {:?} · attempt {} · blocked: {:?} · requests: {}",
            task.id.0,
            task.title,
            task.state,
            task.attempt,
            task.blocked_reason,
            task.pending_requests
        ));
        let parent = task.parent.map_or_else(
            || match task.kind {
                crate::scheduler::TaskKind::RootTurn => "none".into(),
                crate::scheduler::TaskKind::NativeChild => "unconfirmed / unavailable".into(),
            },
            |id| task_reference(id, &by_id),
        );
        raw.push(format!(
            "Parent: {parent} | dependency policy: {:?} | failure policy: {:?}",
            task.policy, task.failure
        ));
        if task.dependencies.is_empty() {
            raw.push("Dependencies: none".into());
        } else {
            raw.push(format!("Dependencies ({:?}):", task.policy));
            for id in &task.dependencies {
                raw.push(format!("  {}", task_reference(*id, &by_id)));
            }
        }
        if task.wait_targets.is_empty() {
            raw.push("Gate wait targets: none".into());
        } else {
            raw.push("Gate wait targets (captured attempts):".into());
            for target in &task.wait_targets {
                let current = by_id.get(&target.task).map_or_else(
                    || format!("#{} unavailable", target.task.0),
                    |linked| {
                        format!(
                            "{} · current attempt {} · {:?}",
                            task_reference(target.task, &by_id),
                            linked.attempt,
                            linked.state
                        )
                    },
                );
                raw.push(format!(
                    "  captured #{} attempt {} → {current}",
                    target.task.0, target.attempt
                ));
            }
        }
        raw.push(format!(
            "Conversation: {}",
            workflow_agent_label(snapshot, task, &projection.agents)
        ));
        if let Some(cursor) = local
            .task_link_cursor
            .filter(|cursor| cursor.target == task.id)
        {
            if let Some(captured) = cursor.captured_attempt {
                raw.push(format!(
                    "Selected Gate link captured attempt {captured}; current task attempt is {}.",
                    task.attempt
                ));
            }
        }
    } else {
        raw.push("No workflow tasks.".into());
    }
    let lines = raw
        .iter()
        .flat_map(|text| wrap(text, width).into_iter().map(Line::from))
        .collect::<Vec<_>>();
    let max_scroll = lines.len().saturating_sub(viewport);
    let mut scroll = local.task_scroll.min(max_scroll);
    if !local.task_manual_scroll {
        if let Some(selected_row) = selected_visual {
            if selected_row < scroll {
                scroll = selected_row;
            } else if selected_row >= scroll.saturating_add(viewport) && viewport > 0 {
                scroll = selected_row + 1 - viewport;
            }
        }
    }
    let title =
        " Workflow · ↑/↓ select · d dependencies · g Gate · Enter conversation · PgUp/PgDn scroll ";
    frame.render_widget(
        Paragraph::new(lines)
            .scroll((scroll.min(u16::MAX as usize) as u16, 0))
            .block(Block::default().borders(Borders::ALL).title(title)),
        area,
    );
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct AgentTreeRow {
    index: usize,
    depth: usize,
    parent_missing: bool,
    relationship_unknown: bool,
    depth_truncated: bool,
}

/// 把快照投影为稳定的父子 DFS 顺序；渲染层不重新推断父链。
fn project_agent_tree(agents: &[AgentSnapshot], root_id: Option<&str>) -> Vec<AgentTreeRow> {
    let root_id = root_id.unwrap_or("root");
    let known_ids = agents
        .iter()
        .map(|agent| agent.info.id.as_str())
        .collect::<HashSet<_>>();
    let mut children = BTreeMap::<&str, Vec<usize>>::new();
    for (index, agent) in agents.iter().enumerate() {
        children
            .entry(agent.info.parent_id.as_str())
            .or_default()
            .push(index);
    }
    let mut seeds = agents
        .iter()
        .enumerate()
        .filter(|(_, agent)| {
            agent.info.parent_id == root_id || !known_ids.contains(agent.info.parent_id.as_str())
        })
        .map(|(index, agent)| {
            let parent_missing = agent.info.parent_id != root_id;
            (index, parent_missing, false)
        })
        .collect::<Vec<_>>();
    // 非 root 连通分量可能是循环；按快照顺序作兜底根，并标记关系未确认。
    let mut is_seed = vec![false; agents.len()];
    for (index, _, _) in &seeds {
        is_seed[*index] = true;
    }
    for (index, _) in agents.iter().enumerate() {
        if !is_seed[index] {
            seeds.push((index, false, true));
        }
    }
    let mut stack = seeds
        .into_iter()
        .rev()
        .map(|(index, parent_missing, relationship_unknown)| {
            (index, 1, parent_missing, relationship_unknown, false)
        })
        .collect::<Vec<_>>();
    let mut rows = Vec::with_capacity(agents.len());
    let mut visited = vec![false; agents.len()];
    while let Some((index, actual_depth, parent_missing, relationship_unknown, depth_truncated)) =
        stack.pop()
    {
        if visited[index] {
            continue;
        }
        visited[index] = true;
        rows.push(AgentTreeRow {
            index,
            depth: actual_depth.min(8),
            parent_missing,
            relationship_unknown: relationship_unknown || !agents[index].info.confirmed,
            depth_truncated,
        });
        if let Some(siblings) = children.get(agents[index].info.id.as_str()) {
            for child in siblings.iter().rev() {
                let child_depth = actual_depth.saturating_add(1);
                stack.push((
                    *child,
                    child_depth,
                    false,
                    relationship_unknown,
                    depth_truncated || child_depth > 8,
                ));
            }
        }
    }
    rows
}

fn task_reference(id: TaskId, tasks: &BTreeMap<TaskId, &TaskSnapshot>) -> String {
    tasks.get(&id).map_or_else(
        || format!("#{} unavailable", id.0),
        |task| format!("#{} {} [{:?}]", id.0, task.title, task.state),
    )
}

fn workflow_agent_label(
    snapshot: &CoreSnapshot,
    task: &TaskSnapshot,
    agent_rows: &HashMap<String, AgentTreeRow>,
) -> String {
    match workflow_conversation(snapshot, task) {
        ConversationTarget::Root => format!(
            "root thread {} · current task attempt {}",
            snapshot.thread_id.as_deref().unwrap_or("unknown"),
            task.attempt
        ),
        ConversationTarget::Child(agent) => {
            let depth = agent_rows
                .get(&agent.info.id)
                .map_or("unknown".to_string(), |row| row.depth.to_string());
            format!(
                "agent {} · Agent tree depth {} · generation {} · {:?}",
                agent.info.id, depth, agent.generation, agent.outcome
            )
        }
        ConversationTarget::Unavailable(message) => message.into(),
    }
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
    input: TerminalInput,
}

impl TerminalGuard {
    fn enter() -> Result<Self, UiError> {
        enable_raw_mode()?;
        let mut input = match TerminalInput::enter() {
            Ok(input) => input,
            Err(error) => {
                let _ = disable_raw_mode();
                return Err(error.into());
            }
        };
        let mut output = stdout();
        if let Err(error) = execute!(output, EnterAlternateScreen, event::EnableBracketedPaste) {
            let _ = input.restore();
            let _ = disable_raw_mode();
            let _ = execute!(output, LeaveAlternateScreen, event::DisableBracketedPaste);
            return Err(error.into());
        }
        match Terminal::new(CrosstermBackend::new(output)) {
            Ok(terminal) => Ok(Self { terminal, input }),
            Err(error) => {
                let _ = input.restore();
                let _ = disable_raw_mode();
                let _ = execute!(stdout(), LeaveAlternateScreen, event::DisableBracketedPaste);
                Err(error.into())
            }
        }
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = self.input.restore();
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
        observed_snapshot_with_requests(&[])
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
            tasks: true,
            ..Default::default()
        };
        let render = |width, height| {
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            terminal
                .draw(|frame| draw_tasks(frame, frame.area(), &snapshot, &local))
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
            .draw(|frame| draw_tasks(frame, frame.area(), &unavailable, &local))
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
        local.tasks = true;
        handle_key(
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            &snapshot,
            &mut local,
            &tx,
        );
        assert!(!local.tasks);
        assert_eq!(local.agent_id.as_deref(), Some("child-thread"));
        assert_eq!(local.editor.text, "unsent draft");
        assert!(rx.try_recv().is_err());

        let mut stale = snapshot.clone();
        stale.agents[0].turn_id = Some("newer-turn".into());
        local.tasks = true;
        local.agent_id = None;
        local.notice = None;
        handle_key(
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            &stale,
            &mut local,
            &tx,
        );
        assert!(local.tasks);
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
        assert!(local.tasks);
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
        assert!(local.tasks);
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
            tasks: true,
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
        assert!(!local.tasks);
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
        assert!(!local.tasks);
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

        local.tasks = true;
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
                tasks: true,
                task_id: Some(TaskId(2)),
                task_link_cursor: Some(stale_cursor),
                ..Default::default()
            };
            handle_key(key, &snapshot, &mut local, &tx);
            assert!(local.tasks, "stale Gate action {:?} left F4", key.code);
            assert_eq!(local.task_id, Some(TaskId(2)));
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
            tasks: true,
            task_id: Some(TaskId(2)),
            task_link_cursor: Some(stale_cursor),
            ..Default::default()
        };
        handle_key(
            KeyEvent::new(KeyCode::Up, KeyModifiers::NONE),
            &snapshot,
            &mut local,
            &tx,
        );
        assert_eq!(local.task_id, Some(TaskId(1)));
        assert!(local.task_link_cursor.is_none());
        handle_key(
            KeyEvent::new(KeyCode::Down, KeyModifiers::NONE),
            &snapshot,
            &mut local,
            &tx,
        );
        assert_eq!(local.task_id, Some(TaskId(2)));
        assert!(local.task_link_cursor.is_none());
        handle_key(
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            &snapshot,
            &mut local,
            &tx,
        );
        assert!(!local.tasks);
        assert_eq!(local.agent_id.as_deref(), Some("child-thread"));
        assert!(rx.try_recv().is_err());

        // A cursor from an earlier source attempt is also invalid even if its
        // captured target attempt happens to match the current task attempt.
        let mut source_stale = LocalState {
            tasks: true,
            task_id: Some(TaskId(2)),
            task_link_cursor: Some(WorkflowLinkCursor {
                origin_attempt: 1,
                captured_attempt: Some(2),
                ..stale_cursor
            }),
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
        assert_eq!(source_stale.task_id, Some(TaskId(2)));
        assert!(source_stale.task_link_cursor.is_none());
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
            tasks: true,
            task_id: Some(TaskId(3)),
            ..Default::default()
        };
        handle_key(
            KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE),
            &snapshot,
            &mut local,
            &tx,
        );
        assert_eq!(local.task_id, Some(TaskId(1)));
        handle_key(
            KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE),
            &snapshot,
            &mut local,
            &tx,
        );
        assert_eq!(local.task_id, Some(TaskId(2)));

        local.task_id = Some(TaskId(3));
        local.task_link_cursor = None;
        handle_key(
            KeyEvent::new(KeyCode::Char('g'), KeyModifiers::NONE),
            &snapshot,
            &mut local,
            &tx,
        );
        assert_eq!(local.task_id, Some(TaskId(2)));
        assert_eq!(local.task_link_cursor.unwrap().captured_attempt, Some(1));
        let mut terminal = Terminal::new(TestBackend::new(100, 24)).unwrap();
        terminal
            .draw(|frame| draw_tasks(frame, frame.area(), &snapshot, &local))
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

        local.task_id = Some(TaskId(1));
        local.task_link_cursor = None;
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
            tasks: true,
            task_manual_scroll: true,
            task_scroll: usize::MAX,
            ..Default::default()
        };
        let mut terminal = Terminal::new(TestBackend::new(48, 8)).unwrap();
        terminal
            .draw(|frame| draw_tasks(frame, frame.area(), &snapshot, &local))
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
            tasks: true,
            task_id: Some(TaskId(1)),
            task_manual_scroll: true,
            task_scroll: 0,
            ..Default::default()
        };
        let mut parent_terminal = Terminal::new(TestBackend::new(48, 24)).unwrap();
        parent_terminal
            .draw(|frame| draw_tasks(frame, frame.area(), &snapshot, &parent_local))
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
            .draw(|frame| draw_tasks(frame, frame.area(), &child_snapshot, &parent_local))
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

        local.task_scroll = 0;
        local.task_manual_scroll = true;
        handle_key(
            KeyEvent::new(KeyCode::End, KeyModifiers::NONE),
            &snapshot,
            &mut local,
            &tokio::sync::mpsc::channel(1).0,
        );
        assert_eq!(local.task_scroll, usize::MAX);
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
            project_agent_tree(agents, Some("root"))
                .into_iter()
                .map(|row| agents[row.index].info.id.clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(ids(&first), ["orphan", "b", "c", "a"]);
        let projected = project_agent_tree(&first, Some("root"));
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
        let uuid_projection = project_agent_tree(&uuid_agents, Some(root_uuid));
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
        let tentative_projection = project_agent_tree(&tentative, Some("root"));
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
        let cycle_projection = project_agent_tree(&cycle, Some("root"));
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
        let deep_projection = project_agent_tree(&deep, Some("root"));
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
