use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use crate::state::{display_text, MESSAGE_BYTES};

#[derive(Default)]
pub(super) struct Editor {
    pub(super) text: String,
    pub(super) cursor: usize,
}

pub(super) struct EditorView {
    pub lines: Vec<String>,
    pub cursor: (usize, usize),
}

impl EditorView {
    pub fn visible_rows(&self, terminal_height: u16) -> u16 {
        let limit = if terminal_height <= 16 { 1 } else { 5 };
        self.lines.len().clamp(1, limit) as u16
    }
}

impl Editor {
    /// Project the draft and cursor together; secret graphemes never enter the view.
    pub fn view(&self, width: usize, secret: bool) -> EditorView {
        let width = width.max(1);
        let mut lines = vec![String::new()];
        let mut column = 0;
        let mut cursor = (0, 0);
        for (offset, grapheme) in self.text.grapheme_indices(true) {
            let displayed = if secret { "*" } else { grapheme };
            if displayed == "\n" {
                if offset == self.cursor {
                    cursor = if column >= width {
                        (0, lines.len())
                    } else {
                        (column, lines.len() - 1)
                    };
                }
                lines.push(String::new());
                column = 0;
                continue;
            }
            let cells = displayed.width();
            if column > 0 && (column >= width || column + cells > width) {
                lines.push(String::new());
                column = 0;
            }
            if offset == self.cursor {
                cursor = (column, lines.len() - 1);
            }
            lines.last_mut().unwrap().push_str(displayed);
            column += cells;
        }
        if self.cursor == self.text.len() {
            if column >= width {
                lines.push(String::new());
                column = 0;
            }
            cursor = (column, lines.len() - 1);
        }
        EditorView { lines, cursor }
    }

    pub(super) fn insert(&mut self, text: &str) {
        let text = display_text(text);
        if self.text.len() + text.len() > MESSAGE_BYTES {
            return;
        }
        self.text.insert_str(self.cursor, &text);
        self.cursor += text.len();
    }

    pub(super) fn left(&mut self) {
        self.cursor = self.text[..self.cursor]
            .grapheme_indices(true)
            .next_back()
            .map_or(0, |(index, _)| index);
    }

    pub(super) fn right(&mut self) {
        if let Some(grapheme) = self.text[self.cursor..].graphemes(true).next() {
            self.cursor += grapheme.len();
        }
    }

    pub(super) fn backspace(&mut self) {
        let end = self.cursor;
        self.left();
        self.text.drain(self.cursor..end);
    }

    pub(super) fn delete(&mut self) {
        let start = self.cursor;
        self.right();
        self.text.drain(start..self.cursor);
        self.cursor = start;
    }

    pub(super) fn clear(&mut self) {
        self.text.clear();
        self.cursor = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::Editor;

    #[test]
    fn multiline_view_tracks_cursor_at_unicode_wraps_and_newlines() {
        let mut editor = Editor::default();
        editor.insert("中e\u{301}\n12345");
        let view = editor.view(4, false);
        assert_eq!(view.lines, ["中e\u{301}", "1234", "5"]);
        assert_eq!(view.cursor, (1, 2));
        editor.left();
        let view = editor.view(4, false);
        assert_eq!(view.cursor, (0, 2));
        editor.clear();
        editor.insert("abcd\nnext");
        editor.cursor = 4;
        assert_eq!(editor.view(4, false).cursor, (0, 1));
    }

    #[test]
    fn secret_view_masks_every_grapheme_and_caps_visible_rows() {
        let mut editor = Editor::default();
        editor.insert("中e\u{301}👨‍👩‍👧‍👦\nsecret");
        let view = editor.view(3, true);
        assert!(view.lines.iter().all(|line| line.chars().all(|c| c == '*')));
        assert_eq!(view.lines.concat(), "**********");
        assert_eq!(view.cursor, (1, 3));
        assert_eq!(view.visible_rows(12), 1);
        assert_eq!(view.visible_rows(30), 4);
    }

    #[test]
    fn editor_keeps_graphemes_intact_while_moving_and_deleting() {
        let mut editor = Editor::default();
        editor.insert("中e\u{301}👨‍👩‍👧‍👦");
        editor.backspace();
        assert_eq!(editor.text, "中e\u{301}");
        editor.left();
        editor.delete();
        assert_eq!(editor.text, "中");
        editor.backspace();
        assert!(editor.text.is_empty());
    }
}
