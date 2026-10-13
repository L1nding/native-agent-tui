//! The agent's own todo list, shown under the conversation while work remains.

use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthStr;

use super::lang::tr;
use super::theme;
use crate::state::{display_text, TodoItem, TodoStatus};

/// Items shown at once; the window follows the first unfinished one.
const VISIBLE: usize = 5;
/// The conversation keeps at least this many rows before the panel appears.
const MIN_CONVERSATION_ROWS: u16 = 8;

/// Rows of the panel, or none when every todo is done or the area is too small.
pub(super) fn lines(todos: &[TodoItem], width: u16, height: u16) -> Vec<Line<'static>> {
    let done = todos
        .iter()
        .filter(|todo| todo.status == TodoStatus::Completed)
        .count();
    if done == todos.len() || width < 24 || height < MIN_CONVERSATION_ROWS + 2 {
        return Vec::new();
    }
    let theme = theme::current();
    let room = usize::from(height - MIN_CONVERSATION_ROWS - 1).min(VISIBLE);
    let current = todos
        .iter()
        .position(|todo| todo.status != TodoStatus::Completed)
        .unwrap_or(0);
    // Keep one finished item above the current one for context when it fits.
    let start = current
        .saturating_sub(1)
        .min(todos.len().saturating_sub(room));
    let shown = &todos[start..(start + room).min(todos.len())];
    let hidden = todos.len() - shown.len();
    let mut out = vec![Line::from(vec![
        Span::styled(
            format!(" {} {done}/{}", tr("Todos", "待办"), todos.len()),
            Style::default()
                .fg(theme.muted)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            if hidden > 0 {
                format!(" · {} {hidden}", tr("not shown", "未显示"))
            } else {
                String::new()
            },
            Style::default().fg(theme.muted),
        ),
    ])];
    let body = usize::from(width).saturating_sub(6);
    for todo in shown {
        // The marker carries the state in words-free ASCII, readable without colour.
        let (marker, style) = match todo.status {
            TodoStatus::Completed => ("[x]", Style::default().fg(theme.muted)),
            TodoStatus::InProgress => (
                "[>]",
                Style::default()
                    .fg(theme.accent)
                    .add_modifier(Modifier::BOLD),
            ),
            TodoStatus::Pending => ("[ ]", Style::default().fg(theme.foreground)),
        };
        let text = display_text(&todo.content).replace('\n', " ");
        out.push(Line::from(vec![
            Span::styled(format!(" {marker} "), style),
            Span::styled(clip(&text, body), style),
        ]));
    }
    out
}

/// Take `rows` from the bottom of the conversation area for the panel.
pub(super) fn split(area: Rect, rows: usize) -> (Rect, Rect) {
    let rows = (rows as u16).min(area.height);
    let conversation = Rect {
        height: area.height - rows,
        ..area
    };
    let panel = Rect {
        y: area.y + conversation.height,
        height: rows,
        ..area
    };
    (conversation, panel)
}

fn clip(text: &str, width: usize) -> String {
    if text.width() <= width {
        return text.to_owned();
    }
    let mut out = String::new();
    for ch in text.chars() {
        if out.width() + unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0) + 1 > width {
            break;
        }
        out.push(ch);
    }
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn todo(content: &str, status: TodoStatus) -> TodoItem {
        TodoItem {
            content: content.into(),
            status,
        }
    }

    fn rows(lines: &[Line<'_>]) -> Vec<String> {
        lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect()
            })
            .collect()
    }

    #[test]
    fn panel_shows_progress_with_ascii_markers_and_hides_when_all_done() {
        let todos = [
            todo("scan sources", TodoStatus::Completed),
            todo("prepare 方案 👋", TodoStatus::InProgress),
            todo("build", TodoStatus::Pending),
        ];
        assert_eq!(
            rows(&lines(&todos, 60, 30)),
            [
                " Todos 1/3",
                " [x] scan sources",
                " [>] prepare 方案 👋",
                " [ ] build"
            ]
        );
        let done: Vec<_> = todos
            .iter()
            .map(|item| todo(&item.content, TodoStatus::Completed))
            .collect();
        assert!(lines(&done, 60, 30).is_empty());
        assert!(lines(&[], 60, 30).is_empty());
    }

    #[test]
    fn long_lists_follow_the_current_item_and_small_areas_keep_the_conversation() {
        let todos: Vec<_> = (0..20)
            .map(|index| {
                todo(
                    &format!("step {index} {}", "long text ".repeat(20)),
                    match index {
                        0..=9 => TodoStatus::Completed,
                        10 => TodoStatus::InProgress,
                        _ => TodoStatus::Pending,
                    },
                )
            })
            .collect();
        let panel = rows(&lines(&todos, 40, 30));
        assert_eq!(panel.len(), 1 + VISIBLE);
        assert_eq!(panel[0], " Todos 10/20 · not shown 15");
        assert!(panel[1].starts_with(" [x] step 9 "));
        assert!(panel[2].starts_with(" [>] step 10 "));
        assert!(panel.iter().all(|row| row.width() <= 40), "{panel:?}");
        // A short conversation area gives the panel fewer rows, then none.
        assert_eq!(lines(&todos, 40, 11).len(), 3);
        assert!(lines(&todos, 40, 9).is_empty());
        assert!(lines(&todos, 20, 30).is_empty());
        let (conversation, panel) = split(Rect::new(2, 3, 40, 20), 4);
        assert_eq!((conversation.height, panel.y, panel.height), (16, 19, 4));
    }
}
