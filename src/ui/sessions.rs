//! Sessions page: this workspace's saved conversations, with the ones that
//! wait for an answer first. Opened with Left from an empty task input;
//! Enter resumes the selected conversation in this session.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use unicode_width::UnicodeWidthStr;

use super::activity::{clip_spans, hint_row, selected_line, tone, truncate_display_label};
use super::lang::{self, tr};
use super::theme;
use crate::history::{HistoryHandle, HistoryRequest, HistoryResult, SessionOverview};
use crate::state::{CoreSnapshot, SessionPhase};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Group {
    NeedsInput,
    Conversations,
}

impl Group {
    fn title(self) -> &'static str {
        match self {
            Self::NeedsInput => tr("Waiting for input", "等待输入"),
            Self::Conversations => tr("Conversations", "对话"),
        }
    }
}

struct Row {
    group: Group,
    id: String,
    title: String,
    live: bool,
    /// What is known about how the session that ran it ended.
    state: String,
    updated_at_ms: u64,
    turns: usize,
}

#[derive(Default)]
pub(super) struct SessionsPanel {
    pub visible: bool,
    pending: Option<u64>,
    /// Execution status per session, from the journal; titles come from Core.
    sessions: Vec<SessionOverview>,
    selected: usize,
    pub notice: Option<String>,
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum SessionsEvent {
    Close,
    /// Continue this saved conversation in the running session.
    Resume(String),
    /// Ask Core to read the saved conversations again.
    Refresh,
    Quit,
    Consumed,
}

fn now_ms() -> Option<u64> {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|elapsed| elapsed.as_millis().try_into().ok())
}

/// How long ago, in the largest fitting unit.
fn elapsed(then: u64, now: Option<u64>) -> String {
    let Some(seconds) = now.map(|now| now.saturating_sub(then) / 1000) else {
        return tr("time unknown", "时间未知").into();
    };
    let (value, unit) = match seconds {
        0..=59 => (seconds, tr("s", "秒")),
        60..=3599 => (seconds / 60, tr("m", "分钟")),
        3600..=86399 => (seconds / 3600, tr("h", "小时")),
        _ => (seconds / 86400, tr("d", "天")),
    };
    if lang::zh() {
        format!("{value} {unit}前")
    } else {
        format!("{value}{unit} ago")
    }
}

impl SessionsPanel {
    /// The caller also asks Core for the conversation list.
    pub fn open(&mut self, service: &mut HistoryHandle) {
        self.visible = true;
        self.selected = 0;
        self.refresh(service);
    }

    fn refresh(&mut self, service: &mut HistoryHandle) {
        if self.pending.is_some() {
            return;
        }
        match service.request(HistoryRequest::Overview) {
            Ok(id) => {
                self.pending = Some(id);
                self.notice = None;
            }
            Err(error) => self.notice = Some(error.to_string()),
        }
    }

    /// Takes the overview this page asked for; other results belong to the history view.
    pub fn updated(&mut self, service: &HistoryHandle) {
        let update = service.status.borrow();
        if self.pending != Some(update.request_id) {
            return;
        }
        let Some(result) = &update.result else {
            return;
        };
        self.pending = None;
        match result {
            Ok(HistoryResult::Overview(sessions)) => self.sessions = sessions.clone(),
            Ok(_) => {}
            Err(error) => self.notice = Some(error.to_string()),
        }
    }

    /// Rows in display order: conversations that wait for an answer, then the
    /// rest, newest first within each group.
    fn rows(&self, snapshot: &CoreSnapshot) -> Vec<Row> {
        let live_id = if snapshot.conversation_id.is_empty() {
            snapshot.observation.session_id.as_str()
        } else {
            snapshot.conversation_id.as_str()
        };
        let mut rows: Vec<Row> = snapshot
            .conversations
            .iter()
            .map(|conversation| {
                let live = conversation.id == live_id;
                let status = self
                    .sessions
                    .iter()
                    .find(|session| session.info.session_id == conversation.id);
                let pending = if live {
                    snapshot.requests.len()
                } else {
                    status
                        .filter(|status| !status.info.session_closed)
                        .and_then(|status| status.pending_requests)
                        .unwrap_or(0)
                };
                let state = if pending > 0 {
                    if lang::zh() {
                        format!("有 {pending} 个请求等待回答")
                    } else {
                        format!("{pending} request(s) waiting for an answer")
                    }
                } else if live {
                    tr("open here", "当前对话").to_owned()
                } else {
                    match status {
                        Some(status) if !status.info.session_closed => {
                            tr("not closed", "未关闭").to_owned()
                        }
                        Some(status) if status.phase == Some(SessionPhase::Completed) => {
                            tr("completed", "已完成").to_owned()
                        }
                        Some(status) => lang::phase(status.phase.unwrap_or(SessionPhase::Unknown)),
                        None => String::new(),
                    }
                };
                Row {
                    group: if pending > 0 {
                        Group::NeedsInput
                    } else {
                        Group::Conversations
                    },
                    id: conversation.id.clone(),
                    title: conversation.title.clone(),
                    live,
                    state,
                    updated_at_ms: conversation.updated_at_ms,
                    turns: conversation.turns,
                }
            })
            .collect();
        // Core 已按时间倒序给出；稳定排序只把等待输入的提到前面。
        rows.sort_by_key(|row| row.group);
        rows
    }

    /// Sessions the journal knows that have no saved conversation to resume.
    fn unsaved(&self, snapshot: &CoreSnapshot) -> usize {
        self.sessions
            .iter()
            .filter(|session| {
                !snapshot
                    .conversations
                    .iter()
                    .any(|conversation| conversation.id == session.info.session_id)
            })
            .count()
    }

    pub fn key(
        &mut self,
        key: KeyEvent,
        service: &mut HistoryHandle,
        snapshot: &CoreSnapshot,
    ) -> SessionsEvent {
        let control = key.modifiers.contains(KeyModifiers::CONTROL);
        let rows = self.rows(snapshot);
        match key.code {
            KeyCode::Char('q' | 'd') if control => return SessionsEvent::Quit,
            KeyCode::Esc | KeyCode::Right => {
                self.visible = false;
                return SessionsEvent::Close;
            }
            KeyCode::Up => self.selected = self.selected.saturating_sub(1),
            KeyCode::Down => self.selected = (self.selected + 1).min(rows.len().saturating_sub(1)),
            KeyCode::Home => self.selected = 0,
            KeyCode::End => self.selected = rows.len().saturating_sub(1),
            KeyCode::Char('r') if !control => {
                self.refresh(service);
                return SessionsEvent::Refresh;
            }
            KeyCode::Enter => {
                if let Some(row) = rows.get(self.selected.min(rows.len().saturating_sub(1))) {
                    self.visible = false;
                    // 当前对话就在主页面，回去即可。
                    return if row.live {
                        SessionsEvent::Close
                    } else {
                        SessionsEvent::Resume(row.id.clone())
                    };
                }
            }
            _ => {}
        }
        SessionsEvent::Consumed
    }

    pub fn draw(&self, frame: &mut ratatui::Frame<'_>, snapshot: &CoreSnapshot) {
        let area = frame.area();
        let theme = theme::current();
        frame.render_widget(Block::default().style(theme.base()), area);
        if area.width < 4 || area.height < 3 {
            return;
        }
        let block = theme::focus_panel()
            .borders(Borders::ALL)
            .title(tr(" Sessions in this workspace ", " 本工作区的会话 "));
        let inner = block.inner(area);
        let width = inner.width as usize;
        let rows = self.rows(snapshot);
        let count = |group| rows.iter().filter(|row| row.group == group).count();
        let mut lines = Vec::new();
        if let Some(notice) = &self.notice {
            lines.push(Line::from(clip_spans(
                vec![Span::styled(notice.clone(), tone(theme.warning))],
                width,
            )));
        }
        // 底部留两行：说明和按键提示。
        let body = (inner.height as usize)
            .saturating_sub(lines.len() + 2)
            .max(1);
        let mut listed: Vec<(Line<'static>, Option<usize>)> = Vec::new();
        if rows.is_empty() {
            listed.push((
                Line::from(Span::styled(
                    tr(
                        "No saved conversations yet. One appears here after its first message.",
                        "还没有保存的对话。发出第一条消息后，它会出现在这里。",
                    ),
                    theme.secondary(),
                )),
                None,
            ));
        }
        let now = now_ms();
        let selected = self.selected.min(rows.len().saturating_sub(1));
        // 时间和状态各占固定宽度，标题用剩下的空间。
        let time_width = 12;
        let state_width = rows
            .iter()
            .map(|row| row.state.width())
            .max()
            .unwrap_or(0)
            .min(34);
        for (index, row) in rows.iter().enumerate() {
            if index == 0 || rows[index - 1].group != row.group {
                if index > 0 {
                    listed.push((Line::from(""), None));
                }
                listed.push((
                    Line::from(Span::styled(
                        format!("{} ({})", row.group.title(), count(row.group)),
                        if row.group == Group::NeedsInput {
                            tone(theme.warning)
                        } else {
                            theme.heading()
                        },
                    )),
                    None,
                ));
            }
            let marker = if index == selected { "> " } else { "  " };
            let time = elapsed(row.updated_at_ms, now);
            let turns = if lang::zh() {
                format!("{} 轮", row.turns)
            } else {
                format!("{} turn(s)", row.turns)
            };
            let fixed = marker.width() + time_width + 1 + state_width + 2 + turns.width() + 2;
            let title = truncate_display_label(&row.title, width.saturating_sub(fixed).max(8));
            let pad = |text: &str, to: usize| " ".repeat(to.saturating_sub(text.width()));
            let spans = vec![
                Span::raw(marker),
                Span::styled(
                    format!("{time}{} ", pad(&time, time_width)),
                    theme.secondary(),
                ),
                Span::raw(format!(
                    "{title}{}  ",
                    pad(&title, width.saturating_sub(fixed))
                )),
                Span::styled(
                    format!("{}{}  ", row.state, pad(&row.state, state_width)),
                    if row.group == Group::NeedsInput {
                        tone(theme.warning)
                    } else {
                        theme.secondary()
                    },
                ),
                Span::styled(turns, theme.faint()),
            ];
            listed.push((
                if index == selected {
                    selected_line(spans, width)
                } else {
                    Line::from(clip_spans(spans, width))
                },
                Some(index),
            ));
        }
        // 让选中的行留在可见范围内。
        let selected_at = listed
            .iter()
            .position(|(_, index)| *index == Some(selected))
            .unwrap_or(0);
        let start = (selected_at + 1).saturating_sub(body);
        lines.extend(
            listed
                .into_iter()
                .skip(start)
                .take(body)
                .map(|(line, _)| line),
        );
        while lines.len() + 2 < inner.height as usize {
            lines.push(Line::from(""));
        }
        let unsaved = self.unsaved(snapshot);
        let note = if unsaved == 0 {
            tr(
                "Enter continues the conversation here with its earlier messages.",
                "Enter 在这里接着聊，之前的消息会一并显示。",
            )
            .to_owned()
        } else if lang::zh() {
            format!("另有 {unsaved} 个更早的会话没有保存对话内容，不能恢复。")
        } else {
            format!(
                "{unsaved} earlier session(s) have no saved conversation and cannot be resumed."
            )
        };
        lines.push(Line::from(clip_spans(
            vec![Span::styled(note, theme.secondary())],
            width,
        )));
        lines.push(hint_row(
            tr(
                "↑/↓ select | Enter resume | r refresh | → or Esc back",
                "↑/↓ 选择 | Enter 恢复对话 | r 刷新 | → 或 Esc 返回",
            ),
            width,
        ));
        frame.render_widget(Paragraph::new(lines).block(block), area);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::conversations::ConversationSummary;
    use crate::journal::SessionInfo;

    fn status(id: &str, closed: bool, phase: SessionPhase, pending: usize) -> SessionOverview {
        SessionOverview {
            info: SessionInfo {
                schema_version: 1,
                workspace_id: "w".into(),
                session_id: id.into(),
                committed_seq: 7,
                committed_bytes: 0,
                snapshot_version: 1,
                recorded_at: None,
                session_closed: closed,
                needs_recovery: !closed,
                execution_result: Some(phase),
            },
            pending_requests: Some(pending),
            phase: Some(phase),
        }
    }

    fn conversation(id: &str, title: &str, minutes_ago: u64, turns: usize) -> ConversationSummary {
        ConversationSummary {
            id: id.into(),
            backend: "deepseek-acp".into(),
            title: title.into(),
            updated_at_ms: now_ms().unwrap() - minutes_ago * 60_000 - 5_000,
            turns,
        }
    }

    fn fixture() -> (SessionsPanel, CoreSnapshot) {
        let panel = SessionsPanel {
            visible: true,
            sessions: vec![
                status("done-1", true, SessionPhase::Completed, 0),
                status("asks-1", false, SessionPhase::Running, 2),
                status("failed-1", true, SessionPhase::Failed, 0),
                status("old-without-text", true, SessionPhase::Completed, 0),
            ],
            ..Default::default()
        };
        let mut snapshot = CoreSnapshot {
            phase: SessionPhase::Ready,
            conversations: vec![
                conversation("live-1", "Refactor the parser", 1, 3),
                conversation("done-1", "修复登录页面的重定向问题", 30, 2),
                conversation("asks-1", "Add retry to the uploader", 125, 5),
                conversation("failed-1", "Explain the build error", 60 * 26, 1),
            ],
            ..Default::default()
        };
        snapshot.observation.session_id = "live-1".into();
        (panel, snapshot)
    }

    fn screen(panel: &SessionsPanel, snapshot: &CoreSnapshot, width: u16, height: u16) -> String {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|frame| panel.draw(frame, snapshot)).unwrap();
        let buffer = terminal.backend().buffer();
        (0..buffer.area.height)
            .map(|y| {
                let mut row = String::new();
                let mut x = 0;
                while x < buffer.area.width {
                    let symbol = buffer[(x, y)].symbol();
                    row.push_str(symbol);
                    x += symbol.width().max(1) as u16;
                }
                row
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn conversations_show_their_titles_with_the_waiting_ones_first() {
        let (panel, snapshot) = fixture();
        let rows = panel.rows(&snapshot);
        assert_eq!(
            rows.iter().map(|row| row.id.as_str()).collect::<Vec<_>>(),
            ["asks-1", "live-1", "done-1", "failed-1"]
        );
        assert_eq!(rows[0].group, Group::NeedsInput);
        assert!(rows[1].live);

        let text = screen(&panel, &snapshot, 120, 20);
        for expected in [
            "Sessions in this workspace",
            "Waiting for input (1)",
            "> 2h ago",
            "Add retry to the uploader",
            "2 request(s) waiting for an answer",
            "5 turn(s)",
            "Conversations (3)",
            "Refactor the parser",
            "open here",
            "修复登录页面的重定向问题",
            "30m ago",
            "completed",
            "Explain the build error",
            "1d ago",
            "Failed",
            "1 earlier session(s) have no saved conversation",
            "Enter resume",
        ] {
            assert!(text.contains(expected), "{expected}: {text}");
        }
        assert!(!text.contains("evidence"), "{text}");

        // 当前对话一有请求就排到“等待输入”里。
        let mut asking = snapshot.clone();
        asking.requests = vec![crate::interactions::RequestView::decode(
            crate::protocol::RpcId::Number(1),
            "item/commandExecution/requestApproval",
            &serde_json::json!({"threadId":"t","turnId":"u","command":"echo"}),
        )
        .unwrap()];
        let rows = panel.rows(&asking);
        assert_eq!(rows[0].id, "live-1");
        assert_eq!(rows[0].group, Group::NeedsInput);
        // 恢复过别的对话后，“当前”跟着 Core 报告的对话走。
        let mut resumed = snapshot.clone();
        resumed.conversation_id = "done-1".into();
        let rows = panel.rows(&resumed);
        assert!(rows.iter().find(|row| row.id == "done-1").unwrap().live);
        assert!(!rows.iter().find(|row| row.id == "live-1").unwrap().live);
        for (width, height) in [(1, 1), (4, 3), (20, 6), (60, 10)] {
            screen(&panel, &snapshot, width, height);
        }
    }

    #[test]
    fn an_empty_workspace_says_when_a_conversation_will_appear() {
        let mut panel = SessionsPanel {
            visible: true,
            ..Default::default()
        };
        let snapshot = CoreSnapshot::default();
        let text = screen(&panel, &snapshot, 110, 12);
        assert!(text.contains("No saved conversations yet."), "{text}");
        assert!(
            text.contains("Enter continues the conversation here"),
            "{text}"
        );
        panel.notice = Some("history worker closed".into());
        assert!(screen(&panel, &snapshot, 110, 12).contains("history worker closed"));
    }
}
