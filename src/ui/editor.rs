use unicode_segmentation::UnicodeSegmentation;

use crate::state::{display_text, MESSAGE_BYTES};

#[derive(Default)]
pub(super) struct Editor {
    pub(super) text: String,
    pub(super) cursor: usize,
}

impl Editor {
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
