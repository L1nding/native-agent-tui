use std::cell::Cell;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

const QUERY_BYTES: usize = 32 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum PaletteAction {
    Search,
    Timeline,
    Context,
    Skills,
    Requests,
    Workflow,
    Evidence,
    Attention,
    Help,
    NextAgent,
}

#[derive(Clone, Copy)]
struct CommandItem {
    label: &'static str,
    hint: &'static str,
    keywords: &'static str,
    action: PaletteAction,
}

const ITEMS: &[CommandItem] = &[
    CommandItem {
        label: "Search conversation",
        hint: "Ctrl+F",
        keywords: "search conversation retained messages text",
        action: PaletteAction::Search,
    },
    CommandItem {
        label: "Timeline",
        hint: "Ctrl+T",
        keywords: "timeline evidence live events",
        action: PaletteAction::Timeline,
    },
    CommandItem {
        label: "Context",
        hint: "Ctrl+G",
        keywords: "context usage compaction budget",
        action: PaletteAction::Context,
    },
    CommandItem {
        label: "Skills inventory",
        hint: "Ctrl+K",
        keywords: "skills inventory",
        action: PaletteAction::Skills,
    },
    CommandItem {
        label: "Requests",
        hint: "F2",
        keywords: "requests approval input",
        action: PaletteAction::Requests,
    },
    CommandItem {
        label: "Workflow",
        hint: "F4",
        keywords: "workflow tasks scheduler",
        action: PaletteAction::Workflow,
    },
    CommandItem {
        label: "Evidence",
        hint: "F11",
        keywords: "evidence activity attention",
        action: PaletteAction::Evidence,
    },
    CommandItem {
        label: "Attention thresholds",
        hint: "F10",
        keywords: "attention thresholds silence reminders",
        action: PaletteAction::Attention,
    },
    CommandItem {
        label: "Help",
        hint: "F1",
        keywords: "help shortcuts keyboard",
        action: PaletteAction::Help,
    },
    CommandItem {
        label: "Switch next agent",
        hint: "F3",
        keywords: "agent child thread switch",
        action: PaletteAction::NextAgent,
    },
];

#[derive(Debug, Default)]
pub(super) struct PaletteState {
    pub visible: bool,
    query: String,
    cursor: usize,
    selected: usize,
    scroll: usize,
    visible_rows: Cell<usize>,
    notice: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum PaletteEvent {
    Close,
    Action(PaletteAction),
    Consumed,
}

impl PaletteState {
    pub fn toggle(&mut self) {
        if self.visible {
            self.close();
        } else {
            self.visible = true;
            self.query.clear();
            self.cursor = 0;
            self.selected = 0;
            self.scroll = 0;
            self.notice = None;
        }
    }

    pub fn close(&mut self) {
        self.visible = false;
    }

    pub fn paste(&mut self, text: &str) {
        if !self.visible {
            return;
        }
        self.insert(text);
    }

    pub fn reject_paste(&mut self) {
        if self.visible {
            self.notice = Some("Paste rejected: too large".into());
        }
    }

    pub fn key(&mut self, key: KeyEvent) -> PaletteEvent {
        if !self.visible {
            return PaletteEvent::Consumed;
        }
        let control = key.modifiers.contains(KeyModifiers::CONTROL);
        if (control && key.code == KeyCode::Char('p')) || key.code == KeyCode::Esc {
            self.close();
            return PaletteEvent::Close;
        }

        let count = self.matches().len();
        match key.code {
            KeyCode::Up if key.modifiers.is_empty() => {
                self.move_selection(-1, count);
            }
            KeyCode::Down if key.modifiers.is_empty() => {
                self.move_selection(1, count);
            }
            KeyCode::Char('k') if control && key.modifiers == KeyModifiers::CONTROL => {
                self.move_selection(-1, count);
            }
            KeyCode::Char('j') if control && key.modifiers == KeyModifiers::CONTROL => {
                self.move_selection(1, count);
            }
            KeyCode::Home if key.modifiers.is_empty() => {
                self.selected = 0;
                self.scroll = 0;
            }
            KeyCode::End if key.modifiers.is_empty() => {
                self.selected = count.saturating_sub(1);
                self.scroll = self.selected;
            }
            KeyCode::Enter if key.modifiers.is_empty() => {
                if let Some(item) = self.matches().get(self.selected) {
                    self.close();
                    return PaletteEvent::Action(item.action);
                }
            }
            KeyCode::Left if key.modifiers.is_empty() => self.left(),
            KeyCode::Right if key.modifiers.is_empty() => self.right(),
            KeyCode::Backspace if key.modifiers.is_empty() => self.backspace(),
            KeyCode::Delete if key.modifiers.is_empty() => self.delete(),
            KeyCode::Char(c)
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                self.insert(&c.to_string());
            }
            _ => {}
        }
        PaletteEvent::Consumed
    }

    pub fn draw(&self, frame: &mut ratatui::Frame<'_>, area: Rect) {
        let width = if area.width == 0 {
            0
        } else {
            area.width.saturating_sub(4).clamp(1, 76).min(area.width)
        };
        let height = if area.height == 0 {
            0
        } else {
            area.height.saturating_sub(2).clamp(1, 18).min(area.height)
        };
        let rect = Rect {
            x: area.x + area.width.saturating_sub(width) / 2,
            y: area.y + area.height.saturating_sub(height) / 2,
            width,
            height,
        };
        frame.render_widget(Clear, rect);
        let query = self.query.replace('\n', "↵");
        let cursor_query = self.query[..self.cursor].replace('\n', "↵");
        let mut lines = vec![Line::from(format!("> {query}"))];
        if let Some(notice) = &self.notice {
            lines.push(Line::from(notice.as_str()).style(Style::default().fg(Color::Red)));
        }
        let matches = self.matches();
        let capacity = Block::default()
            .borders(Borders::ALL)
            .inner(rect)
            .height
            .saturating_sub(1) as usize;
        let visible_rows = capacity.max(1);
        self.visible_rows.set(visible_rows);
        if matches.is_empty() {
            lines.push(
                Line::from("No matching commands.").style(Style::default().fg(Color::Yellow)),
            );
        } else {
            let last = matches.len().saturating_sub(visible_rows);
            let mut start = self.scroll.min(last);
            if self.selected < start {
                start = self.selected;
            } else if self.selected >= start + visible_rows {
                start = self.selected + 1 - visible_rows;
            }
            lines.extend(matches.iter().skip(start).take(capacity).enumerate().map(
                |(offset, item)| {
                    let index = start + offset;
                    let marker = if index == self.selected { ">" } else { " " };
                    Line::from(format!("{marker} {:<28} {}", item.label, item.hint)).style(
                        if index == self.selected {
                            Style::default().fg(Color::Cyan)
                        } else {
                            Style::default()
                        },
                    )
                },
            ));
        }
        frame.render_widget(
            Paragraph::new(lines).block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(" Command palette · Ctrl+P "),
            ),
            rect,
        );
        if rect.width > 2 && rect.height > 2 {
            let cursor = format!("> {cursor_query}").width();
            frame.set_cursor_position((
                rect.x + 1 + cursor.min(rect.width.saturating_sub(2) as usize) as u16,
                rect.y + 1,
            ));
        }
    }

    #[cfg(test)]
    pub fn query(&self) -> &str {
        &self.query
    }

    fn matches(&self) -> Vec<&'static CommandItem> {
        let query = self.query.to_lowercase();
        if query.trim().is_empty() {
            return ITEMS.iter().collect();
        }
        ITEMS
            .iter()
            .filter(|item| {
                format!("{} {} {}", item.label, item.hint, item.keywords)
                    .to_lowercase()
                    .contains(query.trim())
            })
            .collect()
    }

    fn move_selection(&mut self, delta: i8, count: usize) {
        if count == 0 {
            self.selected = 0;
            self.scroll = 0;
            return;
        }
        self.selected = if delta < 0 {
            self.selected.saturating_sub(1)
        } else {
            (self.selected + 1).min(count - 1)
        };
        if self.selected < self.scroll {
            self.scroll = self.selected;
        } else {
            let visible_rows = self.visible_rows.get().max(1);
            if self.selected >= self.scroll + visible_rows {
                self.scroll = self.selected + 1 - visible_rows;
            }
        }
    }

    fn insert(&mut self, text: &str) {
        if self.query.len() + text.len() > QUERY_BYTES {
            return;
        }
        self.query.insert_str(self.cursor, text);
        self.cursor += text.len();
        self.selected = 0;
        self.scroll = 0;
        self.notice = None;
    }

    fn left(&mut self) {
        self.cursor = self.query[..self.cursor]
            .grapheme_indices(true)
            .next_back()
            .map_or(0, |(index, _)| index);
    }

    fn right(&mut self) {
        if let Some(grapheme) = self.query[self.cursor..].graphemes(true).next() {
            self.cursor += grapheme.len();
        }
    }

    fn backspace(&mut self) {
        let end = self.cursor;
        self.left();
        self.query.drain(self.cursor..end);
        self.selected = 0;
        self.scroll = 0;
    }

    fn delete(&mut self) {
        let start = self.cursor;
        self.right();
        self.query.drain(start..self.cursor);
        self.cursor = start;
        self.selected = 0;
        self.scroll = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn palette_filters_and_enters_exact_action() {
        let mut palette = PaletteState::default();
        palette.toggle();
        palette.paste("timeline");
        assert_eq!(palette.query(), "timeline");
        assert_eq!(
            palette.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            PaletteEvent::Action(PaletteAction::Timeline)
        );
        assert!(!palette.visible);
    }

    #[test]
    fn palette_keeps_j_and_k_available_for_filter_text() {
        let mut palette = PaletteState::default();
        palette.toggle();
        palette.key(KeyEvent::new(KeyCode::Char('J'), KeyModifiers::SHIFT));
        assert_eq!(palette.query(), "J");
        palette.key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
        for character in "job".chars() {
            palette.key(KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE));
        }
        assert_eq!(palette.query(), "job");
    }

    #[test]
    fn palette_shows_rejected_paste_notice() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        let mut palette = PaletteState::default();
        palette.toggle();
        palette.reject_paste();
        let mut terminal = Terminal::new(TestBackend::new(40, 10)).unwrap();
        terminal
            .draw(|frame| palette.draw(frame, frame.area()))
            .unwrap();
        let screen: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(screen.contains("Paste rejected: too large"), "{screen}");
    }

    #[test]
    fn palette_scrolls_selection_into_view_on_a_short_terminal() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        let mut palette = PaletteState::default();
        palette.toggle();
        for _ in 0..9 {
            palette.key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        }
        let mut terminal = Terminal::new(TestBackend::new(30, 10)).unwrap();
        terminal
            .draw(|frame| palette.draw(frame, frame.area()))
            .unwrap();
        let screen: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(screen.contains("Switch next agent"), "{screen}");
    }

    #[test]
    fn palette_supports_unicode_multiline_paste_and_no_results() {
        let mut palette = PaletteState::default();
        palette.toggle();
        palette.paste("中文🙂\n无结果");
        assert_eq!(palette.query(), "中文🙂\n无结果");
        assert_eq!(
            palette.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            PaletteEvent::Consumed
        );
    }

    #[test]
    fn palette_navigation_and_escape_are_local() {
        let mut palette = PaletteState::default();
        palette.toggle();
        palette.key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        palette.key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::CONTROL));
        palette.key(KeyEvent::new(KeyCode::Char('k'), KeyModifiers::CONTROL));
        assert_eq!(
            palette.key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
            PaletteEvent::Close
        );
        assert!(!palette.visible);
    }
}
