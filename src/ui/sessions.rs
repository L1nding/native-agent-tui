//! Sessions page: this workspace's dsh sessions by title, with the one that
//! waits for an answer first. Opened with Left from an empty task input;
//! Enter resumes the selected session here.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use unicode_width::UnicodeWidthStr;

use super::activity::{clip_spans, hint_row, selected_line, tone, truncate_display_label};
use super::lang::{self, tr};
use super::theme;
use crate::state::CoreSnapshot;

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
    state: String,
    updated_at_ms: u64,
}

#[derive(Default)]
pub(super) struct SessionsPanel {
    pub visible: bool,
    selected: usize,
    pub notice: Option<String>,
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum SessionsEvent {
    Close,
    /// Continue this dsh session here.
    Resume(String),
    /// Ask Core to list the sessions again.
    Refresh,
    /// Start a fresh conversation in the running agent process.
    New,
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
    /// The caller also asks Core for the session list.
    pub fn open(&mut self) {
        self.visible = true;
        self.selected = 0;
        self.notice = None;
    }

    /// Rows in display order: the session that waits for an answer, then the
    /// rest, newest first. A session nobody has written to has no title and
    /// is left out unless it is the one open here.
    fn rows(&self, snapshot: &CoreSnapshot) -> Vec<Row> {
        let live_id = snapshot.thread_id.as_deref().unwrap_or("");
        let mut rows: Vec<Row> = snapshot
            .conversations
            .iter()
            .filter(|conversation| conversation.id == live_id || !conversation.title.is_empty())
            .map(|conversation| {
                let live = conversation.id == live_id;
                // 只有当前进程里的会话才可能有等待回答的请求。
                let pending = if live { snapshot.requests.len() } else { 0 };
                let state = if pending > 0 {
                    if lang::zh() {
                        format!("有 {pending} 个请求等待回答")
                    } else {
                        format!("{pending} request(s) waiting for an answer")
                    }
                } else if live {
                    tr("open here", "当前对话").to_owned()
                } else {
                    String::new()
                };
                Row {
                    group: if pending > 0 {
                        Group::NeedsInput
                    } else {
                        Group::Conversations
                    },
                    id: conversation.id.clone(),
                    title: if conversation.title.is_empty() {
                        tr("(no message yet)", "（还没有消息）").to_owned()
                    } else {
                        conversation.title.clone()
                    },
                    live,
                    state,
                    updated_at_ms: conversation.updated_at_ms,
                }
            })
            .collect();
        // Core 已按时间倒序给出；稳定排序只把等待输入的提到前面。
        rows.sort_by_key(|row| row.group);
        rows
    }

    pub fn key(&mut self, key: KeyEvent, snapshot: &CoreSnapshot) -> SessionsEvent {
        let control = key.modifiers.contains(KeyModifiers::CONTROL);
        let rows = self.rows(snapshot);
        self.selected = self.selected.min(rows.len());
        match key.code {
            KeyCode::Char('q' | 'd') if control => return SessionsEvent::Quit,
            KeyCode::Esc | KeyCode::Right => {
                self.visible = false;
                return SessionsEvent::Close;
            }
            KeyCode::Up => self.selected = self.selected.saturating_sub(1),
            KeyCode::Down => self.selected = (self.selected + 1).min(rows.len()),
            KeyCode::Home => self.selected = 0,
            KeyCode::End => self.selected = rows.len(),
            KeyCode::Char('r') if !control => return SessionsEvent::Refresh,
            KeyCode::Char('n') if !control => return SessionsEvent::New,
            KeyCode::Enter => {
                if self.selected == 0 {
                    return SessionsEvent::New;
                }
                if let Some(row) = rows.get(self.selected.min(rows.len()) - 1) {
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
        let selected = self.selected.min(rows.len());
        let button = vec![Span::styled(
            format!(
                "{}[ + {} ]",
                if selected == 0 { "> " } else { "  " },
                tr("New session", "新建会话")
            ),
            theme.key(),
        )];
        let mut listed: Vec<(Line<'static>, Option<usize>)> = vec![
            (
                if selected == 0 {
                    selected_line(button, width)
                } else {
                    Line::from(clip_spans(button, width))
                },
                Some(0),
            ),
            (Line::from(""), None),
        ];
        if rows.is_empty() {
            listed.push((
                Line::from(Span::styled(
                    tr(
                        "No sessions yet. One appears here after its first message.",
                        "还没有会话。发出第一条消息后，它会出现在这里。",
                    ),
                    theme.secondary(),
                )),
                None,
            ));
        }
        let now = now_ms();
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
            let marker = if index + 1 == selected { "> " } else { "  " };
            let time = elapsed(row.updated_at_ms, now);
            let fixed = marker.width() + time_width + 1 + state_width + 2;
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
                    format!("{}{}", row.state, pad(&row.state, state_width)),
                    if row.group == Group::NeedsInput {
                        tone(theme.warning)
                    } else {
                        theme.secondary()
                    },
                ),
            ];
            listed.push((
                if index + 1 == selected {
                    selected_line(spans, width)
                } else {
                    Line::from(clip_spans(spans, width))
                },
                Some(index + 1),
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
        let note = tr(
            "Sessions are dsh's own. Enter continues one here with its earlier messages.",
            "会话由 dsh 保存。Enter 在这里接着聊，之前的消息会一并显示。",
        );
        lines.push(Line::from(clip_spans(
            vec![Span::styled(note, theme.secondary())],
            width,
        )));
        lines.push(hint_row(
            tr(
                "↑/↓ select | Enter resume | n new session | r refresh | → or Esc back",
                "↑/↓ 选择 | Enter 恢复对话 | n 新建会话 | r 刷新 | → 或 Esc 返回",
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

    fn conversation(id: &str, title: &str, minutes_ago: u64) -> ConversationSummary {
        ConversationSummary {
            id: id.into(),
            title: title.into(),
            updated_at_ms: now_ms().unwrap() - minutes_ago * 60_000 - 5_000,
        }
    }

    fn fixture() -> (SessionsPanel, CoreSnapshot) {
        let panel = SessionsPanel {
            visible: true,
            selected: 1,
            ..Default::default()
        };
        let snapshot = CoreSnapshot {
            thread_id: Some("live-1".into()),
            conversations: vec![
                conversation("live-1", "Refactor the parser", 1),
                conversation("done-1", "修复登录页面的重定向问题", 30),
                conversation("blank-1", "", 45),
                conversation("old-1", "Explain the build error", 60 * 26),
            ],
            ..Default::default()
        };
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
    fn sessions_show_their_titles_and_the_one_waiting_for_an_answer_comes_first() {
        let (mut panel, snapshot) = fixture();
        let rows = panel.rows(&snapshot);
        // 没有标题的会话还没有人写过消息，不列出。
        assert_eq!(
            rows.iter().map(|row| row.id.as_str()).collect::<Vec<_>>(),
            ["live-1", "done-1", "old-1"]
        );
        assert!(rows[0].live);

        let text = screen(&panel, &snapshot, 120, 20);
        for expected in [
            "Sessions in this workspace",
            "Conversations (3)",
            "> 1m ago",
            "Refactor the parser",
            "open here",
            "修复登录页面的重定向问题",
            "30m ago",
            "Explain the build error",
            "1d ago",
            "Sessions are dsh's own.",
            "Enter resume",
            "n new session",
        ] {
            assert!(text.contains(expected), "{expected}: {text}");
        }

        // 当前对话一有请求就排到“等待输入”里。
        let mut asking = snapshot.clone();
        asking.requests = vec![crate::interactions::RequestView::decode(
            crate::protocol::RpcId::Number(1),
            "item/commandExecution/requestApproval",
            &serde_json::json!({"threadId":"t","turnId":"u","command":"echo"}),
        )
        .unwrap()];
        let rows = panel.rows(&asking);
        assert_eq!(
            (rows[0].id.as_str(), rows[0].group),
            ("live-1", Group::NeedsInput)
        );
        let text = screen(&panel, &asking, 120, 20);
        assert!(text.contains("Waiting for input (1)"), "{text}");
        assert!(
            text.contains("1 request(s) waiting for an answer"),
            "{text}"
        );

        // Enter 在当前对话上只是返回；在别的会话上请求恢复。
        let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(panel.key(enter, &snapshot), SessionsEvent::Close);
        panel.visible = true;
        panel.key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE), &snapshot);
        assert_eq!(
            panel.key(enter, &snapshot),
            SessionsEvent::Resume("done-1".into())
        );
        assert_eq!(
            panel.key(
                KeyEvent::new(KeyCode::Char('r'), KeyModifiers::NONE),
                &snapshot
            ),
            SessionsEvent::Refresh
        );
        assert_eq!(
            panel.key(
                KeyEvent::new(KeyCode::Char('n'), KeyModifiers::NONE),
                &snapshot
            ),
            SessionsEvent::New
        );
        // 恢复之后，“当前”跟着 Core 报告的会话走。
        let mut resumed = snapshot.clone();
        resumed.thread_id = Some("done-1".into());
        let rows = panel.rows(&resumed);
        assert!(rows.iter().find(|row| row.id == "done-1").unwrap().live);
        assert!(!rows.iter().find(|row| row.id == "live-1").unwrap().live);
        for (width, height) in [(1, 1), (4, 3), (20, 6), (60, 10)] {
            screen(&panel, &snapshot, width, height);
        }
    }

    #[test]
    fn new_session_button_works_with_an_empty_list_and_with_existing_conversations() {
        let (mut panel, snapshot) = fixture();
        panel.open();
        let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        assert!(screen(&panel, &snapshot, 100, 20).contains("> [ + New session ]"));
        assert_eq!(panel.key(enter, &snapshot), SessionsEvent::New);
        panel.key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE), &snapshot);
        assert_eq!(panel.key(enter, &snapshot), SessionsEvent::Close);
        panel.visible = true;
        panel.key(KeyEvent::new(KeyCode::End, KeyModifiers::NONE), &snapshot);
        // A refreshed list may be empty while the old selection points past it.
        assert_eq!(
            panel.key(enter, &CoreSnapshot::default()),
            SessionsEvent::New
        );
        assert!(screen(&panel, &CoreSnapshot::default(), 40, 8).contains("New session"));
    }

    #[test]
    fn a_new_session_is_listed_before_its_first_message_and_an_empty_list_says_so() {
        let mut panel = SessionsPanel {
            visible: true,
            ..Default::default()
        };
        let mut snapshot = CoreSnapshot::default();
        let text = screen(&panel, &snapshot, 110, 12);
        assert!(text.contains("No sessions yet."), "{text}");
        snapshot.thread_id = Some("new".into());
        snapshot.conversations = vec![conversation("new", "", 0)];
        let text = screen(&panel, &snapshot, 110, 12);
        assert!(text.contains("(no message yet)"), "{text}");
        assert!(text.contains("open here"), "{text}");
        panel.notice = Some("Sessions could not be listed".into());
        assert!(screen(&panel, &snapshot, 110, 12).contains("Sessions could not be listed"));
    }
}
