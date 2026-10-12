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

/// A turn's tool summary folds once it has more calls than this.
pub(super) const TOOL_FOLD_LIMIT: usize = 4;
/// Unfinished or failed calls that stay visible while a group is folded.
const FOLDED_ATTENTION_ROWS: usize = 3;
const INDENT: usize = 2;
const TOOL_MARKER: &str = "▸ ";

pub(super) struct Transcript {
    pub lines: Vec<Line<'static>>,
    /// Row of the search focus while its message is still present and unchanged.
    pub focused: Option<usize>,
    /// Tool groups long enough to fold; the key hint is only useful when non-zero.
    pub foldable: usize,
}

/// Style runs over one source line as `(end byte, style)`, in order.
type Runs = Vec<(usize, Style)>;

pub(super) fn transcript<'a>(
    messages: impl Iterator<Item = &'a ConversationItem>,
    focus: Option<&Focus>,
    width: usize,
    expand_tools: bool,
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
    for message in messages {
        let target = focus
            .filter(|focus| focus.matches(message))
            .map(|focus| display_text(&message.text[..focus.offset]).len());
        let text = display_text(&message.text);
        let is_tool = message.role == "Tool";
        if !is_tool {
            out.lines.push(header(message, theme));
        }
        let calls = text
            .split('\n')
            .filter(|line| line.starts_with(TOOL_MARKER))
            .count();
        let foldable = is_tool && calls > TOOL_FOLD_LIMIT;
        let folded = foldable && !expand_tools && target.is_none();
        if foldable {
            out.foldable += 1;
            out.lines
                .push(tool_summary(&text, calls, folded, indent, theme));
        }
        let shown = folded.then(|| folded_rows(&text));
        let mut fence = None;
        let mut offset = 0;
        for (index, source) in text.split('\n').enumerate() {
            let start = offset;
            offset += source.len() + 1;
            if shown.as_ref().is_some_and(|rows| !rows.contains(&index)) {
                continue;
            }
            let (runs, hang, code) = match message.role.as_str() {
                "Tool" => tool_runs(source, theme),
                "You" => (vec![(source.len(), Style::default())], 0, false),
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

/// The state word Core wrote at the start of a tool summary line.
fn tool_state(line: &str) -> Option<(&str, Option<ToolLifecycle>)> {
    let rest = line.strip_prefix(TOOL_MARKER)?;
    let word = rest.split(" · ").next().unwrap_or(rest);
    let lifecycle = match word {
        "running" => Some(ToolLifecycle::Running),
        "done" => Some(ToolLifecycle::Completed),
        "failed" => Some(ToolLifecycle::Failed),
        "interrupted" => Some(ToolLifecycle::Interrupted),
        "ended, outcome unknown" => Some(ToolLifecycle::EndedUnknown),
        "unknown" => Some(ToolLifecycle::Unknown),
        // 不认识的状态词保持中性样式，不猜测它的含义。
        _ => None,
    };
    Some((word, lifecycle))
}

fn tool_runs(line: &str, theme: Theme) -> (Runs, usize, bool) {
    let Some((word, lifecycle)) = tool_state(line) else {
        return (vec![(line.len(), theme.secondary())], 0, false);
    };
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
fn folded_rows(text: &str) -> Vec<usize> {
    let lines: Vec<_> = text.split('\n').collect();
    let pending: Vec<_> = lines
        .iter()
        .enumerate()
        .filter(|(_, line)| {
            tool_state(line).is_some_and(|(_, state)| state != Some(ToolLifecycle::Completed))
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
    text: &str,
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
        let count = text
            .split('\n')
            .filter_map(tool_state)
            .filter(|(_, state)| state.is_some_and(|state| states.contains(&state)))
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
