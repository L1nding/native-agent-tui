use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Borders, Clear, Paragraph};

use super::lang::{self, show, tr};
use super::layout::wrap;
use super::reminders::Reminders;
use super::theme;
use crate::observation::{
    ActivityScope, ActivitySnapshot, AttentionLevel, CompactionFact, CompactionFactStatus,
    ExecutionState,
};
use crate::state::{
    display_text, CoreSnapshot, FactSource, UsageFact, UsageIdentity, UsageSummary,
};
/// A byte range of sanitized row text and the style it is painted with.
pub(super) type Mark = (std::ops::Range<usize>, Style);

/// A state word: colored and bold, so it still stands out without color.
pub(super) fn tone(color: Color) -> Style {
    Style::default().fg(color).add_modifier(Modifier::BOLD)
}

pub(super) fn attention_tone(level: AttentionLevel) -> Style {
    let theme = theme::current();
    match level {
        AttentionLevel::Active => tone(theme.accent),
        AttentionLevel::AttentionNeeded
        | AttentionLevel::RequiresAction
        | AttentionLevel::Unknown => tone(theme.warning),
        AttentionLevel::Quiet | AttentionLevel::Ended => tone(theme.muted),
    }
}

pub(super) fn execution_tone(state: ExecutionState) -> Style {
    let theme = theme::current();
    match state {
        ExecutionState::Starting | ExecutionState::Running => tone(theme.accent),
        ExecutionState::Waiting | ExecutionState::Unknown => tone(theme.warning),
        ExecutionState::Failed | ExecutionState::Interrupted => tone(theme.error),
        ExecutionState::Completed => tone(theme.success),
        ExecutionState::Idle => tone(theme.muted),
    }
}

/// The leading `Label:` of a detail row, if the row has one.
pub(super) fn label_mark(clean: &str) -> Option<Mark> {
    let line = clean.split('\n').next().unwrap_or("");
    let start = line.len() - line.trim_start().len();
    let end = line.find(':')?;
    let label = &line[start..end];
    if label.is_empty() || label.chars().count() > 48 || label.contains(['|', '"', '·', '>']) {
        return None;
    }
    Some((start..end + 1, theme::current().label()))
}

/// The first occurrence of a state word inside a row.
pub(super) fn word_mark(clean: &str, word: &str, style: Style) -> Option<Mark> {
    if word.is_empty() {
        return None;
    }
    clean
        .find(word)
        .map(|start| (start..start + word.len(), style))
}

/// Wraps exactly like `layout::wrap` and paints byte ranges of the sanitized
/// text. `marks` receives that sanitized text, so ranges match what is drawn.
pub(super) fn wrap_marked(
    text: &str,
    width: usize,
    base: Style,
    marks: impl FnOnce(&str) -> Vec<Mark>,
) -> Vec<Line<'static>> {
    let clean = display_text(text);
    let mut marks = marks(&clean);
    marks.retain(|(range, _)| {
        range.start < range.end
            && range.end <= clean.len()
            && clean.is_char_boundary(range.start)
            && clean.is_char_boundary(range.end)
    });
    marks.sort_by_key(|(range, _)| range.start);
    let mut lines = Vec::new();
    let mut offset = 0;
    for source in clean.split('\n') {
        for piece in wrap(source, width) {
            let end = offset + piece.len();
            let mut spans = Vec::new();
            let mut cursor = offset;
            for (range, style) in &marks {
                let start = range.start.max(cursor);
                let stop = range.end.min(end);
                if start >= stop {
                    continue;
                }
                if start > cursor {
                    spans.push(Span::styled(clean[cursor..start].to_owned(), base));
                }
                spans.push(Span::styled(clean[start..stop].to_owned(), *style));
                cursor = stop;
            }
            if cursor < end || spans.is_empty() {
                spans.push(Span::styled(clean[cursor..end].to_owned(), base));
            }
            lines.push(Line::from(spans));
            offset = end;
        }
        offset += 1;
    }
    lines
}

/// Cuts a styled row to `width` cells at grapheme boundaries, ending in `…`.
pub(super) fn clip_spans(spans: Vec<Span<'static>>, width: usize) -> Vec<Span<'static>> {
    let total: usize = spans.iter().map(|span| span.content.width()).sum();
    if total <= width {
        return spans;
    }
    let mut output = Vec::new();
    let mut used = 0;
    let budget = width.saturating_sub(1);
    'spans: for span in spans {
        let mut kept = String::new();
        for grapheme in span.content.graphemes(true) {
            let cells = grapheme.width();
            if used + cells > budget {
                if !kept.is_empty() {
                    output.push(Span::styled(kept, span.style));
                }
                break 'spans;
            }
            kept.push_str(grapheme);
            used += cells;
        }
        output.push(Span::styled(kept, span.style));
    }
    if width > 0 {
        let style = output.last().map_or_else(Style::default, |span| span.style);
        output.push(Span::styled("…", style));
    }
    output
}

/// Hint rows written as `Key label | Key label`. A hint without a key is a
/// plain note. Rows too wide for one line fall back to wrapped quiet text, so
/// narrow terminals lose emphasis instead of losing the hint.
pub(super) fn hint_lines(text: &str, width: usize) -> Vec<Line<'static>> {
    let line = theme::hints(text);
    if line.width() <= width {
        return vec![line];
    }
    wrap_marked(
        &text.replace(" | ", " · "),
        width,
        theme::current().secondary(),
        |_| Vec::new(),
    )
}

/// Hints for the bottom border of a panel `panel_width` cells wide. They are
/// padded away from the corners and trailing hints are dropped to fit, so list
/// the close key first.
pub(super) fn footer(text: &str, panel_width: u16) -> Line<'static> {
    let mut line = hint_row(text, (panel_width as usize).saturating_sub(4));
    line.spans.insert(0, Span::raw(" "));
    line.spans.push(Span::raw(" "));
    line
}

/// One hint row that never cuts a hint in half: trailing hints are dropped
/// until the rest fits, so callers list the most important keys first.
pub(super) fn hint_row(text: &str, width: usize) -> Line<'static> {
    let hints: Vec<&str> = text.split(" | ").collect();
    for count in (1..=hints.len()).rev() {
        let line = theme::hints(&hints[..count].join(" | "));
        if line.width() <= width {
            return line;
        }
    }
    Line::from(clip_spans(
        theme::hints(hints.first().copied().unwrap_or("")).spans,
        width,
    ))
}

/// A selected row: clipped, padded to the full width and uniformly highlighted.
pub(super) fn selected_line(spans: Vec<Span<'static>>, width: usize) -> Line<'static> {
    let style = theme::current().selected();
    let mut text: String = clip_spans(spans, width)
        .iter()
        .map(|span| span.content.as_ref())
        .collect();
    let used = text.width();
    text.push_str(&" ".repeat(width.saturating_sub(used)));
    Line::from(Span::styled(text, style))
}

pub(super) fn age(ms: Option<u64>) -> String {
    ms.map_or_else(
        || tr("unknown", "未知").into(),
        |ms| format!("{}s", ms / 1000),
    )
}

/// Placeholder for a fact the UI does not have.
pub(super) fn unavailable() -> &'static str {
    tr("unavailable", "不可用")
}

pub(super) fn yes_no(value: bool) -> &'static str {
    if value {
        tr("yes", "是")
    } else {
        tr("no", "否")
    }
}

/// Execution state as shown in rows. English keeps the variant name.
pub(super) fn execution_word(state: ExecutionState) -> String {
    if !lang::zh() {
        return format!("{state:?}");
    }
    match state {
        ExecutionState::Idle => "空闲",
        ExecutionState::Starting => "启动中",
        ExecutionState::Running => "运行中",
        ExecutionState::Waiting => "等待中",
        ExecutionState::Completed => "已完成",
        ExecutionState::Failed => "失败",
        ExecutionState::Interrupted => "已中断",
        ExecutionState::Unknown => "未知",
    }
    .to_owned()
}

/// Attention level as shown in rows. English keeps the variant name.
pub(super) fn attention_word(level: AttentionLevel) -> String {
    if !lang::zh() {
        return format!("{level:?}");
    }
    match level {
        AttentionLevel::Active => "活跃",
        AttentionLevel::Quiet => "静默",
        AttentionLevel::AttentionNeeded => "需关注",
        AttentionLevel::RequiresAction => "待处理",
        AttentionLevel::Unknown => "未知",
        AttentionLevel::Ended => "已结束",
    }
    .to_owned()
}

pub(super) fn focus_activity<'a>(
    snapshot: &'a CoreSnapshot,
    agent_id: &str,
) -> Option<&'a ActivitySnapshot> {
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

pub(super) fn activity_brief(activity: &ActivitySnapshot) -> String {
    format!(
        "{} {} | {} {} | {}",
        execution_word(activity.execution_state),
        activity.tool_category.map_or_else(
            || format!("{:?}", activity.kind),
            |category| format!("{category:?}")
        ),
        tr("quiet", "静默"),
        age(activity.silence_ms),
        attention_word(activity.attention.level)
    )
}

/// 头部用的直白描述；F11 证据视图仍显示 `activity_brief` 的原始枚举。
pub(super) fn reminder_brief(activity: &ActivitySnapshot, reminders: &Reminders) -> String {
    use crate::observation::ActivityKind::*;
    use crate::protocol::ToolCategory;
    let doing = match activity.kind {
        Starting => tr("Starting", "正在启动"),
        ModelRequest => tr("Waiting for the model", "等待模型响应"),
        ModelStreaming => tr("Agent is writing", "Agent 正在输出"),
        ToolRunning => match activity.tool_category {
            Some(ToolCategory::Shell) => tr("Running a command", "正在运行命令"),
            Some(ToolCategory::File) => tr("Changing files", "正在修改文件"),
            Some(ToolCategory::Mcp | ToolCategory::Dynamic) => tr("Calling a tool", "正在调用工具"),
            Some(ToolCategory::Delegation) => {
                tr("Delegating to a child agent", "正在委派给子 Agent")
            }
            Some(ToolCategory::Web) => tr("Searching the web", "正在搜索网页"),
            Some(ToolCategory::Compaction) => tr("Compacting context", "正在压缩上下文"),
            Some(_) | None => tr("Running a tool", "正在运行工具"),
        },
        WaitingApproval => tr("Needs your approval", "等待你审批"),
        WaitingUserInput => tr("Needs your answer", "等待你回答"),
        WaitingChildren => tr("Waiting for child agents", "等待子 Agent"),
        WaitingTransport => tr("Waiting for app-server", "等待 app-server"),
        Completed => tr("Done", "已完成"),
        Failed => tr("Failed", "失败"),
        Unknown => tr("Outcome unknown", "结果未知"),
    };
    let mut text = format!(
        "{doing} · {} {}",
        tr("quiet", "静默"),
        age(activity.silence_ms)
    );
    match activity.attention.level {
        AttentionLevel::Quiet => text.push_str(tr(" · no recent output", " · 近期没有输出")),
        AttentionLevel::AttentionNeeded => text.push_str(tr(
            " · silent for a while, check it",
            " · 已静默较久，建议查看",
        )),
        _ => {}
    }
    if reminders.is_acknowledged(activity) {
        text.push_str(tr(" · reminder off locally", " · 本地已暂不提醒"));
    }
    text
}

pub(super) fn next_action(activity: &ActivitySnapshot, reminders: &Reminders) -> &'static str {
    if activity.attention.requires_action {
        tr("F2 answer request", "F2 回应请求")
    } else if activity.execution_state == ExecutionState::Unknown {
        tr("inspect unknown outcome", "检查未知结果")
    } else if reminders.is_acknowledged(activity) {
        tr(
            "continuing to wait; Ctrl+W restore",
            "继续等待；Ctrl+W 恢复提醒",
        )
    } else if matches!(
        activity.attention.level,
        AttentionLevel::Quiet | AttentionLevel::AttentionNeeded
    ) {
        tr(
            "F11 inspect; Ctrl+W wait or Ctrl+C",
            "F11 查看；Ctrl+W 继续等待，Ctrl+C 中断",
        )
    } else if activity.attention.level == AttentionLevel::Ended {
        tr("review result", "查看结果")
    } else {
        tr("wait; F11 evidence", "等待；F11 查看证据")
    }
}

pub(super) fn evidence_brief(activity: &ActivitySnapshot, reminders: &Reminders) -> String {
    // 只给出事件类别的可读形式；来源和序号在 F11/Ctrl+T 中查看。
    let next = next_action(activity, reminders);
    if lang::zh() {
        let last = activity
            .last_evidence
            .as_ref()
            .map_or("未知", |evidence| evidence_kind_zh(evidence.kind));
        return format!("下一步：{next} · 最近事件：{last}");
    }
    let last = activity.last_evidence.as_ref().map_or_else(
        || "unknown".into(),
        |evidence| {
            let mut words = String::new();
            for ch in format!("{:?}", evidence.kind).chars() {
                if ch.is_uppercase() && !words.is_empty() {
                    words.push(' ');
                }
                words.extend(ch.to_lowercase());
            }
            words
        },
    );
    format!("Next: {next} · last event: {last}")
}

fn evidence_kind_zh(kind: crate::observation::EvidenceKind) -> &'static str {
    use crate::observation::EvidenceKind::*;
    match kind {
        StartupChanged => "启动状态变化",
        TurnSubmitted => "轮次已提交",
        TurnStarted => "轮次开始",
        StateChanged => "状态变化",
        Output => "有新输出",
        MessageFinalized => "消息完成",
        ToolStarted => "工具开始",
        ToolCompleted => "工具完成",
        RequestCreated => "收到请求",
        RequestAnswered => "请求已回答",
        RequestResolved => "请求已解决",
        RequestExpired => "请求已过期",
        GateEntered => "开始等待子 Agent",
        GateReleased => "子 Agent 等待结束",
        ChildTurnBound => "子 Agent 轮次已绑定",
        ChildTerminal => "子 Agent 已结束",
        TurnCompleted => "轮次完成",
        TurnFailed => "轮次失败",
        TurnInterrupted => "轮次中断",
        ExecutionUnknown => "执行结果未知",
    }
}

/// 阈值及其单位；英文保持 `Some(15000)ms` 这一既有文本。
fn threshold<T: std::fmt::Debug>(value: Option<T>) -> String {
    if super::lang::zh() && value.is_none() {
        return " 无".into();
    }
    format!(" {}ms", show(&value))
}

pub(super) fn draw_evidence(
    frame: &mut ratatui::Frame<'_>,
    area: ratatui::layout::Rect,
    snapshot: &CoreSnapshot,
    reminders: &Reminders,
    evidence_scroll: usize,
    agent_id: &str,
    selected_agent: Option<&crate::agents::AgentSnapshot>,
) {
    // 浮层盖在对话区上；先清空，较短的行才不会透出底下的文字。
    frame.render_widget(Clear, area);
    let mut rows = Vec::new();
    // 行文字保持不变；这里只记录哪些行是活动标题、状态行或次要元数据。
    let mut heads = Vec::new();
    let mut briefs = Vec::new();
    let mut meta = Vec::new();
    let diagnostics = &snapshot.diagnostics;
    let zh = lang::zh();
    rows.push(format!(
        "{} {} / {} | {} {} | {} {}",
        tr("Transport bytes in/out", "传输字节 入/出"),
        diagnostics.transport_bytes_in,
        diagnostics.transport_bytes_out,
        tr("control events", "控制事件"),
        diagnostics.control_events,
        tr("telemetry events", "遥测事件"),
        diagnostics.telemetry_events
    ));
    rows.push(format_usage_evidence(snapshot, selected_agent));
    rows.push(format_token_budget_evidence(snapshot));
    if let Some(journal) = &snapshot.journal {
        rows.push(format!(
            "{} {} / {} {} / {} {} / {} {:?}",
            tr("Session", "会话"),
            journal.session_id,
            tr("committed", "已提交"),
            journal.committed_seq,
            tr("submitted", "已送出"),
            journal.submitted_seq,
            tr("persistence", "持久化"),
            snapshot.persistence
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
    rows.push(if zh {
        format!("保留的压缩记录: {compactions} | 累计总数不可用")
    } else {
        format!("Compactions retained: {compactions} | lifetime total unavailable")
    });
    for activity in snapshot
        .observation
        .activities
        .iter()
        .filter(|activity| activity.identity.agent_id == agent_id)
    {
        heads.push((rows.len(), activity.attention.level));
        rows.push(format!(
            "{:?} {} | {}",
            activity.scope,
            activity.item_id.as_deref().unwrap_or(tr("turn", "轮次")),
            attention_word(activity.attention.level)
        ));
        if activity.tool_category == Some(crate::protocol::ToolCategory::Compaction) {
            rows.push(format!(
                "{} {:?}",
                tr("Compaction source", "压缩来源"),
                activity
                    .last_evidence
                    .as_ref()
                    .map(|evidence| evidence.source)
            ));
            rows.extend(compaction_fact_rows(compaction_fact_for_activity(
                snapshot, activity,
            )));
        }
        briefs.push((rows.len(), activity.execution_state));
        rows.push(activity_brief(activity));
        if reminders.is_acknowledged(activity) {
            rows.push(
                tr(
                    "Silence reminder: off locally until new evidence; Ctrl+W restore",
                    "静默提醒: 本地已暂不提醒，出现新证据后恢复；Ctrl+W 立即恢复",
                )
                .into(),
            );
        }
        rows.push(format!(
            "{}: {}",
            tr("Last", "最近证据"),
            activity.last_evidence.as_ref().map_or_else(
                || tr("unknown", "未知").into(),
                |evidence| format!(
                    "{:?} / {:?} #{}",
                    evidence.kind, evidence.source, evidence.id
                )
            )
        ));
        rows.push(format!(
            "{}: {}",
            tr("Next", "下一步"),
            next_action(activity, reminders)
        ));
        let meta_start = rows.len();
        rows.push(format!(
            "{} {} / {} {} / {} {}",
            tr("Elapsed", "已用时"),
            age(activity.elapsed_ms),
            tr("progress", "进度"),
            activity.progress_seq,
            tr("bytes", "字节"),
            activity.output_bytes
        ));
        rows.push(format!(
            "{} {} {} {} {} {} {} {}",
            tr("Thread", "线程"),
            show(&activity.identity.thread_id),
            tr("turn", "轮次"),
            show(&activity.identity.turn_id),
            tr("gen", "代次"),
            show(&activity.identity.generation),
            tr("attempt", "尝试"),
            show(&activity.identity.attempt_id)
        ));
        rows.push(format!(
            "{}{} / {}{} / {} {}",
            tr("Quiet", "静默阈值"),
            // 中文界面下缺失的阈值显示为“无”，不再带 ms 后缀。
            threshold(activity.attention.quiet_after_ms),
            tr("attention", "提醒阈值"),
            threshold(activity.attention.attention_after_ms),
            tr("source", "来源"),
            show(&activity.attention.config_source)
        ));
        rows.push(
            tr(
                "Provider execution: unavailable",
                "模型服务端执行状态: 不可用",
            )
            .into(),
        );
        if let Some(reason) = activity.wait_reason {
            rows.push(format!(
                "{}: {reason:?} / {}: {:?}",
                tr("Wait", "等待原因"),
                tr("resume", "恢复条件"),
                activity.resume_condition
            ));
        }
        for target in &activity.wait_targets {
            rows.push(format!(
                "{} {} / {} {} {} {} / {} / {} {} / {}",
                tr("Target", "目标"),
                target.thread_id,
                tr("turn", "轮次"),
                show(&target.turn_id),
                tr("gen", "代次"),
                target.generation,
                show(&target.outcome),
                tr("quiet", "静默"),
                age(target.silence_ms),
                show(&target.attention.as_ref().map(|attention| attention.level))
            ));
        }
        meta.extend(meta_start..rows.len());
        rows.push(String::new());
    }
    if heads.is_empty() {
        meta.push(rows.len());
        rows.push(
            tr(
                "No activity recorded for this agent yet. F3 switches agent.",
                "这个 Agent 还没有活动记录。F3 切换 Agent。",
            )
            .into(),
        );
    }
    let theme = theme::current();
    let width = area.width.saturating_sub(2) as usize;
    let lines: Vec<_> = rows
        .iter()
        .enumerate()
        .flat_map(|(index, row)| {
            let head = heads.iter().find(|(row, _)| *row == index);
            let brief = briefs.iter().find(|(row, _)| *row == index);
            let base = if head.is_some() {
                theme.label()
            } else if meta.contains(&index) {
                theme.secondary()
            } else {
                Style::default()
            };
            wrap_marked(row, width, base, |clean| {
                if let Some((_, level)) = head {
                    // 等级词在行尾；用最后一次出现避免和 item id 撞名。
                    let word = attention_word(*level);
                    return clean
                        .rfind(&word)
                        .map(|start| (start..start + word.len(), attention_tone(*level)))
                        .into_iter()
                        .collect();
                }
                if let Some((_, state)) = brief {
                    return word_mark(clean, &execution_word(*state), execution_tone(*state))
                        .into_iter()
                        .collect();
                }
                label_mark(clean).into_iter().collect()
            })
        })
        .collect();
    let height = area.height.saturating_sub(2) as usize;
    let start = evidence_scroll.min(lines.len().saturating_sub(height));
    frame.render_widget(
        Paragraph::new(
            lines
                .into_iter()
                .skip(start)
                .take(height)
                .collect::<Vec<_>>(),
        )
        .block(
            theme::focus_panel()
                .borders(Borders::ALL)
                .title(tr(" Evidence · F11 ", " 证据 · F11 "))
                .title_bottom(footer(
                    tr(
                        "F11/Esc close | PgUp/PgDn scroll",
                        "F11/Esc 关闭 | PgUp/PgDn 滚动",
                    ),
                    area.width,
                )),
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

/// Rows are `Label: value`; the timeline splits them on the first `": "`,
/// so both languages keep that ASCII separator.
pub(super) fn compaction_fact_rows(fact: Option<&CompactionFact>) -> Vec<String> {
    let status = fact.map_or(unavailable(), |fact| match fact.status {
        CompactionFactStatus::Started => tr("Started", "已开始"),
        CompactionFactStatus::Completed => tr("Completed", "已完成"),
        CompactionFactStatus::Unknown => tr("Unknown", "未知"),
    });
    vec![
        format!("{}: {status}", tr("Compaction status", "压缩状态")),
        format!(
            "{}: {} {} | {} {} | {} {} | {} {} | {} {}",
            tr("Compaction usage", "压缩用量"),
            tr("input", "输入"),
            shown_value(fact.and_then(|fact| fact.input_tokens)),
            tr("cached input", "缓存输入"),
            shown_value(fact.and_then(|fact| fact.cached_input_tokens)),
            tr("output", "输出"),
            shown_value(fact.and_then(|fact| fact.output_tokens)),
            tr("total", "总计"),
            shown_value(fact.and_then(|fact| fact.total_tokens)),
            tr("context window", "上下文窗口"),
            shown_value(fact.and_then(|fact| fact.context_window)),
        ),
    ]
}

/// Stays English in both languages: the header strips `/unavailable` from it
/// and compares it with `tokens:unavailable`.
pub(super) fn usage_status(snapshot: &CoreSnapshot) -> String {
    let usage =
        usage_fact_for(snapshot, None).map_or_else(UsageSummary::default, |fact| fact.summary);
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

/// Stays English in both languages; `usage_status` and its callers match on it.
pub(super) fn token_budget_brief(snapshot: &CoreSnapshot) -> String {
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

pub(super) fn format_token_budget_evidence(snapshot: &CoreSnapshot) -> String {
    let budget = snapshot.token_budget;
    if lang::zh() {
        let total = budget.confirmed_total_tokens.map_or_else(
            || "不可用".to_owned(),
            |total| {
                if budget.confirmed_complete {
                    total.to_string()
                } else {
                    format!("部分 {total}")
                }
            },
        );
        return format!(
            "会话 token 预算: tokens:{total}/{} | 已触发停止: {} | 单 Agent token 预算: {}",
            shown_value(budget.limit),
            yes_no(budget.stop_triggered),
            per_agent_budget_brief(budget)
        );
    }
    format!(
        "Session token budget: {} | stop triggered: {} | Per-agent token budget: {}",
        token_budget_brief(snapshot),
        if budget.stop_triggered { "yes" } else { "no" },
        per_agent_budget_brief(budget)
    )
}

pub(super) fn per_agent_budget_brief(budget: crate::state::TokenBudgetSnapshot) -> String {
    let Some(limit) = budget.per_agent_limit else {
        return tr("not set", "未设置").to_owned();
    };
    format!(
        "{} {limit} | {}: {}",
        tr("limit", "上限"),
        tr("stop triggered", "已触发停止"),
        yes_no(budget.per_agent_stop_triggered)
    )
}

pub(super) fn agent_usage_brief(
    snapshot: &CoreSnapshot,
    agent: &crate::agents::AgentSnapshot,
) -> String {
    let missing = || tr("tokens: unavailable", "tokens: 不可用").to_owned();
    usage_fact_for(snapshot, Some(agent)).map_or_else(missing, |fact| {
        match fact.summary.total_tokens {
            Some(tokens) => format!("tokens:{tokens}"),
            None => missing(),
        }
    })
}

pub(super) fn truncate_display_label(text: &str, max_width: usize) -> String {
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

pub(super) fn format_usage_evidence(
    snapshot: &CoreSnapshot,
    selected_agent: Option<&crate::agents::AgentSnapshot>,
) -> String {
    let owner = selected_agent.map_or_else(
        || "root".to_owned(),
        |agent| format!("{} {}", tr("child", "子 Agent"), agent.info.id),
    );
    let (usage, identity) = usage_display_fact(snapshot, selected_agent);
    let source = source_label(usage.source);
    let total = shown_value(usage.total_tokens);
    let input = shown_value(usage.input_tokens);
    let cached = shown_value(usage.cached_input_tokens);
    let output = shown_value(usage.output_tokens);
    let reasoning = shown_value(usage.reasoning_tokens);
    let window = shown_value(usage.context_window);
    let thread = identity_value(identity.thread_id.as_deref());
    let turn = identity_value(identity.turn_id.as_deref());
    let generation = shown_value(identity.generation);
    if lang::zh() {
        format!(
            "用量（{owner}）来源: {source} | 总计 {total} | 输入 {input} | 缓存 {cached} | 输出 {output} | 推理 {reasoning} | 上下文窗口 {window} | 归属 线程 {thread} 轮次 {turn} 代次 {generation}"
        )
    } else {
        format!(
            "Usage ({owner}) source: {source} | total {total} | input {input} | cached {cached} | output {output} | reasoning {reasoning} | context window {window} | identity thread {thread} turn {turn} generation {generation}"
        )
    }
}

pub(super) fn usage_fact_for<'a>(
    snapshot: &'a CoreSnapshot,
    selected_agent: Option<&crate::agents::AgentSnapshot>,
) -> Option<&'a UsageFact> {
    let (thread_id, turn_id, generation) = selected_agent.map_or_else(
        || {
            let thread_id = snapshot.thread_id.as_deref();
            let turn_id = snapshot.turn_id.as_deref();
            let generation = thread_id.and_then(|thread_id| {
                turn_id.and_then(|turn_id| {
                    snapshot
                        .observation
                        .activities
                        .iter()
                        .filter(|activity| {
                            activity.scope == ActivityScope::Turn
                                && activity.identity.thread_id.as_deref() == Some(thread_id)
                                && activity.identity.turn_id.as_deref() == Some(turn_id)
                        })
                        .filter_map(|activity| activity.identity.generation)
                        .max()
                })
            });
            (thread_id, turn_id, generation)
        },
        |agent| {
            if agent.awaiting_turn {
                return (None, None, None);
            }
            (
                Some(agent.info.id.as_str()),
                agent.turn_id.as_deref(),
                Some(agent.generation),
            )
        },
    );
    let thread_id = thread_id?;
    let generation = generation?;
    snapshot.usage_facts.iter().find(|fact| {
        fact.summary.has_value()
            && fact.identity.thread_id.as_deref() == Some(thread_id)
            && fact.identity.turn_id.as_deref() == turn_id
            && fact.identity.generation == Some(generation)
    })
}

fn usage_display_fact(
    snapshot: &CoreSnapshot,
    selected_agent: Option<&crate::agents::AgentSnapshot>,
) -> (UsageSummary, UsageIdentity) {
    usage_fact_for(snapshot, selected_agent).map_or_else(
        || (UsageSummary::default(), UsageIdentity::default()),
        |fact| (fact.summary, fact.identity.clone()),
    )
}

fn identity_value(value: Option<&str>) -> String {
    value.map_or_else(|| unavailable().to_owned(), str::to_owned)
}

/// English in both languages; only for text that callers match on.
pub(super) fn usage_value(value: Option<u64>) -> String {
    value.map_or_else(|| "unavailable".to_owned(), |value| value.to_string())
}

/// A number for display, or the localized placeholder when it is missing.
pub(super) fn shown_value(value: Option<u64>) -> String {
    value.map_or_else(|| unavailable().to_owned(), |value| value.to_string())
}

pub(super) fn source_label(source: FactSource) -> &'static str {
    match source {
        FactSource::ServerConfirmed => tr("server confirmed", "服务端确认"),
        FactSource::LocalEstimate => tr("local estimate", "本地估算"),
        FactSource::Unknown => tr("unknown", "未知"),
    }
}

/// Screen rows as text. A double-width character occupies two cells; the
/// second one is a filler and is skipped, so each row reads as it was written.
#[cfg(test)]
pub(super) fn buffer_rows(buffer: &ratatui::buffer::Buffer) -> Vec<String> {
    (0..buffer.area.height)
        .map(|y| {
            let mut row = String::new();
            let mut filler = 0;
            for x in 0..buffer.area.width {
                if filler > 0 {
                    filler -= 1;
                    continue;
                }
                let symbol = buffer[(x, y)].symbol();
                filler = symbol.width().saturating_sub(1);
                row.push_str(symbol);
            }
            row
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::observation::{
        ActivityIdentity, ActivityKind, Attention, AttentionReason, ExecutionState, Freshness,
    };
    use crate::state::{CoreSnapshot, UsageFact, UsageIdentity};

    fn root_turn(generation: u64) -> ActivitySnapshot {
        ActivitySnapshot {
            session_id: "session".into(),
            clock_epoch: "session:clock".into(),
            activity_id: "root-turn".into(),
            identity: ActivityIdentity {
                agent_id: "root".into(),
                task_id: None,
                attempt_id: None,
                thread_id: Some("root-thread".into()),
                turn_id: Some("root-turn".into()),
                generation: Some(generation),
            },
            scope: ActivityScope::Turn,
            kind: ActivityKind::ModelStreaming,
            execution_state: ExecutionState::Running,
            item_id: None,
            request_id: None,
            interaction_state: None,
            tool_category: None,
            started_at_ms: None,
            last_evidence_at_ms: None,
            elapsed_ms: None,
            silence_ms: None,
            freshness: Freshness::Current,
            last_evidence: None,
            recent_evidence: Vec::new(),
            progress_seq: 0,
            output_bytes: 0,
            transition_count: 0,
            child_terminal_count: 0,
            wait_reason: None,
            resume_condition: None,
            wait_targets: Vec::new(),
            attention: Attention {
                level: AttentionLevel::Active,
                reason: AttentionReason::RecentEvidence,
                requires_action: false,
                quiet_after_ms: None,
                attention_after_ms: None,
                config_source: None,
            },
            provider_state: None,
        }
    }

    fn confirmed_fact(thread: &str, turn: &str, generation: u64) -> UsageFact {
        UsageFact {
            summary: UsageSummary {
                total_tokens: Some(12),
                source: FactSource::ServerConfirmed,
                ..Default::default()
            },
            identity: UsageIdentity {
                thread_id: Some(thread.into()),
                turn_id: Some(turn.into()),
                generation: Some(generation),
            },
        }
    }

    #[test]
    fn root_usage_requires_current_turn_activity_generation() {
        let mut snapshot = CoreSnapshot {
            thread_id: Some("root-thread".into()),
            turn_id: Some("root-turn".into()),
            usage_facts: vec![confirmed_fact("root-thread", "root-turn", 3)],
            ..Default::default()
        };
        assert!(usage_fact_for(&snapshot, None).is_none());
        snapshot.observation.activities.push(root_turn(3));
        assert!(usage_fact_for(&snapshot, None).is_some());
        snapshot.observation.activities[0].identity.generation = Some(4);
        assert!(usage_fact_for(&snapshot, None).is_none());
    }

    #[test]
    fn child_awaiting_turn_cannot_display_previous_usage_fact() {
        let snapshot = CoreSnapshot {
            usage_facts: vec![confirmed_fact("child-thread", "old-turn", 1)],
            ..Default::default()
        };
        let child = crate::agents::AgentSnapshot {
            info: crate::agents::AgentInfo {
                id: "child-thread".into(),
                parent_id: "root-thread".into(),
                path: None,
                nickname: None,
                role: None,
                model: None,
                confirmed: true,
            },
            generation: 2,
            turn_id: Some("old-turn".into()),
            outcome: None,
            awaiting_turn: true,
            usage: UsageSummary::default(),
        };
        assert!(usage_fact_for(&snapshot, Some(&child)).is_none());
    }

    fn text_of(line: &Line<'_>) -> String {
        line.spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect()
    }

    #[test]
    fn marked_wrapping_breaks_exactly_like_plain_wrapping() {
        // 样式不能改变断行：测试和滚动计算都按 `wrap` 的行数来。
        let text = "Label: 中文 e\u{301} 👨‍👩‍👧‍👦 value | Running\n  second line \u{7}with control";
        for width in [1, 2, 3, 7, 20, 200] {
            let lines = wrap_marked(text, width, Style::default(), |clean| {
                label_mark(clean)
                    .into_iter()
                    .chain(word_mark(clean, "Running", tone(Color::Red)))
                    .collect()
            });
            assert_eq!(
                lines.iter().map(text_of).collect::<Vec<_>>(),
                wrap(text, width),
                "width {width}"
            );
        }
        let lines = wrap_marked(text, 200, Style::default(), |clean| {
            label_mark(clean).into_iter().collect()
        });
        assert_eq!(lines[0].spans[0].content, "Label:");
        assert_eq!(lines[0].spans[0].style, theme::current().label());
        // 引号、分隔符之后的冒号不是字段标签。
        assert!(label_mark("Thread Some(\"a:b\") turn").is_none());
        assert!(label_mark("a | b: c").is_none());
        assert!(label_mark("no label here").is_none());
    }

    #[test]
    fn clipped_and_selected_rows_never_exceed_their_width() {
        let spans = || {
            vec![
                Span::raw("> "),
                Span::raw("代理👨‍👩‍👧‍👦e\u{301}-long-name"),
                Span::raw("  Ctrl+F"),
            ]
        };
        for width in 0..30 {
            let clipped = Line::from(clip_spans(spans(), width));
            assert!(clipped.width() <= width, "width {width}: {clipped:?}");
            if width > 0 && width < 20 {
                assert!(text_of(&clipped).ends_with('…'), "width {width}");
            }
            let selected = selected_line(spans(), width);
            assert_eq!(selected.width(), width, "width {width}");
        }
        assert_eq!(
            Line::from(clip_spans(spans(), 40)).width(),
            Line::from(spans()).width()
        );
    }

    #[test]
    fn hint_rows_drop_whole_trailing_hints_instead_of_cutting_one() {
        let hints = "Esc close | Enter open | PgUp/PgDn scroll";
        assert_eq!(
            text_of(&hint_row(hints, 80)),
            "Esc close  ·  Enter open  ·  PgUp/PgDn scroll"
        );
        assert_eq!(text_of(&hint_row(hints, 30)), "Esc close  ·  Enter open");
        assert_eq!(text_of(&hint_row(hints, 12)), "Esc close");
        assert!(hint_row(hints, 4).width() <= 4);
        assert_eq!(text_of(&footer(hints, 16)), " Esc close ");
        // 放不下时退回普通换行文字，提示内容不丢。
        let wrapped: String = hint_lines(hints, 14).iter().map(text_of).collect();
        assert_eq!(wrapped, "Esc close · Enter open · PgUp/PgDn scroll");
    }

    #[test]
    fn header_briefs_are_chinese_only_in_chinese_mode() {
        use crate::observation::{Evidence, EvidenceKind, EvidenceSource};

        let reminders = Reminders::default();
        let mut waiting = root_turn(1);
        waiting.kind = ActivityKind::ModelRequest;
        waiting.silence_ms = Some(12_000);
        waiting.last_evidence = Some(Evidence {
            id: 3,
            kind: EvidenceKind::TurnStarted,
            source: EvidenceSource::AppServer,
            recorded_at_ms: None,
            item_id: None,
            request_id: None,
            output_bytes: 0,
        });
        let mut approval = root_turn(1);
        approval.kind = ActivityKind::WaitingApproval;
        approval.attention.requires_action = true;
        let mut silent = root_turn(1);
        silent.kind = ActivityKind::ToolRunning;
        silent.silence_ms = Some(90_000);
        silent.attention.level = AttentionLevel::AttentionNeeded;

        assert_eq!(
            reminder_brief(&waiting, &reminders),
            "Waiting for the model · quiet 12s"
        );
        assert_eq!(
            evidence_brief(&waiting, &reminders),
            "Next: wait; F11 evidence · last event: turn started"
        );
        assert_eq!(
            reminder_brief(&approval, &reminders),
            "Needs your approval · quiet unknown"
        );
        lang::with_zh(|| {
            assert_eq!(
                reminder_brief(&waiting, &reminders),
                "等待模型响应 · 静默 12s"
            );
            assert_eq!(
                evidence_brief(&waiting, &reminders),
                "下一步：等待；F11 查看证据 · 最近事件：轮次开始"
            );
            assert_eq!(
                reminder_brief(&approval, &reminders),
                "等待你审批 · 静默 未知"
            );
            assert_eq!(
                evidence_brief(&approval, &reminders),
                "下一步：F2 回应请求 · 最近事件：未知"
            );
            assert_eq!(
                reminder_brief(&silent, &reminders),
                "正在运行工具 · 静默 90s · 已静默较久，建议查看"
            );
            assert_eq!(
                activity_brief(&waiting),
                "运行中 ModelRequest | 静默 12s | 活跃"
            );
            // 头部会解析这两个返回值，中文模式下也必须是英文原文。
            let snapshot = CoreSnapshot::default();
            assert_eq!(usage_status(&snapshot), "tokens:unavailable/unavailable");
            assert_eq!(
                token_budget_brief(&snapshot),
                "tokens:unavailable/unavailable"
            );
            // 时间线按第一个 ": " 拆出标签和值。
            for row in compaction_fact_rows(None) {
                let (label, value) = row.split_once(": ").unwrap();
                assert!(label.starts_with("压缩"), "{row}");
                assert!(value.contains("不可用"), "{row}");
            }
        });
        assert_eq!(
            activity_brief(&waiting),
            "Running ModelRequest | quiet 12s | Active"
        );
    }

    #[test]
    fn chinese_evidence_panel_keeps_rows_inside_its_borders() {
        use ratatui::{backend::TestBackend, Terminal};

        let mut snapshot = CoreSnapshot::default();
        snapshot.observation.activities.push(root_turn(3));
        let reminders = Reminders::default();
        for width in [40u16, 100] {
            let mut terminal = Terminal::new(TestBackend::new(width, 30)).unwrap();
            lang::with_zh(|| {
                terminal
                    .draw(|frame| {
                        draw_evidence(frame, frame.area(), &snapshot, &reminders, 0, "root", None)
                    })
                    .unwrap();
            });
            let buffer = terminal.backend().buffer();
            let rows = buffer_rows(buffer);
            let screen = rows.join("\n");
            for text in [
                "证据 · F11",
                "Esc 关闭",
                "PgUp/PgDn 滚动",
                "运行中 ModelStreaming",
                "下一步: 等待；F11 查看证据",
                "最近证据: 未知",
            ] {
                assert!(screen.contains(text), "{width}: {text}\n{screen}");
            }
            let joined: String = rows[1..29]
                .iter()
                .flat_map(|row| row.chars())
                .filter(|ch| *ch != '│' && *ch != ' ')
                .collect();
            for text in [
                "用量（root）来源:未知|总计不可用",
                "会话token预算:tokens:不可用/不可用|已触发停止:否|单Agenttoken预算:未设置",
                "模型服务端执行状态:不可用",
                "运行中ModelStreaming|静默未知|活跃",
                "Turn轮次|活跃",
            ] {
                assert!(joined.contains(text), "{width}: {text}\n{screen}");
            }
            for row in &rows[1..29] {
                assert_eq!(row.width(), width as usize, "{row}");
                assert!(row.starts_with('│') && row.ends_with('│'), "{row}");
            }
            // 状态词按中文着色，而不是只有英文枚举名能匹配上。
            let (y, row) = rows
                .iter()
                .enumerate()
                .find(|(_, row)| row.contains("运行中 ModelStreaming"))
                .unwrap();
            let x = row[..row.find("运行中").unwrap()].width() as u16;
            assert_eq!(
                buffer[(x, y as u16)].fg,
                execution_tone(ExecutionState::Running).fg.unwrap(),
                "{screen}"
            );
        }
    }
}
