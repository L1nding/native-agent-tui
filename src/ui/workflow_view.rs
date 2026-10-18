use std::collections::{BTreeMap, HashMap, HashSet};

use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Borders, Clear, Padding, Paragraph};
use unicode_width::UnicodeWidthStr;

use super::activity::{
    age, agent_usage_brief, attention_tone, clip_spans, focus_activity, footer, label_mark,
    selected_line, tone, truncate_display_label, usage_fact_for, word_mark, wrap_marked, Mark,
};
use super::lang::{self, tr, zh};
use super::layout::wrap;
use super::theme;
use super::workflow::{
    self, project_workflow, selected_task, task_reference, task_state_label, workflow_conversation,
    ConversationTarget,
};
use crate::agents::AgentSnapshot;
use crate::gate::ChildOutcome;
use crate::observation::AttentionLevel;
use crate::scheduler::{TaskSnapshot, TaskState};
use crate::state::CoreSnapshot;

fn agent_name(agent: &AgentSnapshot) -> &str {
    agent
        .info
        .path
        .as_deref()
        .or(agent.info.nickname.as_deref())
        .unwrap_or(&agent.info.id)
}

/// `Label:` mark for a row. Chinese labels end with a full-width colon, which
/// the shared ASCII-only lookup does not see; English keeps the shared lookup.
pub(super) fn label_mark_l10n(clean: &str) -> Option<Mark> {
    if !zh() {
        return label_mark(clean);
    }
    let line = clean.split('\n').next().unwrap_or("");
    let start = line.len() - line.trim_start().len();
    let (end, colon) = line
        .char_indices()
        .find(|(_, character)| matches!(character, ':' | '：'))?;
    let label = line.get(start..end)?;
    if label.is_empty() || label.chars().count() > 48 || label.contains(['|', '"', '·', '>']) {
        return None;
    }
    Some((start..end + colon.len_utf8(), theme::current().label()))
}

/// Child outcome word. English keeps the variant name.
fn outcome_label(outcome: &ChildOutcome) -> String {
    if !zh() {
        return format!("{outcome:?}");
    }
    match outcome {
        ChildOutcome::Completed => "已完成",
        ChildOutcome::Failed => "失败",
        ChildOutcome::Interrupted => "已中断",
    }
    .to_owned()
}

/// Attention level word in the sidebar. English keeps the variant name.
fn attention_label(level: AttentionLevel) -> String {
    if !zh() {
        return format!("{level:?}");
    }
    match level {
        AttentionLevel::Active => "活跃",
        AttentionLevel::Quiet => "静默",
        AttentionLevel::AttentionNeeded => "需要关注",
        AttentionLevel::RequiresAction => "需要处理",
        AttentionLevel::Unknown => "未知",
        AttentionLevel::Ended => "已结束",
    }
    .to_owned()
}

/// ` / gen N` after a status word; `None` is a generation the UI does not know.
fn generation_suffix(generation: Option<u64>) -> String {
    match (zh(), generation) {
        (false, Some(generation)) => format!(" / gen {generation}"),
        (false, None) => " / gen unavailable".to_owned(),
        (true, Some(generation)) => format!(" / 第 {generation} 代"),
        (true, None) => " / 代不可用".to_owned(),
    }
}

/// The status word shown for an agent and its semantic tone. The word always
/// carries the state; the tone only repeats it.
fn agent_status(agent: &AgentSnapshot) -> (String, Style) {
    let theme = theme::current();
    if agent.awaiting_turn {
        return (tr("starting", "启动中").to_owned(), tone(theme.accent));
    }
    match &agent.outcome {
        None => (tr("running", "运行中").to_owned(), tone(theme.accent)),
        Some(outcome) => (
            outcome_label(outcome),
            tone(match outcome {
                ChildOutcome::Completed => theme.success,
                ChildOutcome::Failed => theme.error,
                ChildOutcome::Interrupted => theme.warning,
            }),
        ),
    }
}

/// A detail row under an agent name. The indent shrinks before the content is
/// clipped, so deep tree levels keep their facts inside the sidebar.
fn detail_line(indent: usize, spans: Vec<Span<'static>>, width: usize) -> Line<'static> {
    let content: usize = spans.iter().map(|span| span.content.width()).sum();
    let indent = indent.min(width.saturating_sub(content));
    let mut row = vec![Span::raw(" ".repeat(indent))];
    row.extend(spans);
    Line::from(clip_spans(row, width))
}

pub(super) fn draw_agents(
    frame: &mut ratatui::Frame<'_>,
    area: ratatui::layout::Rect,
    snapshot: &CoreSnapshot,
    selected_id: Option<&str>,
    compact: bool,
) {
    let theme = theme::current();
    if compact {
        let width = area.width as usize;
        let selected = snapshot
            .agents
            .iter()
            .find(|agent| Some(agent.info.id.as_str()) == selected_id);
        let (name, (status, status_style), generation, usage) = selected.map_or_else(
            || {
                let usage = usage_fact_for(snapshot, None)
                    .and_then(|fact| fact.summary.total_tokens)
                    .map_or_else(
                        || tr("tokens: unavailable", "tokens：不可用").into(),
                        |tokens| format!("tokens:{tokens}"),
                    );
                (
                    "root",
                    (lang::phase(snapshot.phase), theme.phase(snapshot.phase)),
                    None,
                    usage,
                )
            },
            |agent| {
                (
                    agent_name(agent),
                    agent_status(agent),
                    Some(agent.generation),
                    agent_usage_brief(snapshot, agent),
                )
            },
        );
        let agent_label = tr("Agent: ", "Agent：");
        let name =
            truncate_display_label(name, width.saturating_sub(agent_label.width()).clamp(1, 25));
        frame.render_widget(
            Paragraph::new(vec![
                Line::from(clip_spans(
                    vec![
                        Span::styled(agent_label, theme.secondary()),
                        Span::styled(name, theme.label()),
                    ],
                    width,
                )),
                Line::from(clip_spans(
                    vec![
                        Span::styled(tr("State: ", "状态："), theme.secondary()),
                        Span::styled(status, status_style),
                        Span::styled(generation_suffix(generation), theme.secondary()),
                    ],
                    width,
                )),
                Line::from(clip_spans(
                    vec![Span::styled(usage, theme.secondary())],
                    width,
                )),
            ])
            .style(theme.base()),
            area,
        );
        return;
    }
    // 与对话区共用一条顶线，只用右侧细线分栏；完整方框会抢走正文的注意力。
    let block = theme::panel()
        .borders(Borders::TOP | Borders::RIGHT)
        .padding(Padding::new(1, 1, 0, 0))
        .title(tr(" Agents · F3 ", " Agent · F3 "));
    let width = block.inner(area).width as usize;
    let name_line = |selected: bool, spans: Vec<Span<'static>>| {
        if selected {
            selected_line(spans, width)
        } else {
            Line::from(clip_spans(spans, width))
        }
    };
    let mut agents = vec![
        name_line(
            selected_id.is_none(),
            vec![Span::raw(if selected_id.is_none() {
                "> root"
            } else {
                "  root"
            })],
        ),
        detail_line(
            4,
            vec![Span::styled(
                lang::phase(snapshot.phase),
                theme.phase(snapshot.phase),
            )],
            width,
        ),
    ];
    let mut selected_row = None;
    for row in project_agent_tree(&snapshot.agents, snapshot.thread_id.as_deref()) {
        let agent = &snapshot.agents[row.index];
        let (status, status_style) = agent_status(agent);
        let tree_prefix = if row.depth == 1 {
            String::new()
        } else {
            format!("{}+- ", "  ".repeat(row.depth - 2))
        };
        let parent_label = if row.parent_missing {
            tr("[parent unavailable] ", "[父级不可用] ")
        } else if row.relationship_unknown {
            tr("[parent unresolved] ", "[父级未确认] ")
        } else {
            ""
        };
        let selected = Some(agent.info.id.as_str()) == selected_id;
        if selected {
            selected_row = Some(agents.len());
        }
        // 标记、树形缩进和父级标签先占掉宽度，名称只截断剩余部分。
        let marker = if selected { "> " } else { "  " };
        let taken = marker.width() + tree_prefix.width() + parent_label.width();
        let name = truncate_display_label(agent_name(agent), width.saturating_sub(taken).max(1));
        agents.push(name_line(
            selected,
            vec![
                Span::raw(marker),
                Span::styled(tree_prefix, theme.faint()),
                Span::styled(parent_label, Style::default().fg(theme.warning)),
                Span::raw(name),
            ],
        ));
        let indent = 4 + 2 * row.depth.saturating_sub(1);
        if row.depth_truncated {
            agents.push(detail_line(
                2,
                vec![Span::styled(
                    tr("[depth truncated]", "[层级已截断]"),
                    Style::default().fg(theme.warning),
                )],
                width,
            ));
        }
        agents.push(detail_line(
            indent,
            vec![
                Span::styled(status, status_style),
                Span::styled(generation_suffix(Some(agent.generation)), theme.secondary()),
            ],
            width,
        ));
        agents.push(detail_line(
            indent,
            vec![Span::styled(
                agent_usage_brief(snapshot, agent),
                theme.secondary(),
            )],
            width,
        ));
        // 已结束的代理只看上一行的终态；静默时长对它没有意义。
        if let Some(activity) = focus_activity(snapshot, &agent.info.id)
            .filter(|activity| activity.attention.level != AttentionLevel::Ended)
        {
            let level = activity.attention.level;
            // 只有需要人看的等级才着色；其余保持次要文字，避免侧栏变花。
            let level_style = if matches!(
                level,
                AttentionLevel::AttentionNeeded
                    | AttentionLevel::RequiresAction
                    | AttentionLevel::Unknown
            ) {
                attention_tone(level)
            } else {
                theme.secondary()
            };
            agents.push(detail_line(
                indent,
                vec![
                    Span::styled(attention_label(level), level_style),
                    Span::styled(
                        if zh() {
                            format!(" / 已静默 {}", age(activity.silence_ms))
                        } else {
                            format!(" / quiet {}", age(activity.silence_ms))
                        },
                        theme.secondary(),
                    ),
                ],
                width,
            ));
        }
    }
    if snapshot.agents.is_empty() {
        for hint in [
            tr("No child agents yet.", "还没有子 Agent。"),
            tr(
                "Delegated agents appear here.",
                "派发的子 Agent 显示在这里。",
            ),
        ] {
            agents.push(detail_line(
                2,
                vec![Span::styled(hint, theme.secondary())],
                width,
            ));
        }
    }
    let visible_height = area.height.saturating_sub(2) as usize;
    let scroll = selected_row
        .unwrap_or(0)
        .saturating_sub(visible_height / 3)
        .min(agents.len().saturating_sub(visible_height))
        .min(u16::MAX as usize) as u16;
    frame.render_widget(
        Paragraph::new(agents).block(block).scroll((scroll, 0)),
        area,
    );
}

pub(super) fn next_agent_id(snapshot: &CoreSnapshot, current: Option<&str>) -> Option<String> {
    let rows = project_agent_tree(&snapshot.agents, snapshot.thread_id.as_deref());
    let next = current
        .and_then(|id| {
            rows.iter()
                .position(|row| snapshot.agents[row.index].info.id == id)
        })
        .map_or(0, |index| index + 1);
    rows.get(next)
        .map(|row| snapshot.agents[row.index].info.id.clone())
}

pub(super) fn draw_workflow(
    frame: &mut ratatui::Frame<'_>,
    area: ratatui::layout::Rect,
    snapshot: &CoreSnapshot,
    selected_id: Option<crate::scheduler::TaskId>,
    link_cursor: Option<workflow::WorkflowLinkCursor>,
    scroll: usize,
    manual_scroll: bool,
) {
    // 浮层盖在对话区上；先清空，较短的行才不会透出底下的文字。
    frame.render_widget(Clear, area);
    let scheduler = &snapshot.scheduler;
    let selected = selected_task(snapshot, selected_id);
    let workflow_order = project_workflow(snapshot);
    let agent_rows = project_agent_tree(&snapshot.agents, snapshot.thread_id.as_deref())
        .into_iter()
        .filter_map(|row| {
            snapshot
                .agents
                .get(row.index)
                .map(|agent| (agent.info.id.clone(), row))
        })
        .collect();
    let width = area.width.saturating_sub(2).max(1) as usize;
    let viewport = area.height.saturating_sub(2) as usize;
    let by_id = scheduler
        .tasks
        .iter()
        .map(|task| (task.id, task))
        .collect::<BTreeMap<_, _>>();
    let ready = if scheduler.ready_roots.is_empty() {
        tr("none", "无").into()
    } else {
        scheduler
            .ready_roots
            .iter()
            .map(|id| format!("#{}", id.0))
            .collect::<Vec<_>>()
            .join(",")
    };
    // 状态词既写进首行，也用来给首行上色；两处必须是同一个词。
    let paused_word = tr("paused", "已暂停");
    let active_word = tr("dispatch active", "派发中");
    let stopping_word = tr("stopping", "停止中");
    let disconnected_word = tr("disconnected", "已断开");
    let dispatch = if scheduler.paused {
        paused_word
    } else {
        active_word
    };
    let stopping = if scheduler.stopping {
        format!(" · {stopping_word}")
    } else {
        String::new()
    };
    let disconnected = if scheduler.disconnected {
        format!(" · {disconnected_word}")
    } else {
        String::new()
    };
    let mut raw = vec![if zh() {
        format!("工作流：{dispatch}{stopping}{disconnected} | 就绪根任务：{ready}")
    } else {
        format!("Workflow: {dispatch}{stopping}{disconnected} | ready roots: {ready}")
    }];
    for agent in &snapshot.dsh_subagents {
        raw.push(agent.line(std::time::SystemTime::now()));
        if !agent.latest_message.is_empty() {
            raw.push(format!("  {}：{}", agent.id, agent.latest_message));
        }
    }
    let native_reserved = scheduler
        .native_slot_capacity
        .map(|capacity| format!("{}/{}", scheduler.native_slots_reserved, capacity))
        .unwrap_or_else(|| {
            format!(
                "{}/{}",
                scheduler.native_slots_reserved,
                tr("unavailable", "不可用")
            )
        });
    let root_slots = scheduler.root_slots_reserved;
    let observed = scheduler.native_turns_observed;
    let summaries = if zh() {
        [
            vec![format!(
                "根任务槽位：{root_slots} | 原生预留：{native_reserved} | 已观察原生轮次：{observed}"
            )],
            vec![
                format!("根任务槽位：{root_slots} | 原生预留：{native_reserved}"),
                format!("已观察原生轮次：{observed}"),
            ],
            vec![
                format!("原生预留：{native_reserved}"),
                format!("原生已观察：{observed}"),
            ],
            vec![
                format!("预留：{native_reserved}"),
                format!("已观察：{observed}"),
            ],
        ]
    } else {
        [
            vec![format!(
                "Root slots: {root_slots} | native reserved: {native_reserved} | native turns observed: {observed}"
            )],
            vec![
                format!("Root slots: {root_slots} | native reserved: {native_reserved}"),
                format!("Native turns observed: {observed}"),
            ],
            vec![
                format!("native reserved: {native_reserved}"),
                format!("native observed: {observed}"),
            ],
            vec![
                format!("N reserved: {native_reserved}"),
                format!("N observed: {observed}"),
            ],
        ]
    };
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
    // 行文字保持不变；这里只记录每行的角色，供下面上色。
    let summary_rows = raw.len();
    let mut task_rows = Vec::new();
    let mut selected_visual = None;
    for id in &workflow_order {
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
                tr(" · slot reserved", " · 槽位已预留")
            }
            crate::scheduler::TaskKind::NativeChild => tr(" · slot free", " · 槽位空闲"),
            crate::scheduler::TaskKind::RootTurn => "",
        };
        let flags = format!(
            "{}{}",
            if task.pause_requested {
                tr(" · pause requested", " · 已请求暂停")
            } else {
                ""
            },
            if task.cancel_requested {
                tr(" · cancel requested", " · 已请求取消")
            } else {
                ""
            }
        );
        let state = task_state_label(task.state);
        let text = if zh() {
            format!(
                "{marker}{tree}#{} {state} · 尝试 {} · 优先级 {}{slot}{flags} · {}",
                task.id.0, task.attempt, task.priority, task.title
            )
        } else {
            format!(
                "{marker}{tree}#{} {state} · attempt {} · priority {}{slot}{flags} · {}",
                task.id.0, task.attempt, task.priority, task.title
            )
        };
        if selected_row {
            selected_visual = Some(
                raw.iter()
                    .map(|line| wrap(line, width).len())
                    .sum::<usize>(),
            );
        }
        task_rows.push((raw.len(), selected_row, task.state));
        raw.push(text);
    }
    let detail_start = raw.len();
    if let Some(task) = selected {
        let state = task_state_label(task.state);
        raw.push(if zh() {
            // 阻塞原因是 Core 的枚举值，原样显示；只有“没有原因”换成中文。
            let blocked = task
                .blocked_reason
                .as_ref()
                .map_or_else(|| "无".to_owned(), |reason| format!("{reason:?}"));
            format!(
                "任务 #{}：{} · {state} · 尝试 {} · 阻塞：{blocked} · 请求：{}",
                task.id.0, task.title, task.attempt, task.pending_requests
            )
        } else {
            format!(
                "Task #{}: {} · {state} · attempt {} · blocked: {:?} · requests: {}",
                task.id.0, task.title, task.attempt, task.blocked_reason, task.pending_requests
            )
        });
        let parent = task.parent.map_or_else(
            || match task.kind {
                crate::scheduler::TaskKind::RootTurn => tr("none", "无").into(),
                crate::scheduler::TaskKind::NativeChild => {
                    tr("unconfirmed / unavailable", "未确认 / 不可用").into()
                }
            },
            |id| task_reference(id, &by_id),
        );
        raw.push(if zh() {
            format!(
                "父任务：{parent} | 依赖策略：{:?} | 失败策略：{:?}",
                task.policy, task.failure
            )
        } else {
            format!(
                "Parent: {parent} | dependency policy: {:?} | failure policy: {:?}",
                task.policy, task.failure
            )
        });
        if task.dependencies.is_empty() {
            raw.push(tr("Dependencies: none", "依赖：无").into());
        } else {
            raw.push(if zh() {
                format!("依赖（{:?}）：", task.policy)
            } else {
                format!("Dependencies ({:?}):", task.policy)
            });
            for id in &task.dependencies {
                raw.push(format!("  {}", task_reference(*id, &by_id)));
            }
        }
        if task.wait_targets.is_empty() {
            raw.push(tr("Gate wait targets: none", "Gate 等待目标：无").into());
        } else {
            raw.push(
                tr(
                    "Gate wait targets (captured attempts):",
                    "Gate 等待目标（捕获的尝试）：",
                )
                .into(),
            );
            for target in &task.wait_targets {
                let current = by_id.get(&target.task).map_or_else(
                    || format!("#{} {}", target.task.0, tr("unavailable", "不可用")),
                    |linked| {
                        format!(
                            "{} · {} {} · {}",
                            task_reference(target.task, &by_id),
                            tr("current attempt", "当前尝试"),
                            linked.attempt,
                            task_state_label(linked.state)
                        )
                    },
                );
                raw.push(format!(
                    "  {} #{} {} {} → {current}",
                    tr("captured", "捕获"),
                    target.task.0,
                    tr("attempt", "尝试"),
                    target.attempt
                ));
            }
        }
        raw.push(format!(
            "{}{}",
            tr("Conversation: ", "对话："),
            workflow_agent_label(snapshot, task, &agent_rows)
        ));
        if let Some(cursor) = link_cursor.filter(|cursor| cursor.target == task.id) {
            if let Some(captured) = cursor.captured_attempt {
                raw.push(if zh() {
                    format!(
                        "所选 Gate 链接捕获的是尝试 {captured}；任务当前是尝试 {}。",
                        task.attempt
                    )
                } else {
                    format!(
                        "Selected Gate link captured attempt {captured}; current task attempt is {}.",
                        task.attempt
                    )
                });
            }
        }
    } else {
        raw.push(tr("No workflow tasks.", "还没有工作流任务。").into());
        raw.push(
            tr(
                "Enter sends a task; Ctrl+S queues one while a turn runs.",
                "Enter 发送任务；轮次运行中按 Ctrl+S 排队。",
            )
            .into(),
        );
    }
    let theme = theme::current();
    let selected_state = selected.map(|task| task.state);
    let lines = raw
        .iter()
        .enumerate()
        .flat_map(|(index, text)| {
            if let Some((_, selected_row, state)) = task_rows.iter().find(|row| row.0 == index) {
                if *selected_row {
                    return wrap(text, width)
                        .into_iter()
                        .map(|piece| selected_line(vec![Span::raw(piece)], width))
                        .collect::<Vec<_>>();
                }
                return wrap_marked(text, width, Style::default(), |clean| {
                    word_mark(clean, &task_state_label(*state), task_tone(*state))
                        .into_iter()
                        .collect()
                });
            }
            if index == 0 {
                return wrap_marked(text, width, Style::default(), |clean| {
                    [
                        label_mark_l10n(clean),
                        word_mark(clean, paused_word, tone(theme.warning)),
                        word_mark(clean, active_word, tone(theme.accent)),
                        word_mark(clean, stopping_word, tone(theme.warning)),
                        word_mark(clean, disconnected_word, tone(theme.error)),
                    ]
                    .into_iter()
                    .flatten()
                    .collect()
                });
            }
            if index < summary_rows || selected.is_none() || text.starts_with(' ') {
                return wrap_marked(text, width, theme.secondary(), |_| Vec::new());
            }
            wrap_marked(text, width, Style::default(), |clean| {
                let mut marks: Vec<_> = label_mark_l10n(clean).into_iter().collect();
                // 详情首行是任务列表和详情之间唯一的视觉分界，用标题样式。
                if index == detail_start {
                    for (_, style) in &mut marks {
                        *style = theme.heading();
                    }
                }
                // 详情首行在标题之后才出现状态词；从标题后开始找，避免标题里同名的词被上色。
                if let (true, Some(state)) = (index == detail_start, selected_state) {
                    let label = task_state_label(state);
                    let word = format!(" · {label} · {} ", tr("attempt", "尝试"));
                    if let Some(start) = clean.rfind(&word) {
                        let start = start + " · ".len();
                        marks.push((start..start + label.len(), task_tone(state)));
                    }
                }
                marks
            })
        })
        .collect::<Vec<_>>();
    let max_scroll = lines.len().saturating_sub(viewport);
    let mut scroll = scroll.min(max_scroll);
    if !manual_scroll {
        if let Some(selected_row) = selected_visual {
            if selected_row < scroll {
                scroll = selected_row;
            } else if selected_row >= scroll.saturating_add(viewport) && viewport > 0 {
                scroll = selected_row + 1 - viewport;
            }
        }
    }
    frame.render_widget(
        Paragraph::new(lines)
            .scroll((scroll.min(u16::MAX as usize) as u16, 0))
            .block(
                theme::focus_panel()
                    .borders(Borders::ALL)
                    .title(tr(" Workflow · F4 ", " 工作流 · F4 "))
                    .title_bottom(footer(
                        tr(
                            "Esc close | ↑/↓ select | Enter conversation | d dependencies | g Gate | PgUp/PgDn scroll",
                            "Esc 关闭 | ↑/↓ 选择 | Enter 对话 | d 依赖 | g Gate | PgUp/PgDn 滚动",
                        ),
                        area.width,
                    )),
            ),
        area,
    );
}

/// Task state tone; the state word itself stays in the row text.
fn task_tone(state: TaskState) -> Style {
    let theme = theme::current();
    tone(match state {
        TaskState::Starting | TaskState::Running => theme.accent,
        TaskState::WaitingChildren
        | TaskState::WaitingApproval
        | TaskState::Cancelling
        | TaskState::Paused
        | TaskState::Blocked
        | TaskState::Unknown => theme.warning,
        TaskState::Failed => theme.error,
        TaskState::Succeeded => theme.success,
        TaskState::Queued | TaskState::Ready | TaskState::Cancelled => theme.muted,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct AgentTreeRow {
    pub(super) index: usize,
    pub(super) depth: usize,
    pub(super) parent_missing: bool,
    pub(super) relationship_unknown: bool,
    pub(super) depth_truncated: bool,
}

/// 把快照投影为稳定的父子 DFS 顺序；渲染层不重新推断父链。
pub(super) fn project_agent_tree(
    agents: &[AgentSnapshot],
    root_id: Option<&str>,
) -> Vec<AgentTreeRow> {
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

fn workflow_agent_label(
    snapshot: &CoreSnapshot,
    task: &TaskSnapshot,
    agent_rows: &HashMap<String, AgentTreeRow>,
) -> String {
    match workflow_conversation(snapshot, task) {
        ConversationTarget::Root => {
            let thread = snapshot
                .thread_id
                .as_deref()
                .unwrap_or(tr("unknown", "未知"));
            if zh() {
                format!("root 线程 {thread} · 任务当前尝试 {}", task.attempt)
            } else {
                format!(
                    "root thread {thread} · current task attempt {}",
                    task.attempt
                )
            }
        }
        ConversationTarget::Child(agent) => {
            let depth = agent_rows
                .get(&agent.info.id)
                .map_or(tr("unknown", "未知").to_string(), |row| {
                    row.depth.to_string()
                });
            if zh() {
                format!(
                    "Agent {} · Agent 树深度 {} · 第 {} 代 · {}",
                    agent.info.id,
                    depth,
                    agent.generation,
                    agent
                        .outcome
                        .as_ref()
                        .map_or_else(|| "未结束".to_owned(), outcome_label)
                )
            } else {
                format!(
                    "agent {} · Agent tree depth {} · generation {} · {:?}",
                    agent.info.id, depth, agent.generation, agent.outcome
                )
            }
        }
        ConversationTarget::Unavailable(message) => message.into(),
    }
}

pub(super) fn gate_status(snapshot: &CoreSnapshot, gate: &crate::state::GateSnapshot) -> String {
    let done = gate
        .targets
        .iter()
        .filter(|target| target.outcome.is_some())
        .count();
    let total = gate.targets.len();
    let queued = snapshot.scheduler.queued_roots;
    let root_starts = snapshot
        .root_start_requests
        .saturating_sub(gate.root_starts_at_enter);
    let mut lines = vec![if zh() {
        format!(
            "等待子 Agent：{done}/{total} | 排队：{queued} | 等待期间启动的根轮次：{root_starts}"
        )
    } else {
        format!(
            "Waiting children: {done}/{total} | queued: {queued} | root starts during wait: {root_starts}"
        )
    }];
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
                    tr("running", "运行中").into()
                } else {
                    tr("awaiting turn", "等待轮次").into()
                }
            },
            outcome_label,
        );
        lines.push(if zh() {
            format!("{name}：{status}（第 {} 代）", target.generation)
        } else {
            format!("{name}: {status} (gen {})", target.generation)
        });
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::AgentInfo;
    use crate::gate::WaitTarget;
    use crate::state::{FactSource, GateSnapshot};
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    fn agent(id: &str, parent_id: &str) -> AgentSnapshot {
        AgentSnapshot {
            info: AgentInfo {
                id: id.into(),
                parent_id: parent_id.into(),
                path: Some(format!("/root/{id}")),
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

    fn rendered(terminal: &Terminal<TestBackend>) -> String {
        terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect()
    }

    #[test]
    fn agent_tree_keeps_snapshot_order_and_exposes_unknown_relationships_and_depth() {
        let agents = vec![
            agent("leaf", "branch"),
            agent("orphan", "missing"),
            agent("branch", "root"),
            agent("sibling", "root"),
        ];
        let rows = project_agent_tree(&agents, Some("root"));
        assert_eq!(
            rows.iter()
                .map(|row| agents[row.index].info.id.as_str())
                .collect::<Vec<_>>(),
            ["orphan", "branch", "leaf", "sibling"]
        );
        assert!(rows[0].parent_missing);
        assert!(!rows[1].parent_missing);

        let cycle = vec![agent("a", "b"), agent("b", "a")];
        let rows = project_agent_tree(&cycle, Some("root"));
        assert_eq!(
            rows.iter()
                .map(|row| cycle[row.index].info.id.as_str())
                .collect::<Vec<_>>(),
            ["a", "b"]
        );
        assert!(rows.iter().all(|row| row.relationship_unknown));

        let mut deep = Vec::new();
        for index in 0..10 {
            deep.push(agent(
                &format!("deep-{index}"),
                if index == 0 { "root" } else { "deep-x" },
            ));
            if index > 0 {
                deep[index].info.parent_id = format!("deep-{}", index - 1);
            }
        }
        let rows = project_agent_tree(&deep, Some("root"));
        assert_eq!(
            rows.iter().map(|row| row.depth).collect::<Vec<_>>(),
            [1, 2, 3, 4, 5, 6, 7, 8, 8, 8]
        );
        assert!(rows[..8].iter().all(|row| !row.depth_truncated));
        assert!(rows[8..].iter().all(|row| row.depth_truncated));
    }

    #[test]
    fn agent_tree_and_workflow_views_render_at_wide_and_narrow_sizes() {
        let snapshot = CoreSnapshot {
            thread_id: Some("root".into()),
            agents: vec![agent("child", "root")],
            usage: crate::state::UsageSummary {
                source: FactSource::ServerConfirmed,
                ..Default::default()
            },
            ..Default::default()
        };
        for (width, height) in [(120, 30), (100, 12), (60, 20), (30, 10)] {
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            terminal
                .draw(|frame| {
                    draw_agents(frame, frame.area(), &snapshot, Some("child"), height <= 16)
                })
                .unwrap();
            if (width, height) == (120, 30) {
                terminal
                    .draw(|frame| {
                        draw_agents(
                            frame,
                            ratatui::layout::Rect::new(0, 0, 32, 12),
                            &snapshot,
                            Some("child"),
                            false,
                        )
                    })
                    .unwrap();
                let panel = rendered(&terminal);
                assert!(panel.contains("Agents · F3"), "{panel}");
                assert!(panel.contains("/root/child"), "{panel}");
                assert!(!panel.contains("Agent: child"), "{panel}");
            }
            let text = rendered(&terminal);
            assert!(text.contains("child"), "{width}x{height}: {text}");
            assert!(text.contains("running"), "{width}x{height}: {text}");

            terminal
                .draw(|frame| draw_workflow(frame, frame.area(), &snapshot, None, None, 0, false))
                .unwrap();
            let text = rendered(&terminal);
            assert!(text.contains("Workflow"), "{width}x{height}: {text}");
            assert!(
                text.contains("No workflow tasks"),
                "{width}x{height}: {text}"
            );
        }
    }

    fn sidebar(
        snapshot: &CoreSnapshot,
        selected: Option<&str>,
        width: u16,
    ) -> Terminal<TestBackend> {
        let mut terminal = Terminal::new(TestBackend::new(width, 40)).unwrap();
        terminal
            .draw(|frame| draw_agents(frame, frame.area(), snapshot, selected, false))
            .unwrap();
        terminal
    }

    fn screen_rows(terminal: &Terminal<TestBackend>) -> Vec<String> {
        let buffer = terminal.backend().buffer();
        (0..buffer.area.height)
            .map(|y| {
                // 宽字符后面的占位格不是文字，跳过后每行就是屏幕上看到的字符串。
                let mut row = String::new();
                let mut x = 0;
                while x < buffer.area.width {
                    let symbol = buffer[(x, y)].symbol();
                    row.push_str(symbol);
                    x += symbol.width().max(1) as u16;
                }
                row
            })
            .collect()
    }

    #[test]
    fn agent_sidebar_truncates_long_and_wide_names_inside_the_border() {
        let mut long = agent("long", "root");
        long.info.path = Some("/root/很长的中文代理名称-worker-with-a-very-long-suffix".into());
        let mut wide = agent("wide", "long");
        wide.info.path = Some("代理👨‍👩‍👧‍👦e\u{301}-组合字符与表情符号的名称需要按显示宽度截断".into());
        let snapshot = CoreSnapshot {
            thread_id: Some("root".into()),
            agents: vec![long, wide],
            ..Default::default()
        };
        for width in [32, 36] {
            let terminal = sidebar(&snapshot, None, width);
            let rows = screen_rows(&terminal);
            for needle in ["/root/很长", "+- 代理"] {
                let row = rows
                    .iter()
                    .find(|row| row.contains(needle))
                    .unwrap_or_else(|| panic!("{needle} missing at {width}: {rows:#?}"));
                // 截断发生在边框之内：省略号后面只剩填充和右边框。
                let content = row.trim_end_matches('│').trim_end();
                assert!(content.ends_with('…'), "{width}: {row}");
                assert!(row.ends_with('│'), "{width}: {row}");
                assert!(content.width() < width as usize, "{width}: {row}");
            }
            let text = rows.join("\n");
            assert_eq!(text.matches("running / gen 1").count(), 2, "{text}");
            assert_eq!(text.matches("tokens: unavailable").count(), 2, "{text}");
        }
    }

    #[test]
    fn agent_sidebar_keeps_facts_visible_at_the_deepest_tree_level() {
        let mut agents = Vec::new();
        for index in 0..9 {
            let mut next = agent(&format!("deep-{index}"), "root");
            if index > 0 {
                next.info.parent_id = format!("deep-{}", index - 1);
            }
            agents.push(next);
        }
        let snapshot = CoreSnapshot {
            thread_id: Some("root".into()),
            agents,
            ..Default::default()
        };
        let text = screen_rows(&sidebar(&snapshot, Some("deep-8"), 32)).join("\n");
        // 缩进会先让位，深层节点的状态、generation 和 token 事实不被右边框裁掉。
        assert_eq!(text.matches("running / gen 1").count(), 9, "{text}");
        assert_eq!(text.matches("tokens: unavailable").count(), 9, "{text}");
        assert!(text.contains("[depth truncated]"), "{text}");
    }

    #[test]
    fn agent_sidebar_selection_fills_the_row_and_does_not_depend_on_color() {
        use ratatui::style::Modifier;

        let mut done = agent("done", "root");
        done.outcome = Some(ChildOutcome::Failed);
        let snapshot = CoreSnapshot {
            thread_id: Some("root".into()),
            agents: vec![agent("child", "root"), done],
            ..Default::default()
        };
        let terminal = sidebar(&snapshot, Some("child"), 34);
        let rows = screen_rows(&terminal);
        let buffer = terminal.backend().buffer();
        let row_of = |needle: &str| rows.iter().position(|row| row.contains(needle)).unwrap();
        let selected = row_of("> /root/child");
        let other = row_of("  /root/done");
        let theme = theme::current();
        // 内容区是第 1 列到分隔线前留白之间的 31 列。
        for x in 1..32 {
            let cell = &buffer[(x, selected as u16)];
            assert_eq!(cell.style().bg, theme.selected().bg, "column {x}");
            // 加粗（无色主题下还有反显）在整行都生效，选中态不靠颜色区分。
            assert!(cell.modifier.contains(Modifier::BOLD), "column {x}");
            assert_eq!(
                cell.modifier.contains(Modifier::REVERSED),
                theme.selected().add_modifier.contains(Modifier::REVERSED),
                "column {x}"
            );
            assert!(
                !buffer[(x, other as u16)].modifier.contains(Modifier::BOLD),
                "column {x}"
            );
        }
        assert!(rows[row_of("  root")].starts_with("   root"), "{rows:#?}");
        // 状态始终有文字；状态词加粗，其余事实保持次要样式。
        let status = &rows[selected + 1];
        let word = status.find("running / gen 1").expect("status words");
        let word = status[..word].width() as u16;
        assert!(buffer[(word, selected as u16 + 1)]
            .modifier
            .contains(Modifier::BOLD));
        let failed = &rows[other + 1];
        assert!(failed.contains("Failed / gen 1"), "{failed}");
        assert!(rows[other + 2].contains("tokens: unavailable"), "{rows:#?}");
    }

    #[test]
    fn agent_views_do_not_panic_at_tiny_sizes() {
        let mut long = agent("long", "root");
        long.info.path = Some("/root/很长的中文代理名称👨‍👩‍👧‍👦".into());
        let snapshot = CoreSnapshot {
            thread_id: Some("root".into()),
            agents: vec![long],
            ..Default::default()
        };
        for (width, height) in [(1, 1), (2, 3), (5, 4), (24, 8), (80, 16)] {
            for compact in [false, true] {
                let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
                terminal
                    .draw(|frame| {
                        draw_agents(frame, frame.area(), &snapshot, Some("long"), compact);
                        draw_workflow(frame, frame.area(), &snapshot, None, None, 0, false);
                    })
                    .unwrap();
            }
        }
    }

    #[test]
    fn gate_status_reports_confirmed_outcomes_and_captured_generations() {
        let mut snapshot = CoreSnapshot {
            scheduler: crate::scheduler::SchedulerSnapshot {
                queued_roots: 3,
                ..Default::default()
            },
            root_start_requests: 5,
            agents: vec![agent("child", "root")],
            ..Default::default()
        };
        let gate = GateSnapshot {
            targets: vec![
                WaitTarget {
                    id: "child".into(),
                    generation: 4,
                    turn_id: Some("turn-4".into()),
                    outcome: None,
                },
                WaitTarget {
                    id: "gone".into(),
                    generation: 2,
                    turn_id: None,
                    outcome: Some(crate::gate::ChildOutcome::Completed),
                },
            ],
            pending: true,
            root_starts_at_enter: 4,
            root_starts_at_release: None,
        };
        let text = gate_status(&snapshot, &gate);
        assert!(text.contains("Waiting children: 1/2"));
        assert!(text.contains("queued: 3"));
        assert!(text.contains("root starts during wait: 1"));
        assert!(text.contains("/root/child: running (gen 4)"));
        assert!(text.contains("gone: Completed (gen 2)"));
        snapshot.root_start_requests = 4;
        assert!(gate_status(&snapshot, &gate).contains("root starts during wait: 0"));
    }

    fn task(id: u64, state: TaskState, title: &str) -> TaskSnapshot {
        TaskSnapshot {
            id: crate::scheduler::TaskId(id),
            kind: crate::scheduler::TaskKind::RootTurn,
            state,
            title: title.into(),
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
        }
    }

    #[test]
    fn chinese_agent_sidebar_keeps_status_rows_aligned_inside_the_border() {
        let mut done = agent("done", "root");
        done.outcome = Some(ChildOutcome::Failed);
        let mut waiting = agent("waiting", "root");
        waiting.awaiting_turn = true;
        let snapshot = CoreSnapshot {
            thread_id: Some("root".into()),
            agents: vec![agent("child", "root"), done, waiting],
            ..Default::default()
        };
        lang::with_zh(|| {
            for width in [32, 36] {
                let rows = screen_rows(&sidebar(&snapshot, None, width));
                let text = rows.join("\n");
                assert!(text.contains("Agent · F3"), "{text}");
                let mut columns = Vec::new();
                for status in ["运行中 / 第 1 代", "失败 / 第 1 代", "启动中 / 第 1 代"]
                {
                    let row = rows
                        .iter()
                        .find(|row| row.contains(status))
                        .unwrap_or_else(|| panic!("{status} missing at {width}: {text}"));
                    // 状态行完整落在边框内：没有省略号，右边框还在原位。
                    assert!(!row.contains('…'), "{width}: {row}");
                    assert!(row.ends_with('│'), "{width}: {row}");
                    assert_eq!(row.width(), width as usize, "{row}");
                    columns.push(row[..row.find(status).unwrap()].width());
                }
                assert!(columns.iter().all(|column| *column == 5), "{columns:?}");
            }
            // 空状态的两行提示在最窄的侧栏里也完整显示。
            let rows = screen_rows(&sidebar(&CoreSnapshot::default(), None, 32));
            let text = rows.join("\n");
            for hint in ["还没有子 Agent。", "派发的子 Agent 显示在这里。"] {
                let row = rows
                    .iter()
                    .find(|row| row.contains(hint))
                    .unwrap_or_else(|| panic!("{hint} missing: {text}"));
                assert!(row.ends_with('│') && !row.contains('…'), "{row}");
                assert_eq!(row.width(), 32, "{row}");
            }
            // 紧凑摘要：标签用全角冒号，名称预算按显示宽度计算。
            let mut terminal = Terminal::new(TestBackend::new(30, 3)).unwrap();
            terminal
                .draw(|frame| draw_agents(frame, frame.area(), &snapshot, Some("child"), true))
                .unwrap();
            let rows = screen_rows(&terminal);
            assert!(rows[0].starts_with("Agent：/root/child"), "{rows:#?}");
            assert!(rows[1].starts_with("状态：运行中 / 第 1 代"), "{rows:#?}");
        });
    }

    #[test]
    fn chinese_workflow_panel_translates_chrome_and_wraps_inside_the_border() {
        let mut second = task(2, TaskState::Queued, "write docs");
        second.dependencies = vec![crate::scheduler::TaskId(1)];
        second.attempt = 0;
        let snapshot = CoreSnapshot {
            thread_id: Some("root".into()),
            scheduler: crate::scheduler::SchedulerSnapshot {
                tasks: vec![task(1, TaskState::Running, "build"), second],
                ..Default::default()
            },
            ..Default::default()
        };
        let draw = |width: u16, height: u16| {
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            terminal
                .draw(|frame| {
                    draw_workflow(
                        frame,
                        frame.area(),
                        &snapshot,
                        Some(crate::scheduler::TaskId(2)),
                        None,
                        0,
                        false,
                    )
                })
                .unwrap();
            screen_rows(&terminal)
        };
        lang::with_zh(|| {
            let text = draw(110, 24).join("\n");
            for expected in [
                "工作流 · F4",
                "工作流：派发中 | 就绪根任务：无",
                "根任务槽位：0 | 原生预留：0/不可用 | 已观察原生轮次：0",
                " #1 运行中 · 尝试 1 · 优先级 0 · build",
                ">#2 排队 · 尝试 0 · 优先级 0 · write docs",
                "任务 #2：write docs · 排队 · 尝试 0 · 阻塞：无 · 请求：0",
                "父任务：无 | 依赖策略：AllRequired | 失败策略：FailFast",
                "依赖（AllRequired）：",
                "  #1 build [运行中]",
                "Gate 等待目标：无",
                "对话：",
                "Esc 关闭",
                "PgUp/PgDn 滚动",
            ] {
                assert!(text.contains(expected), "{expected}: {text}");
            }
            // 窄面板：中文按显示宽度折行，每一行都停在右边框之内。
            let narrow = draw(40, 30);
            for row in &narrow[1..narrow.len() - 1] {
                assert_eq!(row.width(), 40, "{row}");
                assert!(row.starts_with('│') && row.ends_with('│'), "{row}");
            }
            assert!(narrow.join("\n").contains("依赖（AllRequired）："));

            // 全角冒号的标签也能被识别；标签之后的半角冒号不参与。
            let (range, _) = label_mark_l10n("对话：Agent a:b").unwrap();
            assert_eq!(range, 0.."对话：".len());
            let (range, _) = label_mark_l10n("依赖（AllRequired）：").unwrap();
            assert_eq!(range, 0.."依赖（AllRequired）：".len());
            assert!(label_mark_l10n("工作流 | 就绪：无").is_none());
        });
        // 英文模式仍是共享的半角查找。
        assert_eq!(label_mark_l10n("对话：a:b").unwrap().0, 0.."对话：a:".len());
    }

    #[test]
    fn chinese_gate_status_translates_words_and_keeps_names() {
        let snapshot = CoreSnapshot {
            agents: vec![agent("child", "root")],
            ..Default::default()
        };
        let gate = GateSnapshot {
            targets: vec![
                WaitTarget {
                    id: "child".into(),
                    generation: 4,
                    turn_id: Some("turn-4".into()),
                    outcome: None,
                },
                WaitTarget {
                    id: "gone".into(),
                    generation: 2,
                    turn_id: None,
                    outcome: Some(ChildOutcome::Completed),
                },
            ],
            pending: true,
            root_starts_at_enter: 0,
            root_starts_at_release: None,
        };
        let text = lang::with_zh(|| gate_status(&snapshot, &gate));
        assert!(text.contains("等待子 Agent：1/2 | 排队：0"), "{text}");
        assert!(text.contains("/root/child：运行中（第 4 代）"), "{text}");
        assert!(text.contains("gone：已完成（第 2 代）"), "{text}");
    }
}
