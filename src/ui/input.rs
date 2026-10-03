//! Terminal input belongs to the UI; payloads never become execution facts.
use crossterm::event::KeyEvent;
#[cfg(not(windows))]
use std::io;

#[cfg(any(windows, test))]
#[path = "input/vt.rs"]
mod vt;
#[cfg(windows)]
#[path = "input/windows.rs"]
mod windows;
#[cfg(windows)]
pub(crate) use windows::TerminalInput;

pub(crate) const PASTE_BYTES: usize = 32 * 1024;

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum InputEvent {
    Key(KeyEvent),
    Paste(String),
    Resize(u16, u16),
    PasteRejected,
}

fn paste_event(text: String) -> InputEvent {
    if text.len() > PASTE_BYTES {
        return InputEvent::PasteRejected;
    }
    if !text.contains('\r') {
        return InputEvent::Paste(text);
    }
    let mut normalized = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\r' {
            if chars.peek() == Some(&'\n') {
                chars.next();
            }
            normalized.push('\n');
        } else {
            normalized.push(ch);
        }
    }
    InputEvent::Paste(normalized)
}

#[cfg(not(windows))]
pub(crate) struct TerminalInput;

#[cfg(not(windows))]
impl TerminalInput {
    pub(crate) fn enter() -> io::Result<Self> {
        Ok(Self)
    }

    pub(crate) fn read_ready(&mut self) -> io::Result<Option<InputEvent>> {
        use crossterm::event::{self, Event};
        if !event::poll(std::time::Duration::ZERO)? {
            return Ok(None);
        }
        Ok(match event::read()? {
            Event::Key(key) => Some(InputEvent::Key(key)),
            Event::Paste(text) => Some(paste_event(text)),
            Event::Resize(width, height) => Some(InputEvent::Resize(width, height)),
            _ => None,
        })
    }

    pub(crate) fn restore(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
pub(super) fn fixture_events(text: &str) -> Vec<InputEvent> {
    let mut decoder = vt::Decoder::default();
    let now = std::time::Instant::now();
    let mut events = Vec::new();
    for ch in text.chars() {
        decoder.feed(ch, crossterm::event::KeyModifiers::NONE, now);
        while let Some(event) = decoder.next(now) {
            events.push(event);
        }
    }
    events
}
