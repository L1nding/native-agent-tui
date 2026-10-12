//! Form opened from the model picker to add a provider and model to dsh's settings.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Borders, Clear, Paragraph};
use unicode_width::UnicodeWidthStr;

use super::activity::{clip_spans, footer, selected_line, tone};
use super::lang::tr;
use super::theme;
use crate::backend::dsh_config::{self, ProviderSpec, Secret, API_KINDS};
use crate::models::ModelTest;

const PROVIDER: usize = 0;
const BASE_URL: usize = 1;
const API: usize = 2;
const KEY: usize = 3;
const MODEL: usize = 4;
const NAME: usize = 5;
const CONTEXT: usize = 6;
const FIELDS: usize = 7;
const FIELD_BYTES: usize = 2048;

#[derive(Default)]
pub(super) struct ProviderForm {
    pub visible: bool,
    field: usize,
    /// Text of every field; the API row keeps its choice in `api`.
    text: [String; FIELDS],
    /// Byte offset of the caret in each field, always on a character boundary.
    cursor: [usize; FIELDS],
    api: usize,
    /// Opened on an existing model; only the title differs, saving is the same upsert.
    editing: bool,
    /// A test was requested from this form; its result replaces the field hint.
    tested: bool,
    notice: Option<&'static str>,
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum ProviderFormEvent {
    Close,
    Submit(ProviderSpec),
    /// Save, then send one short message through this provider; the form stays open.
    Test(ProviderSpec),
    Interrupt,
    Quit,
    Consumed,
}

fn label(field: usize) -> &'static str {
    match field {
        PROVIDER => tr("Provider name", "Provider 名称"),
        BASE_URL => tr("Base URL", "接口地址 URL"),
        API => tr("API type", "API 类型"),
        KEY => tr("API key", "API key"),
        MODEL => tr("Model id", "模型 ID"),
        NAME => tr("Display name", "显示名称"),
        _ => tr("Context length", "上下文长度"),
    }
}

fn hint(field: usize) -> &'static str {
    match field {
        PROVIDER => tr(
            "Short lowercase name, e.g. myapi. An existing name adds a model to it.",
            "小写短名，例如 myapi。已有的名称会在其下追加模型。",
        ),
        BASE_URL => tr(
            "e.g. https://api.example.com/v1",
            "例如 https://api.example.com/v1",
        ),
        API => tr("←/→ to change.", "←/→ 切换。"),
        KEY => tr(
            "Stored in dsh's credential store. Leave empty to keep the existing one.",
            "保存到 dsh 的凭据文件。留空则沿用已有的凭据。",
        ),
        MODEL => tr(
            "The identifier the service expects.",
            "服务端接受的模型标识。",
        ),
        NAME => tr(
            "Optional; shown in the model list.",
            "可选；显示在模型列表里。",
        ),
        _ => tr(
            "Tokens, e.g. 272000. Empty keeps the stored value.",
            "单位为 token，例如 272000。留空则保持原值。",
        ),
    }
}

impl ProviderForm {
    pub fn open(&mut self) {
        // 重新打开时不保留上次输入的 key。
        *self = Self {
            visible: true,
            api: 1,
            ..Default::default()
        };
    }

    /// Opens the form on a listed model (`provider/model`) with what the
    /// dsh holds for its provider. The key stays empty: empty keeps it.
    pub fn open_edit(
        &mut self,
        model: &crate::models::ModelOption,
        providers: &[dsh_config::ProviderInfo],
    ) {
        self.open();
        self.editing = true;
        let (provider, id) = model.id.split_once('/').unwrap_or(("", model.id.as_str()));
        self.text[PROVIDER] = provider.into();
        self.text[MODEL] = id.into();
        if model.name != model.id {
            self.text[NAME] = model.name.clone();
        }
        if let Some(known) = providers.iter().find(|known| known.name == provider) {
            self.text[BASE_URL] = known.base_url.clone();
            if let Some(api) = API_KINDS.iter().position(|kind| *kind == known.api) {
                self.api = api;
            }
            if let Some((_, tokens)) = known.context_windows.iter().find(|(known, _)| known == id) {
                self.text[CONTEXT] = tokens.to_string();
            }
        }
        for field in 0..FIELDS {
            self.cursor[field] = self.text[field].len();
        }
        self.field = BASE_URL;
    }

    pub fn close(&mut self) {
        *self = Self::default();
    }

    pub fn paste(&mut self, text: &str) {
        for character in text.chars().filter(|character| !character.is_control()) {
            self.push(character);
        }
    }

    fn push(&mut self, character: char) {
        // 上下文长度只接受数字。
        if self.field == CONTEXT && (!character.is_ascii_digit() || self.text[CONTEXT].len() >= 12)
        {
            return;
        }
        if self.field != API && self.text[self.field].len() + character.len_utf8() <= FIELD_BYTES {
            self.text[self.field].insert(self.cursor[self.field], character);
            self.cursor[self.field] += character.len_utf8();
            self.notice = None;
        }
    }

    /// Moves the caret one character left or right inside the focused field.
    fn step(&mut self, forward: bool) {
        let text = &self.text[self.field];
        let at = self.cursor[self.field];
        self.cursor[self.field] = if forward {
            text[at..]
                .chars()
                .next()
                .map_or(at, |character| at + character.len_utf8())
        } else {
            text[..at]
                .chars()
                .next_back()
                .map_or(at, |character| at - character.len_utf8())
        };
    }

    fn spec(&self) -> ProviderSpec {
        let field = |index: usize| self.text[index].trim().to_owned();
        ProviderSpec {
            provider: field(PROVIDER),
            base_url: field(BASE_URL),
            api: API_KINDS[self.api].into(),
            api_key: Secret(field(KEY)),
            model: field(MODEL),
            model_name: field(NAME),
            context_window: field(CONTEXT).parse().ok(),
        }
    }

    fn submit(&mut self) -> ProviderFormEvent {
        let spec = self.spec();
        match dsh_config::validate(&spec) {
            Ok(()) => {
                self.close();
                ProviderFormEvent::Submit(spec)
            }
            Err(error) => {
                self.notice = Some(error);
                ProviderFormEvent::Consumed
            }
        }
    }

    /// Saves like `submit` but keeps the form, so the result can be read and
    /// the fields corrected. The key was written; it is not kept on screen.
    fn test(&mut self) -> ProviderFormEvent {
        let spec = self.spec();
        match dsh_config::validate(&spec) {
            Ok(()) => {
                self.text[KEY].clear();
                self.cursor[KEY] = 0;
                self.editing = true;
                self.tested = true;
                self.notice = None;
                ProviderFormEvent::Test(spec)
            }
            Err(error) => {
                self.notice = Some(error);
                ProviderFormEvent::Consumed
            }
        }
    }

    pub fn key(&mut self, key: KeyEvent) -> ProviderFormEvent {
        let control = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Esc => {
                self.close();
                return ProviderFormEvent::Close;
            }
            KeyCode::Char('q') if control => return ProviderFormEvent::Quit,
            KeyCode::Char('c') if control => return ProviderFormEvent::Interrupt,
            KeyCode::Char('s') if control => return self.submit(),
            KeyCode::Char('t') if control => return self.test(),
            KeyCode::Char('u') if control => {
                self.text[self.field].clear();
                self.cursor[self.field] = 0;
            }
            KeyCode::Up | KeyCode::BackTab => self.field = self.field.saturating_sub(1),
            KeyCode::Down | KeyCode::Tab => self.field = (self.field + 1).min(FIELDS - 1),
            KeyCode::Left if self.field == API => {
                self.api = (self.api + API_KINDS.len() - 1) % API_KINDS.len()
            }
            KeyCode::Right if self.field == API => self.api = (self.api + 1) % API_KINDS.len(),
            KeyCode::Left => self.step(false),
            KeyCode::Right => self.step(true),
            KeyCode::Home => self.cursor[self.field] = 0,
            KeyCode::End => self.cursor[self.field] = self.text[self.field].len(),
            KeyCode::Enter if self.field + 1 == FIELDS => return self.submit(),
            KeyCode::Enter => self.field += 1,
            KeyCode::Backspace => {
                let end = self.cursor[self.field];
                self.step(false);
                self.text[self.field].drain(self.cursor[self.field]..end);
            }
            KeyCode::Delete => {
                let start = self.cursor[self.field];
                self.step(true);
                self.text[self.field].drain(start..self.cursor[self.field]);
                self.cursor[self.field] = start;
            }
            KeyCode::Char(character) if !control && !key.modifiers.contains(KeyModifiers::ALT) => {
                self.push(character)
            }
            _ => {}
        }
        ProviderFormEvent::Consumed
    }

    pub fn draw(&self, frame: &mut ratatui::Frame<'_>, area: Rect, test: &ModelTest) {
        if area.width == 0 || area.height == 0 {
            return;
        }
        let width = area.width.saturating_sub(4).clamp(1, 78).min(area.width);
        let height = area.height.saturating_sub(2).clamp(1, 15).min(area.height);
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
            .title(if self.editing {
                tr(" Edit model provider (dsh) ", " 编辑模型配置（dsh） ")
            } else {
                tr(" Add model provider (dsh) ", " 添加模型配置（dsh） ")
            })
            .title_bottom(footer(
                tr(
                    "Esc close | Enter next | Ctrl+S save | Ctrl+T save and test",
                    "Esc 关闭 | Enter 下一项 | Ctrl+S 保存 | Ctrl+T 保存并测试",
                ),
                rect.width,
            ));
        let inner = block.inner(rect);
        let inner_width = inner.width as usize;
        let label_width = (0..FIELDS)
            .map(|field| label(field).width())
            .max()
            .unwrap_or(0)
            + 2;
        let mut lines = Vec::new();
        let mut cursor = None;
        for field in 0..FIELDS {
            let focused = field == self.field;
            let value: Vec<char> = match field {
                API => format!("‹ {} ›", API_KINDS[self.api]).chars().collect(),
                // key 不回显，每个字符显示为一个圆点。
                KEY => self.text[KEY].chars().map(|_| '•').collect(),
                _ => self.text[field].chars().collect(),
            };
            let name = label(field);
            let prefix = format!(
                "{}{name}{}",
                if focused { "> " } else { "  " },
                " ".repeat(label_width - name.width())
            );
            // 值太长时只显示一段，窗口跟着光标走，光标始终可见。
            let room = inner_width.saturating_sub(prefix.width() + 1).max(1);
            let caret = if focused && field != API {
                self.text[field][..self.cursor[field]].chars().count()
            } else {
                0
            };
            let cells = |range: &[char]| -> usize {
                range
                    .iter()
                    .map(|character| character.to_string().width())
                    .sum()
            };
            let mut start = 0;
            while cells(&value[start..caret]) >= room {
                start += 1;
            }
            let mut end = start;
            while end < value.len() && cells(&value[start..=end]) <= room {
                end += 1;
            }
            let shown: String = value[start..end].iter().collect();
            if focused {
                cursor = Some((lines.len(), prefix.width() + cells(&value[start..caret])));
                lines.push(selected_line(
                    vec![Span::raw(prefix), Span::raw(shown)],
                    inner_width,
                ));
            } else {
                lines.push(Line::from(clip_spans(
                    vec![Span::styled(prefix, theme.secondary()), Span::raw(shown)],
                    inner_width,
                )));
            }
        }
        lines.push(Line::from(""));
        lines.push(Line::from(clip_spans(
            vec![match self.notice {
                Some(notice) => Span::styled(notice, tone(theme.warning)),
                None => Span::styled(hint(self.field), theme.secondary()),
            }],
            inner_width,
        )));
        // 测试结果占最后几行；失败原因可能较长，按宽度折成多行。
        let (status, style) = match test {
            _ if !self.tested => (
                tr(
                    "Saved to dsh's settings; usable at once, no restart.",
                    "保存到 dsh 的设置，立即可用，无需重启。",
                )
                .to_owned(),
                theme.secondary(),
            ),
            ModelTest::Idle => (
                tr(
                    "Saved; waiting for the test to start...",
                    "已保存，等待测试开始…",
                )
                .to_owned(),
                theme.secondary(),
            ),
            ModelTest::Running { model } => (
                format!("{} {model}...", tr("Testing", "正在测试")),
                theme.secondary(),
            ),
            ModelTest::Passed { model, millis } => (
                format!(
                    "{} {model} {} {:.1}s",
                    tr("Test passed:", "测试通过："),
                    tr("replied in", "已回复，用时"),
                    *millis as f64 / 1000.0
                ),
                tone(theme.success),
            ),
            ModelTest::Failed { model, message } => (
                format!("{} {model}: {message}", tr("Test failed:", "测试失败：")),
                tone(theme.error),
            ),
        };
        let mut row = String::new();
        let mut rows = Vec::new();
        for character in status.chars() {
            if row.width() + character.to_string().width() > inner_width.max(1) {
                rows.push(std::mem::take(&mut row));
            }
            row.push(character);
        }
        rows.push(row);
        let room = (inner.height as usize).saturating_sub(lines.len()).max(1);
        lines.extend(
            rows.into_iter()
                .take(room)
                .map(|row| Line::from(Span::styled(row, style))),
        );
        frame.render_widget(Paragraph::new(lines).block(block), rect);
        if let Some((row, column)) = cursor {
            if self.field != API && (row as u16) < inner.height && inner.width > 0 {
                frame.set_cursor_position((
                    inner.x + (column as u16).min(inner.width - 1),
                    inner.y + row as u16,
                ));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn press(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn screen(form: &ProviderForm, width: u16, height: u16) -> String {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|frame| form.draw(frame, frame.area(), &ModelTest::Idle))
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
    fn the_form_collects_a_provider_hides_the_key_and_validates_before_submitting() {
        let mut form = ProviderForm::default();
        form.open();
        let text = screen(&form, 100, 20);
        for expected in [
            "Add model provider (dsh)",
            "> Provider name",
            "Base URL",
            "‹ openai-responses ›",
            "API key",
            "Model id",
            "Ctrl+S save",
        ] {
            assert!(text.contains(expected), "{expected}: {text}");
        }
        // 必填项为空时留在表单里并说明原因。
        assert_eq!(
            form.key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL)),
            ProviderFormEvent::Consumed
        );
        assert!(screen(&form, 100, 20).contains("Provider name: 1-32"));

        form.paste("myapi");
        form.key(press(KeyCode::Enter));
        form.paste("https://example.invalid/v1");
        form.key(press(KeyCode::Down));
        form.key(press(KeyCode::Left));
        form.key(press(KeyCode::Tab));
        form.paste("PRIVATE_KEY_VALUE");
        let text = screen(&form, 100, 20);
        assert!(!text.contains("PRIVATE_KEY_VALUE"), "{text}");
        assert!(text.contains("•••••••••••••••••"), "{text}");
        assert!(text.contains("‹ openai-completions ›"), "{text}");
        form.key(press(KeyCode::Enter));
        for character in "model-x".chars() {
            form.key(press(KeyCode::Char(character)));
        }
        form.key(press(KeyCode::Enter));
        form.key(press(KeyCode::Enter));
        assert_eq!(
            form.key(press(KeyCode::Enter)),
            ProviderFormEvent::Submit(ProviderSpec {
                provider: "myapi".into(),
                base_url: "https://example.invalid/v1".into(),
                api: "openai-completions".into(),
                api_key: Secret("PRIVATE_KEY_VALUE".into()),
                model: "model-x".into(),
                model_name: String::new(),
                context_window: None,
            })
        );
        // 提交后表单清空，key 不留在界面状态里。
        assert!(!form.visible && form.text[KEY].is_empty());

        // 编辑：带出已保存的地址和 API 类型，key 留空表示沿用。
        form.open_edit(
            &crate::models::ModelOption {
                id: "hi/gpt-6-luna".into(),
                name: "GPT-6 Luna".into(),
            },
            &[dsh_config::ProviderInfo {
                name: "hi".into(),
                base_url: "https://hi.invalid".into(),
                api: "anthropic-messages".into(),
                context_windows: vec![("gpt-6-luna".into(), 272000)],
            }],
        );
        let text = screen(&form, 100, 20);
        for expected in [
            "Edit model provider (dsh)",
            "hi",
            "> Base URL",
            "https://hi.invalid",
            "‹ anthropic-messages ›",
            "gpt-6-luna",
            "GPT-6 Luna",
        ] {
            assert!(text.contains(expected), "{expected}: {text}");
        }
        form.paste("/v2");
        let ProviderFormEvent::Submit(edited) =
            form.key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL))
        else {
            panic!()
        };
        assert_eq!(edited.provider, "hi");
        assert_eq!(edited.base_url, "https://hi.invalid/v2");
        assert_eq!(edited.model, "gpt-6-luna");
        assert!(edited.api_key.0.is_empty());

        form.open();
        form.paste("draft");
        assert_eq!(form.key(press(KeyCode::Esc)), ProviderFormEvent::Close);
        assert!(form.text[PROVIDER].is_empty());
    }

    #[test]
    fn context_length_takes_digits_and_ctrl_t_saves_tests_and_keeps_the_form_open() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        let show = |form: &ProviderForm, test: &ModelTest| -> String {
            let mut terminal = Terminal::new(TestBackend::new(100, 22)).unwrap();
            terminal
                .draw(|frame| form.draw(frame, frame.area(), test))
                .unwrap();
            let buffer = terminal.backend().buffer();
            (0..22)
                .map(|y| {
                    (0..100)
                        .map(|x| buffer[(x, y)].symbol())
                        .collect::<String>()
                })
                .collect::<Vec<_>>()
                .join("\n")
        };
        let mut form = ProviderForm::default();
        // 编辑已有模型时带出保存的上下文长度。
        form.open_edit(
            &crate::models::ModelOption {
                id: "hi/gpt-6-luna".into(),
                name: "GPT-6 Luna".into(),
            },
            &[dsh_config::ProviderInfo {
                name: "hi".into(),
                base_url: "https://hi.invalid".into(),
                api: "openai-responses".into(),
                context_windows: vec![("other".into(), 1), ("gpt-6-luna".into(), 272000)],
            }],
        );
        let text = show(&form, &ModelTest::Idle);
        assert!(text.contains("Context length"), "{text}");
        assert!(text.contains("272000"), "{text}");
        assert!(text.contains("Ctrl+T save and test"), "{text}");
        for _ in 0..FIELDS {
            form.key(press(KeyCode::Down));
        }
        form.key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
        form.paste("40x0 000k");
        assert_eq!(form.text[CONTEXT], "400000");
        form.key(press(KeyCode::Up));
        form.key(press(KeyCode::Up));
        form.key(press(KeyCode::Up));
        form.paste("PRIVATE_KEY_VALUE");

        let ProviderFormEvent::Test(spec) =
            form.key(KeyEvent::new(KeyCode::Char('t'), KeyModifiers::CONTROL))
        else {
            panic!()
        };
        assert_eq!(spec.context_window, Some(400000));
        assert_eq!(spec.api_key, Secret("PRIVATE_KEY_VALUE".into()));
        // 表单保持打开以便看结果、改字段；已写入的 key 不留在界面状态里。
        assert!(form.visible && form.text[KEY].is_empty());
        assert_eq!(form.text[BASE_URL], "https://hi.invalid");
        let model = "hi/gpt-6-luna".to_owned();
        for (test, expected) in [
            (ModelTest::Idle, "waiting for the test to start"),
            (
                ModelTest::Running {
                    model: model.clone(),
                },
                "Testing hi/gpt-6-luna...",
            ),
            (
                ModelTest::Passed {
                    model: model.clone(),
                    millis: 1840,
                },
                "Test passed: hi/gpt-6-luna replied in 1.8s",
            ),
        ] {
            let text = show(&form, &test);
            assert!(text.contains(expected), "{expected}: {text}");
        }
        // 很长的失败原因折成多行，而不是被截断成一行。
        let failed = ModelTest::Failed {
            model,
            message: format!("401 {} END_OF_ERROR", "invalid api key ".repeat(8)),
        };
        let text = show(&form, &failed);
        assert!(text.contains("Test failed: hi/gpt-6-luna: 401"), "{text}");
        assert!(text.contains("END_OF_ERROR"), "{text}");

        // 留空表示不改已保存的上下文长度；0 被拒绝。
        form.key(press(KeyCode::Down));
        form.key(press(KeyCode::Down));
        form.key(press(KeyCode::Down));
        form.key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
        assert_eq!(form.spec().context_window, None);
        form.paste("0");
        assert_eq!(
            form.key(KeyEvent::new(KeyCode::Char('t'), KeyModifiers::CONTROL)),
            ProviderFormEvent::Consumed
        );
        assert!(show(&form, &ModelTest::Idle).contains("Context length: a whole number"));
    }

    #[test]
    fn arrows_home_end_and_delete_edit_inside_a_field_and_the_caret_stays_visible() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        let mut form = ProviderForm::default();
        form.open();
        form.paste("abd中f");
        // ← 两次回到 “中” 之前，插入和删除都发生在光标处。
        form.key(press(KeyCode::Left));
        form.key(press(KeyCode::Left));
        form.key(press(KeyCode::Left));
        form.key(press(KeyCode::Char('c')));
        assert_eq!(form.text[PROVIDER], "abcd中f");
        form.key(press(KeyCode::Right));
        form.key(press(KeyCode::Delete));
        assert_eq!(form.text[PROVIDER], "abcdf");
        form.key(press(KeyCode::Backspace));
        assert_eq!(form.text[PROVIDER], "abcf");
        form.key(press(KeyCode::Home));
        form.key(press(KeyCode::Left));
        form.key(press(KeyCode::Char('X')));
        form.key(press(KeyCode::End));
        form.key(press(KeyCode::Right));
        form.key(press(KeyCode::Char('Z')));
        assert_eq!(form.text[PROVIDER], "XabcfZ");
        form.key(press(KeyCode::Home));
        form.key(press(KeyCode::Backspace));
        assert_eq!(form.text[PROVIDER], "XabcfZ");

        // API 类型行的 ←/→ 仍然是切换选项；每个字段记住自己的光标。
        form.key(press(KeyCode::Down));
        form.key(press(KeyCode::Down));
        form.key(press(KeyCode::Right));
        assert_eq!(form.spec().api, "anthropic-messages");
        form.key(press(KeyCode::Up));
        form.key(press(KeyCode::Up));
        form.key(press(KeyCode::Right));
        form.key(press(KeyCode::Char('1')));
        assert_eq!(form.text[PROVIDER], "X1abcfZ");

        // 很长的值：光标在开头时显示开头，在末尾时显示末尾。
        form.key(press(KeyCode::Down));
        let long = format!("https://start.invalid/{}/end", "x".repeat(200));
        form.paste(&long);
        let mut terminal = Terminal::new(TestBackend::new(70, 20)).unwrap();
        let mut render = |form: &ProviderForm| {
            terminal
                .draw(|frame| form.draw(frame, frame.area(), &ModelTest::Idle))
                .unwrap();
            let position = terminal.get_cursor_position().unwrap();
            let buffer = terminal.backend().buffer();
            let row: String = (0..70).map(|x| buffer[(x, position.y)].symbol()).collect();
            (row, position.x)
        };
        let (row, column) = render(&form);
        assert!(row.contains("/end") && !row.contains("https://"), "{row}");
        assert!(column < 69, "{column}");
        form.key(press(KeyCode::Home));
        let (row, column) = render(&form);
        assert!(
            row.contains("https://start") && !row.contains("/end"),
            "{row}"
        );
        assert_eq!(
            column as usize,
            row.find("https://").map(|at| row[..at].width()).unwrap()
        );
        form.key(press(KeyCode::Right));
        assert_eq!(render(&form).1, column + 1);
        for (width, height) in [(1, 1), (3, 3), (10, 4), (24, 8)] {
            screen(&form, width, height);
        }
    }
}
