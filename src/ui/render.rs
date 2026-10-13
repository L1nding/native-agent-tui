//! Read-only terminal rendering composed from a Core snapshot and local UI state.

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Padding, Paragraph};
use unicode_width::UnicodeWidthStr;

use super::activity::{
    draw_evidence, evidence_brief, focus_activity, reminder_brief, usage_status,
};
use super::attention::draw_attention_editor;
use super::help::draw_help;
use super::lang::{self, tr};
use super::layout::{
    conversation_content_size, main_layout, workspace_layout, wrap_text, StatusRows,
};
use super::skills::draw_skills;
use super::workflow_view;
use super::{request_locked, selected_request, LocalState};
use super::{requests, theme};
use crate::interactions::RequestKind;
use crate::observation::{AttentionLevel, ExecutionState};
use crate::state::{display_text, CoreSnapshot, SessionPhase};
pub(super) fn draw(frame: &mut ratatui::Frame<'_>, snapshot: &CoreSnapshot, local: &LocalState) {
    local
        .conversation_viewport
        .set(super::ConversationViewport::default());
    let area = frame.area();
    let theme = theme::current();
    frame.render_widget(Block::default().style(theme.base()), area);
    if local.palette.visible {
        local.palette.draw(frame, area);
        return;
    }
    if local.models.visible {
        local.models.draw(frame, area, snapshot);
        return;
    }
    if local.settings.visible {
        local.settings.draw(frame, area, snapshot);
        return;
    }
    if local.provider_form.visible {
        local.provider_form.draw(frame, area, &snapshot.models.test);
        return;
    }
    if local.domain_visible
        && snapshot.domain.enabled
        && snapshot.requests.is_empty()
        && area.width >= 24
        && area.height >= 8
    {
        super::domain::draw(frame, area, snapshot, local);
        return;
    }
    if area.width < 24 || area.height < 8 {
        frame.render_widget(
            Paragraph::new(format!(
                "{}\n{}",
                lang::phase(snapshot.phase),
                tr(
                    "Resize terminal\nCtrl+Q quit",
                    "请放大终端窗口\nCtrl+Q 退出"
                )
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
        editor_view(snapshot, local, area.width).visible_rows(area.height),
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
        format!(" | {}:{acknowledged}", tr("waiting", "暂不提醒"))
    } else {
        String::new()
    };
    // 只显示有信息量的计数；缺失的 token 事实在 F11 中仍明确显示 unavailable。
    let mut status = vec![
        lang::phase(snapshot.phase),
        format!(
            "{}:{actions} {}:{attention}{reminders_status}",
            tr("action", "待处理"),
            tr("attention", "需关注")
        ),
    ];
    for (label, count) in [
        (tr("turns", "轮次"), snapshot.root_turn_count as usize),
        (tr("children", "子 Agent"), snapshot.agents.len()),
        (tr("queued", "排队"), snapshot.scheduler.queued_roots),
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
        status.push(tr("stopping", "正在停止").into());
    } else if snapshot.scheduler.paused {
        status.push(tr("dispatch paused", "已暂停派发").into());
    }
    let status = status.join(" | ");
    let mut settings = format!(
        "{} | {}",
        snapshot
            .model
            .as_deref()
            .unwrap_or(tr("model pending", "模型待定")),
        snapshot.cwd,
    );
    // 只显示 dsh 报告过的权限预设；没读到就不显示，不猜。
    if !snapshot.permission.current.is_empty() {
        settings.push_str(" | ");
        settings.push_str(&snapshot.permission.current);
    }
    let agent_id = selected_agent.map_or("root", |agent| agent.info.id.as_str());
    let selected_activity = header_activity(snapshot, local);
    let mut status_spans = vec![Span::styled("● ", theme.phase(snapshot.phase))];
    for (index, part) in status.split(" | ").enumerate() {
        if index > 0 {
            status_spans.push(Span::styled(" | ", theme.faint()));
        }
        if index == 0 {
            status_spans.push(Span::styled(part.to_owned(), theme.phase(snapshot.phase)));
        } else if index == 1 {
            // action/attention 计数非零时才提亮，平时保持安静。
            for (position, (word, count)) in part
                .split(' ')
                .zip([actions, attention, acknowledged])
                .enumerate()
            {
                if position > 0 {
                    status_spans.push(Span::raw(" "));
                }
                status_spans.push(Span::styled(
                    word.to_owned(),
                    if count > 0 {
                        Style::default()
                            .fg(theme.warning)
                            .add_modifier(Modifier::BOLD)
                    } else {
                        theme.secondary()
                    },
                ));
            }
        } else {
            status_spans.push(Span::styled(part.to_owned(), theme.secondary()));
        }
    }
    let mut header = vec![Line::from(status_spans)];
    if let Some(activity) = selected_activity {
        header.push(
            Line::from(display_text(&reminder_brief(activity, &local.reminders))).style(
                if activity.attention.requires_action {
                    Style::default()
                        .fg(theme.warning)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(theme.foreground)
                },
            ),
        );
        header.push(
            Line::from(display_text(&evidence_brief(activity, &local.reminders)))
                .style(theme.secondary()),
        );
    }
    let mut settings_spans = Vec::new();
    for (index, part) in display_text(&settings).split(" | ").enumerate() {
        if index > 0 {
            settings_spans.push(Span::styled(" | ", theme.faint()));
        }
        settings_spans.push(Span::styled(
            part.to_owned(),
            if index == 0 {
                Style::default().fg(theme.foreground)
            } else {
                theme.secondary()
            },
        ));
    }
    header.push(Line::from(settings_spans));
    let compact_header = area.height <= 16 && selected_activity.is_some();
    if compact_header {
        header.remove(0);
    }
    frame.render_widget(
        Paragraph::new(header).block(
            theme::panel()
                .borders(Borders::BOTTOM)
                .padding(Padding::horizontal(1))
                .title(if compact_header {
                    Line::from(Span::styled(
                        format!(
                            " {} {}:{actions} {}:{attention}{} ",
                            lang::phase(snapshot.phase),
                            tr("action", "待处理"),
                            tr("attention", "需关注"),
                            if acknowledged > 0 {
                                format!(" {}:{acknowledged}", tr("wait", "暂不提醒"))
                            } else {
                                String::new()
                            }
                        ),
                        theme.phase(snapshot.phase),
                    ))
                } else {
                    Line::from(vec![
                        Span::styled(" Native Agent TUI ", theme.heading()),
                        Span::styled(concat!(env!("CARGO_PKG_VERSION"), " "), theme.faint()),
                    ])
                }),
        ),
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

    let workspace = workspace_layout(
        chunks[1],
        !snapshot.agents.is_empty(),
        wants_inspector(snapshot, local),
    );
    let (agents_area, conversation_area) = (workspace.agents, workspace.conversation);
    if let Some(inspector) = workspace.inspector {
        requests::draw_inspector(frame, inspector, snapshot, local);
    }
    if let Some(agents_area) = agents_area {
        workflow_view::draw_agents(
            frame,
            agents_area,
            snapshot,
            selected_agent.map(|agent| agent.info.id.as_str()),
            area.height <= 16,
        );
    }
    let (width, height) = conversation_content_size(conversation_area);
    let projected = local.conversation_cache.borrow_mut().viewport(
        snapshot
            .messages
            .iter()
            .filter(|message| message.thread_id == selected_thread),
        local.conversation_focus.as_ref(),
        width,
        height,
        local.scroll_from_bottom,
        local.tools_expanded,
    );
    let focused_row = projected.transcript.focused;
    let foldable_tools = projected.transcript.foldable > 0;
    let below = projected.below;
    local
        .conversation_viewport
        .set(super::ConversationViewport {
            area: conversation_area,
            scroll: below,
        });
    let mut transcript = projected.transcript.lines;
    if transcript.is_empty() {
        transcript.push(
            Line::from(if selected_agent.is_some() {
                tr("Waiting for child output.", "等待子 Agent 输出。")
            } else {
                tr(
                    "Type a task below and press Enter.",
                    "在下方输入任务，按 Enter 发送。",
                )
            })
            .style(Style::default().fg(theme.foreground)),
        );
        // 空会话顺带给出最常用的入口；高度不够时只留第一句。
        if selected_agent.is_none() && height >= 7 && width >= 40 {
            transcript.push(Line::default());
            for (key, label) in [
                ("Ctrl+P", tr("commands and panels", "命令与面板")),
                ("Ctrl+F", tr("search this conversation", "搜索当前对话")),
                ("Ctrl+O", tr("new line in the task", "任务内换行")),
                ("F1", tr("all keys", "全部快捷键")),
            ] {
                transcript.push(Line::from(vec![
                    Span::styled(format!("{key:<8}"), theme.key()),
                    Span::styled(label, theme.secondary()),
                ]));
            }
        }
    }
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
    let mut title = vec![
        Span::styled(format!(" {}", display_text(name)), theme.label()),
        Span::styled(tr(" · F3 switch", " · F3 切换"), theme.secondary()),
    ];
    if snapshot.history_truncated {
        title.push(Span::styled(
            tr(" [older content truncated]", " [更早的内容已截断]"),
            Style::default().fg(theme.warning),
        ));
    }
    if local.conversation_focus.is_some() && focused_row.is_none() {
        title.push(Span::styled(
            tr(
                " [search content changed/evicted]",
                " [搜索命中的内容已变化或被移除]",
            ),
            Style::default().fg(theme.warning),
        ));
    }
    if below > 0 {
        title.push(Span::styled(" · ", theme.faint()));
        title.push(Span::styled(
            if lang::zh() {
                format!("下方还有 {below} 行，Ctrl+End 回到最新")
            } else {
                format!("{below} more rows below, Ctrl+End latest")
            },
            theme.heading(),
        ));
    }
    title.push(Span::raw(" "));
    let visible: Vec<_> = transcript
        .into_iter()
        .enumerate()
        .take(height)
        .map(|(row, text)| {
            if Some(row) == focused_row {
                Line::from(
                    text.spans
                        .into_iter()
                        .map(|span| Span::styled(span.content, theme.selected()))
                        .collect::<Vec<_>>(),
                )
                .style(theme.selected())
            } else {
                text
            }
        })
        .collect();
    frame.render_widget(
        Paragraph::new(visible).block(if conversation_area.height < 3 {
            Block::default()
        } else {
            theme::panel()
                .borders(Borders::TOP)
                .padding(Padding::new(1, 1, 0, 1))
                .title(Line::from(title))
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

    let status_color = if request.is_some() {
        theme.warning
    } else if snapshot.last_error.is_some() {
        theme.error
    } else {
        theme.muted
    };
    // 引导条占两格；先按扣除后的宽度换行，内容不会被右边界裁掉。
    let gutter = if chunks[2].width > 12 { 2 } else { 0 };
    let request_actions = request.map(|request| requests::actions(snapshot, local, request));
    let status_width = (chunks[2].width as usize).saturating_sub(gutter);
    // 动作键整项换行：一行放不下时按 ` | ` 分组折到下一行，不在词中间断开。
    let (prose, action_rows) = match request_actions
        .as_deref()
        .and_then(|actions| Some((activity.strip_suffix(actions)?, actions)))
    {
        Some((prose, actions)) => {
            let mut rows: Vec<String> = Vec::new();
            for action in actions.split(" | ") {
                match rows.last_mut() {
                    Some(row) if row.width() + 5 + action.width() <= status_width => {
                        row.push_str(" | ");
                        row.push_str(action);
                    }
                    _ => rows.push(action.to_owned()),
                }
            }
            (prose.trim_end_matches('\n'), rows)
        }
        None => (activity.as_str(), Vec::new()),
    };
    let lines: Vec<_> = wrap_text(prose, status_width)
        .into_iter()
        .map(|text| (text, false))
        .chain(action_rows.into_iter().map(|text| (text, true)))
        .enumerate()
        .map(|(index, (text, is_actions))| {
            let mut line = if is_actions {
                theme::hints(&text)
            } else if index == 0 && (request.is_some() || snapshot.last_error.is_some()) {
                Line::from(Span::styled(
                    text,
                    Style::default()
                        .fg(status_color)
                        .add_modifier(Modifier::BOLD),
                ))
            } else if request.is_some() {
                Line::from(Span::styled(text, Style::default().fg(theme.foreground)))
            } else {
                Line::from(Span::styled(text, Style::default().fg(status_color)))
            };
            if gutter > 0 {
                line.spans
                    .insert(0, Span::styled("▎ ", Style::default().fg(status_color)));
            }
            line
        })
        .collect();
    frame.render_widget(
        Paragraph::new(lines).block(
            theme::panel()
                .borders(Borders::TOP)
                .border_style(Style::default().fg(if request.is_some() {
                    theme.warning
                } else {
                    theme.border
                }))
                .title_style(if request.is_some() {
                    Style::default()
                        .fg(theme.warning)
                        .add_modifier(Modifier::BOLD)
                } else {
                    theme.secondary()
                })
                .title(if request.is_some() {
                    tr(" Pending request ", " 待处理请求 ")
                } else {
                    tr(" Status ", " 状态 ")
                }),
        ),
        chunks[2],
    );

    let answering = request.is_some_and(|r| matches!(r.kind, RequestKind::UserInput { .. }));
    let approving = request.is_some_and(|r| !answering && !request_locked(snapshot, local, r));
    let input = editor_view(snapshot, local, chunks[3].width);
    let prompt = editor_prompt(chunks[3].width);
    let input_height = chunks[3].height.saturating_sub(2) as usize;
    let input_start = input
        .cursor
        .1
        .saturating_sub(input_height.saturating_sub(1));
    let prompt_span = |row: usize| {
        Span::styled(
            if row == 0 { "› " } else { "  " }
                .chars()
                .take(prompt)
                .collect::<String>(),
            theme.heading(),
        )
    };
    let display = if local.editor.text.is_empty() {
        vec![Line::from(vec![
            prompt_span(0),
            Span::styled(
                // 只有 UserInput 请求才把输入框当作回答；审批用快捷键决定。
                if answering {
                    tr("Write an answer…", "输入回答…")
                } else {
                    tr(
                        "Write a task…  Ctrl+O for a new line",
                        "输入任务…  Ctrl+O 换行",
                    )
                },
                theme.secondary(),
            ),
        ])]
    } else {
        input
            .lines
            .iter()
            .enumerate()
            .skip(input_start)
            .take(input_height)
            .map(|(row, line)| Line::from(vec![prompt_span(row), Span::raw(line.clone())]))
            .collect()
    };
    frame.render_widget(
        Paragraph::new(display).block(theme::focus_panel().borders(Borders::ALL).title(
            if answering {
                tr(" Answer ", " 回答 ")
            } else if waiting.is_some() {
                tr(" Root task · Enter to queue ", " 任务 · Enter 加入队列 ")
            } else {
                tr(" Root task ", " 任务 ")
            },
        )),
        chunks[3],
    );
    if !answering {
        let hints = super::commands::slash_hints(&local.editor.text, &snapshot.skills.entries);
        super::commands::draw_slash_hints(frame, chunks[3], &hints, local.slash_selected);
    }
    if input_height > 0 && chunks[3].width > 2 {
        frame.set_cursor_position((
            chunks[3].x
                + 1
                + (prompt + input.cursor.0).min(chunks[3].width.saturating_sub(3) as usize) as u16,
            chunks[3].y + 1 + input.cursor.1.saturating_sub(input_start) as u16,
        ));
    }
    let hints = if local.workflow.visible {
        tr(
            "Up/Down select  F5 workflow  F6 pause  F7 cancel  F8 retry  +/- priority  F9 stop",
            "Up/Down 选择  F5 工作流  F6 暂停  F7 取消  F8 重试  +/- 优先级  F9 停止",
        )
        .to_owned()
    } else if local.evidence {
        tr(
            "Ctrl+W wait/restore  PgUp/PgDn scroll  F11 close  F3 agent",
            "Ctrl+W 暂不提醒/恢复  PgUp/PgDn 滚动  F11 关闭  F3 切换 Agent",
        )
        .to_owned()
    } else {
        // 按重要程度排列；放不下时从最不重要的开始省略，完整列表在 F1。
        let mut hints: Vec<(&str, u8)> = Vec::new();
        if approving {
            // 审批的决定键放在最前面；Enter 仍然只提交输入框里的新任务。
            hints.extend([
                (tr("Ctrl+Y accept", "Ctrl+Y 接受"), 0),
                (tr("Ctrl+N decline", "Ctrl+N 拒绝"), 0),
                (tr("F2 details", "F2 详情"), 2),
            ]);
        } else if answering {
            hints.extend([
                (tr("Enter answer", "Enter 回答"), 0),
                (tr("F2 details", "F2 详情"), 2),
            ]);
        } else {
            hints.push((tr("Enter send", "Enter 发送"), 0));
        }
        hints.extend([
            (tr("Ctrl+P commands", "Ctrl+P 命令"), 3),
            (tr("Ctrl+F search", "Ctrl+F 搜索"), 4),
        ]);
        if foldable_tools {
            hints.push((
                if local.tools_expanded {
                    tr("Ctrl+E fold tools", "Ctrl+E 折叠工具")
                } else {
                    tr("Ctrl+E unfold tools", "Ctrl+E 展开工具")
                },
                5,
            ));
        }
        hints.extend([
            (tr("F1 keys", "F1 快捷键"), 1),
            (tr("Ctrl+Q quit", "Ctrl+Q 退出"), 2),
        ]);
        let room = chunks[4].width.saturating_sub(1) as usize;
        let needed = |hints: &[(&str, u8)]| {
            hints.iter().map(|(text, _)| text.width()).sum::<usize>()
                + hints.len().saturating_sub(1) * 5
        };
        while needed(&hints) > room {
            let Some(least) = hints.iter().map(|&(_, rank)| rank).max().filter(|&r| r > 0) else {
                break;
            };
            hints.retain(|&(_, rank)| rank != least);
        }
        if approving && needed(&hints) > room {
            // 极窄窗口也要同时看到接受和拒绝两个键。
            hints = vec![
                (tr("Ctrl+Y ok", "Ctrl+Y 接受"), 0),
                (tr("Ctrl+N no", "Ctrl+N 拒绝"), 0),
            ];
        }
        hints
            .iter()
            .map(|&(text, _)| text)
            .collect::<Vec<_>>()
            .join(" | ")
    };
    let mut hint_line = theme::hints(&hints);
    hint_line.spans.insert(0, Span::raw(" "));
    frame.render_widget(Paragraph::new(hint_line), chunks[4]);
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

/// 宽屏详情栏只在有选中的待处理请求、且没有打开完整请求面板时出现。
/// 渲染和搜索定位都用它决定对话区宽度。
pub(super) fn wants_inspector(snapshot: &CoreSnapshot, local: &LocalState) -> bool {
    !local.request_panel && selected_request(snapshot, local).is_some()
}

/// 输入框内提示符占用的列数；极窄时不显示，把宽度留给草稿。
fn editor_prompt(width: u16) -> usize {
    if width >= 20 {
        2
    } else {
        0
    }
}

pub(super) fn editor_view(
    snapshot: &CoreSnapshot,
    local: &LocalState,
    width: u16,
) -> super::editor::EditorView {
    let secret = selected_request(snapshot, local).is_some_and(|r| matches!(&r.kind,
        RequestKind::UserInput { questions } if questions.get(local.question_index).is_some_and(|q| q.is_secret)));
    local.editor.view(
        (width.saturating_sub(2) as usize).saturating_sub(editor_prompt(width)),
        secret,
    )
}

/// Status panel text shared by rendering and scroll geometry; empty hides the panel.
pub(super) fn status_text(snapshot: &CoreSnapshot, local: &LocalState) -> String {
    let request = selected_request(snapshot, local);
    let waiting = snapshot.gate.as_ref().filter(|gate| gate.pending);
    // 错误原因始终保留在提示前面，后续提示不能把它遮住。
    let error = snapshot.last_error.as_ref().map(|error| {
        if snapshot.phase == SessionPhase::Failed && !snapshot.startup_blocked {
            if lang::zh() {
                format!("本轮失败：{error}\n输入新任务即可继续；F8 重试失败的工作流任务。")
            } else {
                format!(
                    "Turn failed: {error}
Enter a new task to continue; F8 retries a failed workflow task."
                )
            }
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
        match snapshot.scheduler.queued_roots {
            count if lang::zh() => format!("{count} 个任务等待开始；F4 查看队列。"),
            1 => "1 task waiting to start; F4 shows the queue.".into(),
            count => format!("{count} tasks waiting to start; F4 shows the queue."),
        }
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

#[cfg(test)]
mod tests {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use ratatui::{backend::TestBackend, Terminal};

    use unicode_width::UnicodeWidthStr;

    use super::super::{handle_key, lang, sync_local_requests, LocalState};
    use super::draw;
    use crate::interactions::RequestView;
    use crate::protocol::RpcId;
    use crate::state::{ConversationItem, CoreSnapshot, SessionPhase};

    fn rows(snapshot: &CoreSnapshot, local: &LocalState, width: u16, height: u16) -> Vec<String> {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|frame| draw(frame, snapshot, local)).unwrap();
        let buffer = terminal.backend().buffer();
        (0..height)
            .map(|y| {
                // 双宽字符占两格，第二格是占位，拼接文本时跳过。
                let mut row = String::new();
                let mut x = 0;
                while x < width {
                    let symbol = buffer[(x, y)].symbol();
                    row.push_str(symbol);
                    x += (symbol.width() as u16).max(1);
                }
                row
            })
            .collect()
    }

    fn running(requests: Vec<RequestView>) -> (CoreSnapshot, LocalState) {
        let snapshot = CoreSnapshot {
            phase: SessionPhase::Running,
            thread_id: Some("root".into()),
            requests,
            ..Default::default()
        };
        let mut local = LocalState::default();
        sync_local_requests(&mut local, &snapshot);
        (snapshot, local)
    }

    fn approval() -> RequestView {
        RequestView::decode(
            RpcId::Number(1),
            "item/commandExecution/requestApproval",
            &serde_json::json!({"threadId":"root","turnId":"turn","command":"cargo check"}),
        )
        .unwrap()
    }

    #[test]
    fn approval_keeps_the_task_prompt_and_only_user_input_asks_for_an_answer() {
        let (snapshot, local) = running(vec![approval()]);
        let screen = rows(&snapshot, &local, 120, 30).join("\n");
        assert!(!screen.contains("Write an answer"), "{screen}");
        assert!(screen.contains("Write a task"), "{screen}");
        assert!(screen.contains(" Root task "), "{screen}");

        let question = RequestView::decode(
            RpcId::Number(2),
            "item/tool/requestUserInput",
            &serde_json::json!({"threadId":"root","turnId":"turn","questions":[{
                "id":"name","header":"Name","question":"Which name?"}]}),
        )
        .unwrap();
        let (snapshot, local) = running(vec![question]);
        let screen = rows(&snapshot, &local, 120, 30).join("\n");
        assert!(screen.contains("Write an answer"), "{screen}");
        assert!(screen.contains(" Answer "), "{screen}");
        assert!(screen.contains("Enter answer"), "{screen}");
    }

    #[test]
    fn key_hints_fit_the_row_and_keep_the_decision_keys_at_every_width() {
        for width in [24, 40, 64, 65, 80, 100, 120, 160] {
            for requests in [vec![], vec![approval()]] {
                let approving = !requests.is_empty();
                let (snapshot, local) = running(requests);
                let all = rows(&snapshot, &local, width, 30);
                let hints = all.last().unwrap();
                // 行尾是完整的标签，说明这一行没有被右边界裁掉。
                assert!(
                    ["send", "keys", "quit", "decline", "details", "no"]
                        .iter()
                        .any(|label| hints.trim_end().ends_with(label)),
                    "{width}: {hints:?}"
                );
                if approving {
                    assert!(
                        hints.contains("Ctrl+Y") && hints.contains("Ctrl+N"),
                        "{width}: {hints:?}"
                    );
                } else {
                    assert!(hints.contains("Enter send"), "{width}: {hints:?}");
                }
                assert!(!hints.contains("Ctrl+Q") || hints.contains("Ctrl+Q quit"));
            }
        }
    }

    #[test]
    fn status_text_wraps_at_words_inside_its_gutter() {
        let (mut snapshot, local) = running(vec![]);
        snapshot.phase = SessionPhase::Failed;
        snapshot.last_error = Some(
            "Connection interrupted. Review the last confirmed result before continuing.".into(),
        );
        let screen = rows(&snapshot, &local, 60, 24);
        let first = screen
            .iter()
            .position(|row| row.contains("Turn failed"))
            .unwrap();
        assert!(screen[first].starts_with("▎ Turn failed"), "{screen:?}");
        assert!(screen[first].trim_end().ends_with("last"), "{screen:?}");
        assert!(
            screen[first + 1].starts_with("▎ confirmed result before continuing."),
            "{screen:?}"
        );
    }

    #[test]
    fn conversation_shows_reasoning_and_tool_updates_without_stale_cached_rows() {
        let (mut snapshot, local) = running(vec![]);
        for (id, role, text, thread) in [
            ("shared", "Reasoning", "Inspect the input", "root"),
            ("tools:turn", "Tool", "▸ running · echo probe", "root"),
            ("shared", "Agent", "Here is the answer", "root"),
            ("shared", "Reasoning", "Other agent thought", "child"),
        ] {
            snapshot.messages.push(ConversationItem {
                id: id.into(),
                thread_id: thread.into(),
                turn_id: "turn".into(),
                role: role.into(),
                text: text.into(),
                complete: false,
                truncated: false,
                tools: Vec::new(),
            });
        }
        let screen = rows(&snapshot, &local, 120, 30).join("\n");
        assert!(
            screen.contains("Thinking") && screen.contains("Inspect the input"),
            "{screen}"
        );
        assert!(screen.contains("running · echo probe"), "{screen}");
        assert!(screen.contains("Here is the answer"), "{screen}");
        assert!(!screen.contains("Other agent thought"), "{screen}");
        snapshot.messages[0].text.push_str(" carefully");
        snapshot.messages[1].text = "▸ failed · exit 1 · echo probe".into();
        let screen = rows(&snapshot, &local, 120, 30).join("\n");
        assert!(screen.contains("Inspect the input carefully"), "{screen}");
        assert!(screen.contains("failed · exit 1 · echo probe"), "{screen}");
        assert!(!screen.contains("running · echo probe"), "{screen}");
        lang::with_zh(|| {
            let screen = rows(&snapshot, &local, 120, 30).join("\n");
            assert!(screen.contains("思考过程"), "{screen}");
        });
    }

    #[test]
    fn ctrl_e_unfolds_tool_groups_and_leaves_the_draft_alone() {
        let (mut snapshot, mut local) = running(vec![]);
        snapshot.messages.push(ConversationItem {
            id: "tools:turn".into(),
            thread_id: "root".into(),
            turn_id: "turn".into(),
            role: "Tool".into(),
            text: (1..=6)
                .map(|index| format!("▸ done · exit 0 · step-{index}"))
                .collect::<Vec<_>>()
                .join("\n"),
            complete: true,
            truncated: false,
            tools: (1..=6)
                .map(|index| crate::tool_details::ConversationTool {
                    item_id: index.to_string(),
                    category: crate::protocol::ToolCategory::Shell,
                    lifecycle: crate::tool_details::ToolLifecycle::Completed,
                    name: None,
                    command: None,
                    cwd: None,
                    target: None,
                    exit_code: Some(0),
                    output_lines: None,
                    added_lines: None,
                    removed_lines: None,
                })
                .collect(),
        });
        local.editor.insert("draft");
        let screen = rows(&snapshot, &local, 120, 30).join("\n");
        assert!(screen.contains("6 tool calls · 6 done"), "{screen}");
        assert!(
            screen.contains("step-6") && !screen.contains("step-2"),
            "{screen}"
        );
        assert!(screen.contains("Ctrl+E unfold tools"), "{screen}");

        let (tx, mut rx) = tokio::sync::mpsc::channel(4);
        let toggle = KeyEvent::new(KeyCode::Char('e'), KeyModifiers::CONTROL);
        assert!(!handle_key(toggle, &snapshot, &mut local, &tx));
        let screen = rows(&snapshot, &local, 120, 30).join("\n");
        assert!(
            (1..=6).all(|index| screen.contains(&format!("step-{index}"))),
            "{screen}"
        );
        assert!(screen.contains("Ctrl+E fold tools"), "{screen}");
        assert_eq!(local.editor.text, "draft");
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn request_panel_scroll_stays_in_range_so_page_up_responds_after_the_end() {
        let (snapshot, mut local) = running(vec![approval()]);
        local.request_panel = true;
        local.viewport = ratatui::layout::Rect::new(0, 0, 60, 16);
        let max = super::requests::max_scroll(&snapshot, &local, local.viewport);
        assert!(max > 2, "fixture must overflow the panel: {max}");
        let (tx, _rx) = tokio::sync::mpsc::channel(4);
        let press = |code, modifiers, local: &mut LocalState| {
            handle_key(KeyEvent::new(code, modifiers), &snapshot, local, &tx);
        };
        press(KeyCode::End, KeyModifiers::CONTROL, &mut local);
        assert_eq!(local.request_scroll, max);
        for _ in 0..5 {
            press(KeyCode::PageDown, KeyModifiers::NONE, &mut local);
        }
        assert_eq!(local.request_scroll, max);
        press(KeyCode::PageUp, KeyModifiers::NONE, &mut local);
        assert_eq!(local.request_scroll, max - 1);
    }

    #[test]
    fn chinese_interface_translates_chrome_and_keeps_core_text_as_written() {
        let (mut snapshot, local) = running(vec![approval()]);
        snapshot.messages.push(ConversationItem {
            id: "user-1".into(),
            thread_id: "root".into(),
            turn_id: String::new(),
            role: "You".into(),
            text: "Review the layout".into(),
            complete: true,
            truncated: false,
            tools: Vec::new(),
        });
        for width in [40, 64, 100, 140] {
            let screen = lang::with_zh(|| rows(&snapshot, &local, width, 30));
            let text = screen.join("\n");
            for expected in [
                "运行中",
                "待处理请求",
                "输入任务",
                "Ctrl+Y 接受",
                "Ctrl+N 拒绝",
            ] {
                assert!(
                    text.contains(expected),
                    "{width}: missing {expected}\n{text}"
                );
            }
            for english in ["Root task", "Pending request", "Enter send", "accept"] {
                assert!(!text.contains(english), "{width}: {english}\n{text}");
            }
            // 用户写的内容和 Core 给出的命令原样显示。
            assert!(text.contains("Review the layout"), "{text}");
            assert!(text.contains("cargo check"), "{text}");
            assert!(
                screen.iter().any(|row| row.trim_start().starts_with('你')),
                "{text}"
            );
            // 动作键整项换行，不会把“停止本轮”拆成两行。
            assert!(text.contains("Ctrl+B 停止本轮"), "{width}\n{text}");
            // 双宽文案不会把任何一行撑出终端宽度。
            assert!(
                screen.iter().all(|row| row.width() <= width as usize),
                "{width}\n{text}"
            );
        }
        let english = rows(&snapshot, &local, 100, 30).join("\n");
        assert!(english.contains(" Root task ") && english.contains("Ctrl+Y accept"));
    }

    #[test]
    fn wide_terminals_show_request_details_beside_the_conversation() {
        let (mut snapshot, mut local) = running(vec![approval()]);
        snapshot.messages.push(ConversationItem {
            id: "agent-1".into(),
            thread_id: "root".into(),
            turn_id: "turn".into(),
            role: "Agent".into(),
            text: "word ".repeat(400),
            complete: true,
            truncated: false,
            tools: Vec::new(),
        });
        let wide = rows(&snapshot, &local, 160, 40);
        let title = wide
            .iter()
            .find(|row| row.contains(" Request details "))
            .expect("inspector title");
        // 按字符列比较：各行的多字节边框字符数量不同，字节偏移不能跨行使用。
        let column = title[..title.find(" Request details ").unwrap()]
            .chars()
            .count();
        let side = |row: &String| row.chars().skip(column).collect::<String>();
        assert!(
            column > 90,
            "inspector sits right of the conversation: {column}"
        );
        // 决策内容在详情栏里；对话正文不会画进详情栏。
        assert!(wide.iter().any(|row| side(row).contains("cargo check")));
        assert!(wide
            .iter()
            .skip(8)
            .take(20)
            .all(|row| !side(row).contains("word word")));

        let medium = rows(&snapshot, &local, 139, 40).join("\n");
        assert!(!medium.contains(" Request details "), "{medium}");
        // 打开完整请求面板后不再重复显示详情栏。
        local.request_panel = true;
        let full = rows(&snapshot, &local, 160, 40).join("\n");
        assert!(!full.contains(" Request details "), "{full}");
        // 没有待处理请求时对话区使用全部宽度。
        let (idle, local) = running(vec![]);
        let idle = rows(&idle, &local, 160, 40).join("\n");
        assert!(!idle.contains(" Request details "), "{idle}");
    }

    #[test]
    fn overlays_cover_the_conversation_without_letting_its_text_show_through() {
        let (mut snapshot, _) = running(vec![]);
        snapshot.messages.push(ConversationItem {
            id: "agent-1".into(),
            thread_id: "root".into(),
            turn_id: "turn".into(),
            role: "Agent".into(),
            text: vec!["LEAKWORD ".repeat(12); 40].join("\n"),
            complete: true,
            truncated: false,
            tools: Vec::new(),
        });
        let plain = rows(&snapshot, &LocalState::default(), 120, 30).join("\n");
        assert!(plain.contains("LEAKWORD"), "fixture must fill the screen");
        for open in ["evidence", "workflow", "context"] {
            let mut local = LocalState::default();
            match open {
                "evidence" => local.evidence = true,
                "workflow" => local.workflow.visible = true,
                _ => local.context.toggle(),
            }
            for (width, height) in [(120, 30), (80, 24)] {
                let screen = rows(&snapshot, &local, width, height).join("\n");
                assert!(
                    !screen.contains("LEAKWORD"),
                    "{open} {width}x{height}\n{screen}"
                );
            }
        }
    }

    #[test]
    fn empty_session_lists_entry_points_only_when_there_is_room() {
        let snapshot = CoreSnapshot {
            phase: SessionPhase::Ready,
            thread_id: Some("root".into()),
            ..Default::default()
        };
        let local = LocalState::default();
        let roomy = rows(&snapshot, &local, 80, 24).join("\n");
        assert!(
            roomy.contains("Type a task below and press Enter."),
            "{roomy}"
        );
        assert!(roomy.contains("Ctrl+P  commands and panels"), "{roomy}");
        let tight = rows(&snapshot, &local, 80, 12).join("\n");
        assert!(
            tight.contains("Type a task below and press Enter."),
            "{tight}"
        );
        assert!(!tight.contains("commands and panels"), "{tight}");
    }
}
