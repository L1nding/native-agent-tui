//! `/setting`: choices that are saved and used again at the next start.

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
    /// Open the existing threshold editor; its values are saved when applied.
    Attention,
    Interrupt,
    Quit,
    Consumed,
}

/// The four choices, then the row that opens the attention thresholds.
const ROWS: usize = Setting::ALL.len() + 1;

fn label(setting: Setting) -> &'static str {
    match setting {
        Setting::Language => tr("Language", "界面语言"),
        Setting::Theme => tr("Theme", "主题"),
        Setting::Sandbox => tr("Sandbox", "沙箱模式"),
        Setting::Approval => tr("Approval", "审批模式"),
    }
}

fn detail(setting: Setting) -> &'static str {
    match setting {
        Setting::Language => tr("Applies now.", "立即生效。"),
        Setting::Theme => tr(
            "Applies now. NO_COLOR still forces mono.",
            "立即生效。设置了 NO_COLOR 时仍为单色。",
        ),
        Setting::Sandbox => tr(
            "What commands may touch. Applies from the next start; --sandbox overrides it.",
            "命令可以访问的范围。下次启动生效；--sandbox 参数优先。",
        ),
        Setting::Approval => tr(
            "When the agent must ask first. Applies from the next start; --approval overrides it.",
            "Agent 何时需要先征求同意。下次启动生效；--approval 参数优先。",
        ),
    }
}

/// The value the panel shows: what is in effect, or what was saved for next time.
fn shown(setting: Setting, snapshot: &CoreSnapshot) -> String {
    match setting {
        Setting::Language => lang::name().into(),
        Setting::Theme => theme::name().into(),
        Setting::Sandbox | Setting::Approval => snapshot
            .saved_settings
            .get(&setting)
            .cloned()
            .unwrap_or_else(|| running(setting, snapshot).to_owned()),
    }
}

fn running(setting: Setting, snapshot: &CoreSnapshot) -> &str {
    match setting {
        Setting::Sandbox => &snapshot.sandbox,
        _ => &snapshot.approval_policy,
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
        let Some(setting) = Setting::ALL.get(self.selected).copied() else {
            self.close();
            return SettingsEvent::Attention;
        };
        let values = setting.values();
        let current = shown(setting, snapshot);
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
        let label_width = Setting::ALL
            .iter()
            .map(|setting| label(*setting).width())
            .chain([tr("Attention thresholds", "提醒阈值").width()])
            .max()
            .unwrap_or(0)
            + 2;
        let mut lines = Vec::new();
        for (index, setting) in Setting::ALL.iter().copied().enumerate() {
            let marker = if index == self.selected { "> " } else { "  " };
            let name = label(setting);
            let value = shown(setting, snapshot);
            let mut spans = vec![
                Span::raw(marker),
                Span::raw(format!("{name}{}", " ".repeat(label_width - name.width()))),
                Span::styled(format!("‹ {value} ›"), theme.key()),
            ];
            if !setting.applies_immediately() && value != running(setting, snapshot) {
                spans.push(Span::styled(
                    format!(
                        "  {} {}",
                        tr("next start; now", "下次启动生效；当前"),
                        running(setting, snapshot)
                    ),
                    theme.secondary(),
                ));
            }
            lines.push(if index == self.selected {
                selected_line(spans, inner)
            } else {
                Line::from(clip_spans(spans, inner))
            });
        }
        let attention = vec![
            Span::raw(if self.selected == ROWS - 1 {
                "> "
            } else {
                "  "
            }),
            Span::raw(tr("Attention thresholds", "提醒阈值")),
            Span::styled(tr("  Enter to edit", "  按 Enter 编辑"), theme.secondary()),
        ];
        lines.push(if self.selected == ROWS - 1 {
            selected_line(attention, inner)
        } else {
            Line::from(clip_spans(attention, inner))
        });
        lines.push(Line::from(""));
        let help = Setting::ALL.get(self.selected).map_or(
            tr(
                "Quiet and reminder timing per activity; saved when applied.",
                "各类活动的静默与提醒时间；应用后保存。",
            ),
            |setting| detail(*setting),
        );
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

    #[test]
    fn rows_cycle_through_allowed_values_and_show_what_waits_for_the_next_start() {
        let mut snapshot = CoreSnapshot {
            sandbox: "workspace-write".into(),
            approval_policy: "on-request".into(),
            ..Default::default()
        };
        let mut panel = SettingsPanel::default();
        panel.open();
        let text = screen(&panel, &snapshot, 100, 20);
        for expected in [
            "Settings · /setting",
            "> Language",
            "‹ en ›",
            "‹ dark ›",
            "‹ workspace-write ›",
            "‹ on-request ›",
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
        panel.key(press(KeyCode::Down), &snapshot);
        assert_eq!(
            panel.key(press(KeyCode::Enter), &snapshot),
            SettingsEvent::Change(Setting::Sandbox, "danger-full-access")
        );
        snapshot
            .saved_settings
            .insert(Setting::Sandbox, "read-only".into());
        let text = screen(&panel, &snapshot, 100, 20);
        assert!(text.contains("‹ read-only ›"), "{text}");
        assert!(text.contains("next start; now workspace-write"), "{text}");
        assert_eq!(
            panel.key(press(KeyCode::Right), &snapshot),
            SettingsEvent::Change(Setting::Sandbox, "workspace-write")
        );

        panel.key(press(KeyCode::Down), &snapshot);
        panel.key(press(KeyCode::Down), &snapshot);
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
