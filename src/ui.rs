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
    request_index: usize,
    answering: Option<RpcId>,
    question_index: usize,
    answers: BTreeMap<String, Vec<String>>,
}

pub async fn run(mut client: ClientHandle, goal: Option<String>) -> Result<(), UiError> {
    let result = async {
        let mut terminal = TerminalGuard::enter()?;
        let mut local = LocalState::default();
        if let Some(text) = goal {
            client.commands.send(Command::SubmitRootInput { text }).await
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
        KeyCode::F(1) => local.help = !local.help,
        KeyCode::F(2) => {
            local.request_index += 1;
            sync_questions(local, selected_request(snapshot, local).cloned());
        }
        KeyCode::Esc => {
            local.help = false;
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
            if !snapshot.phase.can_submit() || snapshot.thread_id.is_none() {
                local.notice =
                    Some("Wait for the current turn, or use Ctrl+C to interrupt.".into());
            } else if send(Command::SubmitRootInput { text }, tx, local) {
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
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(if area.height > 16 { 4 } else { 3 }),
            Constraint::Min(1),
            Constraint::Length(if request.is_some() || snapshot.last_error.is_some() {
                6
            } else {
                2
            }),
            Constraint::Length(3),
            Constraint::Length(1),
        ])
        .split(area);
    let status = format!(
        "{:?} | {} | turns: {} | tokens: {}",
        snapshot.phase,
        snapshot.model.as_deref().unwrap_or("model pending"),
        snapshot.root_turn_count,
        snapshot
            .total_tokens
            .map_or_else(|| "unknown".into(), |tokens| tokens.to_string())
    );
    let settings = format!(
        "{} | {} | {}",
        snapshot.cwd, snapshot.sandbox, snapshot.approval_policy
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

    let width = chunks[1].width.saturating_sub(2) as usize;
    let mut transcript = Vec::new();
    if snapshot.messages.is_empty() {
        transcript.push("Type a task below and press Enter.".into());
    }
    for message in &snapshot.messages {
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
    if transcript.last().is_some_and(|line| line.is_empty()) {
        transcript.pop();
    }
    let height = chunks[1].height.saturating_sub(2) as usize;
    let max_scroll = transcript.len().saturating_sub(height);
    let scroll = local.scroll_from_bottom.min(max_scroll);
    let start = max_scroll.saturating_sub(scroll);
    let title = if snapshot.history_truncated {
        " Conversation [older content truncated] "
    } else {
        " Conversation "
    };
    let visible: Vec<_> = transcript
        .into_iter()
        .skip(start)
        .take(height)
        .map(Line::from)
        .collect();
    frame.render_widget(
        Paragraph::new(visible).block(Block::default().borders(Borders::ALL).title(title)),
        chunks[1],
    );

    let notice = local
        .notice
        .as_ref()
        .or(snapshot.last_error.as_ref())
        .or(snapshot.notice.as_ref());
    let activity = if local.help {
        "Enter submit | Shift+Enter newline | Ctrl+C interrupt | Ctrl+Q quit | PgUp/PgDn scroll | Ctrl+U clear | F2 next request".to_owned()
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
                    "{} ({}/{})\n{}\n{}",
                    question.header,
                    local.question_index + 1,
                    questions.len(),
                    question.question,
                    options
                )
            }
            _ => format!(
                "{}\nCtrl+Y approve once | Ctrl+N decline | F2 next request",
                request.summary
            ),
        }
    } else if let Some(notice) = notice {
        notice.clone()
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
                " Task "
            },
        )),
        chunks[3],
    );
    frame.set_cursor_position((chunks[3].x + 1 + cursor as u16, chunks[3].y + 1));
    frame.render_widget(
        Paragraph::new("Enter send  Ctrl+C interrupt  Ctrl+Q quit  PgUp/PgDn scroll  F1 help"),
        chunks[4],
    );
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
