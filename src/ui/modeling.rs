//! Modeling view: local selection/editor only; domain facts come from the modeling owner.
use super::editor::Editor;
use super::input::InputEvent;
use super::layout::wrap;
use super::terminal::TerminalGuard;
use super::UiError;
use crate::modeling::{self, Command, Handle, Options, Snapshot};
use crate::state::display_text;
use crossterm::event::{KeyCode, KeyEventKind, KeyModifiers};
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Color, Style};
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use std::time::Duration;

#[derive(Default)]
struct Local {
    editor: Editor,
    selected: usize,
    scroll: u16,
    notice: String,
    request: Option<(String, String)>,
    sent: bool,
}
impl Local {
    fn sync(&mut self, snapshot: Option<&Snapshot>) {
        let request = snapshot
            .and_then(|s| s.run_id.as_ref().zip(s.question.as_ref()))
            .map(|(run, q)| (run.clone(), q.request_id.clone()));
        if self.request != request {
            self.scroll = 0;
            if self.sent {
                self.notice.clear();
            }
            self.sent = false;
            self.request = request;
        }
    }
}

pub async fn run_modeling(options: Options, target: Option<String>) -> Result<(), UiError> {
    let mut terminal = TerminalGuard::enter()?;
    let mut local = Local::default();
    let mut verified = true;
    let mut handle = target.map(|target| Handle::start(options.clone(), target, verified));
    let result = async {
        let mut tick = tokio::time::interval(Duration::from_millis(40));
        loop {
            tick.tick().await;
            let snapshot = handle
                .as_mut()
                .map(|h| h.snapshots.borrow_and_update().clone());
            local.sync(snapshot.as_deref());
            terminal
                .terminal
                .draw(|frame| draw(frame, &options, snapshot.as_deref(), &local, verified))?;
            for _ in 0..128 {
                let Some(event) = terminal.input.read_ready()? else {
                    break;
                };
                match event {
                    InputEvent::Key(key) if key.kind != KeyEventKind::Release => {
                        let control = key.modifiers.contains(KeyModifiers::CONTROL);
                        match key.code {
                            KeyCode::Char('q') if control => return Ok(()),
                            KeyCode::Char('c') if control => {
                                if let Some(h) = &handle {
                                    let _ = h.commands.try_send(Command::Cancel);
                                }
                                local.notice = "正在取消并回收工作进程……".into();
                            }
                            KeyCode::Char('y' | 'n') if control && !local.sent => {
                                if let (Some(h), Some(s)) = (&handle, &snapshot) {
                                    if let (Some(run_id), Some(q)) = (&s.run_id, &s.question) {
                                        if h.commands
                                            .try_send(Command::Answer {
                                                run_id: run_id.clone(),
                                                request_id: q.request_id.clone(),
                                                accepted: key.code == KeyCode::Char('y'),
                                            })
                                            .is_ok()
                                        {
                                            local.notice = "回答已发送，等待工作流确认……".into();
                                            local.sent = true;
                                        }
                                    }
                                }
                            }
                            KeyCode::Char('o') if control => {
                                if let Some(a) = snapshot
                                    .as_ref()
                                    .and_then(|s| s.artifacts.get(local.selected))
                                {
                                    local.notice = match modeling::open_artifact(&options, &a.path)
                                    {
                                        Ok(()) => format!("已请求打开：{}", a.name),
                                        Err(error) => error.to_string(),
                                    };
                                }
                            }
                            KeyCode::F(2)
                                if snapshot.as_ref().is_none_or(|s| s.phase.terminal()) =>
                            {
                                verified = !verified
                            }
                            KeyCode::F(5)
                                if snapshot.as_ref().is_some_and(|s| s.phase.terminal()) =>
                            {
                                if let Some(run_id) =
                                    snapshot.as_ref().and_then(|s| s.run_id.clone())
                                {
                                    if let Some(old) = handle.take() {
                                        let _ = old.join.await;
                                    }
                                    handle = Some(Handle::retry_review(options.clone(), run_id));
                                    local = Local::default();
                                }
                            }
                            KeyCode::Up => local.selected = local.selected.saturating_sub(1),
                            KeyCode::Down => {
                                let count = snapshot.as_ref().map_or(0, |s| s.artifacts.len());
                                local.selected = (local.selected + 1).min(count.saturating_sub(1));
                            }
                            KeyCode::PageUp => local.scroll = local.scroll.saturating_sub(5),
                            KeyCode::PageDown => local.scroll = local.scroll.saturating_add(5),
                            KeyCode::Enter
                                if snapshot.as_ref().is_none_or(|s| s.phase.terminal()) =>
                            {
                                let target = local.editor.text.trim().to_owned();
                                if !target.is_empty() {
                                    if let Some(old) = handle.take() {
                                        let _ = old.join.await;
                                    }
                                    handle = Some(Handle::start(options.clone(), target, verified));
                                    local = Local::default();
                                }
                            }
                            KeyCode::Char(character) if !control => {
                                local.editor.insert(&character.to_string())
                            }
                            KeyCode::Left => local.editor.left(),
                            KeyCode::Right => local.editor.right(),
                            KeyCode::Home => local.editor.cursor = 0,
                            KeyCode::End => local.editor.cursor = local.editor.text.len(),
                            KeyCode::Backspace => local.editor.backspace(),
                            KeyCode::Delete => local.editor.delete(),
                            _ => {}
                        }
                    }
                    InputEvent::Paste(text) => local.editor.insert(&text),
                    InputEvent::PasteRejected => local.notice = "粘贴内容过长".into(),
                    _ => {}
                }
            }
        }
    }
    .await;
    if let Some(h) = handle {
        let _ = h.commands.send(Command::Cancel).await;
        let _ = h.join.await;
    }
    result
}

fn draw(
    frame: &mut ratatui::Frame<'_>,
    options: &Options,
    snapshot: Option<&Snapshot>,
    local: &Local,
    verified: bool,
) {
    let pending = snapshot.and_then(|s| s.question.as_ref());
    let sections = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(4),
            Constraint::Min(4),
            Constraint::Length(if pending.is_some() {
                frame.area().height.saturating_sub(18).clamp(5, 10)
            } else {
                3
            }),
            Constraint::Length(4),
            Constraint::Length(3),
        ])
        .split(frame.area());
    let phase = snapshot.map_or("输入楼层即可开始", |s| s.phase.label());
    let policy = if verified {
        "有依据且无待定项的补丁自动采纳"
    } else {
        "补丁逐项确认"
    };
    frame.render_widget(
        Paragraph::new(format!(
            "项目：{}  |  {phase}\n任务授权：{policy} · F2 切换 · F5 重试复核",
            display_text(&options.project)
        ))
        .block(Block::default().borders(Borders::ALL).title("建筑 AI 建模")),
        sections[0],
    );
    let columns = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(40), Constraint::Percentage(60)])
        .split(sections[1]);
    let stages: Vec<Line> = snapshot.map_or_else(Vec::new, |s| {
        s.stages
            .iter()
            .rev()
            .take(24)
            .rev()
            .map(|stage| {
                let (marker, color) = match stage.state.as_str() {
                    "completed" => ("✓", Color::Green),
                    "failed" => ("!", Color::Red),
                    _ => ("●", Color::Cyan),
                };
                Line::styled(
                    format!(
                        "{marker} {} {}",
                        display_text(&stage.floor),
                        display_text(&stage.name)
                    ),
                    Style::default().fg(color),
                )
            })
            .collect()
    });
    frame.render_widget(
        Paragraph::new(stages)
            .wrap(Wrap { trim: false })
            .block(Block::default().borders(Borders::ALL).title("阶段进度")),
        columns[0],
    );
    let count = columns[1].height.saturating_sub(2) as usize;
    let width = columns[1].width.saturating_sub(2) as usize;
    let rows: Vec<String> = snapshot.map_or_else(Vec::new, |s| {
        s.notes.iter().flat_map(|n| wrap(n, width)).collect()
    });
    let notes: Vec<Line> = rows
        .iter()
        .skip(rows.len().saturating_sub(count))
        .map(|n| Line::from(n.as_str()))
        .collect();
    frame.render_widget(
        Paragraph::new(notes).block(
            Block::default()
                .borders(Borders::ALL)
                .title("AI 与工作流动态"),
        ),
        columns[1],
    );
    let question = pending.map_or_else(
        || display_text(&local.notice),
        |q| {
            format!(
                "{}\n{}",
                display_text(&q.title),
                q.lines
                    .iter()
                    .map(|s| display_text(s))
                    .collect::<Vec<_>>()
                    .join("\n")
            )
        },
    );
    let mut rows = wrap(&question, sections[2].width.saturating_sub(2) as usize);
    if local.sent {
        rows.push(local.notice.clone());
    }
    let max_scroll = rows
        .len()
        .saturating_sub(sections[2].height.saturating_sub(2) as usize);
    frame.render_widget(
        Paragraph::new(rows.into_iter().map(Line::from).collect::<Vec<_>>())
            .scroll((
                local.scroll.min(max_scroll.min(u16::MAX as usize) as u16),
                0,
            ))
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title("必要确认 · Ctrl+Y 采纳 / Ctrl+N 拒绝 · PgUp/PgDn 查看"),
            ),
        sections[2],
    );
    let artifact = snapshot
        .and_then(|s| {
            s.artifacts.get(local.selected).map(|a| {
                format!(
                    "[{}/{}] {}\n{}",
                    local.selected + 1,
                    s.artifacts.len(),
                    display_text(&a.name),
                    display_text(&a.path.to_string_lossy())
                )
            })
        })
        .unwrap_or_else(|| "模型、预览和报告将在此显示".into());
    frame.render_widget(
        Paragraph::new(artifact).wrap(Wrap { trim: false }).block(
            Block::default()
                .borders(Borders::ALL)
                .title("产物 · ↑↓ 选择 / Ctrl+O 打开"),
        ),
        sections[3],
    );
    frame.render_widget(
        Paragraph::new(if local.editor.text.is_empty() {
            "输入建模八层、整栋，或复核上次"
        } else {
            local.editor.text.as_str()
        })
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title("目标 · Enter 启动 · Ctrl+C 取消 · Ctrl+Q 退出"),
        ),
        sections[4],
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    #[test]
    fn modeling_entry_renders_chinese_target_and_authorization() {
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        let mut local = Local::default();
        local.editor.insert("建模八层");
        terminal
            .draw(|f| {
                draw(
                    f,
                    &Options {
                        repository: ".".into(),
                        project: "医院项目".into(),
                    },
                    None,
                    &local,
                    true,
                )
            })
            .unwrap();
        let text: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(text.replace(' ', "").contains("建筑AI建模"));
        assert!(text.replace(' ', "").contains("建模八层"));
        assert!(text.contains("Ctrl+Y"));
        assert!(text.contains("Ctrl+O"));
        assert!(text.replace(' ', "").contains("任务授权"));
    }
    #[test]
    fn confirmation_ack_clears_sent_notice_and_new_request_resets_scroll() {
        use crate::modeling::{Phase, Question};
        let mut state = Snapshot {
            run_id: Some("run".into()),
            phase: Phase::Waiting,
            target: "八层".into(),
            stages: vec![],
            question: Some(Question {
                request_id: "q1".into(),
                title: "层高".into(),
                lines: vec![],
            }),
            artifacts: vec![],
            notes: Default::default(),
            sequence: 1,
            outcome: None,
        };
        let mut local = Local::default();
        local.sync(Some(&state));
        local.sent = true;
        local.notice = "回答已发送".into();
        local.scroll = 100;
        state.question = None;
        local.sync(Some(&state));
        assert!(!local.sent && local.notice.is_empty() && local.scroll == 0);
        state.question = Some(Question {
            request_id: "q2".into(),
            title: "门宽".into(),
            lines: vec![],
        });
        local.scroll = 50;
        local.sync(Some(&state));
        assert_eq!(local.scroll, 0);
    }
}
