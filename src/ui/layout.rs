//! Pure terminal geometry and grapheme-aware text wrapping.

use std::ops::Range;
use std::rc::Rc;

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use crate::state::display_text;

pub(super) fn wrap(text: &str, width: usize) -> Vec<String> {
    let mut output = Vec::new();
    for source in display_text(text).split('\n') {
        let mut line = String::new();
        let mut cells = 0;
        for grapheme in source.graphemes(true) {
            let count = grapheme.width();
            if cells + count > width.max(1) && !line.is_empty() {
                output.push(std::mem::take(&mut line));
                cells = 0;
            }
            line.push_str(grapheme);
            cells += count;
        }
        output.push(line);
    }
    output
}

/// Word-aware wrapping of one line (no `\n`). The returned byte ranges cover the
/// line exactly and in order, so a source offset maps to its row without a second
/// wrapping algorithm. Rows after the first are `hang` cells narrower.
///
/// Breaks prefer the gap after a space or around a wide (CJK) grapheme; a word
/// longer than a row is split. One space may hang past the edge instead of
/// starting the next row.
pub(super) fn wrap_words(line: &str, width: usize, hang: usize) -> Vec<Range<usize>> {
    let rest = width.saturating_sub(hang).max(1);
    let mut limit = width.max(1);
    let mut rows = Vec::new();
    let mut start = 0;
    let mut cells = 0;
    // 最近一个可断点：字节位置，以及该位置之前本行已占的格数。
    let mut soft: Option<(usize, usize)> = None;
    for (index, grapheme) in line.grapheme_indices(true) {
        let count = grapheme.width();
        let end = index + grapheme.len();
        if count >= 2 && index > start {
            soft = Some((index, cells));
        }
        if cells + count > limit && index > start {
            if grapheme == " " {
                rows.push(start..end);
                (start, cells, soft, limit) = (end, 0, None, rest);
                continue;
            }
            match soft.take().filter(|&(at, _)| at > start) {
                Some((at, before)) => {
                    rows.push(start..at);
                    (start, cells) = (at, cells - before);
                }
                None => {
                    rows.push(start..index);
                    (start, cells) = (index, 0);
                }
            }
            limit = rest;
            if cells + count > limit && index > start {
                rows.push(start..index);
                (start, cells) = (index, 0);
            }
        }
        cells += count;
        if grapheme == " " || count >= 2 {
            soft = Some((end, cells));
        }
    }
    rows.push(start..line.len());
    rows
}

/// Word-aware wrapping of sanitized multi-line text for prose outside the transcript.
pub(super) fn wrap_text(text: &str, width: usize) -> Vec<String> {
    display_text(text)
        .split('\n')
        .flat_map(|line| {
            wrap_words(line, width, 0)
                .into_iter()
                .map(move |range| line[range].trim_end().to_owned())
        })
        .collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum StatusRows {
    Hidden,
    Compact,
    Expanded,
}

pub(super) fn main_layout(
    area: Rect,
    has_activity: bool,
    status: StatusRows,
    input_rows: u16,
) -> Rc<[Rect]> {
    Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            // 状态行、设置行和下边框；有活动时多两行提醒与证据。没有标题行。
            Constraint::Length(if area.height > 16 && has_activity {
                5
            } else {
                3
            }),
            Constraint::Min(1),
            Constraint::Length(match status {
                StatusRows::Hidden => 0,
                StatusRows::Expanded if area.height > 16 => 6,
                _ => 2,
            }),
            Constraint::Length(input_rows.clamp(1, 5) + 2),
            Constraint::Length(1),
        ])
        .split(area)
}

/// Columns of the main workspace: optional agent sidebar, conversation, optional inspector.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Workspace {
    pub agents: Option<Rect>,
    pub conversation: Rect,
    pub inspector: Option<Rect>,
}

/// Narrowest terminal that gets an inspector column beside the conversation.
pub(super) const INSPECTOR_MIN_WIDTH: u16 = 140;

/// The same geometry is used for painting and locating conversation search hits.
/// The sidebar appears from 100 columns when agents exist; the inspector from
/// `INSPECTOR_MIN_WIDTH` columns when the caller has something to show in it.
pub(super) fn workspace_layout(area: Rect, has_agents: bool, wants_inspector: bool) -> Workspace {
    let sidebar = if area.width >= 100 && has_agents {
        (32 + area.width.saturating_sub(100) / 20).clamp(32, 36)
    } else {
        0
    };
    let inspector = if area.width >= INSPECTOR_MIN_WIDTH && wants_inspector {
        (area.width / 3).clamp(44, 60)
    } else {
        0
    };
    let panels = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Length(sidebar),
            Constraint::Min(1),
            Constraint::Length(inspector),
        ])
        .split(area);
    Workspace {
        agents: (sidebar > 0).then_some(panels[0]),
        conversation: panels[1],
        inspector: (inspector > 0).then_some(panels[2]),
    }
}

pub(super) fn conversation_content_size(area: Rect) -> (usize, usize) {
    let border = if area.height < 3 { 0 } else { 2 };
    (
        area.width.saturating_sub(border) as usize,
        area.height.saturating_sub(border) as usize,
    )
}

#[cfg(test)]
mod tests {
    use super::{conversation_content_size, main_layout, wrap, wrap_words, StatusRows};
    use ratatui::layout::Rect;

    #[test]
    fn wrap_keeps_graphemes_and_sanitizes_line_breaks() {
        assert_eq!(
            wrap("e\u{301}👨‍👩‍👧‍👦\nnext", 2),
            vec![
                "e\u{301}".to_owned(),
                "👨‍👩‍👧‍👦".to_owned(),
                "ne".to_owned(),
                "xt".to_owned(),
            ]
        );
    }

    fn rows(line: &str, width: usize, hang: usize) -> Vec<&str> {
        wrap_words(line, width, hang)
            .into_iter()
            .map(|range| &line[range])
            .collect()
    }

    #[test]
    fn wrap_words_breaks_at_spaces_and_covers_the_line_exactly() {
        let line = "The next step is a calmer visual hierarchy";
        let wrapped = rows(line, 16, 0);
        assert_eq!(
            wrapped,
            ["The next step is ", "a calmer visual ", "hierarchy"]
        );
        assert_eq!(wrapped.concat(), line);
        // 行尾刚好放不下的单个空格挂在行尾，下一行不以空格开头。
        assert_eq!(rows("abcd efgh", 4, 0), ["abcd ", "efgh"]);
        assert_eq!(rows("", 10, 0), [""]);
    }

    #[test]
    fn wrap_words_splits_long_words_wide_text_and_applies_hanging_width() {
        assert_eq!(rows("abcdefghij", 4, 0), ["abcd", "efgh", "ij"]);
        assert_eq!(rows("ab 中文字符", 6, 0), ["ab 中", "文字符"]);
        assert_eq!(rows("word 中文", 5, 0), ["word ", "中文"]);
        assert_eq!(
            rows("- one two three four", 9, 2),
            ["- one two ", "three ", "four"]
        );
        let emoji = "e\u{301}👨‍👩‍👧‍👦x";
        assert_eq!(rows(emoji, 2, 0), ["e\u{301}", "👨‍👩‍👧‍👦", "x"]);
        assert_eq!(rows("abc", 0, 5), ["a", "b", "c"]);
    }

    #[test]
    fn main_layout_expands_header_and_status_only_when_requested() {
        let hidden = main_layout(Rect::new(0, 0, 80, 20), false, StatusRows::Hidden, 1);
        assert_eq!(hidden[2].height, 0);

        let compact = main_layout(Rect::new(0, 0, 80, 20), false, StatusRows::Compact, 1);
        assert_eq!(compact[0].height, 3);
        assert_eq!(compact[2].height, 2);

        let expanded = main_layout(Rect::new(0, 0, 80, 20), true, StatusRows::Expanded, 1);
        assert_eq!(expanded[0].height, 5);
        assert_eq!(expanded[2].height, 6);
    }

    #[test]
    fn workspace_adds_the_sidebar_and_inspector_only_at_their_breakpoints() {
        use super::workspace_layout;
        let narrow = workspace_layout(Rect::new(0, 0, 99, 20), true, true);
        assert_eq!((narrow.agents, narrow.inspector), (None, None));
        assert_eq!(narrow.conversation.width, 99);
        let medium = workspace_layout(Rect::new(0, 0, 139, 20), true, true);
        assert!(medium.agents.is_some() && medium.inspector.is_none());
        let wide = workspace_layout(Rect::new(0, 0, 160, 20), true, true);
        let (agents, inspector) = (wide.agents.unwrap(), wide.inspector.unwrap());
        assert_eq!(
            agents.width + wide.conversation.width + inspector.width,
            160
        );
        assert!(wide.conversation.width >= 60, "{wide:?}");
        // 没有内容可放时不占用对话宽度。
        let idle = workspace_layout(Rect::new(0, 0, 160, 20), false, false);
        assert_eq!(idle.conversation.width, 160);
    }

    #[test]
    fn conversation_content_size_accounts_for_borders_and_narrow_areas() {
        assert_eq!(conversation_content_size(Rect::new(0, 0, 20, 10)), (18, 8));
        assert_eq!(conversation_content_size(Rect::new(0, 0, 1, 2)), (1, 2));
    }
}
