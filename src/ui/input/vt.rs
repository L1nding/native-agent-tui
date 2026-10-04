//! Bounded VT framing. A paste body cannot be interpreted as keyboard commands.
use std::collections::VecDeque;
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::{InputEvent, PASTE_BYTES};

const ESCAPE_DELAY: Duration = Duration::from_millis(50);
const SEQUENCE_BYTES: usize = 64;
const PASTE_END: &[u8] = b"\x1b[201~";

#[derive(Default)]
pub(super) struct Decoder {
    state: State,
    events: VecDeque<InputEvent>,
}

#[derive(Default)]
enum State {
    #[default]
    Ground,
    Escape {
        text: String,
        since: Instant,
        overflow: bool,
    },
    Paste(Paste),
}

#[derive(Default)]
struct Paste {
    text: String,
    end_match: usize,
    overflow: bool,
}

impl Paste {
    fn append(&mut self, ch: char) {
        if self.overflow {
            return;
        }
        if self.text.len() + ch.len_utf8() > PASTE_BYTES {
            self.text.clear();
            self.overflow = true;
        } else {
            self.text.push(ch);
        }
    }

    fn feed(&mut self, ch: char) -> bool {
        if ch == char::from(PASTE_END[self.end_match]) {
            self.end_match += 1;
            return self.end_match == PASTE_END.len();
        }
        for byte in &PASTE_END[..self.end_match] {
            self.append(char::from(*byte));
        }
        self.end_match = usize::from(ch == '\x1b');
        if self.end_match == 0 {
            self.append(ch);
        }
        false
    }
}

impl Decoder {
    pub(super) fn queued(&mut self) -> Option<InputEvent> {
        self.events.pop_front()
    }

    pub(super) fn next(&mut self, now: Instant) -> Option<InputEvent> {
        if matches!(&self.state, State::Escape { text, since, .. }
            if text.is_empty() && now.saturating_duration_since(*since) >= ESCAPE_DELAY)
        {
            self.state = State::Ground;
            self.events.push_back(InputEvent::Key(KeyEvent::new(
                KeyCode::Esc,
                KeyModifiers::NONE,
            )));
        }
        self.queued()
    }

    #[cfg(windows)]
    pub(super) fn resize(&mut self, width: u16, height: u16) {
        self.events.push_back(InputEvent::Resize(width, height));
    }

    /// Physical Win32 keys retain modifiers, including Shift/Ctrl+Enter.
    /// During a framed paste their character is part of the body.
    pub(super) fn physical_key(&mut self, key: KeyEvent, now: Instant) {
        if matches!(self.state, State::Paste(_)) {
            let ch = match key.code {
                KeyCode::Enter => Some('\r'),
                KeyCode::Tab | KeyCode::BackTab => Some('\t'),
                KeyCode::Backspace => Some('\x08'),
                KeyCode::Esc => Some('\x1b'),
                KeyCode::Char(ch) => Some(ch),
                _ => None,
            };
            if let Some(ch) = ch {
                self.feed(ch, KeyModifiers::NONE, now);
            }
            return;
        }
        if matches!(&self.state, State::Escape { text, .. } if text.is_empty()) {
            self.events.push_back(InputEvent::Key(KeyEvent::new(
                KeyCode::Esc,
                KeyModifiers::NONE,
            )));
        }
        self.state = State::Ground;
        self.events.push_back(InputEvent::Key(key));
    }

    pub(super) fn feed(&mut self, ch: char, modifiers: KeyModifiers, now: Instant) {
        let state = std::mem::take(&mut self.state);
        self.state = match state {
            State::Ground if ch == '\x1b' => State::Escape {
                text: String::new(),
                since: now,
                overflow: false,
            },
            State::Ground => {
                self.events
                    .push_back(InputEvent::Key(character_key(ch, modifiers)));
                State::Ground
            }
            State::Paste(mut paste) => {
                if paste.feed(ch) {
                    self.events.push_back(if paste.overflow {
                        InputEvent::PasteRejected
                    } else {
                        super::paste_event(paste.text)
                    });
                    State::Ground
                } else {
                    State::Paste(paste)
                }
            }
            State::Escape {
                mut text,
                since,
                mut overflow,
            } => {
                if ch == '\x1b' {
                    if text.is_empty() {
                        self.events.push_back(InputEvent::Key(KeyEvent::new(
                            KeyCode::Esc,
                            KeyModifiers::NONE,
                        )));
                    }
                    State::Escape {
                        text: String::new(),
                        since: now,
                        overflow: false,
                    }
                } else if text.is_empty() && !matches!(ch, '[' | 'O') {
                    self.events.push_back(InputEvent::Key(character_key(
                        ch,
                        modifiers | KeyModifiers::ALT,
                    )));
                    State::Ground
                } else {
                    if text.len() + ch.len_utf8() <= SEQUENCE_BYTES {
                        text.push(ch);
                    } else {
                        overflow = true;
                    }
                    let finished = text.len() > 1 && ('\x40'..='\x7e').contains(&ch);
                    if finished {
                        if !overflow && text == "[200~" {
                            State::Paste(Paste::default())
                        } else {
                            if !overflow {
                                if let Some(key) = sequence_key(&text) {
                                    self.events.push_back(InputEvent::Key(key));
                                }
                            }
                            State::Ground
                        }
                    } else {
                        State::Escape {
                            text,
                            since,
                            overflow,
                        }
                    }
                }
            }
        };
    }
}

fn character_key(ch: char, mut modifiers: KeyModifiers) -> KeyEvent {
    let code = match ch {
        '\r' | '\n' => KeyCode::Enter,
        '\t' if modifiers.contains(KeyModifiers::SHIFT) => KeyCode::BackTab,
        '\t' => KeyCode::Tab,
        '\x08' | '\x7f' => KeyCode::Backspace,
        '\x00' => {
            modifiers |= KeyModifiers::CONTROL;
            KeyCode::Char(' ')
        }
        '\x01'..='\x1a' => {
            modifiers |= KeyModifiers::CONTROL;
            KeyCode::Char(char::from(b'a' + ch as u8 - 1))
        }
        '\x1c'..='\x1f' => {
            modifiers |= KeyModifiers::CONTROL;
            KeyCode::Char(char::from(b'\\' + ch as u8 - 0x1c))
        }
        _ => KeyCode::Char(ch),
    };
    KeyEvent::new(code, modifiers)
}

fn sequence_key(text: &str) -> Option<KeyEvent> {
    let body = text.strip_prefix('[').or_else(|| text.strip_prefix('O'))?;
    let final_byte = body.chars().last()?;
    let parameters = &body[..body.len() - final_byte.len_utf8()];
    let mut fields = parameters.split(';');
    let first: u32 = match fields.next()? {
        "" => 1,
        field => field.parse().ok()?,
    };
    let modifier: u8 = match fields.next() {
        None => 1,
        Some(field) => field.parse().ok()?,
    };
    if !(1..=8).contains(&modifier) {
        return None;
    }
    let mut modifiers = KeyModifiers::NONE;
    if (modifier - 1) & 1 != 0 {
        modifiers |= KeyModifiers::SHIFT;
    }
    if (modifier - 1) & 2 != 0 {
        modifiers |= KeyModifiers::ALT;
    }
    if (modifier - 1) & 4 != 0 {
        modifiers |= KeyModifiers::CONTROL;
    }
    if final_byte == '~' && first == 27 {
        let value = char::from_u32(fields.next()?.parse().ok()?)?;
        if fields.next().is_some() {
            return None;
        }
        return Some(character_key(value, modifiers));
    }
    if fields.next().is_some() {
        return None;
    }
    let code = match final_byte {
        'A' if first == 1 => KeyCode::Up,
        'B' if first == 1 => KeyCode::Down,
        'C' if first == 1 => KeyCode::Right,
        'D' if first == 1 => KeyCode::Left,
        'H' if first == 1 => KeyCode::Home,
        'F' if first == 1 => KeyCode::End,
        'Z' if first == 1 => {
            modifiers |= KeyModifiers::SHIFT;
            KeyCode::BackTab
        }
        'P'..='S' if first == 1 => KeyCode::F(final_byte as u8 - b'P' + 1),
        '~' => match first {
            1 | 7 => KeyCode::Home,
            2 => KeyCode::Insert,
            3 => KeyCode::Delete,
            4 | 8 => KeyCode::End,
            5 => KeyCode::PageUp,
            6 => KeyCode::PageDown,
            11..=15 => KeyCode::F((first - 10) as u8),
            17..=21 => KeyCode::F((first - 11) as u8),
            23..=26 => KeyCode::F((first - 12) as u8),
            28..=29 => KeyCode::F((first - 13) as u8),
            31..=34 => KeyCode::F((first - 14) as u8),
            _ => return None,
        },
        'u' => return Some(character_key(char::from_u32(first)?, modifiers)),
        _ => return None,
    };
    Some(KeyEvent::new(code, modifiers))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feed(decoder: &mut Decoder, text: &str, now: Instant) {
        for ch in text.chars() {
            decoder.feed(ch, KeyModifiers::NONE, now);
        }
    }

    fn drain(decoder: &mut Decoder, now: Instant) -> Vec<InputEvent> {
        std::iter::from_fn(|| decoder.next(now)).collect()
    }

    #[test]
    fn fragmented_unicode_paste_delivers_one_body_and_no_shortcut_keys() {
        let body = "FIRST中文👋\nSECOND\t\x03\x19\x0e\x11\x1bOP";
        let frame = format!("\x1b[200~{body}\x1b[201~");
        for (split, _) in frame.char_indices() {
            let mut decoder = Decoder::default();
            let now = Instant::now();
            feed(&mut decoder, &frame[..split], now);
            assert!(drain(&mut decoder, now).is_empty());
            feed(&mut decoder, &frame[split..], now);
            assert_eq!(
                drain(&mut decoder, now),
                vec![InputEvent::Paste(body.into())]
            );
        }
    }

    #[test]
    fn oversized_paste_discards_the_entire_body_until_the_closing_boundary() {
        let mut decoder = Decoder::default();
        let now = Instant::now();
        feed(
            &mut decoder,
            &format!(
                "\x1b[200~{}\x11\x03\x19\n",
                "中".repeat(PASTE_BYTES / 3 + 1)
            ),
            now,
        );
        assert!(drain(&mut decoder, now).is_empty());
        assert!(
            matches!(&decoder.state, State::Paste(paste) if paste.overflow && paste.text.is_empty())
        );
        feed(&mut decoder, "\x1b[201~\x06\x11", now);
        assert_eq!(
            drain(&mut decoder, now),
            vec![
                InputEvent::PasteRejected,
                InputEvent::Key(KeyEvent::new(KeyCode::Char('f'), KeyModifiers::CONTROL)),
                InputEvent::Key(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::CONTROL))
            ]
        );
    }

    #[test]
    fn paste_byte_limit_excludes_the_closing_delimiter_and_failed_prefixes_are_body() {
        let mut decoder = Decoder::default();
        let now = Instant::now();
        let body = format!("\x1b[201X{}", "a".repeat(PASTE_BYTES - 6));
        feed(&mut decoder, &format!("\x1b[200~{body}\x1b[201~"), now);
        assert_eq!(drain(&mut decoder, now), vec![InputEvent::Paste(body)]);
    }

    #[test]
    fn incomplete_paste_and_partial_csi_never_expire_into_shortcut_keys() {
        let mut decoder = Decoder::default();
        let now = Instant::now();
        feed(&mut decoder, "\x1b[20", now);
        assert!(drain(&mut decoder, now + Duration::from_secs(60)).is_empty());
        feed(
            &mut decoder,
            "0~secret\x11\x03\x19\n",
            now + Duration::from_secs(60),
        );
        assert!(drain(&mut decoder, now + Duration::from_secs(120)).is_empty());
        feed(&mut decoder, "\x1b[201~", now + Duration::from_secs(120));
        assert_eq!(
            drain(&mut decoder, now + Duration::from_secs(120)),
            vec![InputEvent::Paste("secret\x11\x03\x19\n".into())]
        );
    }

    #[test]
    fn navigation_function_keys_and_modified_enter_are_preserved() {
        let mut decoder = Decoder::default();
        let now = Instant::now();
        feed(
            &mut decoder,
            "\x06\x1b[A\x1b[1;5H\x1b[1;5F\x1b[Z\x1bOP\x1b[24~\x1b[27;2;13~\x1b[13;5u",
            now,
        );
        let key = |code, mods| InputEvent::Key(KeyEvent::new(code, mods));
        assert_eq!(
            drain(&mut decoder, now),
            vec![
                key(KeyCode::Char('f'), KeyModifiers::CONTROL),
                key(KeyCode::Up, KeyModifiers::NONE),
                key(KeyCode::Home, KeyModifiers::CONTROL),
                key(KeyCode::End, KeyModifiers::CONTROL),
                key(KeyCode::BackTab, KeyModifiers::SHIFT),
                key(KeyCode::F(1), KeyModifiers::NONE),
                key(KeyCode::F(12), KeyModifiers::NONE),
                key(KeyCode::Enter, KeyModifiers::SHIFT),
                key(KeyCode::Enter, KeyModifiers::CONTROL)
            ]
        );
    }

    #[test]
    fn standalone_escape_and_alt_unicode_work_without_replaying_unknown_sequences() {
        let mut decoder = Decoder::default();
        let now = Instant::now();
        feed(&mut decoder, "\x1b", now);
        assert!(decoder.next(now).is_none());
        assert_eq!(
            decoder.next(now + ESCAPE_DELAY),
            Some(InputEvent::Key(KeyEvent::new(
                KeyCode::Esc,
                KeyModifiers::NONE
            )))
        );
        feed(&mut decoder, "\x1b中\x1b[<0;1;2M\x1b[?25h\x1b[201~", now);
        assert_eq!(
            drain(&mut decoder, now),
            vec![InputEvent::Key(KeyEvent::new(
                KeyCode::Char('中'),
                KeyModifiers::ALT
            ))]
        );
        feed(&mut decoder, &format!("\x1b[{}", "1;".repeat(4096)), now);
        assert!(
            matches!(&decoder.state, State::Escape { text, overflow: true, .. } if text.len() <= SEQUENCE_BYTES)
        );
        feed(&mut decoder, "A\x11", now);
        assert_eq!(
            drain(&mut decoder, now),
            vec![InputEvent::Key(KeyEvent::new(
                KeyCode::Char('q'),
                KeyModifiers::CONTROL
            ))]
        );
    }

    #[test]
    fn physical_keys_inside_a_paste_cannot_escape_into_commands() {
        let mut decoder = Decoder::default();
        let now = Instant::now();
        feed(&mut decoder, "\x1b[200~body", now);
        decoder.physical_key(KeyEvent::new(KeyCode::F(2), KeyModifiers::NONE), now);
        decoder.physical_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::CONTROL), now);
        feed(&mut decoder, "\x1b[201~", now);
        assert_eq!(
            drain(&mut decoder, now),
            vec![InputEvent::Paste("body\n".into())]
        );
        decoder.physical_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT), now);
        assert_eq!(
            decoder.next(now),
            Some(InputEvent::Key(KeyEvent::new(
                KeyCode::Enter,
                KeyModifiers::SHIFT
            )))
        );
    }

    #[test]
    fn windows_and_unix_line_endings_become_paste_text_without_enter_keys() {
        let mut decoder = Decoder::default();
        let now = Instant::now();
        feed(&mut decoder, "\x1b[200~a\r\nb\rc\nd\x1b[201~", now);
        assert_eq!(
            drain(&mut decoder, now),
            vec![InputEvent::Paste("a\nb\nc\nd".into())]
        );
    }
}
