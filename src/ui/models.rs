//! Model picker opened with `/model`; it lists Core's inventory and submits one selection.

use std::cell::Cell;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Borders, Clear, Paragraph};
use unicode_width::UnicodeWidthStr;

use super::activity::{clip_spans, footer, selected_line, tone, truncate_display_label};
use super::lang::tr;
use super::theme;
use crate::models::{ModelOption, ModelsAvailability, MAX_MODEL_BYTES};
use crate::state::CoreSnapshot;

#[derive(Debug, Default)]
pub(super) struct ModelPicker {
    pub visible: bool,
    query: String,
    selected: usize,
    scroll: usize,
    visible_rows: Cell<usize>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum ModelPickerEvent {
    Close,
    Select(String),
    /// Open the provider form empty, or on the highlighted model.
    AddProvider,
    EditProvider(ModelOption),
    /// Restart the backend so edited configuration is read; the picker stays open.
    Reload,
    /// Send one short message through the highlighted model; the picker stays open.
    Test(String),
    Interrupt,
    Quit,
    Consumed,
}

impl ModelPicker {
    pub fn open(&mut self) {
        *self = Self {
            visible: true,
            ..Default::default()
        };
    }

    pub fn close(&mut self) {
        self.visible = false;
    }

    pub fn paste(&mut self, text: &str) {
        for character in text.chars().filter(|character| !character.is_control()) {
            self.push(character);
        }
    }

    fn push(&mut self, character: char) {
        if self.query.len() + character.len_utf8() <= MAX_MODEL_BYTES {
            self.query.push(character);
            self.selected = 0;
            self.scroll = 0;
        }
    }

    fn matches<'a>(&self, snapshot: &'a CoreSnapshot) -> Vec<&'a ModelOption> {
        let query = self.query.trim().to_lowercase();
        snapshot
            .models
            .options
            .iter()
            .filter(|option| {
                query.is_empty()
                    || option.id.to_lowercase().contains(&query)
                    || option.name.to_lowercase().contains(&query)
            })
            .collect()
    }

    pub fn key(&mut self, key: KeyEvent, snapshot: &CoreSnapshot) -> ModelPickerEvent {
        let control = key.modifiers.contains(KeyModifiers::CONTROL);
        let count = self.matches(snapshot).len();
        match key.code {
            KeyCode::Esc => {
                self.close();
                return ModelPickerEvent::Close;
            }
            KeyCode::Char('q') if control => return ModelPickerEvent::Quit,
            KeyCode::Char('c') if control => return ModelPickerEvent::Interrupt,
            KeyCode::Char('r') if control => return ModelPickerEvent::Reload,
            KeyCode::Char('t') if control => {
                if let Some(option) = self.matches(snapshot).get(self.selected) {
                    return ModelPickerEvent::Test(option.id.clone());
                }
            }
            KeyCode::Char('n') if control => {
                self.close();
                return ModelPickerEvent::AddProvider;
            }
            KeyCode::Char('e') if control => {
                if let Some(option) = self.matches(snapshot).get(self.selected) {
                    let option = (*option).clone();
                    self.close();
                    return ModelPickerEvent::EditProvider(option);
                }
            }
            KeyCode::Up => self.select(self.selected.saturating_sub(1), count),
            KeyCode::Down => self.select(self.selected + 1, count),
            KeyCode::PageUp => self.select(
                self.selected.saturating_sub(self.visible_rows.get().max(1)),
                count,
            ),
            KeyCode::PageDown => self.select(self.selected + self.visible_rows.get().max(1), count),
            KeyCode::Home => self.select(0, count),
            KeyCode::End => self.select(usize::MAX, count),
            KeyCode::Enter => {
                let chosen = self
                    .matches(snapshot)
                    .get(self.selected)
                    .map(|option| option.id.clone())
                    // 清单不可用时，输入的名字原样交给 Core 和后端判断。
                    .or_else(|| {
                        let typed = self.query.trim();
                        (snapshot.models.options.is_empty() && !typed.is_empty())
                            .then(|| typed.to_owned())
                    });
                if let Some(model) = chosen {
                    self.close();
                    return ModelPickerEvent::Select(model);
                }
            }
            KeyCode::Backspace => {
                self.query.pop();
                self.selected = 0;
                self.scroll = 0;
            }
            KeyCode::Char(character) if !control && !key.modifiers.contains(KeyModifiers::ALT) => {
                self.push(character)
            }
            _ => {}
        }
        ModelPickerEvent::Consumed
    }

    fn select(&mut self, index: usize, count: usize) {
        self.selected = index.min(count.saturating_sub(1));
        let rows = self.visible_rows.get().max(1);
        if self.selected < self.scroll {
            self.scroll = self.selected;
        } else if self.selected >= self.scroll + rows {
            self.scroll = self.selected + 1 - rows;
        }
    }

    pub fn draw(&self, frame: &mut ratatui::Frame<'_>, area: Rect, snapshot: &CoreSnapshot) {
        let width = if area.width == 0 {
            0
        } else {
            area.width.saturating_sub(4).clamp(1, 76).min(area.width)
        };
        let height = if area.height == 0 {
            0
        } else {
            area.height.saturating_sub(2).clamp(1, 20).min(area.height)
        };
        let rect = Rect {
            x: area.x + area.width.saturating_sub(width) / 2,
            y: area.y + area.height.saturating_sub(height) / 2,
            width,
            height,
        };
        frame.render_widget(Clear, rect);
        let theme = theme::current();
        let block = theme::focus_panel()
            .borders(Borders::ALL)
            .title(tr(" Model · /model ", " 选择模型 · /model "))
            .title_bottom(footer(
                tr(
                    "Esc close | Enter select | ↑/↓ move",
                    "Esc 关闭 | Enter 选择 | ↑/↓ 移动",
                ),
                rect.width,
            ));
        let inner = block.inner(rect);
        let inner_width = inner.width as usize;
        let mut input = vec![Span::styled("> ", theme.key())];
        if self.query.is_empty() {
            input.push(Span::styled(
                tr("Type to filter models", "输入以筛选模型"),
                theme.faint(),
            ));
        } else {
            input.push(Span::raw(self.query.clone()));
        }
        let mut lines = vec![Line::from(clip_spans(input, inner_width))];
        let current = snapshot.model.as_deref();
        use crate::models::ModelTest;
        let status = match (&snapshot.models.switching, current) {
            _ if snapshot.models.reloading => {
                tr("Reloading configuration...", "正在刷新配置…").to_owned()
            }
            // 测试结果留在状态行，直到下一次测试。
            _ if snapshot.models.test != ModelTest::Idle => match &snapshot.models.test {
                ModelTest::Running { model } => {
                    format!("{} {model}...", tr("Testing", "正在测试"))
                }
                ModelTest::Passed { model, millis } => format!(
                    "{} {model} ({:.1}s)",
                    tr("Test passed:", "测试通过："),
                    *millis as f64 / 1000.0
                ),
                ModelTest::Failed { model, message } => {
                    format!("{} {model}: {message}", tr("Test failed:", "测试失败："))
                }
                ModelTest::Idle => String::new(),
            },
            (Some(model), _) => format!("{} {model}", tr("Switching to", "正在切换到")),
            (None, Some(model)) => format!("{} {model}", tr("Current:", "当前：")),
            (None, None) => tr("Current: backend default", "当前：后端默认").to_owned(),
        };
        lines.push(Line::from(clip_spans(
            vec![Span::styled(status, theme.secondary())],
            inner_width,
        )));
        // 配置相关的按键单独一行，底部边框放不下这么多。
        if inner.height >= 6 {
            lines.push(Line::from(clip_spans(
                vec![Span::styled(
                    tr(
                        "Ctrl+N add  Ctrl+E edit  Ctrl+T test  Ctrl+R reload config",
                        "Ctrl+N 添加  Ctrl+E 编辑  Ctrl+T 测试  Ctrl+R 刷新配置",
                    ),
                    theme.faint(),
                )],
                inner_width,
            )));
        }
        let matches = self.matches(snapshot);
        let capacity = (inner.height as usize).saturating_sub(lines.len());
        self.visible_rows.set(capacity.max(1));
        if matches.is_empty() {
            let message = match snapshot.models.availability {
                _ if !snapshot.models.options.is_empty() => {
                    tr("No matching models.", "没有匹配的模型。")
                }
                ModelsAvailability::NotQueried | ModelsAvailability::Loading => {
                    tr("Loading models...", "正在读取模型列表…")
                }
                ModelsAvailability::Available => {
                    tr("The backend lists no models.", "后端没有列出模型。")
                }
                ModelsAvailability::Unsupported => tr(
                    "This backend cannot list models; type a name and press Enter.",
                    "此后端不能列出模型；输入模型名后按 Enter。",
                ),
                ModelsAvailability::TimedOut => tr(
                    "The model list timed out; type a name and press Enter.",
                    "读取模型列表超时；输入模型名后按 Enter。",
                ),
                ModelsAvailability::RpcError
                | ModelsAvailability::Malformed
                | ModelsAvailability::TransportUnavailable => tr(
                    "The model list is unavailable; type a name and press Enter.",
                    "模型列表不可用；输入模型名后按 Enter。",
                ),
            };
            lines.push(Line::from(clip_spans(
                vec![Span::styled(message, tone(theme.warning))],
                inner_width,
            )));
        } else {
            let selected = self.selected.min(matches.len() - 1);
            let start = self
                .scroll
                .min(selected)
                .max((selected + 1).saturating_sub(capacity.max(1)));
            let id_width = matches
                .iter()
                .map(|option| option.id.width())
                .max()
                .unwrap_or(0);
            for (index, option) in matches.iter().enumerate().skip(start).take(capacity) {
                let marker = if index == selected { "> " } else { "  " };
                let is_current = current == Some(option.id.as_str());
                let mark = if is_current { "● " } else { "  " };
                let id = truncate_display_label(
                    &option.id,
                    inner_width
                        .saturating_sub(marker.width() + mark.width())
                        .max(1),
                );
                let mut used = marker.width() + mark.width() + id.width();
                let mut spans = vec![Span::raw(marker), Span::raw(mark), Span::raw(id)];
                let name_column = marker.width() + mark.width() + id_width + 2;
                if option.name != option.id && inner_width > name_column + 4 {
                    spans.push(Span::raw(" ".repeat(name_column - used)));
                    used = name_column;
                    spans.push(Span::styled(
                        truncate_display_label(&option.name, inner_width - used),
                        theme.secondary(),
                    ));
                }
                lines.push(if index == selected {
                    selected_line(spans, inner_width)
                } else {
                    Line::from(clip_spans(spans, inner_width))
                });
            }
        }
        frame.render_widget(Paragraph::new(lines).block(block), rect);
        if rect.width > 2 && rect.height > 2 {
            let cursor = format!("> {}", self.query).width();
            frame.set_cursor_position((
                rect.x + 1 + cursor.min(rect.width.saturating_sub(2) as usize) as u16,
                rect.y + 1,
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::ModelsSnapshot;

    fn snapshot() -> CoreSnapshot {
        CoreSnapshot {
            model: Some("hi/gpt-6-luna".into()),
            models: ModelsSnapshot {
                availability: ModelsAvailability::Available,
                options: [
                    "deepseek-official/deepseek-v4-flash",
                    "hi/gpt-6-luna",
                    "hi/gpt-6.1-sol",
                ]
                .into_iter()
                .map(|id| ModelOption {
                    id: id.into(),
                    name: format!("Name of {id}"),
                })
                .collect(),
                ..Default::default()
            },
            ..Default::default()
        }
    }

    fn press(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn screen(picker: &ModelPicker, snapshot: &CoreSnapshot, width: u16, height: u16) -> String {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|frame| picker.draw(frame, frame.area(), snapshot))
            .unwrap();
        let buffer = terminal.backend().buffer();
        (0..height)
            .map(|y| {
                (0..width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn picker_moves_filters_and_selects_the_listed_identifier() {
        let snapshot = snapshot();
        let mut picker = ModelPicker::default();
        picker.open();
        picker.key(press(KeyCode::Down), &snapshot);
        assert_eq!(
            picker.key(press(KeyCode::Enter), &snapshot),
            ModelPickerEvent::Select("hi/gpt-6-luna".into())
        );
        assert!(!picker.visible);

        picker.open();
        for character in "SOL".chars() {
            picker.key(press(KeyCode::Char(character)), &snapshot);
        }
        let text = screen(&picker, &snapshot, 80, 16);
        assert!(text.contains("hi/gpt-6.1-sol"), "{text}");
        assert!(!text.contains("deepseek-v4-flash"), "{text}");
        assert_eq!(
            picker.key(press(KeyCode::Enter), &snapshot),
            ModelPickerEvent::Select("hi/gpt-6.1-sol".into())
        );

        picker.open();
        picker.paste("zzz");
        assert_eq!(
            picker.key(press(KeyCode::Enter), &snapshot),
            ModelPickerEvent::Consumed
        );
        assert!(screen(&picker, &snapshot, 80, 16).contains("No matching models."));
        assert_eq!(
            picker.key(press(KeyCode::Esc), &snapshot),
            ModelPickerEvent::Close
        );
        assert!(!picker.visible);
    }

    #[test]
    fn picker_marks_the_current_model_and_explains_an_unavailable_list() {
        let snapshot = snapshot();
        let mut picker = ModelPicker::default();
        picker.open();
        let text = screen(&picker, &snapshot, 90, 16);
        assert!(text.contains("Current: hi/gpt-6-luna"), "{text}");
        assert!(text.contains("● hi/gpt-6-luna"), "{text}");
        assert!(text.contains("Name of hi/gpt-6.1-sol"), "{text}");
        assert!(text.contains("Enter select"), "{text}");
        assert!(text.contains("Ctrl+N add"), "{text}");
        assert!(text.contains("Ctrl+E edit"), "{text}");
        assert!(text.contains("Ctrl+R reload"), "{text}");
        assert_eq!(
            picker.key(
                KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL),
                &snapshot
            ),
            ModelPickerEvent::Reload
        );
        assert!(picker.visible);
        let mut reloading = snapshot.clone();
        reloading.models.reloading = true;
        assert!(screen(&picker, &reloading, 90, 16).contains("Reloading configuration..."));
        let control = |character| KeyEvent::new(KeyCode::Char(character), KeyModifiers::CONTROL);
        picker.key(press(KeyCode::Down), &snapshot);
        assert_eq!(
            picker.key(control('e'), &snapshot),
            ModelPickerEvent::EditProvider(snapshot.models.options[1].clone())
        );
        picker.open();
        assert_eq!(
            picker.key(control('n'), &snapshot),
            ModelPickerEvent::AddProvider
        );
        picker.open();

        let mut empty = CoreSnapshot::default();
        assert!(screen(&picker, &empty, 80, 12).contains("Loading models..."));
        empty.models.availability = ModelsAvailability::Unsupported;
        assert!(screen(&picker, &empty, 90, 12).contains("cannot list models"));
        picker.paste("custom-model");
        assert_eq!(
            picker.key(press(KeyCode::Enter), &empty),
            ModelPickerEvent::Select("custom-model".into())
        );
        for (width, height) in [(1, 1), (3, 3), (10, 4), (24, 8)] {
            screen(&picker, &snapshot, width, height);
        }
    }

    #[test]
    fn picker_scrolls_the_selection_into_view() {
        let mut snapshot = snapshot();
        snapshot.models.options = (0..40)
            .map(|index| ModelOption {
                id: format!("model-{index:02}"),
                name: format!("model-{index:02}"),
            })
            .collect();
        let mut picker = ModelPicker::default();
        picker.open();
        screen(&picker, &snapshot, 60, 12);
        picker.key(press(KeyCode::End), &snapshot);
        let text = screen(&picker, &snapshot, 60, 12);
        assert!(text.contains("> ") && text.contains("model-39"), "{text}");
        assert!(!text.contains("model-00"), "{text}");
        picker.key(press(KeyCode::Home), &snapshot);
        assert!(screen(&picker, &snapshot, 60, 12).contains("model-00"));
    }
}
