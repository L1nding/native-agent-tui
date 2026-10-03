//! Native ConPTY regression fixture using the production terminal input adapter.
//! Only fixed-fixture verdicts and counters are recorded; never input contents.
#[cfg(not(test))]
#[path = "../src/ui/input.rs"]
mod input;

#[cfg(not(test))]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use std::io::{self, Write};
    use std::time::{Duration, Instant};

    use crossterm::event::{
        DisableBracketedPaste, EnableBracketedPaste, KeyCode, KeyEvent, KeyEventKind, KeyModifiers,
    };
    use crossterm::{execute, terminal};
    use input::{InputEvent, TerminalInput};

    #[cfg(windows)]
    let (console_input, console_output) = attach_fixture_handles()?;
    #[cfg(windows)]
    let original_mode = console_mode(console_input)?;
    let record =
        std::env::var_os("NATIVE_TERMINAL_FIXTURE_RECORD").ok_or("missing fixture record")?;
    terminal::enable_raw_mode()?;
    let result = (|| -> Result<_, Box<dyn std::error::Error>> {
        let mut input = TerminalInput::enter()?;
        execute!(io::stdout(), EnableBracketedPaste)?;
        println!("INPUT_FIXTURE_READY");
        io::stdout().flush()?;
        let deadline = Instant::now() + Duration::from_secs(3);
        let mut paste_events = 0;
        let mut key_events = 0;
        let mut enter_events = 0;
        let mut rejected_events = 0;
        let mut exact_paste = false;
        let mut control_paste = false;
        let mut limit_paste = false;
        let mut keys = Vec::new();
        let mut quit = false;
        while Instant::now() < deadline && !quit {
            match input.read_ready()? {
                Some(InputEvent::Paste(text)) => {
                    paste_events += 1;
                    exact_paste |= text == "FIRST中文👋\nSECOND";
                    control_paste |= text == "FIRST中文👋\nSECOND\x03\x19\x0e\x11\x1bOP";
                    limit_paste |= text == format!("{}ab", "中".repeat(input::PASTE_BYTES / 3));
                }
                Some(InputEvent::PasteRejected) => rejected_events += 1,
                Some(InputEvent::Key(key)) if key.kind != KeyEventKind::Release => {
                    key_events += 1;
                    enter_events += usize::from(key.code == KeyCode::Enter);
                    if keys.len() < 64 {
                        keys.push(key);
                    }
                    quit = key.code == KeyCode::Char('q')
                        && key.modifiers.contains(KeyModifiers::CONTROL);
                }
                _ => std::thread::sleep(Duration::from_millis(2)),
            }
        }
        input.restore()?;
        let expected = [
            (KeyCode::Char('f'), KeyModifiers::CONTROL),
            (KeyCode::F(1), KeyModifiers::NONE),
            (KeyCode::F(2), KeyModifiers::NONE),
            (KeyCode::F(3), KeyModifiers::NONE),
            (KeyCode::F(4), KeyModifiers::NONE),
            (KeyCode::F(5), KeyModifiers::NONE),
            (KeyCode::F(6), KeyModifiers::NONE),
            (KeyCode::F(7), KeyModifiers::NONE),
            (KeyCode::F(8), KeyModifiers::NONE),
            (KeyCode::F(9), KeyModifiers::NONE),
            (KeyCode::F(10), KeyModifiers::NONE),
            (KeyCode::F(11), KeyModifiers::NONE),
            (KeyCode::F(12), KeyModifiers::NONE),
            (KeyCode::Home, KeyModifiers::CONTROL),
            (KeyCode::End, KeyModifiers::CONTROL),
            (KeyCode::Up, KeyModifiers::NONE),
            (KeyCode::Down, KeyModifiers::NONE),
            (KeyCode::Left, KeyModifiers::NONE),
            (KeyCode::Right, KeyModifiers::NONE),
            (KeyCode::PageUp, KeyModifiers::NONE),
            (KeyCode::PageDown, KeyModifiers::NONE),
            (KeyCode::Home, KeyModifiers::NONE),
            (KeyCode::End, KeyModifiers::NONE),
            (KeyCode::Delete, KeyModifiers::NONE),
            (KeyCode::Backspace, KeyModifiers::NONE),
            (KeyCode::Tab, KeyModifiers::NONE),
            (KeyCode::BackTab, KeyModifiers::SHIFT),
            (KeyCode::Enter, KeyModifiers::SHIFT),
            (KeyCode::Enter, KeyModifiers::CONTROL),
            (KeyCode::Char('q'), KeyModifiers::CONTROL),
        ]
        .into_iter()
        .map(|(code, modifiers)| KeyEvent::new(code, modifiers))
        .collect::<Vec<_>>();
        let physical = [
            KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT),
            KeyEvent::new(KeyCode::Enter, KeyModifiers::CONTROL),
            KeyEvent::new(KeyCode::F(2), KeyModifiers::NONE),
            KeyEvent::new(KeyCode::Char('q'), KeyModifiers::CONTROL),
        ];
        let plain_physical = [
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            physical[2],
            physical[3],
        ];
        let portable = [
            KeyEvent::new(KeyCode::Char('o'), KeyModifiers::CONTROL),
            KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL),
            physical[3],
        ];
        Ok(
            serde_json::json!({"paste_events":paste_events,"key_events":key_events,
            "enter_events":enter_events,"rejected_events":rejected_events,
            "exact_paste":exact_paste,"quit":quit,"control_paste":control_paste,
            "limit_paste":limit_paste,"keys_match":keys==expected,"physical_keys_match":keys==physical,
            "physical_records_decoded":keys==physical || keys==plain_physical,
            "portable_keys_match":keys==portable,
            "physical_shift_enter":keys.first()==Some(&physical[0]),
            "physical_ctrl_enter":keys.get(1)==Some(&physical[1]),
            "physical_f2":keys.get(2)==Some(&physical[2]),
            "plain_enter_count":keys.iter().filter(|key|key.code==KeyCode::Enter && key.modifiers==KeyModifiers::NONE).count()}),
        )
    })();
    let _ = execute!(io::stdout(), DisableBracketedPaste);
    let _ = terminal::disable_raw_mode();
    let report = result?;
    #[cfg(windows)]
    let report = {
        let mut report = report;
        report["mode_restored"] =
            serde_json::Value::Bool(console_mode(console_input)? == original_mode);
        report
    };
    std::fs::write(record, serde_json::to_vec(&report)?)?;
    println!("INPUT_FIXTURE_DONE");
    #[cfg(windows)]
    unsafe {
        windows_sys::Win32::Foundation::CloseHandle(console_input);
        windows_sys::Win32::Foundation::CloseHandle(console_output);
    }
    Ok(())
}

#[cfg(all(windows, not(test)))]
fn console_mode(handle: windows_sys::Win32::Foundation::HANDLE) -> std::io::Result<u32> {
    let mut mode = 0;
    if unsafe { windows_sys::Win32::System::Console::GetConsoleMode(handle, &mut mode) } == 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(mode)
}

// A direct ConPTY child may inherit the parent's redirected standard handles.
// Bind this fixture's standard handles to its own attached console.
#[cfg(all(windows, not(test)))]
fn attach_fixture_handles() -> std::io::Result<(
    windows_sys::Win32::Foundation::HANDLE,
    windows_sys::Win32::Foundation::HANDLE,
)> {
    use windows_sys::Win32::Foundation::{
        CloseHandle, GENERIC_READ, GENERIC_WRITE, INVALID_HANDLE_VALUE,
    };
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
    };
    use windows_sys::Win32::System::Console::{SetStdHandle, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE};
    unsafe {
        let input: Vec<u16> = "CONIN$\0".encode_utf16().collect();
        let output: Vec<u16> = "CONOUT$\0".encode_utf16().collect();
        let ih = CreateFileW(
            input.as_ptr(),
            GENERIC_READ | GENERIC_WRITE,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            std::ptr::null(),
            OPEN_EXISTING,
            0,
            std::ptr::null_mut(),
        );
        if ih == INVALID_HANDLE_VALUE {
            return Err(std::io::Error::last_os_error());
        }
        let oh = CreateFileW(
            output.as_ptr(),
            GENERIC_READ | GENERIC_WRITE,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            std::ptr::null(),
            OPEN_EXISTING,
            0,
            std::ptr::null_mut(),
        );
        if oh == INVALID_HANDLE_VALUE {
            let error = std::io::Error::last_os_error();
            CloseHandle(ih);
            return Err(error);
        }
        if SetStdHandle(STD_INPUT_HANDLE, ih) == 0 || SetStdHandle(STD_OUTPUT_HANDLE, oh) == 0 {
            let error = std::io::Error::last_os_error();
            CloseHandle(ih);
            CloseHandle(oh);
            return Err(error);
        }
        Ok((ih, oh))
    }
}

#[cfg(test)]
fn main() {}
