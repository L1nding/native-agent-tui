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

    #[cfg(test)]
    pub(crate) fn env(&mut self, key: impl AsRef<OsStr>, value: impl AsRef<OsStr>) -> &mut Self {
        self.0.env(key, value);
        self
    }

    #[cfg(test)]
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
