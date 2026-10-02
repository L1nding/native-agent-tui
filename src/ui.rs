use std::collections::BTreeMap;
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
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Terminal;
use thiserror::Error;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use crate::client::{ClientHandle, Command};
use crate::interactions::{ApprovalDecision, RequestKind, RequestView};
use crate::protocol::RpcId;
use crate::scheduler::{
    RootTaskSpec, SchedulerCommand, TaskAttempt, TaskId, TaskSnapshot, ROOT_QUEUE_LIMIT,
};
use crate::state::{display_text, CoreSnapshot, MESSAGE_BYTES};

#[derive(Debug, Error)]
pub enum UiError {
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
    editor: Editor,
    task_draft: Option<Editor>,
    scroll_from_bottom: usize,
    notice: Option<String>,
    help: bool,
    tasks: bool,
    task_id: Option<TaskId>,
    confirm_stop: bool,
    request_index: usize,
    agent_id: Option<String>,
    answering: Option<RpcId>,
    question_index: usize,
    answers: BTreeMap<String, Vec<String>>,
}

pub async fn run(client: ClientHandle, goal: Option<String>) -> Result<(), UiError> {
    run_tasks(client, goal.into_iter().map(RootTaskSpec::input).collect()).await
}

pub async fn run_tasks(mut client: ClientHandle, tasks: Vec<RootTaskSpec>) -> Result<(), UiError> {
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
                let request = selected_request(&snapshot, &local).cloned();
                sync_questions(&mut local, request);
                terminal.terminal.draw(|frame| draw(frame, &snapshot, &local))?;
                dirty = false;
            }
            tokio::select! {
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
                                if handle_key(key, &snapshot, &mut local, &client.commands) { return Ok(()); }
                                dirty = true;
                            }
                            Event::Paste(text) => {
                                let snapshot = client.snapshots.borrow().clone();
                                let request = selected_request(&snapshot, &local).cloned();
                                sync_questions(&mut local, request);
                                local.editor.insert(&text); dirty = true;
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
    if let Some(error) = report.cleanup_error {
        return Err(UiError::Shutdown(error));
    }
    result
}

fn selected_request<'a>(snapshot: &'a CoreSnapshot, local: &LocalState) -> Option<&'a RequestView> {
    let requests: Vec<_> = snapshot
        .requests
        .iter()
        .filter(|request| !request.responding)
        .collect();
    requests
        .get(local.request_index % requests.len().max(1))
        .copied()
}

fn sync_questions(local: &mut LocalState, request: Option<RequestView>) {
    let id = request
        .filter(|r| matches!(r.kind, RequestKind::UserInput { .. }))
        .map(|r| r.id);
    if local.answering != id {
        match (local.answering.is_some(), id.is_some()) {
            (false, true) => local.task_draft = Some(std::mem::take(&mut local.editor)),
            (true, false) => local.editor = local.task_draft.take().unwrap_or_default(),
            (true, true) => local.editor.clear(),
            (false, false) => {}
        }
        local.answering = id;
        local.answers.clear();
        local.question_index = 0;
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
    sync_questions(local, selected_request(snapshot, local).cloned());
    let control = key.modifiers.contains(KeyModifiers::CONTROL);
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
            KeyCode::Char('y') | KeyCode::Char('n') => {
                if let Some(request) = selected_request(snapshot, local) {
                    send(
                        Command::AnswerApproval {
                            request_id: request.id.clone(),
                            decision: if key.code == KeyCode::Char('y') {
                                ApprovalDecision::Accept
                            } else {
                                ApprovalDecision::Decline
                            },
                        },
                        tx,
                        local,
                    );
                }
                return false;
            }
            _ => {}
        }
    }
    match key.code {
        KeyCode::F(4) => local.tasks = !local.tasks,
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
            local.request_index += 1;
            sync_questions(local, selected_request(snapshot, local).cloned());
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
            local.help = false;
            local.tasks = false;
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
                    local
                        .answers
                        .insert(questions[local.question_index].id.clone(), vec![text]);
                    if local.question_index + 1 < questions.len() {
                        local.question_index += 1;
                        local.editor.clear();
                    } else if send(
                        Command::AnswerUserInput {
                            request_id: request.id.clone(),
                            answers: local.answers.clone(),
                        },
                        tx,
                        local,
                    ) {
                        local.editor.clear();
                        local.answers.clear();
                    }
                    return false;
                }
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
            Constraint::Length(if area.height > 16 { 4 } else { 3 }),
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
    let status = format!(
        "{:?} | turns: {} | children: {} | queued: {}{}",
        snapshot.phase,
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
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(status),
            Line::from(display_text(&settings)),
        ])
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(format!(" Native Agent TUI {} ", env!("CARGO_PKG_VERSION"))),
        ),
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

    let notice = local
        .notice
        .as_ref()
        .or(snapshot.notice.as_ref())
        .or(snapshot.last_error.as_ref());
    let activity = if local.help {
        "Enter root task | Ctrl+Enter queue | Shift+Enter newline | Ctrl+C interrupt root | Ctrl+Q quit | F2 request | F3 agent | F4 tasks | F5 pause dispatch | F6 pause task | F7 cancel | F8 retry (may repeat effects) | +/- priority | F9 twice stop workflow".to_owned()
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
                "{} · {}\nCtrl+Y approve once | Ctrl+N decline | F2 next request",
                request.thread_id, request.summary
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
        } else {
            "Enter send  Ctrl+Enter queue  Ctrl+C interrupt  Ctrl+Q quit  F3 agent  F4 tasks"
        }),
        chunks[4],
    );
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
