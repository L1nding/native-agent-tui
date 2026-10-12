//! `/setting`: interface choices saved for the next start, dsh's permission
//! preset for this session, and the attention thresholds.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Borders, Clear, Paragraph};
use unicode_width::UnicodeWidthStr;

use super::activity::{clip_spans, footer, selected_line};
use super::lang::{self, tr};
use super::theme;
use crate::preferences::Setting;
use crate::state::CoreSnapshot;

#[derive(Debug, Default)]
pub(super) struct SettingsPanel {
    pub visible: bool,
    selected: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum SettingsEvent {
    Close,
    Change(Setting, &'static str),
    /// Switch this session's dsh permission preset.
    Permission(String),
    /// Open the existing threshold editor; its values are saved when applied.
    Attention,
    Interrupt,
    Quit,
    Consumed,
}

/// The interface choices, then the permission preset, then the row that opens
/// the attention thresholds.
const PERMISSION_ROW: usize = Setting::ALL.len();
const ATTENTION_ROW: usize = PERMISSION_ROW + 1;
const ROWS: usize = ATTENTION_ROW + 1;

fn label(setting: Setting) -> &'static str {
    match setting {
        Setting::Language => tr("Language", "界面语言"),
        Setting::Theme => tr("Theme", "主题"),
    }
}

fn detail(setting: Setting) -> &'static str {
    match setting {
        Setting::Language => tr("Applies now.", "立即生效。"),
        Setting::Theme => tr(
            "Applies now. NO_COLOR still forces mono.",
            "立即生效。设置了 NO_COLOR 时仍为单色。",
        ),
    }
}

fn shown(setting: Setting) -> &'static str {
    match setting {
        Setting::Language => lang::name(),
        Setting::Theme => theme::name(),
    }
}

fn permission_label() -> &'static str {
    tr("Permission preset", "权限预设")
}

/// What the permission row shows: the switch dsh has not confirmed yet, the
/// preset in effect, or that nothing has been read.
fn permission_value(snapshot: &CoreSnapshot) -> String {
    let permission = &snapshot.permission;
    if let Some(target) = &permission.switching {
        return format!("{target}…");
    }
    if permission.current.is_empty() {
        return tr("not available", "不可用").into();
    }
    permission.current.clone()
}

fn permission_detail(snapshot: &CoreSnapshot) -> String {
    let permission = &snapshot.permission;
    if permission.current.is_empty() {
        return tr(
            "dsh has not reported a permission preset for this session.",
            "dsh 没有报告这个会话的权限预设。",
        )
        .into();
    }
    let described = permission
        .presets
        .iter()
        .find(|preset| preset.value == permission.current)
        .map(|preset| preset.description.as_str())
        .filter(|description| !description.is_empty());
    match described {
        Some(description) => description.to_owned(),
        None => tr(
            "dsh's sandbox and approval bundle. Applies to this session now and to new sessions.",
            "dsh 的沙箱与审批组合。立即作用于当前会话，并成为新会话的默认值。",
        )
        .into(),
    }
}

impl SettingsPanel {
    pub fn open(&mut self) {
        *self = Self {
            visible: true,
            selected: 0,
        };
    }

    pub fn close(&mut self) {
        self.visible = false;
    }

    pub fn key(&mut self, key: KeyEvent, snapshot: &CoreSnapshot) -> SettingsEvent {
        let control = key.modifiers.contains(KeyModifiers::CONTROL);
        let step = match key.code {
            KeyCode::Esc => {
                self.close();
                return SettingsEvent::Close;
            }
            KeyCode::Char('q') if control => return SettingsEvent::Quit,
            KeyCode::Char('c') if control => return SettingsEvent::Interrupt,
            KeyCode::Up => {
                self.selected = self.selected.saturating_sub(1);
                return SettingsEvent::Consumed;
            }
            KeyCode::Down => {
                self.selected = (self.selected + 1).min(ROWS - 1);
                return SettingsEvent::Consumed;
            }
            KeyCode::Left => -1isize,
            KeyCode::Right | KeyCode::Enter | KeyCode::Char(' ') => 1,
            _ => return SettingsEvent::Consumed,
        };
        if self.selected == ATTENTION_ROW {
            self.close();
            return SettingsEvent::Attention;
        }
        if self.selected == PERMISSION_ROW {
            let permission = &snapshot.permission;
            // 只在 dsh 给出的预设之间切换；等待确认时不再发第二次。
            let choices: Vec<&str> = permission
                .presets
                .iter()
                .map(|preset| preset.value.as_str())
                .filter(|value| *value != "custom")
                .collect();
            if choices.is_empty() || permission.switching.is_some() {
                return SettingsEvent::Consumed;
            }
            let next = match choices
                .iter()
                .position(|value| *value == permission.current)
            {
                Some(index) => (index as isize + step).rem_euclid(choices.len() as isize) as usize,
                None => 0,
            };
            return if choices[next] == permission.current {
                SettingsEvent::Consumed
            } else {
                SettingsEvent::Permission(choices[next].to_owned())
            };
        }
        let setting = Setting::ALL[self.selected];
        let values = setting.values();
        let current = shown(setting);
        let index = values
            .iter()
            .position(|value| *value == current)
            .unwrap_or(0) as isize;
        let next = (index + step).rem_euclid(values.len() as isize) as usize;
        SettingsEvent::Change(setting, values[next])
    }

    pub fn draw(&self, frame: &mut ratatui::Frame<'_>, area: Rect, snapshot: &CoreSnapshot) {
        let width = area.width.saturating_sub(4).clamp(1, 78).min(area.width);
        let height = area.height.saturating_sub(2).clamp(1, 12).min(area.height);
        if area.width == 0 || area.height == 0 {
            return;
        }
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
            .title(tr(" Settings · /setting ", " 设置 · /setting "))
            .title_bottom(footer(
                tr(
                    "Esc close | ↑/↓ select | ←/→ change",
                    "Esc 关闭 | ↑/↓ 选择 | ←/→ 修改",
                ),
                rect.width,
            ));
        let inner = block.inner(rect).width as usize;
        let attention_label = tr("Attention thresholds", "提醒阈值");
        let label_width = Setting::ALL
            .iter()
            .map(|setting| label(*setting).width())
            .chain([permission_label().width(), attention_label.width()])
            .max()
            .unwrap_or(0)
            + 2;
        let row = |index: usize, name: &str, value: String| {
            let marker = if index == self.selected { "> " } else { "  " };
            let spans = vec![
                Span::raw(marker),
                Span::raw(format!(
                    "{name}{}",
                    " ".repeat(label_width.saturating_sub(name.width()))
                )),
                Span::styled(format!("‹ {value} ›"), theme.key()),
            ];
            if index == self.selected {
                selected_line(spans, inner)
            } else {
                Line::from(clip_spans(spans, inner))
            }
        };
        let mut lines = Vec::new();
        for (index, setting) in Setting::ALL.iter().copied().enumerate() {
            lines.push(row(index, label(setting), shown(setting).to_owned()));
        }
        lines.push(row(
            PERMISSION_ROW,
            permission_label(),
            permission_value(snapshot),
        ));
        let attention = vec![
            Span::raw(if self.selected == ATTENTION_ROW {
                "> "
            } else {
                "  "
            }),
            Span::raw(attention_label),
            Span::styled(tr("  Enter to edit", "  按 Enter 编辑"), theme.secondary()),
        ];
        lines.push(if self.selected == ATTENTION_ROW {
            selected_line(attention, inner)
        } else {
            Line::from(clip_spans(attention, inner))
        });
        lines.push(Line::from(""));
        let help = match self.selected {
            PERMISSION_ROW => permission_detail(snapshot),
            ATTENTION_ROW => tr(
                "Quiet and reminder timing per activity; saved when applied.",
                "各类活动的静默与提醒时间；应用后保存。",
            )
            .to_owned(),
            index => detail(Setting::ALL[index]).to_owned(),
        };
        lines.push(Line::from(clip_spans(
            vec![Span::styled(help, theme.secondary())],
            inner,
        )));
        frame.render_widget(Paragraph::new(lines).block(block), rect);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::permission::{PermissionPreset, PermissionSnapshot};

    fn press(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn screen(panel: &SettingsPanel, snapshot: &CoreSnapshot, width: u16, height: u16) -> String {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|frame| panel.draw(frame, frame.area(), snapshot))
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

    fn preset(value: &str, description: &str) -> PermissionPreset {
        PermissionPreset {
            value: value.into(),
            name: value.into(),
            description: description.into(),
        }
    }

    #[test]
    fn interface_rows_cycle_and_the_permission_row_offers_only_what_dsh_reported() {
        let mut snapshot = CoreSnapshot::default();
        let mut panel = SettingsPanel::default();
        panel.open();
        let text = screen(&panel, &snapshot, 100, 20);
        for expected in [
            "Settings · /setting",
            "> Language",
            "‹ en ›",
            "‹ dark ›",
            "Permission preset",
            "‹ not available ›",
            "Attention thresholds",
            "←/→ change",
        ] {
            assert!(text.contains(expected), "{expected}: {text}");
        }
        assert_eq!(
            panel.key(press(KeyCode::Right), &snapshot),
            SettingsEvent::Change(Setting::Language, "zh")
        );
        panel.key(press(KeyCode::Down), &snapshot);
        assert_eq!(
            panel.key(press(KeyCode::Left), &snapshot),
            SettingsEvent::Change(Setting::Theme, "mono")
        );

        // dsh 没有报告预设时，这一行不发出任何修改。
        panel.key(press(KeyCode::Down), &snapshot);
        assert_eq!(
            panel.key(press(KeyCode::Right), &snapshot),
            SettingsEvent::Consumed
        );
        assert!(
            screen(&panel, &snapshot, 100, 20).contains("dsh has not reported a permission preset")
        );

        snapshot.permission = PermissionSnapshot {
            current: "workspace-write".into(),
            presets: vec![
                preset("read-only", ""),
                preset("workspace-write", "Write inside the workspace."),
                preset("danger-full-access", ""),
            ],
            switching: None,
        };
        let text = screen(&panel, &snapshot, 100, 20);
        assert!(text.contains("‹ workspace-write ›"), "{text}");
        assert!(text.contains("Write inside the workspace."), "{text}");
        assert_eq!(
            panel.key(press(KeyCode::Right), &snapshot),
            SettingsEvent::Permission("danger-full-access".into())
        );
        assert_eq!(
            panel.key(press(KeyCode::Left), &snapshot),
            SettingsEvent::Permission("read-only".into())
        );
        // 等 dsh 确认期间显示目标，并且不再发第二次。
        snapshot.permission.switching = Some("read-only".into());
        assert!(screen(&panel, &snapshot, 100, 20).contains("‹ read-only… ›"));
        assert_eq!(
            panel.key(press(KeyCode::Right), &snapshot),
            SettingsEvent::Consumed
        );
        // 与任何预设都不匹配的会话照实显示，切换从第一个预设开始。
        snapshot.permission.switching = None;
        snapshot.permission.current = "custom".into();
        snapshot.permission.presets.push(preset("custom", ""));
        assert!(screen(&panel, &snapshot, 100, 20).contains("‹ custom ›"));
        assert_eq!(
            panel.key(press(KeyCode::Right), &snapshot),
            SettingsEvent::Permission("read-only".into())
        );

        panel.key(press(KeyCode::Down), &snapshot);
        assert_eq!(
            panel.key(press(KeyCode::Enter), &snapshot),
            SettingsEvent::Attention
        );
        assert!(!panel.visible);
        panel.open();
        assert_eq!(
            panel.key(press(KeyCode::Esc), &snapshot),
            SettingsEvent::Close
        );
        for (width, height) in [(1, 1), (3, 3), (10, 4), (24, 8)] {
            screen(&panel, &snapshot, width, height);
        }
    }
}
