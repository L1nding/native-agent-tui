//! Process launch and lifetime. Windows assigns the job during creation.
use std::ffi::OsStr;
use std::io;
use std::path::Path;

pub(crate) struct Command(std::process::Command);

impl Command {
    pub(crate) fn new(program: impl AsRef<OsStr>) -> Self {
        Self(std::process::Command::new(program))
    }

    pub(crate) fn args<I, S>(&mut self, arguments: I) -> &mut Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        self.0.args(arguments);
        self
    }

    pub(crate) fn current_dir(&mut self, directory: impl AsRef<Path>) -> &mut Self {
        self.0.current_dir(directory);
        self
    }

    #[cfg(all(test, windows))]
    pub(crate) fn env(&mut self, key: impl AsRef<OsStr>, value: impl AsRef<OsStr>) -> &mut Self {
        self.0.env(key, value);
        self
    }

    #[cfg(all(test, windows))]
    pub(crate) fn env_remove(&mut self, key: impl AsRef<OsStr>) -> &mut Self {
        self.0.env_remove(key);
        self
    }
}

#[derive(Clone, Copy)]
pub(crate) enum Input {
    Pipe,
    Null,
}

#[cfg(windows)]
#[path = "owned_process/windows.rs"]
pub(crate) mod windows;
#[cfg(not(windows))]
pub(crate) use tokio::process::Child;
#[cfg(windows)]
pub(crate) use windows::Child;

/// Whether a process with this id may still be running. Unknown counts as
/// running, so a caller that waits for it to be gone stays on the safe side.
// The standalone native fixture includes this module and does not call it.
#[allow(dead_code)]
pub(crate) fn process_alive(pid: u32) -> bool {
    #[cfg(windows)]
    {
        windows::process_alive(pid)
    }
    #[cfg(not(windows))]
    {
        // 只有 Linux 的 /proc 能在不发信号的情况下回答；其他平台按“可能在运行”处理。
        let proc_root = Path::new("/proc");
        !proc_root.join("self").exists() || proc_root.join(pid.to_string()).exists()
    }
}

pub(crate) fn spawn(command: Command, input: Input) -> io::Result<Child> {
    #[cfg(windows)]
    {
        windows::suspend(command, input)?.resume()
    }
    #[cfg(not(windows))]
    {
        use std::process::Stdio;
        let mut command = tokio::process::Command::from(command.0);
        command
            .stdin(match input {
                Input::Pipe => Stdio::piped(),
                Input::Null => Stdio::null(),
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
    }
}
