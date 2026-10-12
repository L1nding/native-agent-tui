//! Read-only tool detail panel, plus the styled-row helpers shared by the
//! timeline and search panels. Rows carry their meaning when they are built, so
//! wrapped continuation rows keep the style of the logical row they belong to.
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Borders, Clear, Paragraph};

use super::lang::{self, tr};
use super::layout::wrap;
use super::scope::Scope;
use super::theme::{self, Theme};
use super::tool_trace;
use crate::observation::{ActivityScope, ExecutionState};
use crate::state::{display_text, CoreSnapshot};
use crate::tool_details::{ToolDetail, ToolDetailLocator, ToolLifecycle, ToolTextSource};

/// One run of text with the style it keeps across wrapped rows.
pub(super) type Segment = (String, Style);

/// How a field value should read. The value text always carries the state;
/// tone only adds emphasis.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Tone {
    Value,
    /// Missing or secondary facts: readable, not competing with real values.
    Quiet,
    Warning,
    Error,
    Lifecycle(ToolLifecycle),
}

impl Tone {
    pub(super) fn style(self, theme: Theme) -> Style {
        match self {
            Self::Value => Style::default().fg(theme.foreground),
            Self::Quiet => theme.secondary(),
            Self::Warning => Style::default()
                .fg(theme.warning)
                .add_modifier(Modifier::BOLD),
            Self::Error => Style::default()
                .fg(theme.error)
                .add_modifier(Modifier::BOLD),
            Self::Lifecycle(lifecycle) => theme.lifecycle(lifecycle),
        }
    }
}

/// `label: value`; an empty label renders the value alone.
///
/// `label` and `value` are the English wording and never change with the
/// interface language: `field_text` builds query-matching text from them. The
/// Chinese wording is only used when the row is drawn.
#[derive(Clone, Debug)]
pub(super) struct Field {
    pub label: String,
    pub value: String,
    pub tone: Tone,
    zh_label: Option<String>,
    zh_value: Option<String>,
}

impl Field {
    pub(super) fn new(label: &str, value: impl Into<String>) -> Self {
        Self {
            label: label.to_owned(),
            value: value.into(),
            tone: Tone::Value,
            zh_label: None,
            zh_value: None,
        }
    }

    /// Chinese label; drawn as `标签：值`.
    pub(super) fn zh(mut self, label: &str) -> Self {
        self.zh_label = Some(label.to_owned());
        self
    }

    /// Chinese wording of a value the UI generated itself.
    pub(super) fn zh_value(mut self, value: impl Into<String>) -> Self {
        self.zh_value = Some(value.into());
        self
    }

    /// A value that may be missing; `None` reads `unavailable` in a quiet tone.
    pub(super) fn optional(label: &str, value: Option<&str>) -> Self {
        match value {
            Some(value) => Self::new(label, value),
            None => Self::new(label, "unavailable")
                .zh_value("不可用")
                .tone(Tone::Quiet),
        }
    }

    pub(super) fn tone(mut self, tone: Tone) -> Self {
        self.tone = tone;
        self
    }
}

/// The English text of a field row; neither styling nor the interface language
/// changes it, so queries match the same rows in both languages.
pub(super) fn field_text(fields: &[Field], separator: &str) -> String {
    fields
        .iter()
        .map(|field| {
            if field.label.is_empty() {
                field.value.clone()
            } else {
                format!("{}: {}", field.label, field.value)
            }
        })
        .collect::<Vec<_>>()
        .join(separator)
}

/// The text of a field row as it is drawn in the current language.
pub(super) fn field_shown_text(fields: &[Field], separator: &str) -> String {
    field_segments(theme::current(), fields, separator)
        .into_iter()
        .map(|(text, _)| text)
        .collect()
}

pub(super) fn field_segments(theme: Theme, fields: &[Field], separator: &str) -> Vec<Segment> {
    let zh = lang::zh();
    let mut segments = Vec::new();
    for (index, field) in fields.iter().enumerate() {
        if index > 0 {
            segments.push((separator.to_owned(), theme.faint()));
        }
        let value = match &field.zh_value {
            Some(value) if zh => value,
            _ => &field.value,
        };
        if field.label.is_empty() {
            segments.push((value.clone(), field.tone.style(theme)));
        } else if let Some(label) = field.zh_label.as_ref().filter(|_| zh) {
            // 全角冒号自带间隔，后面不再加空格。
            segments.push((format!("{label}："), theme.label()));
            segments.push((value.clone(), field.tone.style(theme)));
        } else {
            segments.push((format!("{}:", field.label), theme.label()));
            segments.push((format!(" {value}"), field.tone.style(theme)));
        }
    }
    segments
}

/// Splits a help row into its keys and the rest, separator included. English
/// rows read `keys: action`; Chinese rows read `keys：action`.
pub(super) fn help_parts(line: &str) -> Option<(&str, &str)> {
    let at = if lang::zh() {
        line.find('：')
    } else {
        line.find(": ")
    }?;
    Some(line.split_at(at))
}

/// Execution state as shown to the user. English keeps the variant name.
pub(super) fn execution_text(state: ExecutionState) -> String {
    if !lang::zh() {
        return format!("{state:?}");
    }
    execution_zh(state).to_owned()
}

pub(super) fn execution_zh(state: ExecutionState) -> &'static str {
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
}

pub(super) fn activity_scope_zh(scope: ActivityScope) -> &'static str {
    match scope {
        ActivityScope::Startup => "启动",
        ActivityScope::Turn => "轮次",
        ActivityScope::Tool => "工具",
        ActivityScope::Interaction => "交互",
    }
}

pub(super) fn lifecycle_zh(lifecycle: ToolLifecycle) -> &'static str {
    match lifecycle {
        ToolLifecycle::Running => "运行中",
        ToolLifecycle::Completed => "已完成",
        ToolLifecycle::Failed => "失败",
        ToolLifecycle::Interrupted => "已中断",
        ToolLifecycle::EndedUnknown => "已结束，结果未知",
        ToolLifecycle::Unknown => "未知",
    }
}

/// Thread scope of a filter. English keeps the variant name.
pub(super) fn scope_text(scope: Scope) -> String {
    if !lang::zh() {
        return format!("{scope:?}");
    }
    match scope {
        Scope::Thread => "当前线程",
        Scope::Subtree => "子树",
        Scope::Path => "路径",
        Scope::All => "全部",
    }
    .to_owned()
}

/// Wraps styled segments with the shared grapheme-aware `wrap`; every wrapped
/// row keeps the style of the segment its text came from.
pub(super) fn wrap_segments(segments: &[Segment], width: usize) -> Vec<Line<'static>> {
    let clean: Vec<Segment> = segments
        .iter()
        .map(|(text, style)| (display_text(text), *style))
        .filter(|(text, _)| !text.is_empty())
        .collect();
    let full: String = clean.iter().map(|(text, _)| text.as_str()).collect();
    let mut segment = 0;
    let mut used = 0;
    let mut lines = Vec::new();
    for (index, row) in wrap(&full, width).into_iter().enumerate() {
        while segment < clean.len() && used == clean[segment].0.len() {
            segment += 1;
            used = 0;
        }
        // `wrap` drops the line break that ended the previous source line.
        if index > 0 && segment < clean.len() && clean[segment].0[used..].starts_with('\n') {
            used += 1;
        }
        let mut spans = Vec::new();
        let mut needed = row.len();
        while needed > 0 && segment < clean.len() {
            let (text, style) = &clean[segment];
            let take = needed.min(text.len() - used);
            if take > 0 {
                spans.push(Span::styled(text[used..used + take].to_owned(), *style));
            }
            used += take;
            needed -= take;
            if used == text.len() {
                segment += 1;
                used = 0;
            }
        }
        lines.push(Line::from(spans));
    }
    lines
}

/// The first hint row that fits `width`. Candidates use the `theme::hints`
/// format and are ordered from most to least complete.
pub(super) fn fit_hints(candidates: &[&str], width: usize) -> Option<Line<'static>> {
    candidates
        .iter()
        .map(|candidate| theme::hints(candidate))
        .find(|line| line.width() <= width)
}

/// What a logical row means. Wrapped rows inherit it from the row they came from.
enum Row {
    /// Which item this is: secondary metadata.
    Identity(String),
    /// A notice the reader should not miss; the text states the condition.
    Notice(String),
    Fields(Vec<Field>),
    /// A section heading with an optional quiet remark; a blank row precedes it.
    Section {
        title: String,
        remark: Option<String>,
    },
    /// Tool output shown verbatim; never parsed for labels.
    Output(String),
    Trace {
        text: String,
        uncertain: bool,
    },
    Blank,
}

impl Row {
    fn segments(&self, theme: Theme) -> Vec<Segment> {
        match self {
            Self::Identity(text) => vec![(text.clone(), theme.secondary())],
            Self::Notice(text) => vec![(text.clone(), Tone::Warning.style(theme))],
            Self::Fields(fields) => field_segments(theme, fields, " · "),
            Self::Section { title, remark } => {
                let mut segments = vec![(title.clone(), theme.heading())];
                if let Some(remark) = remark {
                    // 全角冒号后不需要再加空格。
                    let gap = if title.ends_with('：') { "" } else { " " };
                    segments.push((format!("{gap}{remark}"), theme.secondary()));
                }
                segments
            }
            Self::Output(text) => vec![(text.clone(), Style::default().fg(theme.foreground))],
            Self::Trace { text, uncertain } => vec![(
                text.clone(),
                if *uncertain {
                    Style::default().fg(theme.warning)
                } else {
                    theme.secondary()
                },
            )],
            Self::Blank => Vec::new(),
        }
    }
}

fn section(rows: &mut Vec<Row>, title: &str, remark: Option<&str>) {
    rows.push(Row::Blank);
    rows.push(Row::Section {
        title: title.to_owned(),
        remark: remark.map(str::to_owned),
    });
}

fn detail_rows(rows: &mut Vec<Row>, detail: &ToolDetail) {
    rows.push(Row::Fields(vec![
        Field::new("Category", format!("{:?}", detail.category)).zh("类别"),
        Field::new("lifecycle", format!("{:?}", detail.lifecycle))
            .zh("生命周期")
            .zh_value(lifecycle_zh(detail.lifecycle))
            .tone(Tone::Lifecycle(detail.lifecycle)),
        Field::new("authoritative", detail.authoritative.to_string())
            .zh("权威记录")
            .zh_value(if detail.authoritative { "是" } else { "否" }),
    ]));
    if matches!(
        detail.lifecycle,
        ToolLifecycle::EndedUnknown | ToolLifecycle::Unknown
    ) {
        // Restates the Core lifecycle fact; no result is inferred here.
        rows.push(Row::Notice(
            tr(
                "Outcome unknown: Core has no confirmed result for this tool.",
                "结果未知：Core 没有确认这个工具的结果。",
            )
            .into(),
        ));
    }
    rows.push(Row::Fields(vec![
        Field::optional(
            "Exit code",
            detail.exit_code.map(|c| c.to_string()).as_deref(),
        )
        .zh("退出码"),
        match detail.duration_ms {
            Some(ms) => Field::new("duration", format!("{ms} ms")),
            None => Field::new("duration", "unavailable ms")
                .zh_value("不可用")
                .tone(Tone::Quiet),
        }
        .zh("耗时"),
    ]));
    if let Some(name) = &detail.name {
        rows.push(Row::Fields(vec![
            Field::new("Tool", name.clone()).zh("工具")
        ]));
    }
    for (label, zh, value) in [
        ("Command", "命令", &detail.command),
        ("Cwd", "工作目录", &detail.cwd),
        ("Parameters", "参数", &detail.parameters),
        ("Result", "结果", &detail.result),
    ] {
        rows.push(Row::Fields(vec![
            Field::optional(label, value.as_deref()).zh(zh)
        ]));
    }
    rows.push(Row::Fields(vec![
        Field::new("Output source", format!("{:?}", detail.output_source))
            .zh("输出来源")
            .zh_value(match detail.output_source {
                ToolTextSource::CommandAggregate => "命令汇总输出",
                ToolTextSource::OutputDelta => "增量输出",
                ToolTextSource::Unavailable => "不可用",
            })
            .tone(Tone::Quiet),
        Field::new("", format!("observed {} bytes", detail.bytes_observed))
            .zh_value(format!("已观测 {} 字节", detail.bytes_observed))
            .tone(Tone::Quiet),
        Field::new("", format!("retained {} bytes", detail.bytes_retained))
            .zh_value(format!("已保留 {} 字节", detail.bytes_retained))
            .tone(Tone::Quiet),
        Field::new("", format!("clipped {}", detail.clipped))
            .zh_value(if detail.clipped {
                "已截断"
            } else {
                "未截断"
            })
            .tone(if detail.clipped {
                Tone::Warning
            } else {
                Tone::Quiet
            }),
    ]));
    let heading = tr("Combined output:", "合并输出：");
    if detail.output.is_empty() {
        section(rows, heading, Some(tr("unavailable", "不可用")));
    } else {
        section(rows, heading, None);
        rows.push(Row::Output(detail.output.clone()));
    }
}

fn rows(current: &CoreSnapshot, locator: &ToolDetailLocator, notice: Option<&str>) -> Vec<Row> {
    let missing = tr("unavailable", "不可用");
    let thread = locator.identity.thread_id.as_deref().unwrap_or(missing);
    let turn = locator.identity.turn_id.as_deref().unwrap_or(missing);
    let mut rows = vec![Row::Identity(if lang::zh() {
        format!("条目：{} · 线程：{thread} · 轮次：{turn}", locator.item_id)
    } else {
        format!(
            "Item: {} · thread: {thread} · turn: {turn}",
            locator.item_id
        )
    })];
    if let Some(notice) = notice {
        rows.push(Row::Notice(notice.to_owned()));
    }
    if let Some(detail) = current.tool_details.get(locator) {
        detail_rows(&mut rows, &detail);
    } else {
        rows.push(Row::Notice(
            tr(
                "Tool detail unavailable: exact detail was not retained.",
                "工具详情不可用：没有保留这条的精确详情。",
            )
            .into(),
        ));
    }
    section(
        &mut rows,
        tr("Live tool trace · metadata only", "实时工具轨迹 · 仅元数据"),
        None,
    );
    rows.extend(
        tool_trace::rows(locator, current)
            .into_iter()
            .map(|row| Row::Trace {
                text: row.text,
                uncertain: row.uncertain,
            }),
    );
    rows
}

/// Draws the panel and returns the largest useful scroll offset.
pub(super) fn draw(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    current: &CoreSnapshot,
    locator: &ToolDetailLocator,
    scroll: usize,
    notice: Option<&str>,
) -> usize {
    draw_themed(
        frame,
        area,
        current,
        locator,
        scroll,
        notice,
        theme::current(),
    )
}

fn draw_themed(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    current: &CoreSnapshot,
    locator: &ToolDetailLocator,
    scroll: usize,
    notice: Option<&str>,
    theme: Theme,
) -> usize {
    frame.render_widget(Clear, area);
    let width = area.width.saturating_sub(2) as usize;
    let wrapped: Vec<Line<'static>> = rows(current, locator, notice)
        .iter()
        .flat_map(|row| wrap_segments(&row.segments(theme), width))
        .collect();
    let body_height = area.height.saturating_sub(2) as usize;
    let max = wrapped.len().saturating_sub(body_height);
    // The frame names the panel once; close and scroll hints sit on the bottom
    // edge so they never take a body row.
    let mut block = theme::focus_panel()
        .borders(Borders::ALL)
        .title(tr(" Tool detail ", " 工具详情 "));
    if let Some(hints) = fit_hints(
        &[
            tr("Esc back | PgUp/PgDn scroll", "Esc 返回 | PgUp/PgDn 滚动"),
            tr("Esc back | PgUp/PgDn", "Esc 返回 | PgUp/PgDn"),
            tr("Esc back", "Esc 返回"),
        ],
        width.saturating_sub(2),
    ) {
        let mut spans = vec![Span::raw(" ")];
        spans.extend(hints.spans);
        spans.push(Span::raw(" "));
        block = block.title_bottom(Line::from(spans));
    }
    frame.render_widget(
        Paragraph::new(
            wrapped
                .into_iter()
                .skip(scroll.min(max))
                .take(body_height)
                .collect::<Vec<_>>(),
        )
        .block(block),
        area,
    );
    max
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::observation::{
        ActivityIdentity, ActivityKind, ActivityScope, Evidence, EvidenceKind, EvidenceSource,
        ExecutionState,
    };
    use crate::protocol::ToolCategory;
    use crate::timeline::{TimelineEntry, TimelineSnapshot};
    use crate::tool_details::{ToolDetailsSnapshot, ToolTextSource};
    use ratatui::backend::TestBackend;
    use ratatui::buffer::Buffer;
    use ratatui::style::Color;
    use ratatui::Terminal;
    use std::collections::VecDeque;
    use std::sync::Arc;

    /// Distinct colors per role so a wrong style cannot pass by coincidence.
    fn palette() -> Theme {
        Theme {
            background: Color::Indexed(1),
            surface: Color::Indexed(2),
            foreground: Color::Indexed(3),
            muted: Color::Indexed(4),
            border: Color::Indexed(5),
            accent: Color::Indexed(6),
            agent: Color::Indexed(7),
            success: Color::Indexed(8),
            warning: Color::Indexed(9),
            error: Color::Indexed(10),
            selection: Color::Indexed(11),
            monochrome: false,
        }
    }

    fn mono() -> Theme {
        Theme {
            background: Color::Reset,
            surface: Color::Reset,
            foreground: Color::Reset,
            muted: Color::Reset,
            border: Color::Reset,
            accent: Color::Reset,
            agent: Color::Reset,
            success: Color::Reset,
            warning: Color::Reset,
            error: Color::Reset,
            selection: Color::Reset,
            monochrome: true,
        }
    }

    fn locator() -> ToolDetailLocator {
        ToolDetailLocator {
            session_id: "session".into(),
            identity: ActivityIdentity {
                agent_id: "root".into(),
                task_id: None,
                attempt_id: Some(1),
                thread_id: Some("thread".into()),
                turn_id: Some("turn".into()),
                generation: Some(1),
            },
            item_id: "item".into(),
        }
    }

    fn detail(lifecycle: ToolLifecycle) -> ToolDetail {
        ToolDetail {
            locator: locator(),
            revision: 1,
            category: ToolCategory::Shell,
            lifecycle,
            name: None,
            command: Some("echo hi".into()),
            cwd: None,
            parameters: None,
            result: None,
            output: "plain output".into(),
            output_source: ToolTextSource::CommandAggregate,
            exit_code: Some(0),
            duration_ms: Some(5),
            bytes_observed: 12,
            bytes_retained: 12,
            clipped: false,
            authoritative: true,
        }
    }

    fn trace_entry(id: u64, kind: EvidenceKind, state: ExecutionState) -> Arc<TimelineEntry> {
        let locator = locator();
        Arc::new(TimelineEntry {
            activity_id: "tool".into(),
            identity: locator.identity,
            scope: ActivityScope::Tool,
            activity_kind: ActivityKind::ToolRunning,
            execution_state: state,
            interaction_state: None,
            tool_category: Some(ToolCategory::Shell),
            item_id: Some(locator.item_id.clone()),
            evidence: Evidence {
                id,
                kind,
                source: EvidenceSource::AppServer,
                recorded_at_ms: Some(id),
                item_id: Some(locator.item_id),
                request_id: None,
                output_bytes: 0,
            },
            request: None,
            wait_targets: Vec::new(),
            compaction: None,
        })
    }

    fn snapshot(detail: Option<ToolDetail>, trace: Vec<Arc<TimelineEntry>>) -> CoreSnapshot {
        CoreSnapshot {
            tool_details: ToolDetailsSnapshot {
                entries: Arc::new(detail.into_iter().map(Arc::new).collect::<VecDeque<_>>()),
                ..Default::default()
            },
            timeline: TimelineSnapshot {
                session_id: "session".into(),
                entries: Arc::new(trace.into()),
                ..Default::default()
            },
            ..Default::default()
        }
    }

    fn render(
        current: &CoreSnapshot,
        width: u16,
        height: u16,
        scroll: usize,
        theme: Theme,
    ) -> (Buffer, usize) {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        let mut max = 0;
        terminal
            .draw(|frame| {
                max = draw_themed(
                    frame,
                    frame.area(),
                    current,
                    &locator(),
                    scroll,
                    None,
                    theme,
                );
            })
            .unwrap();
        (terminal.backend().buffer().clone(), max)
    }

    fn screen_rows(buffer: &Buffer) -> Vec<String> {
        (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect()
            })
            .collect()
    }

    /// Body rows without the frame; fixtures use single-width characters only.
    fn body_rows(buffer: &Buffer) -> Vec<String> {
        let rows = screen_rows(buffer);
        rows[1..rows.len() - 1]
            .iter()
            .map(|row| row[3..row.len() - 3].trim_end().to_owned())
            .collect()
    }

    /// Screen position of the first body cell showing `needle`.
    fn locate(buffer: &Buffer, needle: &str) -> (u16, u16) {
        for (y, row) in body_rows(buffer).iter().enumerate() {
            if let Some(index) = row.find(needle) {
                let x = row[..index].chars().count();
                return (x as u16 + 1, y as u16 + 1);
            }
        }
        panic!("{needle:?} not rendered in {:#?}", screen_rows(buffer));
    }

    fn style_at(buffer: &Buffer, needle: &str) -> (Color, Modifier) {
        let cell = &buffer[locate(buffer, needle)];
        (cell.fg, cell.modifier)
    }

    #[test]
    fn lifecycle_value_takes_its_semantic_color_and_keeps_the_state_word() {
        let theme = palette();
        for (lifecycle, word, color) in [
            (ToolLifecycle::Running, "Running", theme.accent),
            (ToolLifecycle::Completed, "Completed", theme.success),
            (ToolLifecycle::Failed, "Failed", theme.error),
            (ToolLifecycle::Interrupted, "Interrupted", theme.error),
            (ToolLifecycle::EndedUnknown, "EndedUnknown", theme.warning),
            (ToolLifecycle::Unknown, "Unknown", theme.warning),
        ] {
            let current = snapshot(Some(detail(lifecycle)), Vec::new());
            let (buffer, _) = render(&current, 100, 30, 0, theme);
            let text = body_rows(&buffer).join("\n");
            assert!(
                text.contains(&format!(
                    "Category: Shell · lifecycle: {word} · authoritative: true"
                )),
                "{text}"
            );
            let (x, y) = locate(&buffer, &format!("lifecycle: {word}"));
            let value = &buffer[(x + "lifecycle: ".len() as u16, y)];
            assert_eq!(value.fg, color, "{lifecycle:?}");
            assert!(value.modifier.contains(Modifier::BOLD), "{lifecycle:?}");
            // The label and the neighbouring value are not painted as lifecycle.
            assert_eq!(buffer[(x, y)].fg, theme.foreground);
            assert_eq!(style_at(&buffer, "Shell").0, theme.foreground);

            let (plain, _) = render(&current, 100, 30, 0, mono());
            let text = body_rows(&plain).join("\n");
            assert!(text.contains(&format!("lifecycle: {word}")), "{text}");
            let unknown = matches!(
                lifecycle,
                ToolLifecycle::EndedUnknown | ToolLifecycle::Unknown
            );
            assert_eq!(text.contains("Outcome unknown"), unknown, "{text}");
        }
    }

    #[test]
    fn unknown_outcome_notice_and_uncertain_trace_rows_use_warning() {
        let theme = palette();
        let current = snapshot(
            Some(detail(ToolLifecycle::EndedUnknown)),
            vec![
                trace_entry(1, EvidenceKind::ToolStarted, ExecutionState::Running),
                trace_entry(2, EvidenceKind::ExecutionUnknown, ExecutionState::Unknown),
            ],
        );
        let (buffer, _) = render(&current, 110, 30, 0, theme);
        let (fg, modifier) = style_at(&buffer, "Outcome unknown");
        assert_eq!(fg, theme.warning);
        assert!(modifier.contains(Modifier::BOLD));
        assert_eq!(style_at(&buffer, "Evidence #1").0, theme.muted);
        assert_eq!(style_at(&buffer, "Evidence #2").0, theme.warning);
    }

    #[test]
    fn title_and_hints_appear_once_and_narrow_windows_stay_aligned() {
        let current = snapshot(Some(detail(ToolLifecycle::Completed)), Vec::new());
        for (width, height) in [(30, 10), (24, 8), (80, 24)] {
            let (buffer, _) = render(&current, width, height, 0, palette());
            let rows = screen_rows(&buffer);
            let text = rows.join("\n");
            assert_eq!(text.matches("Tool detail").count(), 1, "{text}");
            assert_eq!(text.matches("Esc back").count(), 1, "{text}");
            assert!(rows[0].contains(" Tool detail "), "{text}");
            assert!(rows[height as usize - 1].contains("Esc back"), "{text}");
            // The body starts with the identity row, not a repeated title.
            assert!(rows[1].contains("Item: item"), "{text}");
            // Nothing spills over the frame.
            for row in &rows[1..height as usize - 1] {
                assert!(row.starts_with('│') && row.ends_with('│'), "{text}");
            }
        }
        let (wide, _) = render(&current, 80, 24, 0, palette());
        assert!(screen_rows(&wide)[23].contains("PgUp/PgDn scroll"));
        // Hints follow the shared key/label hierarchy.
        let bottom = &screen_rows(&wide)[23];
        let x = bottom.chars().position(|c| c == 'E').unwrap() as u16;
        assert!(wide[(x, 23)].modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn wrapped_fields_keep_their_style_and_output_is_never_read_as_a_field() {
        let theme = palette();
        let mut value = detail(ToolLifecycle::Completed);
        value.command = Some("c".repeat(60));
        value.output = "note: BODYMARK\nError: OUTMARK".into();
        let current = snapshot(Some(value), Vec::new());
        let (buffer, _) = render(&current, 32, 40, 0, theme);
        let rows = body_rows(&buffer);

        let command = rows
            .iter()
            .position(|row| row.starts_with("Command: "))
            .unwrap();
        assert_eq!(rows[command], format!("Command: {}", "c".repeat(21)));
        assert_eq!(rows[command + 1], "c".repeat(30));
        assert_eq!(rows[command + 2], "c".repeat(9));
        let y = command as u16 + 1;
        // Label, then value; continuation rows stay value rows.
        assert_eq!(buffer[(1, y)].fg, theme.foreground);
        assert!(buffer[(1, y)].modifier.contains(Modifier::BOLD));
        for cell in [(10, y), (1, y + 1), (30, y + 1), (1, y + 2)] {
            assert_eq!(buffer[cell].fg, theme.foreground, "{cell:?}");
            assert!(
                !buffer[cell].modifier.contains(Modifier::BOLD),
                "continuation {cell:?} must not look like a label"
            );
        }

        // Output lines keep their text and line breaks, in one plain style.
        let note = rows.iter().position(|row| row == "note: BODYMARK").unwrap();
        assert_eq!(rows[note + 1], "Error: OUTMARK");
        for row in [note, note + 1] {
            for x in 1..=14 {
                let cell = &buffer[(x, row as u16 + 1)];
                assert_eq!(cell.fg, theme.foreground);
                assert!(!cell.modifier.contains(Modifier::BOLD), "{}", rows[row]);
            }
        }
    }

    #[test]
    fn sections_metadata_and_missing_values_form_a_visual_hierarchy() {
        let theme = palette();
        let current = snapshot(
            Some(detail(ToolLifecycle::Completed)),
            vec![trace_entry(
                1,
                EvidenceKind::ToolCompleted,
                ExecutionState::Completed,
            )],
        );
        let (buffer, max) = render(&current, 100, 30, 0, theme);
        let rows = body_rows(&buffer);
        assert_eq!(max, 0);

        // Identity and source metadata recede.
        assert_eq!(style_at(&buffer, "Item: item").0, theme.muted);
        assert_eq!(style_at(&buffer, "CommandAggregate").0, theme.muted);
        // Labels are bold foreground; values are plain foreground.
        let (fg, modifier) = style_at(&buffer, "Command:");
        assert_eq!(fg, theme.foreground);
        assert!(modifier.contains(Modifier::BOLD));
        let (fg, modifier) = style_at(&buffer, "echo hi");
        assert_eq!(fg, theme.foreground);
        assert!(!modifier.contains(Modifier::BOLD));
        // Missing values stay readable but quiet.
        assert!(rows.contains(&"Cwd: unavailable".to_owned()));
        let (x, y) = locate(&buffer, "Cwd: unavailable");
        assert_eq!(buffer[(x + 5, y)].fg, theme.muted);

        // Section headings stand out and get a blank row above.
        for heading in ["Combined output:", "Live tool trace · metadata only"] {
            let index = rows.iter().position(|row| row == heading).unwrap();
            assert_eq!(rows[index - 1], "", "no blank row above {heading}");
            let cell = &buffer[(1, index as u16 + 1)];
            assert_eq!(cell.fg, theme.accent);
            assert!(cell.modifier.contains(Modifier::BOLD));
        }
        assert_eq!(style_at(&buffer, "Evidence #1").0, theme.muted);

        // Without color, the hierarchy still shows through weight and text.
        let (plain, _) = render(&current, 100, 30, 0, mono());
        assert!(style_at(&plain, "Command:").1.contains(Modifier::BOLD));
        assert!(!style_at(&plain, "echo hi").1.contains(Modifier::BOLD));
        assert!(style_at(&plain, "Combined output:")
            .1
            .contains(Modifier::BOLD));

        let mut empty = detail(ToolLifecycle::Running);
        empty.output.clear();
        let (buffer, _) = render(&snapshot(Some(empty), Vec::new()), 100, 30, 0, theme);
        let (x, y) = locate(&buffer, "Combined output: unavailable");
        assert_eq!(buffer[(x, y)].fg, theme.accent);
        assert_eq!(buffer[(x + 17, y)].fg, theme.muted);
    }

    #[test]
    fn max_scroll_reaches_exactly_the_last_wrapped_row() {
        let mut value = detail(ToolLifecycle::Completed);
        value.output = (1..=40)
            .map(|line| format!("line-{line:02}"))
            .collect::<Vec<_>>()
            .join("\n");
        let current = snapshot(
            Some(value),
            vec![trace_entry(
                1,
                EvidenceKind::ToolCompleted,
                ExecutionState::Completed,
            )],
        );
        for (width, height) in [(80, 12), (30, 10), (24, 8)] {
            let (top, max) = render(&current, width, height, 0, palette());
            assert!(max > 0);
            assert!(body_rows(&top)[0].starts_with("Item: item"));
            let (bottom, same) = render(&current, width, height, max, palette());
            assert_eq!(same, max);
            let rows = body_rows(&bottom);
            let text = rows.join("");
            assert!(text.ends_with("recorded_at_ms=1"), "{rows:#?}");
            assert!(!rows.last().unwrap().is_empty(), "scrolled past the end");
            // Scrolling beyond the range is clamped to the same view.
            let (clamped, _) = render(&current, width, height, max + 50, palette());
            assert_eq!(body_rows(&clamped), rows);
        }
    }

    #[test]
    fn wrap_segments_keeps_styles_across_wraps_line_breaks_and_wide_graphemes() {
        let a = Style::default().fg(Color::Indexed(1));
        let b = Style::default().fg(Color::Indexed(2));
        let lines = wrap_segments(
            &[
                ("ab\u{1b}".into(), a),
                (String::new(), a),
                ("cd\n\n中文e".into(), b),
            ],
            3,
        );
        let seen: Vec<Vec<(String, Style)>> = lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| (span.content.to_string(), span.style))
                    .collect()
            })
            .collect();
        assert_eq!(
            seen,
            vec![
                vec![("ab".to_owned(), a), ("c".to_owned(), b)],
                vec![("d".to_owned(), b)],
                vec![],
                vec![("中".to_owned(), b)],
                vec![("文e".to_owned(), b)],
            ]
        );
        assert_eq!(wrap_segments(&[], 10).len(), 1);
    }
}
