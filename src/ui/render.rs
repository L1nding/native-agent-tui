//! Read-only terminal rendering composed from a Core snapshot and local UI state.

use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders, Paragraph};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use super::activity::{
    draw_evidence, evidence_brief, focus_activity, reminder_brief, usage_status,
};
use super::attention::draw_attention_editor;
use super::help::draw_help;
use super::layout::{conversation_content_size, main_layout, wrap, StatusRows};
use super::requests;
use super::skills::draw_skills;
use super::workflow_view;
use super::{selected_request, LocalState};
use crate::interactions::RequestKind;
use crate::observation::{AttentionLevel, ExecutionState};
use crate::state::{display_text, ConversationItem, CoreSnapshot, SessionPhase};
pub(super) fn draw(frame: &mut ratatui::Frame<'_>, snapshot: &CoreSnapshot, local: &LocalState) {
    let area = frame.area();
    if local.palette.visible {
        local.palette.draw(frame, area);
        return;
    }
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
    let activity = status_text(snapshot, local);
    let chunks = main_layout(
        area,
        header_activity(snapshot, local).is_some(),
        status_rows(snapshot, local, &activity),
    );
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
    // 只显示有信息量的计数；缺失的 token 事实在 F11 中仍明确显示 unavailable。
    let mut status = vec![
        format!("{:?}", snapshot.phase),
        format!("action:{actions} attention:{attention}{reminders_status}"),
    ];
    for (label, count) in [
        ("turns", snapshot.root_turn_count as usize),
        ("children", snapshot.agents.len()),
        ("queued", snapshot.scheduler.queued_roots),
    ] {
        if count > 0 {
            status.push(format!("{label}: {count}"));
        }
    }
    // 没有设置预算时省略 "/unavailable" 上限；F11 仍显示完整字段。
    let usage = usage_status(snapshot).replacen("/unavailable", "", 1);
    if usage != "tokens:unavailable" {
        status.push(usage);
    }
    if snapshot.scheduler.stopping {
        status.push("stopping".into());
    } else if snapshot.scheduler.paused {
        status.push("dispatch paused".into());
    }
    let status = status.join(" | ");
    let settings = format!(
        "{} | {} | {} | {}",
        snapshot.model.as_deref().unwrap_or("model pending"),
        snapshot.cwd,
        snapshot.sandbox,
        snapshot.approval_policy
    );
    let agent_id = selected_agent.map_or("root", |agent| agent.info.id.as_str());
    let selected_activity = header_activity(snapshot, local);
    let mut header = vec![Line::from(status)];
    if let Some(activity) = selected_activity {
        header.push(Line::from(display_text(&reminder_brief(
            activity,
            &local.reminders,
        ))));
        header.push(Line::from(display_text(&evidence_brief(
            activity,
            &local.reminders,
        ))));
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
        draw_skills(frame, area, snapshot, local.skills_scroll);
        return;
    }

    if local.context.visible {
        local.context.draw(frame, area, snapshot, selected_agent);
        return;
    }

    let conversation_area = if area.width >= 100 && !snapshot.agents.is_empty() {
        let panels = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Length(32), Constraint::Min(1)])
            .split(chunks[1]);
        workflow_view::draw_agents(
            frame,
            panels[0],
            snapshot,
            selected_agent.map(|agent| agent.info.id.as_str()),
            area.height <= 16,
        );
        panels[1]
    } else {
        chunks[1]
    };
    let (width, height) = conversation_content_size(conversation_area);
    let mut transcript = Vec::new();
    let mut role_rows = std::collections::HashMap::new();
    let mut focused_row = None;
    for message in snapshot
        .messages
        .iter()
        .filter(|message| message.thread_id == selected_thread)
    {
        let (rows, header) = message_rows(message, width);
        if header > 0 {
            role_rows.insert(
                transcript.len(),
                if message.role == "You" {
                    Color::Cyan
                } else {
                    Color::Green
                },
            );
        } else {
            for row in transcript.len()..transcript.len() + rows.len() {
                role_rows.insert(row, Color::DarkGray);
            }
        }
        if let Some(focus) = local
            .conversation_focus
            .as_ref()
            .filter(|focus| focus.matches(message))
        {
            focused_row = Some(transcript.len() + header + focus.row(width));
        }
        transcript.extend(rows);
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
    let below = max_scroll.saturating_sub(start);
    let title = display_text(&format!(
        " {name} · F3 switch{}{}{} ",
        if snapshot.history_truncated {
            " [older content truncated]"
        } else {
            ""
        },
        if local.conversation_focus.is_some() && focused_row.is_none() {
            " [search content changed/evicted]"
        } else {
            ""
        },
        if below > 0 {
            format!(" · {below} more rows below, Ctrl+End latest")
        } else {
            String::new()
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
            } else if let Some(&color) = role_rows.get(&row) {
                Line::from(text).style(if color == Color::DarkGray {
                    Style::default().fg(color)
                } else {
                    Style::default().fg(color).add_modifier(Modifier::BOLD)
                })
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
    if local.workflow.visible {
        workflow_view::draw_workflow(
            frame,
            chunks[1],
            snapshot,
            local.workflow.selected_id,
            local.workflow.link_cursor,
            local.workflow.scroll,
            local.workflow.manual_scroll,
        );
    }
    if local.evidence {
        draw_evidence(
            frame,
            chunks[1],
            snapshot,
            &local.reminders,
            local.evidence_scroll,
            agent_id,
            selected_agent,
        );
    }

    let lines: Vec<_> = wrap(&activity, chunks[2].width as usize)
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
                    .borders(Borders::TOP)
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
        Paragraph::new(if local.workflow.visible {
            "Up/Down select  F5 workflow  F6 pause  F7 cancel  F8 retry  +/- priority  F9 stop"
        } else if local.evidence {
            "Ctrl+W wait/restore  PgUp/PgDn scroll  F11 close  F3 agent"
        } else {
            "F1 keys | Enter send | Ctrl+P commands | Ctrl+F search | Ctrl+T timeline | Ctrl+Q quit"
        }),
        chunks[4],
    );
    if let Some(editor) = &local.attention_editor {
        draw_attention_editor(frame, snapshot, editor);
    }
    if local.request_panel && local.attention_editor.is_none() {
        requests::draw(frame, snapshot, local);
    }
    if local.help {
        draw_help(frame, area);
    }
}

/// Status panel text shared by rendering and scroll geometry; empty hides the panel.
pub(super) fn status_text(snapshot: &CoreSnapshot, local: &LocalState) -> String {
    let request = selected_request(snapshot, local);
    let waiting = snapshot.gate.as_ref().filter(|gate| gate.pending);
    // 错误原因始终保留在提示前面，后续提示不能把它遮住。
    let error = snapshot.last_error.as_ref().map(|error| {
        if snapshot.phase == SessionPhase::Failed && !snapshot.startup_blocked {
            format!(
                "Turn failed: {error}
Enter a new task to continue; F8 retries a failed workflow task."
            )
        } else {
            error.clone()
        }
    });
    let notice = match (snapshot.notice.as_ref(), error.as_ref()) {
        (Some(notice), Some(error)) if notice != error => Some(format!(
            "{error}
{notice}"
        )),
        (notice, error) => notice.or(error).cloned(),
    };
    let notice = local.notice.as_ref().or(notice.as_ref());
    if let Some(notice) = &local.notice {
        match &error {
            Some(error) => format!(
                "{error}
{notice}"
            ),
            None => notice.clone(),
        }
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
                    agent_label(snapshot, &request.thread_id),
                    question.header,
                    local.question_index + 1,
                    questions.len(),
                    question.question,
                    options
                )
            }
            _ => format!(
                "{} · {}\n{}",
                agent_label(snapshot, &request.thread_id),
                request.summary,
                requests::actions(snapshot, local, request)
            ),
        }
    } else if let Some(notice) = notice {
        if let Some(gate) = waiting {
            format!("{}\n{notice}", workflow_view::gate_status(snapshot, gate))
        } else {
            notice.clone()
        }
    } else if let Some(gate) = waiting {
        workflow_view::gate_status(snapshot, gate)
    } else if snapshot.scheduler.queued_roots > 0 {
        format!(
            "{} root tasks pending; see dependencies and controls in F4.",
            snapshot.scheduler.queued_roots
        )
    } else {
        String::new()
    }
}

/// 状态区行数：无内容时隐藏；请求、错误和等待需要更多行。
pub(super) fn status_rows(snapshot: &CoreSnapshot, local: &LocalState, text: &str) -> StatusRows {
    if text.is_empty() {
        StatusRows::Hidden
    } else if selected_request(snapshot, local).is_some()
        || snapshot.last_error.is_some()
        || snapshot.gate.as_ref().is_some_and(|gate| gate.pending)
    {
        StatusRows::Expanded
    } else {
        StatusRows::Compact
    }
}

/// 头部展示的活动；渲染和滚动几何共用，保证头部高度一致。
pub(super) fn header_activity<'a>(
    snapshot: &'a CoreSnapshot,
    local: &LocalState,
) -> Option<&'a crate::observation::ActivitySnapshot> {
    let agent_id = local
        .agent_id
        .as_deref()
        .filter(|id| snapshot.agents.iter().any(|agent| agent.info.id == *id))
        .unwrap_or("root");
    // 已结束的活动只会显示“Completed/review result”，在空闲时误导用户，留给 F11 查看。
    focus_activity(snapshot, agent_id).filter(|activity| {
        activity.attention.requires_action
            || activity.execution_state == ExecutionState::Unknown
            || activity.attention.level != AttentionLevel::Ended
    })
}

/// 一条消息在对话区占用的行（含末尾空行）以及标题行数；渲染和滚动定位共用。
/// 工具摘要没有角色标题，避免每个轮次多出噪音行。
pub(super) fn message_rows(message: &ConversationItem, width: usize) -> (Vec<String>, usize) {
    let mut rows = Vec::new();
    let header = usize::from(message.role != "Tool");
    if header > 0 {
        rows.push(format!(
            "{}{}",
            message.role,
            if message.truncated {
                " [truncated]"
            } else {
                ""
            }
        ));
    }
    rows.extend(wrap(&message.text, width));
    rows.push(String::new());
    (rows, header)
}

/// 请求所属代理的可读名称：根线程显示 root，子代理优先路径或昵称。
fn agent_label<'a>(snapshot: &'a CoreSnapshot, thread_id: &'a str) -> &'a str {
    if snapshot.thread_id.as_deref() == Some(thread_id) {
        return "root";
    }
    snapshot
        .agents
        .iter()
        .find(|agent| agent.info.id == thread_id)
        .and_then(|agent| {
            agent
                .info
                .path
                .as_deref()
                .or(agent.info.nickname.as_deref())
        })
        .unwrap_or(thread_id)
}
