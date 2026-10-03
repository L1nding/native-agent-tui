//! Read VT characters from Win32 records, retaining UTF-16 Alt-code releases.
//! ReadFile converts lone Alt-code surrogate units before they can be paired.
use std::io;
use std::time::Instant;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use windows_sys::Win32::Foundation::{
    CloseHandle, GENERIC_READ, GENERIC_WRITE, HANDLE, INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
};
use windows_sys::Win32::System::Console::{
    GetConsoleMode, GetNumberOfConsoleInputEvents, ReadConsoleInputW, SetConsoleMode,
    ENABLE_VIRTUAL_TERMINAL_INPUT, ENABLE_WINDOW_INPUT, INPUT_RECORD, KEY_EVENT, KEY_EVENT_RECORD,
    LEFT_ALT_PRESSED, LEFT_CTRL_PRESSED, RIGHT_ALT_PRESSED, RIGHT_CTRL_PRESSED, SHIFT_PRESSED,
    WINDOW_BUFFER_SIZE_EVENT,
};

use super::vt::Decoder;
use super::InputEvent;

pub(crate) struct TerminalInput {
    handle: HANDLE,
    original_mode: Option<u32>,
    decoder: Decoder,
    surrogate: Option<(u16, KeyModifiers)>,
}

impl TerminalInput {
    pub(crate) fn enter() -> io::Result<Self> {
        let name: Vec<u16> = "CONIN$\0".encode_utf16().collect();
        // SAFETY: NUL-terminated name; the owned handle is closed on every exit.
        let handle = unsafe {
            CreateFileW(
                name.as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                std::ptr::null(),
                OPEN_EXISTING,
                0,
                std::ptr::null_mut(),
            )
        };
        if handle == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }
        let mut input = Self {
            handle,
            original_mode: None,
            decoder: Decoder::default(),
            surrogate: None,
        };
        let mut mode = 0;
        // SAFETY: live console handle and valid output pointer.
        if unsafe { GetConsoleMode(handle, &mut mode) } == 0 {
            return Err(io::Error::last_os_error());
        }
        // Capture the mode after raw-mode entry; restore it before disabling raw mode.
        if unsafe {
            SetConsoleMode(
                handle,
                mode | ENABLE_VIRTUAL_TERMINAL_INPUT | ENABLE_WINDOW_INPUT,
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        input.original_mode = Some(mode);
        Ok(input)
    }

    pub(crate) fn restore(&mut self) -> io::Result<()> {
        if let Some(mode) = self.original_mode {
            // SAFETY: only this adapter owns and changes its console mode.
            if unsafe { SetConsoleMode(self.handle, mode) } == 0 {
                return Err(io::Error::last_os_error());
            }
            self.original_mode = None;
        }
        Ok(())
    }

    pub(crate) fn read_ready(&mut self) -> io::Result<Option<InputEvent>> {
        let now = Instant::now();
        if let Some(event) = self.decoder.queued() {
            return Ok(Some(event));
        }
        // A partial paste produces no event. Drain bounded batches before yielding,
        // rather than consuming only 64 records per UI redraw interval.
        for _ in 0..16 {
            let mut available = 0;
            // SAFETY: the handle is valid, and this is the only console-input reader.
            if unsafe { GetNumberOfConsoleInputEvents(self.handle, &mut available) } == 0 {
                return Err(io::Error::last_os_error());
            }
            if available == 0 {
                return Ok(self.decoder.next(now));
            }
            // Never block waiting for another record or accumulate an unbounded batch.
            let mut records: [INPUT_RECORD; 64] = unsafe { std::mem::zeroed() };
            let mut read = 0;
            if unsafe {
                ReadConsoleInputW(
                    self.handle,
                    records.as_mut_ptr(),
                    available.min(64),
                    &mut read,
                )
            } == 0
            {
                return Err(io::Error::last_os_error());
            }
            for record in &records[..read as usize] {
                match u32::from(record.EventType) {
                    // SAFETY: EventType selects the matching union member.
                    KEY_EVENT => self.key(unsafe { record.Event.KeyEvent }, now),
                    WINDOW_BUFFER_SIZE_EVENT => {
                        let (width, height) = crossterm::terminal::size()?;
                        self.decoder.resize(width, height);
                    }
                    _ => {}
                }
            }
            if let Some(event) = self.decoder.queued() {
                return Ok(Some(event));
            }
        }
        Ok(None)
    }

    fn key(&mut self, key: KEY_EVENT_RECORD, now: Instant) {
        feed_key(&mut self.decoder, &mut self.surrogate, key, now);
    }
}

fn feed_key(
    decoder: &mut Decoder,
    surrogate: &mut Option<(u16, KeyModifiers)>,
    key: KEY_EVENT_RECORD,
    now: Instant,
) {
    // SAFETY: uChar is the UTF-16 member of a console KEY_EVENT record.
    let unit = unsafe { key.uChar.UnicodeChar };
    let alt_release = key.wVirtualKeyCode == 0x12 && key.bKeyDown == 0 && unit != 0;
    if key.bKeyDown == 0 && !alt_release {
        return;
    }
    let mut modifiers = KeyModifiers::NONE;
    if key.dwControlKeyState & SHIFT_PRESSED != 0 {
        modifiers |= KeyModifiers::SHIFT;
    }
    if key.dwControlKeyState & (LEFT_ALT_PRESSED | RIGHT_ALT_PRESSED) != 0 {
        modifiers |= KeyModifiers::ALT;
    }
    if key.dwControlKeyState & (LEFT_CTRL_PRESSED | RIGHT_CTRL_PRESSED) != 0 {
        modifiers |= KeyModifiers::CONTROL;
    }
    if !alt_release {
        let code = match key.wVirtualKeyCode {
            0x08 => Some(KeyCode::Backspace),
            0x09 if modifiers.contains(KeyModifiers::SHIFT) => Some(KeyCode::BackTab),
            0x09 => Some(KeyCode::Tab),
            0x0d => Some(KeyCode::Enter),
            0x1b => Some(KeyCode::Esc),
            0x21 => Some(KeyCode::PageUp),
            0x22 => Some(KeyCode::PageDown),
            0x23 => Some(KeyCode::End),
            0x24 => Some(KeyCode::Home),
            0x25 => Some(KeyCode::Left),
            0x26 => Some(KeyCode::Up),
            0x27 => Some(KeyCode::Right),
            0x28 => Some(KeyCode::Down),
            0x2d => Some(KeyCode::Insert),
            0x2e => Some(KeyCode::Delete),
            0x70..=0x87 => Some(KeyCode::F((key.wVirtualKeyCode - 0x70 + 1) as u8)),
            0x20 if unit == 0 && modifiers.contains(KeyModifiers::CONTROL) => {
                Some(KeyCode::Char(' '))
            }
            0x41..=0x5a if unit == 0 && modifiers.contains(KeyModifiers::CONTROL) => Some(
                KeyCode::Char(char::from(key.wVirtualKeyCode as u8 - b'A' + b'a')),
            ),
            _ => None,
        };
        if let Some(code) = code {
            *surrogate = None;
            decoder.physical_key(KeyEvent::new(code, modifiers), now);
            return;
        }
    }
    if unit == 0 {
        return;
    }
    match unit {
        0xd800..=0xdbff => {
            if surrogate.replace((unit, modifiers)).is_some() {
                decoder.feed('\u{fffd}', KeyModifiers::NONE, now);
            }
        }
        0xdc00..=0xdfff => {
            let (ch, modifiers) =
                surrogate
                    .take()
                    .map_or(('\u{fffd}', modifiers), |(high, mods)| {
                        (
                            char::from_u32(
                                0x10000 + ((u32::from(high) - 0xd800) << 10) + u32::from(unit)
                                    - 0xdc00,
                            )
                            .unwrap(),
                            mods,
                        )
                    });
            decoder.feed(ch, modifiers, now);
        }
        _ => {
            if surrogate.take().is_some() {
                decoder.feed('\u{fffd}', KeyModifiers::NONE, now);
            }
            decoder.feed(char::from_u32(u32::from(unit)).unwrap(), modifiers, now);
        }
    }
}

impl Drop for TerminalInput {
    fn drop(&mut self) {
        let _ = self.restore();
        // SAFETY: owned handle, closed once after console-mode restoration.
        unsafe {
            CloseHandle(self.handle);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(vk: u16, unit: u16, down: bool, controls: u32) -> KEY_EVENT_RECORD {
        let mut key: KEY_EVENT_RECORD = unsafe { std::mem::zeroed() };
        key.wVirtualKeyCode = vk;
        key.uChar.UnicodeChar = unit;
        key.bKeyDown = i32::from(down);
        key.wRepeatCount = 1;
        key.dwControlKeyState = controls;
        key
    }

    #[test]
    fn alt_release_surrogates_survive_separate_windows_record_batches() {
        let mut decoder = Decoder::default();
        let mut surrogate = None;
        let now = Instant::now();
        for unit in "\x1b[200~中文".encode_utf16() {
            feed_key(&mut decoder, &mut surrogate, record(0, unit, true, 0), now);
        }
        feed_key(
            &mut decoder,
            &mut surrogate,
            record(0x12, 0xd83d, false, 0),
            now,
        );
        assert!(decoder.next(now).is_none());
        // Modifier releases have no character and must not clear a pending pair.
        feed_key(&mut decoder, &mut surrogate, record(0x10, 0, false, 0), now);
        feed_key(
            &mut decoder,
            &mut surrogate,
            record(0x12, 0xdc4b, false, 0),
            now,
        );
        feed_key(
            &mut decoder,
            &mut surrogate,
            record(0x41, b'a' as u16, false, 0),
            now,
        );
        for unit in "\x1b[201~".encode_utf16() {
            feed_key(&mut decoder, &mut surrogate, record(0, unit, true, 0), now);
        }
        assert_eq!(decoder.next(now), Some(InputEvent::Paste("中文👋".into())));
        assert!(decoder.next(now).is_none());
        assert!(surrogate.is_none());
    }

    #[test]
    fn windows_records_preserve_modifiers_when_the_host_supplies_them() {
        let mut decoder = Decoder::default();
        let mut surrogate = None;
        let now = Instant::now();
        for (code, controls, expected) in [
            (
                0x0d,
                SHIFT_PRESSED,
                KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT),
            ),
            (
                0x0d,
                LEFT_CTRL_PRESSED,
                KeyEvent::new(KeyCode::Enter, KeyModifiers::CONTROL),
            ),
            (
                0x24,
                RIGHT_CTRL_PRESSED | SHIFT_PRESSED,
                KeyEvent::new(KeyCode::Home, KeyModifiers::CONTROL | KeyModifiers::SHIFT),
            ),
            (0x79, 0, KeyEvent::new(KeyCode::F(10), KeyModifiers::NONE)),
        ] {
            feed_key(
                &mut decoder,
                &mut surrogate,
                record(code, 0, true, controls),
                now,
            );
            assert_eq!(decoder.next(now), Some(InputEvent::Key(expected)));
        }
    }
}
