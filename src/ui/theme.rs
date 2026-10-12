//! Semantic terminal styles. Colors express priority; labels still carry state.
use std::sync::OnceLock;

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType};

use crate::state::SessionPhase;
use crate::tool_details::ToolLifecycle;

#[derive(Clone, Copy)]
pub(super) struct Theme {
    pub background: Color,
    /// Slightly raised background for bars and chips; never the only cue.
    pub surface: Color,
    pub foreground: Color,
    pub muted: Color,
    pub border: Color,
    pub accent: Color,
    /// Second accent reserved for the assistant voice, so roles differ at a glance.
    pub agent: Color,
    pub success: Color,
    pub warning: Color,
    pub error: Color,
    pub selection: Color,
    pub monochrome: bool,
}

impl Theme {
    fn named(name: &str, no_color: bool) -> Self {
        if no_color || name == "mono" {
            return Self {
                background: Color::Reset,
                surface: Color::Reset,
                foreground: Color::Reset,
                muted: Color::Reset,
                border: Color::Reset,
                accent: Color::Reset,
                agent: Color::Reset,
                success: Color::Reset,
                warning: Color::Reset,
                error: Color::Reset,
                selection: Color::Reset,
                monochrome: true,
            };
        }
        match name {
            "light" => Self {
                background: Color::Rgb(247, 248, 250),
                surface: Color::Rgb(233, 237, 243),
                foreground: Color::Rgb(38, 49, 64),
                muted: Color::Rgb(96, 111, 130),
                border: Color::Rgb(189, 199, 211),
                accent: Color::Rgb(20, 102, 130),
                agent: Color::Rgb(104, 82, 168),
                success: Color::Rgb(40, 112, 77),
                warning: Color::Rgb(143, 94, 25),
                error: Color::Rgb(172, 57, 67),
                selection: Color::Rgb(224, 235, 242),
                monochrome: false,
            },
            "terminal" => Self {
                background: Color::Reset,
                surface: Color::Reset,
                foreground: Color::Reset,
                muted: Color::Gray,
                border: Color::DarkGray,
                accent: Color::Cyan,
                agent: Color::Magenta,
                success: Color::Green,
                warning: Color::Yellow,
                error: Color::Red,
                selection: Color::Reset,
                monochrome: false,
            },
            _ => Self {
                background: Color::Rgb(21, 25, 31),
                surface: Color::Rgb(30, 36, 45),
                foreground: Color::Rgb(218, 225, 235),
                muted: Color::Rgb(143, 157, 177),
                border: Color::Rgb(61, 74, 92),
                accent: Color::Rgb(120, 199, 214),
                agent: Color::Rgb(184, 166, 232),
                success: Color::Rgb(141, 196, 160),
                warning: Color::Rgb(224, 186, 119),
                error: Color::Rgb(231, 143, 149),
                selection: Color::Rgb(36, 53, 67),
                monochrome: false,
            },
        }
    }

    pub fn base(self) -> Style {
        Style::default().fg(self.foreground).bg(self.background)
    }

    pub fn secondary(self) -> Style {
        Style::default().fg(self.muted)
    }

    /// Separators, gutters and other chrome that should recede behind text.
    pub fn faint(self) -> Style {
        Style::default().fg(self.border)
    }

    pub fn heading(self) -> Style {
        Style::default()
            .fg(self.accent)
            .add_modifier(Modifier::BOLD)
    }

    /// Field labels inside detail panels: readable, quieter than headings.
    pub fn label(self) -> Style {
        Style::default()
            .fg(self.foreground)
            .add_modifier(Modifier::BOLD)
    }

    /// A shortcut key in a hint row.
    pub fn key(self) -> Style {
        self.heading()
    }

    pub fn selected(self) -> Style {
        let style = self.heading().bg(self.selection);
        if self.monochrome || self.selection == Color::Reset {
            style.add_modifier(Modifier::REVERSED)
        } else {
            style
        }
    }

    pub fn phase_color(self, phase: SessionPhase) -> Color {
        match phase {
            SessionPhase::Failed | SessionPhase::Disconnected => self.error,
            SessionPhase::Unknown | SessionPhase::GatePending | SessionPhase::Interrupted => {
                self.warning
            }
            SessionPhase::Ready | SessionPhase::Completed => self.success,
            SessionPhase::Stopped | SessionPhase::Stopping | SessionPhase::ClosingTransport => {
                self.muted
            }
            _ => self.accent,
        }
    }

    pub fn phase(self, phase: SessionPhase) -> Style {
        Style::default()
            .fg(self.phase_color(phase))
            .add_modifier(Modifier::BOLD)
    }

    /// Tool state color; the state word itself must stay in the text.
    pub fn lifecycle(self, lifecycle: ToolLifecycle) -> Style {
        let color = match lifecycle {
            ToolLifecycle::Running => self.accent,
            ToolLifecycle::Completed => self.success,
            ToolLifecycle::Failed | ToolLifecycle::Interrupted => self.error,
            ToolLifecycle::EndedUnknown | ToolLifecycle::Unknown => self.warning,
        };
        Style::default().fg(color).add_modifier(Modifier::BOLD)
    }
}

const NAMES: [&str; 4] = ["dark", "light", "terminal", "mono"];

// 测试并行运行；切换主题只影响当前测试线程。
#[cfg(test)]
thread_local! {
    static CHOSEN: std::cell::Cell<Option<usize>> = const { std::cell::Cell::new(None) };
}
#[cfg(not(test))]
static CHOSEN: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(usize::MAX);

fn chosen() -> Option<usize> {
    #[cfg(test)]
    {
        CHOSEN.with(std::cell::Cell::get)
    }
    #[cfg(not(test))]
    {
        Some(CHOSEN.load(std::sync::atomic::Ordering::Relaxed)).filter(|index| *index < NAMES.len())
    }
}

/// The theme named by `NATIVE_AGENT_TUI_THEME`, when it is set to a known one.
pub(super) fn from_environment() -> Option<&'static str> {
    static NAME: OnceLock<Option<&'static str>> = OnceLock::new();
    *NAME.get_or_init(|| {
        let value = std::env::var("NATIVE_AGENT_TUI_THEME").ok()?;
        NAMES.iter().copied().find(|name| *name == value)
    })
}

/// The current theme as a `Setting::Theme` value.
pub(super) fn name() -> &'static str {
    chosen()
        .map(|index| NAMES[index])
        .or_else(from_environment)
        .unwrap_or("dark")
}

/// Switches the theme for the rest of this run; unknown names are ignored.
pub(super) fn set(value: &str) {
    let Some(index) = NAMES.iter().position(|name| *name == value) else {
        return;
    };
    #[cfg(test)]
    CHOSEN.with(|chosen| chosen.set(Some(index)));
    #[cfg(not(test))]
    CHOSEN.store(index, std::sync::atomic::Ordering::Relaxed);
}

pub(super) fn current() -> Theme {
    static NO_COLOR: OnceLock<bool> = OnceLock::new();
    let no_color = *NO_COLOR
        .get_or_init(|| std::env::var_os("NO_COLOR").is_some_and(|value| !value.is_empty()));
    Theme::named(name(), no_color)
}

/// A shared frame style; callers choose which edges they actually need.
pub(super) fn panel<'a>() -> Block<'a> {
    let theme = current();
    Block::default()
        .style(theme.base())
        .border_style(Style::default().fg(theme.border))
        .title_style(theme.secondary())
}

pub(super) fn focus_panel<'a>() -> Block<'a> {
    panel()
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(current().accent))
        .title_style(current().heading())
}

/// One hint row written as `Key label | Key label`. Keys are emphasized and
/// labels stay quiet; only the ` | ` separators are redrawn.
pub(super) fn hints(text: &str) -> Line<'static> {
    let theme = current();
    let mut spans = Vec::new();
    for (index, hint) in text.split(" | ").enumerate() {
        if index > 0 {
            spans.push(Span::styled("  ·  ", theme.faint()));
        }
        if let Some((key, label)) = hint.split_once(' ') {
            spans.push(Span::styled(key.to_owned(), theme.key()));
            spans.push(Span::styled(format!(" {label}"), theme.secondary()));
        } else {
            spans.push(Span::styled(hint.to_owned(), theme.secondary()));
        }
    }
    Line::from(spans)
}
