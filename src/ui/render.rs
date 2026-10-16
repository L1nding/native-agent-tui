//! Read-only terminal rendering composed from a Core snapshot and local UI state.

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Padding, Paragraph};
use unicode_width::UnicodeWidthStr;

use super::activity::{
    draw_evidence, evidence_brief, focus_activity, reminder_brief, token_budget_brief,
    truncate_display_label, usage_fact_for,
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
    local.extension.clear_displayed();
    let mut area = frame.area();
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
    let mut extension_context = super::extensions::ViewContext::new(
        snapshot,
        &local.extension,
        &local.editor.text,
        local.notice.as_deref(),
    );
    extension_context.activity_marker = local.activity.marker();
    if !local.projects.visible
        && !local.workflow.visible
        && !local.help
        && !local.evidence
        && local.attention_editor.is_none()
        && !local.request_panel
        && super::extensions::REGISTRY.draw(frame, snapshot, &extension_context)
    {
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
    if let Some(selection) = snapshot
        .extensions
        .iter()
        .find_map(crate::extensions::ExtensionSnapshot::modeling)
        .and_then(|domain| domain.preview_selection.as_ref())
        .filter(|s| !s.objects.is_empty())
    {
        if area.height > 22 {
            let banner = ratatui::layout::Rect { height: 5, ..area };
            let text = selection
                .details()
                .into_iter()
                .take(4)
                .collect::<Vec<_>>()
                .join("\n");
            frame.render_widget(
                Paragraph::new(display_text(&text))
                    .style(theme.secondary())
                    .block(Block::default().borders(Borders::BOTTOM)),
                banner,
            );
            area.y += 5;
            area.height -= 5;
        }
    }
    if let Some(domain) = super::domain_workflow::domain(snapshot)
        .filter(|domain| {
            area.height > 25
                && (domain.floor_progress.is_some()
                    || domain.groups.is_some()
                    || domain.ledger.is_some()
                    || domain.linework.is_some())
        })
        .filter(|_| !local.workflow.visible && !local.help && !local.projects.visible)
    {
        let mut lines = super::domain::floor_progress_lines(domain);
        let footer = lines.split_off(lines.len().saturating_sub(2));
        let mut lines: Vec<_> = lines
            .into_iter()
            .flat_map(|line| wrap_text(&line.to_string(), usize::from(area.width)))
            .map(Line::from)
            .collect();
        let footer: Vec<_> = footer
            .into_iter()
            .flat_map(|line| wrap_text(&line.to_string(), usize::from(area.width)))
            .map(Line::from)
            .collect();
        let budget = usize::from((area.height / 3).min(10));
        if lines.len() + footer.len() > budget {
            lines.truncate(budget.saturating_sub(footer.len() + 1));
            lines.push(Line::from("其余类别见 F6 详情"));
        }
        lines.extend(footer);
        if lines.len() > budget {
            lines.truncate(budget.saturating_sub(1));
            lines.push(Line::from("进度表未完整显示，F6 查看详情"));
        }
        let height = lines.len() as u16 + 1;
        frame.render_widget(
            Paragraph::new(lines).block(Block::default().borders(Borders::BOTTOM)),
            ratatui::layout::Rect { height, ..area },
        );
        area.y += height;
        area.height -= height;
    }
    // Modeling sessions keep the current verified workflow stage visible.
    if let Some(line) = super::domain_workflow::domain(snapshot)
        .filter(|_| area.height > 16 && !local.workflow.visible)
        .and_then(|domain| super::domain_workflow::progress_line(domain, usize::from(area.width)))
    {
        frame.render_widget(
            Paragraph::new(line).block(Block::default().borders(Borders::BOTTOM)),
            ratatui::layout::Rect { height: 2, ..area },
        );
        area.y += 2;
        area.height -= 2;
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
    if snapshot.scheduler.stopping {
        status.push(tr("stopping", "正在停止").into());
    } else if snapshot.scheduler.paused {
        status.push(tr("dispatch paused", "已暂停派发").into());
    }
    let status = status.join(" | ");
    let mut settings = snapshot.cwd.clone();
    if let Some(branch) = &snapshot.branch {
        settings.push_str(" | ");
        settings.push_str(branch);
    }
    settings.push_str(" | ");
    settings.push_str(
        snapshot
            .model
            .as_deref()
            .unwrap_or(tr("model pending", "模型待定")),
    );
    settings.push_str(" | ");
    settings.push_str(&token_budget_brief(snapshot).replacen("/unavailable", "", 1));
    let usage = usage_fact_for(snapshot, None).map(|fact| fact.summary);
    settings.push_str(" | ctx:");
    match usage
        .and_then(|usage| usage.input_tokens.zip(usage.context_window))
        .filter(|(_, window)| *window > 0)
    {
        Some((input, window)) => settings.push_str(&format!(
            "{input}/{window} ({:.0}%)",
            input as f64 / window as f64 * 100.0
        )),
        None => settings.push_str("unavailable"),
    }
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
    let settings = display_text(&settings);
    let parts: Vec<_> = settings.split(" | ").collect();
    let widths: Vec<_> = parts.iter().map(|part| part.width()).collect();
    let room = (chunks[5].width as usize).saturating_sub(3 * parts.len().saturating_sub(1));
    // Share a narrow strip across the fields so a long directory cannot hide model/usage.
    let (mut low, mut high) = (0, widths.iter().copied().max().unwrap_or(0));
    while low < high {
        let middle = low + (high - low).div_ceil(2);
        if widths
            .iter()
            .map(|width| (*width).min(middle))
            .sum::<usize>()
            <= room
        {
            low = middle;
        } else {
            high = middle - 1;
        }
    }
    let mut settings_spans = Vec::new();
    for (index, part) in parts.into_iter().enumerate() {
        if index > 0 {
            settings_spans.push(Span::styled(" | ", theme.faint()));
        }
        settings_spans.push(Span::styled(
            truncate_display_label(part, low),
            if index == 0 {
                Style::default().fg(theme.foreground)
            } else {
                theme.secondary()
            },
        ));
    }
    frame.render_widget(Paragraph::new(Line::from(settings_spans)), chunks[5]);
    let compact_header = area.height <= 16 && selected_activity.is_some();
    if compact_header {
        header.remove(0);
    }
    let mut header_block = theme::panel()
        .borders(Borders::BOTTOM)
        .padding(Padding::horizontal(1));
    // 产品名不是执行事实：平时没有标题行，这一行留给对话。只有矮窗口里
    // 把阶段和计数压进标题时才用它。
    if compact_header {
        header_block = header_block.title(Line::from(Span::styled(
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
        )));
    }
    frame.render_widget(Paragraph::new(header).block(header_block), chunks[0]);

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

    let workspace_area = if selected_agent.is_none()
        && !local.projects.visible
        && !local.workflow.visible
        && !local.evidence
        && !local.help
        && local.attention_editor.is_none()
        && !local.request_panel
        && snapshot.requests.is_empty()
    {
        super::extensions::REGISTRY.draw_conversation_card(
            frame,
            chunks[1],
            snapshot,
            &local.extension,
        )
    } else {
        chunks[1]
    };
    let workspace = workspace_layout(
        workspace_area,
        !snapshot.agents.is_empty(),
        wants_inspector(snapshot, local),
    );
    let (agents_area, conversation_area) = (workspace.agents, workspace.conversation);
    // todo 面板占用对话区底部几行；只显示 root 会话的列表。
    let todo_lines = if selected_agent.is_none() {
        super::todos::lines(
            &snapshot.todos,
            conversation_area.width,
            conversation_area.height,
        )
    } else {
        Vec::new()
    };
    let (conversation_area, todo_area) = super::todos::split(conversation_area, todo_lines.len());
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
    local
        .conversation_cache
        .borrow_mut()
        .replace_anchors(&snapshot.message_replacements);
    let projected = local.conversation_cache.borrow_mut().viewport(
        snapshot
            .messages
            .iter()
            .filter(|message| message.thread_id == selected_thread),
        local.conversation_focus.as_ref(),
        width,
        height,
        local.scroll_from_bottom,
        local.tool_display(),
    );
    let modeling_review = snapshot.extensions.iter().any(|extension| {
        extension
            .modeling()
            .is_some_and(|domain| domain.enabled && domain.proposal.is_some())
    });
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
                (
                    "Ctrl+O",
                    if modeling_review {
                        "外部打开本版复核图"
                    } else {
                        tr("new line in the task", "任务内换行")
                    },
                ),
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
    if !todo_lines.is_empty() {
        frame.render_widget(Paragraph::new(todo_lines), todo_area);
    }
    if local.workflow.visible {
        if let Some(domain) =
            super::domain_workflow::domain(snapshot).filter(|_| !local.workflow.show_tasks)
        {
            super::domain_workflow::draw(frame, chunks[1], domain, local.workflow.modeling_node);
        } else {
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
            if row == 0 {
                if super::busy::label(snapshot).is_some() {
                    local.activity.marker()
                } else {
                    "› "
                }
            } else {
                "  "
            }
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
                } else if let Some(label) = super::busy::label(snapshot) {
                    label
                } else if modeling_review {
                    "输入任务…  Shift+Enter 换行 · Ctrl+O 打开复核图"
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
        let hints = super::commands::slash_hints_with(
            &local.editor.text,
            &snapshot.skills.entries,
            &snapshot.commands,
        );
        super::commands::draw_slash_hints(frame, chunks[3], &hints, local.slash_selected);
        let paths = super::commands::path_hints(
            &local.editor.text,
            local.editor.cursor,
            &snapshot.path_hints,
        );
        super::commands::draw_path_hints(frame, chunks[3], &paths, local.slash_selected);
    }
    if input_height > 0 && chunks[3].width > 2 {
        frame.set_cursor_position((
            chunks[3].x
                + 1
                + (prompt + input.cursor.0).min(chunks[3].width.saturating_sub(3) as usize) as u16,
            chunks[3].y + 1 + input.cursor.1.saturating_sub(input_start) as u16,
        ));
    }
    let hints = if local.workflow.visible
        && !local.workflow.show_tasks
        && super::domain_workflow::domain(snapshot).is_some()
    {
        "↑↓ 选择节点 · Tab 调度任务 · Enter 具体方案 · Esc 返回".into()
    } else if local.workflow.visible {
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
            // 提示只列当前状态下真的可用的操作。
            use crate::state::SessionPhase::*;
            match snapshot.phase {
                Running => hints.extend([
                    (tr("/steer guide", "/steer 引导"), 0),
                    (tr("Ctrl+S queue", "Ctrl+S 排队"), 7),
                    (tr("Ctrl+C interrupt", "Ctrl+C 中断"), 6),
                ]),
                GatePending => hints.extend([
                    (tr("Enter queue", "Enter 排队"), 0),
                    (tr("Ctrl+C interrupt", "Ctrl+C 中断"), 6),
                ]),
                Ready | Completed | Interrupted | Failed => {
                    hints.push((tr("Enter send", "Enter 发送"), 0))
                }
                Created | Launching | Initializing | CheckingShell | StartingTurn => {
                    hints.push((tr("starting…", "正在启动…"), 0))
                }
                Stopping | ClosingTransport | Stopped | Disconnected | Unknown => {
                    hints.push((tr("session ended", "会话已结束"), 0))
                }
            }
        }
        hints.extend([
            (tr("Ctrl+P commands", "Ctrl+P 命令"), 3),
            (tr("Ctrl+F search", "Ctrl+F 搜索"), 4),
        ]);
        if foldable_tools || local.tools_expanded || local.tools_hidden {
            hints.push((
                match local.tool_display() {
                    super::conversation::ToolDisplay::Preview => {
                        tr("Ctrl+E unfold tools", "Ctrl+E 展开工具")
                    }
                    super::conversation::ToolDisplay::Expanded => {
                        tr("Ctrl+E hide tools", "Ctrl+E 隐藏工具")
                    }
                    super::conversation::ToolDisplay::Hidden => {
                        tr("Ctrl+E show tools", "Ctrl+E 显示工具")
                    }
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
                let values = question.options.as_deref().unwrap_or_default();
                // 面板在标题下只有四行。问题有多行（例如待评审的计划）、可以多选或
                // 选项超过三个时，选项压成带编号的一行放在问题前面，不让它们被挤到
                // 看不见的地方；说明文字留给 F2 详情。
                let compact = !values.is_empty()
                    && (question.multi_select
                        || question.question.contains('\n')
                        || values.len() > 3);
                let choices = if compact {
                    format!(
                        "{}{}\n",
                        if question.multi_select {
                            tr(
                                "Type one or more, separated by commas: ",
                                "输入一项或多项，用逗号分隔：",
                            )
                        } else {
                            tr("Type one: ", "输入其中一项：")
                        },
                        values
                            .iter()
                            .enumerate()
                            .map(|(index, v)| format!("{} {}", index + 1, v.label))
                            .collect::<Vec<_>>()
                            .join(" | ")
                    )
                } else {
                    String::new()
                };
                let options = if compact {
                    String::new()
                } else {
                    values
                        .iter()
                        .enumerate()
                        .map(|(index, v)| {
                            format!("{} {}", requests::option_line(index, v), v.description)
                                .trim_end()
                                .to_owned()
                        })
                        .collect::<Vec<_>>()
                        .join("\n")
                };
                let mut title = agent_label(snapshot, &request.thread_id).to_owned();
                if !question.header.trim().is_empty() {
                    title = format!("{title} · {}", question.header);
                }
                if questions.len() > 1 {
                    title = format!("{title} ({}/{})", local.question_index + 1, questions.len());
                }
                format!("{title}\n{choices}{}\n{options}", question.question)
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
    fn approval_explains_the_wait_and_only_user_input_asks_for_an_answer() {
        let (snapshot, local) = running(vec![approval()]);
        let screen = rows(&snapshot, &local, 120, 30).join("\n");
        assert!(!screen.contains("Write an answer"), "{screen}");
        assert!(screen.contains("Waiting for action"), "{screen}");
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
        assert!(!screen.contains("Type one"), "{screen}");

        // 待评审的计划比面板长：可选的回答仍然在看得见的那几行里。
        let review = RequestView::decode(
            RpcId::Number(3),
            "item/tool/requestUserInput",
            &serde_json::json!({"threadId":"root","turnId":"turn","questions":[{
                "id":"plan","header":"Plan review",
                "question":format!("Approve this plan?\n\n{}", "1. a long step\n".repeat(30)),
                "options":[{"label":"Approve","description":"Leave plan mode"},
                           {"label":"Keep planning","description":"Stay in plan mode"}]}]}),
        )
        .unwrap();
        let (snapshot, local) = running(vec![review]);
        let screen = rows(&snapshot, &local, 120, 30).join("\n");
        assert!(
            screen.contains("Type one: 1 Approve | 2 Keep planning"),
            "{screen}"
        );

        // 模型提的选择题：选项带编号；可多选时说明怎么选。
        let choice = RequestView::decode(
            RpcId::Number(4),
            "item/tool/requestUserInput",
            &serde_json::json!({"threadId":"root","turnId":"turn","questions":[{
                "id":"colour","header":"Colour","question":"Which colours?","multiSelect":true,
                "options":[{"label":"Crimson","description":"Warm"},
                           {"label":"Indigo","description":""}]}]}),
        )
        .unwrap();
        let (snapshot, local) = running(vec![choice]);
        let screen = rows(&snapshot, &local, 120, 30).join("\n");
        assert!(
            screen.contains("Type one or more, separated by commas: 1 Crimson | 2 Indigo"),
            "{screen}"
        );
        // 单选、选项少：逐行列出，带说明；没有标题和题号时不留空位。
        let short = RequestView::decode(
            RpcId::Number(5),
            "item/tool/requestUserInput",
            &serde_json::json!({"threadId":"root","turnId":"turn","questions":[{
                "id":"colour","header":"","question":"Which colour?",
                "options":[{"label":"Crimson","description":"Warm"},
                           {"label":"Indigo","description":""}]}]}),
        )
        .unwrap();
        let (snapshot, local) = running(vec![short]);
        let screen = rows(&snapshot, &local, 120, 30).join("\n");
        assert!(screen.contains("1. Crimson: Warm"), "{screen}");
        assert!(
            screen.contains("2. Indigo") && !screen.contains("2. Indigo:"),
            "{screen}"
        );
        assert!(
            !screen.contains("Type one")
                && !screen.contains("(1/1)")
                && !screen.contains("▎ root · "),
            "{screen}"
        );
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
                    // 运行中的提示以 /steer guide 开头，不再是 Enter send。
                    [
                        "send",
                        "keys",
                        "quit",
                        "decline",
                        "details",
                        "no",
                        "guide",
                        "queue",
                        "interrupt"
                    ]
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
                    // 运行中最重要的操作是引导，它在任何宽度下都保留。
                    assert!(hints.contains("/steer guide"), "{width}: {hints:?}");
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
            // 思考过程是回答里的一节：竖线标出，与回答共用一个标题。
            screen.contains("┆ Inspect the input")
                && screen.lines().filter(|row| row.trim() == "Agent").count() == 1,
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
            // 换界面语言后缓存的行重新排版；思考一节不依赖文字标签。
            assert!(screen.contains("┆ Inspect the input carefully"), "{screen}");
            assert!(screen.contains("运行中"), "{screen}");
        });
    }

    #[test]
    fn key_hints_follow_what_the_session_can_do_now() {
        let (mut snapshot, local) = running(vec![]);
        let hints = |snapshot: &CoreSnapshot| rows(snapshot, &local, 140, 30).pop().unwrap();
        let row = hints(&snapshot);
        assert!(
            row.contains("/steer guide") && row.contains("Ctrl+C interrupt"),
            "{row}"
        );
        assert!(!row.contains("Enter send"), "{row}");
        snapshot.phase = SessionPhase::GatePending;
        let row = hints(&snapshot);
        assert!(
            row.contains("Enter queue") && !row.contains("/steer"),
            "{row}"
        );
        snapshot.phase = SessionPhase::Completed;
        let row = hints(&snapshot);
        assert!(
            row.contains("Enter send") && !row.contains("interrupt"),
            "{row}"
        );
        snapshot.phase = SessionPhase::Disconnected;
        let row = hints(&snapshot);
        assert!(
            row.contains("session ended") && !row.contains("Enter send"),
            "{row}"
        );
        snapshot.phase = SessionPhase::Initializing;
        assert!(hints(&snapshot).contains("starting…"));
    }

    #[test]
    fn metadata_sits_above_input_and_header_keeps_execution_facts() {
        let (mut snapshot, local) = running(vec![]);
        snapshot.cwd = "D:/project/demo".into();
        snapshot.branch = Some("feat/中文-branch".into());
        let screen = rows(&snapshot, &local, 120, 30);
        // 第一行就是执行状态；产品名不占常驻区。
        assert!(screen[0].contains('●'), "{screen:#?}");
        let at = screen
            .iter()
            .position(|row| row.contains("D:/project/demo | feat/中文-branch"))
            .unwrap();
        let input = super::main_layout(
            ratatui::layout::Rect::new(0, 0, 120, 30),
            false,
            super::StatusRows::Hidden,
            1,
        )[3];
        assert_eq!(at + 1, input.y as usize, "{screen:#?}");
        assert!(
            screen[at].contains("model pending | tokens:unavailable | ctx:unavailable"),
            "{screen:#?}"
        );
        assert!(!screen[..at]
            .iter()
            .any(|row| row.contains("D:/project/demo")));
        assert!(
            !screen.iter().any(|row| row.contains("Native Agent TUI")),
            "{screen:#?}"
        );
        // 不是仓库时不显示分支，也不留占位。
        snapshot.branch = None;
        let screen = rows(&snapshot, &local, 120, 30);
        assert!(
            screen[at].contains("D:/project/demo | model pending"),
            "{screen:#?}"
        );
    }

    #[test]
    fn terminal_diff_and_generic_cards_render_all_three_modes_and_unicode() {
        use super::super::conversation::ToolDisplay;
        use crate::protocol::ToolCategory;
        use crate::tool_details::{ConversationTool, ToolLifecycle};
        for (category, kind) in [
            (ToolCategory::Shell, "terminal"),
            (ToolCategory::File, "diff"),
            (ToolCategory::Mcp, "tool"),
        ] {
            let (mut snapshot, mut local) = running(vec![]);
            snapshot.messages.push(ConversationItem {
                id: "tool-group".into(),
                thread_id: "root".into(),
                turn_id: "turn".into(),
                role: "Tool".into(),
                text: "▸ done · card".into(),
                complete: true,
                truncated: false,
                tools: vec![ConversationTool {
                    item_id: "card".into(),
                    category,
                    lifecycle: ToolLifecycle::Completed,
                    name: Some("测试👩‍💻".into()),
                    command: (category == ToolCategory::Shell).then(|| "echo 中文".into()),
                    cwd: Some("/workspace".into()),
                    target: (category == ToolCategory::File).then(|| "src/中文.rs".into()),
                    exit_code: (category == ToolCategory::Shell).then_some(0),
                    output_lines: Some(5),
                    added_lines: (category == ToolCategory::File).then_some(1),
                    removed_lines: (category == ToolCategory::File).then_some(1),
                    output: Some("first 中文👩‍💻e\u{301}\n-old\n+new\nfourth\nlast".into()),
                    output_clipped: false,
                }],
            });
            for mode in [
                ToolDisplay::Preview,
                ToolDisplay::Expanded,
                ToolDisplay::Hidden,
            ] {
                local.tools_expanded = mode == ToolDisplay::Expanded;
                local.tools_hidden = mode == ToolDisplay::Hidden;
                let screen = rows(&snapshot, &local, 100, 50).join("\n");
                if mode == ToolDisplay::Hidden {
                    assert!(!screen.contains("Tool /"), "{screen}");
                    assert!(!screen.contains("first 中文"), "{screen}");
                    continue;
                }
                // The summary row names the tool; the card title is expanded detail only.
                assert_eq!(
                    screen.contains(&format!("Tool / {kind} / 测试👩‍💻")),
                    mode == ToolDisplay::Expanded,
                    "{screen}"
                );
                assert!(screen.contains("first 中文👩‍💻e\u{301}"), "{screen}");
                if mode == ToolDisplay::Preview {
                    assert!(screen.contains("… +2 lines (Ctrl+E to expand)"), "{screen}");
                    assert!(!screen.contains("fourth"), "{screen}");
                } else {
                    assert!(
                        screen.contains("fourth") && screen.contains("last"),
                        "{screen}"
                    );
                }
                match category {
                    ToolCategory::Shell => assert!(
                        screen.contains("$ echo 中文") && screen.contains("[exit 0]"),
                        "{screen}"
                    ),
                    ToolCategory::File => assert!(
                        screen.contains("src/中文.rs") && screen.contains("└ +1 -1"),
                        "{screen}"
                    ),
                    _ => assert!(
                        screen.contains("-old") && screen.contains("+new"),
                        "{screen}"
                    ),
                }
                let narrow = rows(&snapshot, &local, 40, 200);
                assert!(narrow.iter().all(|row| row.width() <= 40), "{narrow:#?}");
                assert!(narrow.join("").contains("👩‍💻e\u{301}"), "{narrow:#?}");
                let tiny = rows(&snapshot, &local, 18, 30);
                assert!(tiny.iter().any(|row| row.contains("Resize terminal")));
            }
        }
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
                    output: None,
                    output_clipped: false,
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
        assert!(screen.contains("Ctrl+E hide tools"), "{screen}");
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
                "正在等待确认",
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
    fn workflow_panel_is_visible_over_the_modeling_extension() {
        let snapshot = CoreSnapshot {
            extensions: vec![crate::extensions::ExtensionSnapshot::Modeling(
                crate::domain::DomainSnapshot {
                    enabled: true,
                    ..Default::default()
                },
            )],
            ..Default::default()
        };
        let mut local = LocalState {
            extension: super::super::extensions::ExtensionPanel::new(Some(
                crate::extensions::ExtensionId::Modeling,
            )),
            ..Default::default()
        };
        local.workflow.visible = true;
        let screen = rows(&snapshot, &local, 120, 40).join("\n");
        assert!(
            screen.contains("Workflow") || screen.contains("工作流 · F4"),
            "{screen}"
        );
        for label in ["建模工作流", "对象发现与AI识别", "修订/补资料", "↺04"] {
            assert!(screen.contains(label), "{screen}");
        }
        assert!(!screen.contains("方案与执行事实"), "{screen}");
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
