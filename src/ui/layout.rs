//! Pure terminal geometry and grapheme-aware text wrapping.

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum StatusRows {
    Hidden,
    Compact,
    Expanded,
}

pub(super) fn main_layout(area: Rect, has_activity: bool, status: StatusRows) -> Rc<[Rect]> {
    Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(if area.height > 16 {
                if has_activity {
                    6
                } else {
                    4
                }
            } else {
                3
            }),
            Constraint::Min(1),
            Constraint::Length(match status {
                StatusRows::Hidden => 0,
                StatusRows::Expanded if area.height > 16 => 6,
                _ => 2,
            }),
            Constraint::Length(3),
            Constraint::Length(1),
        ])
        .split(area)
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
    use super::{conversation_content_size, main_layout, wrap, StatusRows};
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

    #[test]
    fn main_layout_expands_header_and_status_only_when_requested() {
        let hidden = main_layout(Rect::new(0, 0, 80, 20), false, StatusRows::Hidden);
        assert_eq!(hidden[2].height, 0);

        let compact = main_layout(Rect::new(0, 0, 80, 20), false, StatusRows::Compact);
        assert_eq!(compact[0].height, 4);
        assert_eq!(compact[2].height, 2);

        let expanded = main_layout(Rect::new(0, 0, 80, 20), true, StatusRows::Expanded);
        assert_eq!(expanded[0].height, 6);
        assert_eq!(expanded[2].height, 6);
    }

    #[test]
    fn conversation_content_size_accounts_for_borders_and_narrow_areas() {
        assert_eq!(conversation_content_size(Rect::new(0, 0, 20, 10)), (18, 8));
        assert_eq!(conversation_content_size(Rect::new(0, 0, 1, 2)), (1, 2));
    }
}
