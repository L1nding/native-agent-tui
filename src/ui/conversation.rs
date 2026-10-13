//! Conversation projection: one layout shared by painting, scrolling and search focus.
//!
//! Every source grapheme stays on screen and wrapped rows are byte ranges of the
//! sanitized message text, so a search offset maps to its row without a second
//! wrapping algorithm. Markdown delimiters remain visible but subdued, which keeps
//! streaming fragments and literal search honest. The only rows that hide text are
//! folded tool groups, and those unfold whenever a search focus points into them.
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthStr;

use super::lang::{self, tr};
use super::layout::wrap_words;
use super::search::Focus;
use super::theme::{self, Theme};
use crate::state::{display_text, ConversationItem};
use crate::tool_details::ToolLifecycle;

#[cfg(test)]
thread_local! {
    pub(super) static PROJECTED_MESSAGES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// A turn's tool summary folds once it has more calls than this.
pub(super) const TOOL_FOLD_LIMIT: usize = 4;
/// Unfinished or failed calls that stay visible while a group is folded.
const FOLDED_ATTENTION_ROWS: usize = 3;
const INDENT: usize = 2;
const TOOL_MARKER: &str = "▸ ";

/// How tool calls appear in the conversation; `Ctrl+E` cycles through them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(super) enum ToolDisplay {
    /// One row per call; long groups fold to a summary.
    #[default]
    Preview,
    /// Every call, each followed by the facts Core has about it.
    Expanded,
    /// No tool rows; the steps of one answer read as a single message.
    Hidden,
}

impl From<bool> for ToolDisplay {
    fn from(expanded: bool) -> Self {
        if expanded {
            Self::Expanded
        } else {
            Self::Preview
        }
    }
}

/// Hidden tool groups stay out of the layout unless the search focus is in one.
fn shown(message: &ConversationItem, display: ToolDisplay, focus: Option<&Focus>) -> bool {
    display != ToolDisplay::Hidden
        || message.role != "Tool"
        || focus.is_some_and(|focus| focus.matches(message))
}

fn continues(previous: Option<&ConversationItem>, message: &ConversationItem) -> bool {
    previous.is_some_and(|previous| {
        previous.role == "Agent"
            && message.role == "Agent"
            && previous.thread_id == message.thread_id
            && previous.turn_id == message.turn_id
    })
}

/// With tools hidden, the later steps of one answer do not repeat its title.
fn continued<'a>(
    messages: impl Iterator<Item = &'a ConversationItem>,
    display: ToolDisplay,
) -> std::collections::HashSet<*const ConversationItem> {
    let mut set = std::collections::HashSet::new();
    if display == ToolDisplay::Hidden {
        let mut previous = None;
        for message in messages {
            if continues(previous, message) {
                set.insert(std::ptr::from_ref(message));
            }
            previous = Some(message);
        }
    }
    set
}

pub(super) struct Transcript {
    pub lines: Vec<Line<'static>>,
    /// Row of the search focus while its message is still present and unchanged.
    pub focused: Option<usize>,
    /// Tool groups long enough to fold; the key hint is only useful when non-zero.
    pub foldable: usize,
}

/// A bounded cache of message layouts, owned by this UI only. Width changes are
/// lazy: old history is laid out only when scrolling brings it into the viewport.
#[derive(Default)]
pub(super) struct ProjectionCache {
    entries: std::collections::VecDeque<CachedMessage>,
    bytes: usize,
    anchor: Option<ScrollAnchor>,
    requested_scroll: usize,
}

struct ScrollAnchor {
    thread: String,
    turn: String,
    item: String,
    role: String,
    row: usize,
}

impl ScrollAnchor {
    fn matches(&self, message: &ConversationItem) -> bool {
        self.thread == message.thread_id
            && self.turn == message.turn_id
            && self.item == message.id
            && self.role == message.role
    }
}

struct CachedMessage {
    message: ConversationItem,
    width: usize,
    display: ToolDisplay,
    continued: bool,
    target: Option<usize>,
    theme: &'static str,
    zh: bool,
    transcript: Transcript,
    bytes: usize,
}

pub(super) struct Viewport {
    pub transcript: Transcript,
    pub below: usize,
}

impl ProjectionCache {
    fn message(
        &mut self,
        message: &ConversationItem,
        focus: Option<&Focus>,
        width: usize,
        display: ToolDisplay,
        continued: bool,
    ) -> &Transcript {
        let target = focus
            .filter(|focus| focus.matches(message))
            .map(|focus| focus.offset);
        let found = self.entries.iter().position(|entry| {
            entry.message.id == message.id
                && entry.message.thread_id == message.thread_id
                && entry.message.turn_id == message.turn_id
                && entry.message.role == message.role
        });
        if let Some(index) = found {
            let entry = self.entries.remove(index).unwrap();
            self.bytes -= entry.bytes;
            if entry.message == *message
                && entry.width == width
                && entry.display == display
                && entry.continued == continued
                && entry.target == target
                && entry.theme == theme::name()
                && entry.zh == lang::zh()
            {
                self.bytes += entry.bytes;
                self.entries.push_front(entry);
                return &self.entries[0].transcript;
            }
        }
        let transcript = layout(std::iter::once(message), focus, width, display, continued);
        let bytes = std::mem::size_of::<CachedMessage>()
            + message.text.len()
            + message.id.len()
            + message.thread_id.len()
            + message.turn_id.len()
            + message.role.len()
            + message
                .tools
                .iter()
                .map(crate::tool_details::ConversationTool::bytes)
                .sum::<usize>()
            + transcript.lines.capacity() * std::mem::size_of::<Line<'static>>()
            + transcript
                .lines
                .iter()
                .map(|line| {
                    line.spans.capacity() * std::mem::size_of::<Span<'static>>()
                        + line
                            .spans
                            .iter()
                            .map(|span| span.content.len())
                            .sum::<usize>()
                })
                .sum::<usize>();
        // Keep at most 64 hot messages / 4 MiB of derived UI data. One oversized
        // message may occupy the cache alone, so returning a layout stays simple.
        while !self.entries.is_empty()
            && (self.entries.len() >= 64 || self.bytes + bytes > 4 * 1024 * 1024)
        {
            self.bytes -= self.entries.pop_back().unwrap().bytes;
        }
        self.bytes += bytes;
        self.entries.push_front(CachedMessage {
            message: message.clone(),
            width,
            display,
            continued,
            target,
            theme: theme::name(),
            zh: lang::zh(),
            transcript,
            bytes,
        });
        &self.entries[0].transcript
    }

    /// Search navigation and painting use exactly the same cached row positions.
    pub fn focused_scroll<'a>(
        &mut self,
        messages: impl DoubleEndedIterator<Item = &'a ConversationItem> + Clone,
        focus: &Focus,
        width: usize,
        height: usize,
        display: impl Into<ToolDisplay>,
    ) -> Option<usize> {
        let display = display.into();
        let messages = messages.filter(|message| shown(message, display, Some(focus)));
        let continued = continued(messages.clone(), display);
        let mut below = 0;
        for message in messages.rev() {
            let layout = self.message(
                message,
                Some(focus),
                width,
                display,
                continued.contains(&std::ptr::from_ref(message)),
            );
            if let Some(row) = layout.focused {
                below += layout.lines.len().saturating_sub(row + 1);
                return Some(below.saturating_sub(height.saturating_sub(height / 3 + 1)));
            }
            below += layout.lines.len() + 1;
        }
        None
    }

    pub fn viewport<'a>(
        &mut self,
        messages: impl DoubleEndedIterator<Item = &'a ConversationItem> + Clone,
        focus: Option<&Focus>,
        width: usize,
        height: usize,
        scroll: usize,
        display: impl Into<ToolDisplay>,
    ) -> Viewport {
        let display = display.into();
        let messages = messages.filter(|message| shown(message, display, focus));
        let continued = continued(messages.clone(), display);
        let continued = &continued;
        let requested = scroll;
        let anchored = if focus.is_none() && scroll > 0 && scroll == self.requested_scroll {
            self.anchor.take().map(|anchor| {
                let mut below = 0;
                for message in messages.clone().rev() {
                    if below > 0 {
                        below += 1;
                    }
                    let layout = self.message(
                        message,
                        None,
                        width,
                        display,
                        continued.contains(&std::ptr::from_ref(message)),
                    );
                    if anchor.matches(message) {
                        let row = anchor.row.min(layout.lines.len());
                        return (below + layout.lines.len()).saturating_sub(row + height);
                    }
                    below += layout.lines.len();
                }
                // 被淘汰的锚点回到最早保留内容，不能突然跳到流式尾部。
                below.saturating_sub(height)
            })
        } else {
            None
        };
        let scroll = focus
            .filter(|focus| messages.clone().any(|message| focus.matches(message)))
            .and_then(|focus| self.focused_scroll(messages.clone(), focus, width, height, display))
            .or(anchored)
            .unwrap_or(scroll);
        let view = self.window(
            messages.clone(),
            focus,
            width,
            height,
            scroll,
            display,
            continued,
        );
        self.requested_scroll = requested;
        self.anchor = if view.below > 0 {
            let top = view.below + view.transcript.lines.len();
            let mut below = 0;
            let mut anchor = None;
            for message in messages.rev() {
                let separator = usize::from(below > 0);
                let layout = self.message(
                    message,
                    focus,
                    width,
                    display,
                    continued.contains(&std::ptr::from_ref(message)),
                );
                let end = below + separator + layout.lines.len();
                if top <= end {
                    anchor = Some(ScrollAnchor {
                        thread: message.thread_id.clone(),
                        turn: message.turn_id.clone(),
                        item: message.id.clone(),
                        role: message.role.clone(),
                        row: end.saturating_sub(top).min(layout.lines.len()),
                    });
                    break;
                }
                below = end;
            }
            anchor
        } else {
            None
        };
        view
    }

    #[allow(clippy::too_many_arguments)]
    fn window<'a>(
        &mut self,
        messages: impl DoubleEndedIterator<Item = &'a ConversationItem> + Clone,
        focus: Option<&Focus>,
        width: usize,
        height: usize,
        scroll: usize,
        display: ToolDisplay,
        continued: &std::collections::HashSet<*const ConversationItem>,
    ) -> Viewport {
        let retry = messages.clone();
        let mut visible = std::collections::VecDeque::new();
        let mut seen = 0usize;
        let mut foldable = 0;
        let mut focus_from_bottom = None;
        for message in messages.rev() {
            if seen > 0 {
                if seen >= scroll && visible.len() < height {
                    visible.push_front(Line::default());
                }
                seen += 1;
            }
            let layout = self.message(
                message,
                focus,
                width,
                display,
                continued.contains(&std::ptr::from_ref(message)),
            );
            foldable += layout.foldable;
            if let Some(row) = layout.focused {
                focus_from_bottom = Some(seen + layout.lines.len().saturating_sub(row + 1));
            }
            // Clone only visible styled rows, never the whole retained history.
            for line in layout
                .lines
                .iter()
                .rev()
                .skip(scroll.saturating_sub(seen))
                .take(height.saturating_sub(visible.len()))
            {
                visible.push_front(line.clone());
            }
            seen += layout.lines.len();
            if seen >= scroll.saturating_add(height) {
                break;
            }
        }
        // Ctrl+PageUp can overshoot the oldest row. Fill from the top just as the
        // uncached transcript did, instead of showing an empty conversation.
        if seen < scroll.saturating_add(height) && scroll > 0 {
            let clamped = seen.saturating_sub(height);
            if clamped != scroll {
                return self.window(retry, focus, width, height, clamped, display, continued);
            }
        }
        let focused = focus_from_bottom.and_then(|row| {
            row.checked_sub(scroll)
                .and_then(|from_bottom| visible.len().checked_sub(from_bottom + 1))
        });
        Viewport {
            transcript: Transcript {
                lines: visible.into(),
                focused,
                foldable,
            },
            below: scroll,
        }
    }
}

/// Style runs over one source line as `(end byte, style)`, in order.
type Runs = Vec<(usize, Style)>;

#[cfg(test)]
pub(super) fn transcript<'a>(
    messages: impl Iterator<Item = &'a ConversationItem>,
    focus: Option<&Focus>,
    width: usize,
    display: impl Into<ToolDisplay>,
) -> Transcript {
    layout(messages, focus, width, display.into(), false)
}

/// Rows under one expanded tool call: only facts Core reported, nothing inferred.
/// The outcome and exit code are already in the call's own row.
fn tool_detail_rows(tool: &crate::tool_details::ConversationTool) -> Vec<String> {
    let mut rows = Vec::new();
    if let Some(cwd) = &tool.cwd {
        rows.push(display_text(cwd));
    }
    if tool.command.is_some() {
        if let Some(target) = &tool.target {
            rows.push(display_text(target));
        }
    }
    if tool.added_lines.is_some() || tool.removed_lines.is_some() {
        rows.push(format!(
            "└ +{} -{}",
            tool.added_lines.unwrap_or(0),
            tool.removed_lines.unwrap_or(0)
        ));
    }
    let mut tail = Vec::new();
    if let Some(lines) = tool.output_lines {
        tail.push(format!(
            "… {lines} {}",
            tr("output lines (Ctrl+T for details)", "行输出（Ctrl+T 查看）")
        ));
    }
    if !tail.is_empty() {
        rows.push(tail.join(" · "));
    }
    rows
}

/// `first_continued`: the first message continues an answer laid out before it.
fn layout<'a>(
    messages: impl Iterator<Item = &'a ConversationItem>,
    focus: Option<&Focus>,
    width: usize,
    display: ToolDisplay,
    first_continued: bool,
) -> Transcript {
    let theme = theme::current();
    // 极窄窗口不缩进，把宽度全部留给正文。
    let indent = if width > 12 { INDENT } else { 0 };
    let body = width.saturating_sub(indent).max(1);
    let mut out = Transcript {
        lines: Vec::new(),
        focused: None,
        foldable: 0,
    };
    let mut previous: Option<&ConversationItem> = None;
    let mut first = true;
    for message in messages {
        if !shown(message, display, focus) {
            continue;
        }
        let continued = (first && first_continued)
            || (display == ToolDisplay::Hidden && continues(previous, message));
        first = false;
        previous = Some(message);
        #[cfg(test)]
        PROJECTED_MESSAGES.with(|count| count.set(count.get() + 1));
        let target = focus
            .filter(|focus| focus.matches(message))
            .map(|focus| display_text(&message.text[..focus.offset]).len());
        let text = display_text(&message.text);
        let is_tool = message.role == "Tool";
        // 耗时行和工具行一样没有标题，直接跟在这一步的内容后面。
        if !is_tool && !continued && !matches!(message.role.as_str(), "Timing" | "Notice") {
            out.lines.push(header(message, theme));
        }
        let calls = message.tools.len();
        let foldable = is_tool && calls > TOOL_FOLD_LIMIT;
        let folded = foldable && display == ToolDisplay::Preview && target.is_none();
        if foldable {
            out.foldable += 1;
            out.lines
                .push(tool_summary(&message.tools, calls, folded, indent, theme));
        }
        let shown = folded.then(|| folded_rows(&text, &message.tools));
        let mut fence = None;
        let mut offset = 0;
        let mut tools = message.tools.iter();
        for (index, source) in text.split('\n').enumerate() {
            let start = offset;
            offset += source.len() + 1;
            let tool = source
                .starts_with(TOOL_MARKER)
                .then(|| tools.next())
                .flatten();
            if shown.as_ref().is_some_and(|rows| !rows.contains(&index)) {
                continue;
            }
            let (runs, hang, code) = match message.role.as_str() {
                "Tool" => tool_runs(source, tool.map(|tool| tool.lifecycle), theme),
                "You" => (vec![(source.len(), Style::default())], 0, false),
                "Timing" | "Notice" => (
                    vec![(source.len(), Style::default().fg(theme.muted))],
                    0,
                    false,
                ),
                _ => markdown_runs(source, &mut fence, theme),
            };
            // 悬挂缩进只在行足够宽时使用，否则续行几乎放不下内容。
            let hang = if body >= hang + 12 { hang } else { 0 };
            for (row, range) in wrap_words(source, body, hang).into_iter().enumerate() {
                if target.is_some_and(|target| start + range.start <= target) {
                    out.focused = Some(out.lines.len());
                }
                let mut spans = Vec::new();
                if indent > 0 {
                    spans.push(if message.role == "You" {
                        Span::styled("▎ ", Style::default().fg(theme.accent))
                    } else {
                        Span::raw("  ")
                    });
                }
                if row > 0 && hang > 0 {
                    spans.push(Span::raw(" ".repeat(hang)));
                }
                let used = if row > 0 { hang } else { 0 } + source[range.clone()].width();
                slice_runs(source, &runs, range, &mut spans);
                if code && used < body {
                    spans.push(Span::styled(" ".repeat(body - used), code_style(theme)));
                }
                out.lines.push(Line::from(spans));
            }
            if let Some(tool) = tool.filter(|_| display == ToolDisplay::Expanded) {
                let detail = body.saturating_sub(4).max(1);
                for row in tool_detail_rows(tool) {
                    for range in wrap_words(&row, detail, 0) {
                        out.lines.push(Line::from(vec![
                            Span::raw(" ".repeat(indent + 4)),
                            Span::styled(row[range].to_owned(), Style::default().fg(theme.muted)),
                        ]));
                    }
                }
            }
        }
        out.lines.push(Line::default());
    }
    // 末尾不留空行，最后一条消息贴近输入区。
    out.lines.pop();
    out
}

fn header(message: &ConversationItem, theme: Theme) -> Line<'static> {
    let color = match message.role.as_str() {
        "You" => theme.accent,
        "Agent" => theme.agent,
        "Reasoning" => theme.muted,
        _ => theme.foreground,
    };
    // 角色名由 Core 写入；这里只换成界面语言的称呼，不改消息内容。
    let role = match message.role.as_str() {
        "You" => tr("You", "你"),
        "Reasoning" => tr("Thinking", "思考过程"),
        other => other,
    };
    let mut spans = vec![Span::styled(
        role.to_owned(),
        Style::default().fg(color).add_modifier(Modifier::BOLD),
    )];
    if message.truncated {
        spans.push(Span::styled(
            tr(" [truncated]", " [已截断]"),
            Style::default().fg(theme.warning),
        ));
    }
    Line::from(spans)
}

fn code_style(theme: Theme) -> Style {
    Style::default().fg(theme.foreground).bg(theme.surface)
}

fn tool_runs(line: &str, lifecycle: Option<ToolLifecycle>, theme: Theme) -> (Runs, usize, bool) {
    let Some(rest) = line.strip_prefix(TOOL_MARKER) else {
        return (vec![(line.len(), theme.secondary())], 0, false);
    };
    let word = rest.split(" · ").next().unwrap_or(rest);
    let state = lifecycle.map_or_else(|| theme.secondary(), |value| theme.lifecycle(value));
    let mut end = TOOL_MARKER.len() + word.len();
    let mut runs = vec![(end, state)];
    // 状态之后依次是可选的退出码和耗时，其余是命令或工具名。
    let mut rest = &line[end..];
    while let Some(tail) = rest.strip_prefix(" · ") {
        let field = tail.split(" · ").next().unwrap_or(tail);
        let is_exit = field
            .strip_prefix("exit ")
            .is_some_and(|code| code.parse::<i64>().is_ok());
        let is_time = field
            .strip_suffix('s')
            .is_some_and(|secs| secs.parse::<f64>().is_ok());
        runs.push((end + " · ".len(), theme.faint()));
        if !(is_exit || is_time) || tail.len() == field.len() {
            break;
        }
        end += " · ".len() + field.len();
        runs.push((end, theme.secondary()));
        rest = &line[end..];
    }
    runs.push((line.len(), Style::default().fg(theme.foreground)));
    (runs, TOOL_MARKER.width(), false)
}

/// Lines kept while a tool group is folded: Core's own notices, the most recent
/// calls that are not `done`, and the latest call.
fn folded_rows(text: &str, tools: &[crate::tool_details::ConversationTool]) -> Vec<usize> {
    let lines: Vec<_> = text.split('\n').collect();
    let mut tools = tools.iter();
    let pending: Vec<_> = lines
        .iter()
        .enumerate()
        .filter(|(_, line)| {
            line.starts_with(TOOL_MARKER)
                && tools
                    .next()
                    .is_some_and(|tool| tool.lifecycle != ToolLifecycle::Completed)
        })
        .map(|(index, _)| index)
        .collect();
    let mut rows: Vec<_> = lines
        .iter()
        .enumerate()
        .filter(|(_, line)| !line.starts_with(TOOL_MARKER))
        .map(|(index, _)| index)
        .chain(
            pending
                .iter()
                .skip(pending.len().saturating_sub(FOLDED_ATTENTION_ROWS))
                .copied(),
        )
        .chain(lines.iter().rposition(|line| line.starts_with(TOOL_MARKER)))
        .collect();
    rows.sort_unstable();
    rows.dedup();
    rows
}

fn tool_summary(
    tools: &[crate::tool_details::ConversationTool],
    calls: usize,
    folded: bool,
    indent: usize,
    theme: Theme,
) -> Line<'static> {
    let mut spans = vec![
        Span::raw(" ".repeat(indent)),
        Span::styled(if folded { "▸ " } else { "▾ " }, theme.heading()),
        Span::styled(
            if lang::zh() {
                format!("{calls} 次工具调用")
            } else {
                format!("{calls} tool calls")
            },
            theme.label(),
        ),
    ];
    for (label, states) in [
        (tr("failed", "失败"), &[ToolLifecycle::Failed][..]),
        (tr("interrupted", "已中断"), &[ToolLifecycle::Interrupted]),
        (
            tr("outcome unknown", "结果未知"),
            &[ToolLifecycle::EndedUnknown, ToolLifecycle::Unknown],
        ),
        (tr("running", "运行中"), &[ToolLifecycle::Running]),
        (tr("done", "已完成"), &[ToolLifecycle::Completed]),
    ] {
        let count = tools
            .iter()
            .filter(|tool| states.contains(&tool.lifecycle))
            .count();
        if count > 0 {
            spans.push(Span::styled(" · ", theme.faint()));
            spans.push(Span::styled(
                format!("{count} {label}"),
                theme.lifecycle(states[0]).remove_modifier(Modifier::BOLD),
            ));
        }
    }
    spans.push(Span::styled("  ·  ", theme.faint()));
    spans.push(Span::styled("Ctrl+E", theme.key()));
    spans.push(Span::styled(
        if folded {
            tr(" expand", " 展开")
        } else {
            tr(" collapse", " 折叠")
        },
        theme.secondary(),
    ));
    Line::from(spans)
}

fn markdown_runs(line: &str, fence: &mut Option<char>, theme: Theme) -> (Runs, usize, bool) {
    let trimmed = line.trim_start();
    let lead = line.len() - trimmed.len();
    let delimiter = ['`', '~']
        .into_iter()
        .find(|mark| trimmed.starts_with(&mark.to_string().repeat(3)));
    if let Some(delimiter) = delimiter {
        if *fence == Some(delimiter) {
            *fence = None;
        } else if fence.is_none() {
            *fence = Some(delimiter);
        }
        return (vec![(line.len(), theme.secondary())], 0, false);
    }
    if fence.is_some() {
        return (vec![(line.len(), code_style(theme))], 0, true);
    }
    let hashes = trimmed.bytes().take_while(|&byte| byte == b'#').count();
    if (1..=6).contains(&hashes) && trimmed.as_bytes().get(hashes) == Some(&b' ') {
        return (
            vec![
                (lead + hashes, theme.faint()),
                (line.len(), theme.heading()),
            ],
            0,
            false,
        );
    }
    if trimmed.len() >= 3 && trimmed.bytes().all(|byte| byte == b'-') {
        return (vec![(line.len(), theme.faint())], 0, false);
    }
    if trimmed.starts_with("> ") || trimmed == ">" {
        return (
            vec![
                (lead + 1, theme.faint()),
                (line.len(), theme.secondary().add_modifier(Modifier::ITALIC)),
            ],
            lead + 2,
            false,
        );
    }
    let digits = trimmed.bytes().take_while(u8::is_ascii_digit).count();
    let marker = if ["- ", "* ", "+ "]
        .iter()
        .any(|bullet| trimmed.starts_with(bullet))
    {
        2
    } else if (1..=9).contains(&digits) && trimmed[digits..].starts_with(". ") {
        digits + 2
    } else {
        0
    };
    let mut runs = Vec::new();
    if marker > 0 {
        runs.push((lead + marker, Style::default().fg(theme.accent)));
    }
    inline_runs(line, lead + marker, theme, &mut runs);
    (runs, if marker > 0 { lead + marker } else { 0 }, false)
}

/// Inline code and strong emphasis. Unclosed delimiters stay plain text.
fn inline_runs(line: &str, mut at: usize, theme: Theme, runs: &mut Runs) {
    while at < line.len() {
        let rest = &line[at..];
        let Some(open) = rest.find(['`', '*']) else {
            break;
        };
        let (mark, style) = if rest[open..].starts_with('`') {
            ("`", Style::default().fg(theme.accent))
        } else if rest[open..].starts_with("**") {
            ("**", Style::default().add_modifier(Modifier::BOLD))
        } else {
            runs.push((at + open + 1, Style::default()));
            at += open + 1;
            continue;
        };
        let inner = at + open + mark.len();
        let close = line[inner..].find(mark).filter(|&length| length > 0);
        let Some(length) = close else {
            runs.push((inner, Style::default()));
            at = inner;
            continue;
        };
        runs.push((at + open, Style::default()));
        runs.push((inner, theme.faint()));
        runs.push((inner + length, style));
        runs.push((inner + length + mark.len(), theme.faint()));
        at = inner + length + mark.len();
    }
    runs.push((line.len(), Style::default()));
}

/// Emit the part of each style run that falls inside one wrapped row.
fn slice_runs(
    line: &str,
    runs: &Runs,
    row: std::ops::Range<usize>,
    spans: &mut Vec<Span<'static>>,
) {
    let mut start = 0;
    for &(end, style) in runs {
        let from = start.max(row.start);
        let to = end.min(row.end);
        if from < to {
            spans.push(Span::styled(line[from..to].to_owned(), style));
        }
        start = start.max(end);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(role: &str, text: &str) -> ConversationItem {
        ConversationItem {
            id: format!("{role}-item"),
            thread_id: "root".into(),
            turn_id: "turn".into(),
            role: role.into(),
            text: text.into(),
            complete: true,
            truncated: false,
            tools: if role == "Tool" {
                text.lines()
                    .filter_map(|line| {
                        let word = line.strip_prefix("▸ ")?.split(" · ").next()?;
                        let lifecycle = match word {
                            "running" => ToolLifecycle::Running,
                            "done" => ToolLifecycle::Completed,
                            "failed" => ToolLifecycle::Failed,
                            "ended, outcome unknown" => ToolLifecycle::EndedUnknown,
                            _ => ToolLifecycle::Unknown,
                        };
                        Some(crate::tool_details::ConversationTool {
                            item_id: word.into(),
                            category: crate::protocol::ToolCategory::Shell,
                            lifecycle,
                            name: None,
                            command: None,
                            cwd: None,
                            target: None,
                            exit_code: None,
                            output_lines: None,
                            added_lines: None,
                            removed_lines: None,
                        })
                    })
                    .collect()
            } else {
                Vec::new()
            },
        }
    }

    fn text(line: &Line<'_>) -> String {
        line.spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect()
    }

    fn rows(transcript: &Transcript) -> Vec<String> {
        transcript.lines.iter().map(text).collect()
    }

    fn focus_at(message: &ConversationItem, needle: &str) -> Focus {
        let mut focus = Focus::message(message);
        focus.offset = message.text.find(needle).unwrap();
        focus
    }

    #[test]
    fn tool_display_levels_show_facts_or_hide_tools_and_join_the_steps_of_one_answer() {
        let mut tools = item("Tool", "▸ done · cargo test\n▸ failed · edit src/view.rs");
        tools.tools[0].command = Some("cargo test".into());
        tools.tools[0].cwd = Some("/workspace/project".into());
        tools.tools[0].exit_code = Some(0);
        tools.tools[0].output_lines = Some(42);
        tools.tools[1].added_lines = Some(3);
        tools.tools[1].removed_lines = Some(1);
        let mut first = item("Agent", "first step");
        first.id = "a1".into();
        let mut second = item("Agent", "second step");
        second.id = "a2".into();
        let mut other = item("Agent", "another turn");
        other.id = "a3".into();
        other.turn_id = "next".into();
        let messages = [
            first,
            tools,
            second,
            other,
            item("Timing", "Model wait 1.0s"),
        ];

        // 预览：每次调用一行，没有额外事实。
        let preview = rows(&transcript(messages.iter(), None, 80, ToolDisplay::Preview));
        assert!(preview.contains(&"  ▸ done · cargo test".to_owned()));
        assert!(!preview.iter().any(|row| row.contains("/workspace/project")));

        // 展开：每次调用下面列出 Core 给出的事实；没有的事实不出现。
        let expanded = rows(&transcript(
            messages.iter(),
            None,
            80,
            ToolDisplay::Expanded,
        ));
        let at = expanded
            .iter()
            .position(|row| row == "  ▸ done · cargo test")
            .unwrap();
        assert_eq!(
            &expanded[at + 1..at + 5],
            [
                "      /workspace/project",
                "      … 42 output lines (Ctrl+T for details)",
                "  ▸ failed · edit src/view.rs",
                "      └ +3 -1",
            ]
        );

        // 隐藏：没有工具行；同一轮的后续步骤不重复标题，另一轮照常有标题。
        let hidden = rows(&transcript(messages.iter(), None, 80, ToolDisplay::Hidden));
        assert_eq!(
            hidden,
            [
                "Agent",
                "  first step",
                "",
                "  second step",
                "",
                "Agent",
                "  another turn",
                "",
                "  Model wait 1.0s",
            ]
        );

        // 搜索焦点落在被隐藏的工具组里时，这一组照常显示，标题也随之恢复。
        let focus = focus_at(&messages[1], "edit src");
        let focused = rows(&transcript(
            messages.iter(),
            Some(&focus),
            80,
            ToolDisplay::Hidden,
        ));
        assert!(focused.contains(&"  ▸ failed · edit src/view.rs".to_owned()));
        assert_eq!(focused.iter().filter(|row| *row == "Agent").count(), 3);

        // 缓存的视口与整篇排版逐行一致，三档和窄宽度都如此。
        let mut cache = ProjectionCache::default();
        for display in [
            ToolDisplay::Preview,
            ToolDisplay::Expanded,
            ToolDisplay::Hidden,
        ] {
            for width in [24, 80] {
                for focus in [None, Some(&focus)] {
                    let full = transcript(messages.iter(), focus, width, display);
                    cache.anchor = None;
                    let view = cache.viewport(messages.iter(), focus, width, 1000, 0, display);
                    assert_eq!(rows(&view.transcript), rows(&full), "{display:?} {width}");
                }
            }
        }
    }

    #[test]
    fn timing_row_is_a_muted_line_without_a_title_after_the_step() {
        let messages = [
            item("Agent", "answer"),
            item(
                "Timing",
                "Model wait 1.0s · Response 3.0s · Completed 14:32:12 UTC",
            ),
        ];
        let view = transcript(messages.iter(), None, 80, false);
        assert_eq!(
            rows(&view),
            [
                "Agent",
                "  answer",
                "",
                "  Model wait 1.0s · Response 3.0s · Completed 14:32:12 UTC"
            ]
        );
        let theme = theme::current();
        assert_eq!(view.lines[3].spans[1].style.fg, Some(theme.muted));
        let notice = [item(
            "Notice",
            "Retrying model request (1/2) in 500ms: rate_limit",
        )];
        let view = transcript(notice.iter(), None, 80, false);
        assert_eq!(
            rows(&view),
            ["  Retrying model request (1/2) in 500ms: rate_limit"]
        );
        assert_eq!(view.lines[0].spans[1].style.fg, Some(theme.muted));
        // 窄窗口按词换行，不丢阶段。
        let narrow = rows(&transcript(messages.iter(), None, 24, false)).join(" ");
        for stage in ["Model wait 1.0s", "Response 3.0s", "14:32:12 UTC"] {
            assert!(narrow.contains(stage), "{narrow}");
        }
    }

    #[test]
    fn cached_viewport_matches_full_transcript_for_scroll_resize_focus_and_folding() {
        let mut messages = [
            item("You", "First 中文 e\u{301} question"),
            item(
                "Agent",
                "## Heading\n- long **answer** with `inline code` 中文\n```rust\nlet x = 42;\n```",
            ),
            item(
                "Tool",
                "▸ done · a\n▸ running · b\n▸ failed · c\n▸ done · d\n▸ done · e\n▸ running · f",
            ),
            item(
                "Agent",
                &"Last answer with emoji 👋 and 中文 words.\n".repeat(10),
            ),
        ];
        for (index, message) in messages.iter_mut().enumerate() {
            message.id = index.to_string();
        }
        let mut cache = ProjectionCache::default();
        let focus = focus_at(&messages[1], "answer");
        let tool_focus = focus_at(&messages[2], "done · d");
        for width in [12, 40, 80, 40] {
            for height in [1, 7, 25, 100] {
                for expanded in [false, true] {
                    for focus in [None, Some(&focus), Some(&tool_focus)] {
                        let full = transcript(messages.iter(), focus, width, expanded);
                        for scroll in [0, 1, 13, 10000] {
                            // 每个组合代表一次独立导航；流式锚点另有回归。
                            cache.anchor = None;
                            let max_scroll = full.lines.len().saturating_sub(height);
                            let start = full.focused.map_or_else(
                                || max_scroll.saturating_sub(scroll.min(max_scroll)),
                                |row| row.saturating_sub(height / 3).min(max_scroll),
                            );
                            let expected =
                                &full.lines[start..(start + height).min(full.lines.len())];
                            let view = cache.viewport(
                                messages.iter(),
                                focus,
                                width,
                                height,
                                scroll,
                                expanded,
                            );
                            assert_eq!(
                                view.transcript.lines, expected,
                                "width={width} height={height} scroll={scroll} focus={focus:?}"
                            );
                            assert_eq!(view.below, max_scroll.saturating_sub(start));
                            assert_eq!(
                                view.transcript.focused,
                                full.focused.and_then(|row| row
                                    .checked_sub(start)
                                    .filter(|row| *row < height))
                            );
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn streaming_only_rebuilds_the_changed_visible_message() {
        let mut messages = vec![item("You", "question"), item("Agent", "streaming")];
        messages[1].complete = false;
        let mut cache = ProjectionCache::default();
        cache.viewport(messages.iter(), None, 80, 30, 0, false);
        PROJECTED_MESSAGES.with(|count| count.set(0));
        messages[1].text.push_str(" response 中文");
        cache.viewport(messages.iter(), None, 80, 30, 0, false);
        assert_eq!(PROJECTED_MESSAGES.with(|count| count.replace(0)), 1);
        messages[1].complete = true;
        messages[1].truncated = true;
        let view = cache.viewport(messages.iter(), None, 80, 30, 0, false);
        assert!(view
            .transcript
            .lines
            .iter()
            .any(|line| text(line).contains("[truncated]")));
        messages.remove(0);
        let view = cache.viewport(messages.iter(), None, 80, 30, 0, false);
        assert_eq!(
            view.transcript.lines,
            transcript(messages.iter(), None, 80, false).lines
        );
    }

    #[test]
    fn scrolled_viewport_keeps_its_message_row_during_growth_and_returns_to_tail() {
        let mut messages = vec![item("Agent", &"中文 👋 e\u{301}\n".repeat(50))];
        messages[0].complete = false;
        let mut cache = ProjectionCache::default();
        let before = cache.viewport(messages.iter(), None, 40, 10, 8, false);
        messages[0].text.push_str(&"new response\n".repeat(20));
        let after = cache.viewport(messages.iter(), None, 40, 10, 8, false);
        assert_eq!(before.transcript.lines, after.transcript.lines);
        assert_eq!(after.below, before.below + 20);
        messages.push(item("You", "next message"));
        let next = cache.viewport(messages.iter(), None, 40, 10, 8, false);
        assert_eq!(before.transcript.lines, next.transcript.lines);
        let tail = cache.viewport(messages.iter(), None, 40, 10, 0, false);
        assert_eq!(tail.below, 0);
        assert!(rows(&tail.transcript)
            .iter()
            .any(|row| row.contains("next message")));
    }

    #[test]
    fn evicted_anchor_clamps_to_retained_history_without_binding_a_reused_item_id() {
        let mut messages = vec![item("Agent", &"old\n".repeat(50)), item("You", "tail")];
        let mut cache = ProjectionCache::default();
        cache.viewport(messages.iter(), None, 40, 10, 20, false);
        messages.remove(0);
        messages.insert(0, item("Agent", &"replacement\n".repeat(40)));
        messages[0].turn_id = "another-turn".into();
        let view = cache.viewport(messages.iter(), None, 40, 10, 20, false);
        assert_eq!(rows(&view.transcript)[0], "Agent");
    }

    #[test]
    fn cached_messages_follow_theme_language_and_thread_changes() {
        let mut message = item("You", "same id, different conversation");
        let mut cache = ProjectionCache::default();
        let original_theme = theme::name();
        cache.viewport(std::iter::once(&message), None, 80, 10, 0, false);
        theme::set("light");
        let view = cache.viewport(std::iter::once(&message), None, 80, 10, 0, false);
        assert_eq!(
            view.transcript.lines,
            transcript(std::iter::once(&message), None, 80, false).lines
        );
        lang::with_zh(|| {
            let view = cache.viewport(std::iter::once(&message), None, 80, 10, 0, false);
            assert_eq!(text(&view.transcript.lines[0]), "你");
        });
        theme::set(original_theme);
        message.thread_id = "child".into();
        message.text = "Child output".into();
        let view = cache.viewport(std::iter::once(&message), None, 80, 10, 0, false);
        assert!(view
            .transcript
            .lines
            .iter()
            .any(|line| text(line).contains("Child output")));
    }

    #[test]
    fn every_source_grapheme_stays_visible_after_styling_and_wrapping() {
        let source = "## 中文 e\u{301} heading\n- use `ClientHandle` and **keep** ids\n```rust\nlet x = \"a long line of code\";\n```\nunfinished `code and **bold\n> quoted words wrap too\n1. numbered";
        for width in [14, 24, 40, 80] {
            let message = item("Agent", source);
            let transcript = transcript([&message].into_iter(), None, width, false);
            let shown: String = rows(&transcript)[1..]
                .iter()
                .map(|row| row.trim_end().trim_start_matches(' ').to_owned())
                .collect::<Vec<_>>()
                .join("");
            let expected: String = source
                .split('\n')
                .map(str::trim_end)
                .collect::<Vec<_>>()
                .join("");
            assert_eq!(
                shown.replace(' ', ""),
                expected.replace(' ', ""),
                "width {width}"
            );
            assert!(
                transcript
                    .lines
                    .iter()
                    .all(|line| line.width() <= width + 1),
                "width {width}"
            );
        }
    }

    #[test]
    fn user_text_stays_literal_and_markdown_roles_are_styled() {
        let user = item("You", "# a task with `ticks`");
        let agent = item("Agent", "# title\ntext `code`");
        let transcript = transcript([&user, &agent].into_iter(), None, 60, false);
        // 用户输入只有引导标记和一段原文，不会被渲染成系统样式。
        assert_eq!(transcript.lines[1].spans.len(), 2);
        assert_eq!(text(&transcript.lines[1]), "▎ # a task with `ticks`");
        let title = &transcript.lines[4];
        assert!(title
            .spans
            .last()
            .unwrap()
            .style
            .add_modifier
            .contains(Modifier::BOLD));
        assert_eq!(text(&transcript.lines[5]), "  text `code`");
    }

    #[test]
    fn core_tool_summaries_keep_one_marker_and_wrap_inside_the_width() {
        let tool = item(
            "Tool",
            "▸ done · exit 0 · 1.2s · cargo check --locked --all-targets\n▸ failed · exit 101 · cargo test",
        );
        let transcript = transcript([&tool].into_iter(), None, 30, false);
        let rows = rows(&transcript);
        assert_eq!(
            rows,
            [
                "  ▸ done · exit 0 · 1.2s · ",
                "    cargo check --locked ",
                "    --all-targets",
                "  ▸ failed · exit 101 · cargo ",
                "    test",
            ]
        );
        assert!(transcript.lines.iter().all(|line| line.width() <= 31));
        assert_eq!(rows.concat().matches('▸').count(), 2);
        // 状态词带语义样式，颜色之外仍有文字。
        let state = &transcript.lines[3].spans[1];
        assert_eq!(state.content, "▸ failed");
        assert!(state.style.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn tool_styles_and_folding_use_core_facts_even_when_summary_words_disagree() {
        let mut tool = item("Tool", &"▸ done · ambiguous command\n".repeat(6));
        tool.tools[0].lifecycle = ToolLifecycle::Failed;
        tool.tools[5].lifecycle = ToolLifecycle::Running;
        let projected = transcript([&tool].into_iter(), None, 100, false);
        assert!(rows(&projected)[0].contains("1 failed"));
        assert!(rows(&projected)[0].contains("1 running"));
        assert_eq!(
            projected.lines[1].spans[1].style,
            theme::current().lifecycle(ToolLifecycle::Failed)
        );
        tool.tools.clear();
        let legacy = transcript([&tool].into_iter(), None, 100, false);
        assert_eq!(legacy.foldable, 0);
        assert_eq!(legacy.lines[0].spans[1].style, theme::current().secondary());
    }

    fn long_group() -> ConversationItem {
        item(
            "Tool",
            "… 5 earlier tool calls (Ctrl+T)\n▸ done · exit 0 · one\n▸ failed · exit 1 · two\n▸ done · three\n▸ done · four\n▸ ended, outcome unknown · five\n▸ done · six\n▸ running · seven\n▸ done · exit 0 · eight",
        )
    }

    #[test]
    fn long_tool_groups_fold_to_counts_unfinished_calls_and_the_latest_call() {
        let tool = long_group();
        let folded = transcript([&tool].into_iter(), None, 100, false);
        assert_eq!(folded.foldable, 1);
        assert_eq!(
            rows(&folded),
            [
                "  ▸ 8 tool calls · 1 failed · 1 outcome unknown · 1 running · 5 done  ·  Ctrl+E expand",
                "  … 5 earlier tool calls (Ctrl+T)",
                "  ▸ failed · exit 1 · two",
                "  ▸ ended, outcome unknown · five",
                "  ▸ running · seven",
                "  ▸ done · exit 0 · eight",
            ]
        );
        let open = transcript([&tool].into_iter(), None, 100, true);
        assert_eq!(open.lines.len(), 10);
        assert!(rows(&open)[0].ends_with("Ctrl+E collapse"));
        // 调用数不超过上限的工具组不折叠，也不显示汇总行。
        let short = item("Tool", "▸ done · one\n▸ done · two");
        let short = transcript([&short].into_iter(), None, 100, false);
        assert_eq!((short.foldable, short.lines.len()), (0, 2));
    }

    #[test]
    fn search_focus_unfolds_its_tool_group_and_lands_on_the_matching_row() {
        let tool = long_group();
        let focus = focus_at(&tool, "three");
        for width in [24, 60, 120] {
            let transcript = transcript([&tool].into_iter(), Some(&focus), width, false);
            let row = transcript.focused.expect("focused row");
            assert!(text(&transcript.lines[row]).contains("three"), "{width}");
        }
        let agent = item(
            "Agent",
            "first paragraph that wraps over several rows at narrow widths\n\n- bullet with the NEEDLE inside a long hanging list item",
        );
        let focus = focus_at(&agent, "NEEDLE");
        for width in [16, 30, 90] {
            let transcript = transcript([&agent].into_iter(), Some(&focus), width, false);
            let row = transcript.focused.expect("focused row");
            assert!(text(&transcript.lines[row]).contains("NEEDLE"), "{width}");
        }
        // 组合字符、ZWJ emoji、被过滤的控制字符和硬换行都不会让定位偏行。
        for (source, width) in [
            ("abcd中文", 4),
            ("a\n中文", 20),
            ("👨‍👩‍👧‍👦中文", 2),
            ("ab\u{001b}中文", 4),
            ("e\u{301}中文", 2),
        ] {
            let message = item("Agent", source);
            let focus = focus_at(&message, "中文");
            let transcript = transcript([&message].into_iter(), Some(&focus), width, false);
            let row = transcript.focused.expect("focused row");
            assert!(text(&transcript.lines[row]).contains('中'), "{source:?}");
        }
        // 文本变化后旧焦点不再匹配，不会指向错误的行。
        let changed = item("Agent", "different text NEEDLE");
        assert!(transcript([&changed].into_iter(), Some(&focus), 40, false)
            .focused
            .is_none());
    }
}
