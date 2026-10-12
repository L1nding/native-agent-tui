use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph};
use unicode_width::UnicodeWidthStr;

use crate::observation::{AttentionClass, ConfigSource};
use crate::state::CoreSnapshot;

use super::activity::{clip_spans, hint_lines, selected_line, tone, wrap_marked};
use super::lang::{self, tr};
use super::theme;

/// 阈值类别；英文保持枚举名。
fn class_word(class: AttentionClass) -> String {
    if !lang::zh() {
        return format!("{class:?}");
    }
    match class {
        AttentionClass::Model => "模型",
        AttentionClass::Tool => "工具",
        AttentionClass::Children => "子 Agent",
        AttentionClass::Transport => "传输",
    }
    .to_owned()
}

/// 阈值来源；英文保持枚举名。
fn source_word(source: ConfigSource) -> String {
    if !lang::zh() {
        return format!("{source:?}");
    }
    match source {
        ConfigSource::Default => "默认值",
        ConfigSource::Global => "全局配置",
        ConfigSource::Cli => "命令行参数",
        ConfigSource::Tui => "本界面设置",
    }
    .to_owned()
}

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
    let theme = theme::current();
    let screen = frame.area();
    let width = screen.width.min(68);
    let height = screen.height.min(12);
    let area = ratatui::layout::Rect::new(
        screen.x + (screen.width - width) / 2,
        screen.y + (screen.height - height) / 2,
        width,
        height,
    );
    let inner = width.saturating_sub(2) as usize;
    let class = AttentionClass::ALL[editor.class_index];
    let effective = snapshot.observation.settings.get(class);
    let class_name = class_word(class);
    let mut lines = wrap_marked(
        &format!(
            "{class_name} / {} {}",
            tr("effective source", "生效来源"),
            source_word(effective.source)
        ),
        inner,
        theme.secondary(),
        |_| vec![(0..class_name.len(), theme.label())],
    );
    // 光标跟在当前字段的值后面；标记和标签的宽度先算进去。
    let mut cursor = None;
    for (index, (label, value)) in [
        (tr("Quiet ms: ", "静默阈值 ms："), &editor.quiet),
        (tr("Attention ms: ", "提醒阈值 ms："), &editor.attention),
    ]
    .into_iter()
    .enumerate()
    {
        let focused = editor.field == index;
        let text = format!("{} {label}{value}", if focused { ">" } else { " " });
        if focused {
            cursor = Some((lines.len(), text.width()));
            lines.push(selected_line(vec![Span::raw(text)], inner));
        } else {
            lines.push(Line::from(clip_spans(
                vec![
                    Span::styled(format!("  {label}"), theme.secondary()),
                    Span::raw(value.clone()),
                ],
                inner,
            )));
        }
    }
    if let Some(notice) = editor.notice.as_deref().filter(|notice| !notice.is_empty()) {
        lines.extend(wrap_marked(notice, inner, tone(theme.warning), |_| {
            Vec::new()
        }));
    }
    lines.extend(hint_lines(
        tr(
            "Up/Down class | Tab field | Ctrl+U clear",
            "Up/Down 类别 | Tab 字段 | Ctrl+U 清空",
        ),
        inner,
    ));
    // “session only”是说明而不是键位，单独用次要样式接在提示后面。
    let mut apply = hint_lines(
        tr("Enter apply | Esc close", "Enter 应用 | Esc 关闭"),
        inner,
    );
    let session_only = tr("session only", "仅本次会话有效");
    let note = format!("  ·  {session_only}");
    match apply.as_mut_slice() {
        [line] if line.width() + note.width() <= inner => {
            line.push_span(Span::styled(note, theme.secondary()));
        }
        _ => apply.extend(wrap_marked(session_only, inner, theme.secondary(), |_| {
            Vec::new()
        })),
    }
    lines.extend(apply);
    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new(lines).block(
            theme::focus_panel()
                .borders(ratatui::widgets::Borders::ALL)
                .title(tr(" Attention · F10 ", " 提醒阈值 · F10 ")),
        ),
        area,
    );
    if let Some((row, column)) = cursor {
        if width > 2 && (row as u16) < height.saturating_sub(2) {
            frame.set_cursor_position((
                area.x + 1 + column.min(inner.saturating_sub(1)) as u16,
                area.y + 1 + row as u16,
            ));
        }
    }
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

    #[test]
    fn attention_editor_marks_the_focused_field_and_puts_the_cursor_after_its_value() {
        use ratatui::style::Modifier;

        let snapshot = CoreSnapshot::default();
        let mut editor = AttentionEditor::new(&snapshot, 0);
        editor.field = 1;
        editor.attention = "4500".into();
        editor.notice = Some("Require 0 < quiet < attention <= 604800000".into());
        for (width, height) in [(80, 20), (30, 14)] {
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            terminal
                .draw(|frame| draw_attention_editor(frame, &snapshot, &editor))
                .unwrap();
            let cursor = terminal.get_cursor_position().unwrap();
            let buffer = terminal.backend().buffer();
            let rows: Vec<String> = (0..height)
                .map(|y| (0..width).map(|x| buffer[(x, y)].symbol()).collect())
                .collect();
            let text = rows.join("\n");
            let focused = rows
                .iter()
                .position(|row| row.contains("> Attention ms: 4500"))
                .unwrap_or_else(|| panic!("{text}"));
            let start = rows[focused].find("> Attention").unwrap();
            let start = rows[focused][..start].chars().count() as u16;
            // 焦点字段同时有 `>` 标记和加粗；另一个字段两者都没有。
            assert!(buffer[(start, focused as u16)]
                .modifier
                .contains(Modifier::BOLD));
            let other = rows
                .iter()
                .position(|row| row.contains("  Quiet ms: "))
                .unwrap_or_else(|| panic!("{text}"));
            assert!(!buffer[(start + 2, other as u16)]
                .modifier
                .contains(Modifier::BOLD));
            assert_eq!(cursor.y, focused as u16, "{text}");
            assert_eq!(
                cursor.x,
                start + "> Attention ms: 4500".len() as u16,
                "{text}"
            );
            assert!(text.contains("Require 0 < quiet"), "{text}");
            assert!(text.contains("session only"), "{text}");
            assert!(text.contains("Esc close"), "{text}");
        }
        for (width, height) in [(1, 1), (4, 3), (24, 8)] {
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            terminal
                .draw(|frame| draw_attention_editor(frame, &snapshot, &editor))
                .unwrap();
        }
    }

    #[test]
    fn chinese_attention_editor_keeps_the_cursor_after_the_value_at_display_width() {
        use unicode_width::UnicodeWidthStr;

        let snapshot = CoreSnapshot::default();
        let mut editor = AttentionEditor::new(&snapshot, 2);
        editor.field = 1;
        editor.attention = "4500".into();
        for (width, height) in [(80u16, 20u16), (30, 14)] {
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            crate::ui::lang::with_zh(|| {
                terminal
                    .draw(|frame| draw_attention_editor(frame, &snapshot, &editor))
                    .unwrap();
            });
            let cursor = terminal.get_cursor_position().unwrap();
            let buffer = terminal.backend().buffer();
            let rows = crate::ui::activity::buffer_rows(buffer);
            let text = rows.join("\n");
            for word in [
                "提醒阈值 · F10",
                "子 Agent / 生效来源 默认值",
                "静默阈值 ms：",
                "Enter 应用",
                "Esc 关闭",
                "仅本次会话有效",
            ] {
                assert!(text.contains(word), "{width}: {word}\n{text}");
            }
            let focused = rows
                .iter()
                .position(|row| row.contains("> 提醒阈值 ms：4500"))
                .unwrap_or_else(|| panic!("{text}"));
            let before = &rows[focused][..rows[focused].find("> 提醒").unwrap()];
            assert_eq!(cursor.y, focused as u16, "{text}");
            assert_eq!(
                cursor.x as usize,
                before.width() + "> 提醒阈值 ms：4500".width(),
                "{text}"
            );
            // 面板内每一行的显示宽度不变，右边框没有被双宽字符挤走。
            for row in rows.iter().filter(|row| row.contains('│')) {
                assert_eq!(row.width(), width as usize, "{row}");
                assert!(row.trim_end().ends_with('│'), "{row}");
            }
        }
    }
}
