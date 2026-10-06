use ratatui::text::Line;
use ratatui::widgets::{Block, Clear, Paragraph};

use crate::observation::AttentionClass;
use crate::state::CoreSnapshot;

pub(super) struct AttentionEditor {
    pub(super) class_index: usize,
    pub(super) field: usize,
    pub(super) quiet: String,
    pub(super) attention: String,
    pub(super) notice: Option<String>,
}

impl AttentionEditor {
    pub(super) fn new(snapshot: &CoreSnapshot, class_index: usize) -> Self {
        let pair = snapshot
            .observation
            .settings
            .get(AttentionClass::ALL[class_index]);
        Self {
            class_index,
            field: 0,
            quiet: pair.quiet_ms.to_string(),
            attention: pair.attention_ms.to_string(),
            notice: None,
        }
    }

    pub(super) fn input(&mut self) -> &mut String {
        if self.field == 0 {
            &mut self.quiet
        } else {
            &mut self.attention
        }
    }
}

pub(super) fn draw_attention_editor(
    frame: &mut ratatui::Frame<'_>,
    snapshot: &CoreSnapshot,
    editor: &AttentionEditor,
) {
    let screen = frame.area();
    let width = screen.width.min(68);
    let height = screen.height.min(12);
    let area = ratatui::layout::Rect::new(
        screen.x + (screen.width - width) / 2,
        screen.y + (screen.height - height) / 2,
        width,
        height,
    );
    let class = AttentionClass::ALL[editor.class_index];
    let effective = snapshot.observation.settings.get(class);
    let rows = [
        format!("{class:?} / effective source {:?}", effective.source),
        format!(
            "{} Quiet ms: {}",
            if editor.field == 0 { ">" } else { " " },
            editor.quiet
        ),
        format!(
            "{} Attention ms: {}",
            if editor.field == 1 { ">" } else { " " },
            editor.attention
        ),
        "Up/Down class · Tab field · Ctrl+U clear".into(),
        "Enter apply · Esc close · session only".into(),
        editor.notice.clone().unwrap_or_default(),
    ];
    let lines: Vec<_> = rows
        .iter()
        .flat_map(|row| super::wrap(row, width.saturating_sub(2) as usize))
        .map(Line::from)
        .collect();
    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new(lines).block(
            Block::default()
                .borders(ratatui::widgets::Borders::ALL)
                .title(" Attention · F10 "),
        ),
        area,
    );
    frame.set_cursor_position((area.x + 1, area.y + 2 + editor.field as u16));
}

#[cfg(test)]
mod tests {
    use super::{draw_attention_editor, AttentionEditor};
    use crate::state::CoreSnapshot;
    use ratatui::{backend::TestBackend, Terminal};

    #[test]
    fn attention_editor_renders_effective_source_and_controls() {
        let snapshot = CoreSnapshot::default();
        let editor = AttentionEditor::new(&snapshot, 0);
        let mut terminal = Terminal::new(TestBackend::new(80, 20)).unwrap();
        terminal
            .draw(|frame| draw_attention_editor(frame, &snapshot, &editor))
            .unwrap();
        let screen: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(screen.contains("effective source"), "{screen}");
        assert!(screen.contains("Enter apply"), "{screen}");
    }
}
