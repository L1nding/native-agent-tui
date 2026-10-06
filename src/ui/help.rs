//! Static key reference shown as a modal overlay; it never reads or changes execution state.

use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders, Clear, Paragraph};

const SECTIONS: &[(&str, &[(&str, &str)])] = &[
    (
        "Basics",
        &[
            ("Enter", "send task / answer"),
            ("Ctrl+O", "new line (Shift+Enter)"),
            ("Ctrl+S", "queue task while running"),
            ("Ctrl+U", "clear input"),
            ("Ctrl+C", "interrupt root turn"),
            ("Esc", "close panel, keep draft"),
            ("Ctrl+Q", "quit"),
        ],
    ),
    (
        "Requests",
        &[
            ("F2", "next request / details"),
            ("Ctrl+Y", "accept approval"),
            ("Ctrl+N", "decline approval"),
            ("Ctrl+B", "cancel approval"),
        ],
    ),
    (
        "Views",
        &[
            ("Ctrl+P", "command palette"),
            ("Ctrl+F", "search conversation"),
            ("Ctrl+T", "evidence timeline"),
            ("Ctrl+G", "context and usage"),
            ("Ctrl+K", "skills inventory"),
            ("F3", "next agent"),
            ("F11", "activity evidence"),
            ("F12", "history and export"),
            ("PgUp/PgDn", "scroll"),
        ],
    ),
    (
        "Workflow",
        &[
            ("F4", "tasks panel"),
            ("F5 / F6", "pause dispatch / task"),
            ("F7 / F8", "cancel / retry (repeats)"),
            ("+ / -", "queued priority"),
            ("F9 twice", "stop workflow"),
            ("F10", "attention thresholds"),
            ("Ctrl+W", "silence reminder"),
        ],
    ),
];

fn section_lines(sections: &[(&str, &[(&str, &str)])]) -> Vec<Line<'static>> {
    let width = sections
        .iter()
        .flat_map(|(_, keys)| keys.iter().map(|(key, _)| key.len()))
        .max()
        .unwrap_or(0);
    let mut lines = Vec::new();
    for (title, keys) in sections {
        if !lines.is_empty() {
            lines.push(Line::from(""));
        }
        lines.push(
            Line::from(title.to_string()).style(
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
        );
        lines.extend(
            keys.iter()
                .map(|(key, action)| Line::from(format!("{key:<width$}  {action}"))),
        );
    }
    lines
}

pub(super) fn draw_help(frame: &mut ratatui::Frame<'_>, area: Rect) {
    let two_columns = area.width >= 80;
    let width = area
        .width
        .saturating_sub(2)
        .min(if two_columns { 90 } else { 50 });
    let height = area.height.min(if two_columns { 22 } else { 38 });
    let rect = Rect {
        x: area.x + area.width.saturating_sub(width) / 2,
        y: area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    };
    frame.render_widget(Clear, rect);
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Keys · F1/Esc close ");
    let inner = block.inner(rect);
    frame.render_widget(block, rect);
    if two_columns {
        let half = inner.width / 2;
        let (left, right) = SECTIONS.split_at(2);
        for (column, x, w) in [
            (left, inner.x + 1, half.saturating_sub(1)),
            (right, inner.x + half + 1, inner.width - half - 1),
        ] {
            frame.render_widget(
                Paragraph::new(section_lines(column)),
                Rect {
                    x,
                    width: w,
                    ..inner
                },
            );
        }
    } else {
        frame.render_widget(Paragraph::new(section_lines(SECTIONS)), inner);
    }
}
