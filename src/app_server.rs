#[cfg(windows)]
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use serde_json::{json, Value};
use thiserror::Error;
use tokio::io::AsyncReadExt;
use tokio::process::{Child, Command};
use tokio::task::JoinHandle;

use crate::config::Config;
use crate::protocol::{Envelope, RpcId};
use crate::transport::{PipeTransport, TransportError};

#[derive(Debug, Error)]
pub enum AppServerError {
    #[error("could not launch app-server: {0}")]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Transport(#[from] TransportError),
    #[error("invalid working directory: {0}")]
    InvalidDirectory(String),
}

/// Owns the process, stderr drain, reader and single writer for their entire lifetime.
pub(crate) struct AppServer {
    pub pipe: Option<PipeTransport>,
    child: Child,
    stderr: JoinHandle<()>,
    #[cfg(windows)]
    job: Option<WindowsJob>,
}

impl AppServer {
    pub fn spawn(config: &Config) -> Result<Self, AppServerError> {
        let mut command = Command::new(&config.executable);
        #[cfg(windows)]
        if let Some(mode) = &config.windows_sandbox {
            command.args(["-c", &format!("windows.sandbox=\"{mode}\"")]);
        }
        command
            .args(["app-server", "--strict-config", "--listen", "stdio://"])
            .current_dir(&config.cwd);
        Self::spawn_command(command)
    }

    pub(crate) fn spawn_command(mut command: Command) -> Result<Self, AppServerError> {
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        #[cfg(windows)]
        command.creation_flags(0x08000000); // CREATE_NO_WINDOW: no console helper windows.
        let mut child = command.spawn()?;
        #[cfg(windows)]
        let job = match WindowsJob::attach(&child) {
            Ok(job) => Some(job),
            Err(error) => {
                let _ = child.start_kill();
                return Err(error.into());
            }
        };
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| std::io::Error::other("missing app-server stdout"))?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| std::io::Error::other("missing app-server stdin"))?;
        let mut stderr = child
            .stderr
            .take()
            .ok_or_else(|| std::io::Error::other("missing app-server stderr"))?;
        let stderr = tokio::spawn(async move {
            // Drain in fixed chunks; an unbounded stderr line cannot consume memory.
            let mut buffer = [0; 8192];
            while matches!(stderr.read(&mut buffer).await, Ok(n) if n > 0) {}
        });
        Ok(Self {
            pipe: Some(PipeTransport::new(stdout, stdin)),
            child,
            stderr,
            #[cfg(windows)]
            job,
        })
    }

    pub async fn shutdown(&mut self) -> Result<(), AppServerError> {
        if let Some(pipe) = &mut self.pipe {
            pipe.close_writer().await;
        }
        let exited = tokio::time::timeout(Duration::from_secs(2), self.child.wait()).await;
        #[cfg(windows)]
        self.job.take(); // Closing the job terminates any surviving descendants too.
        match exited {
            Ok(status) => {
                status?;
            }
            Err(_) => {
                self.child.kill().await?;
            }
        }
        self.stderr.abort();
        let _ = (&mut self.stderr).await;
        Ok(())
    }
}

impl Drop for AppServer {
    fn drop(&mut self) {
        #[cfg(windows)]
        self.job.take();
        let _ = self.child.start_kill();
        self.stderr.abort();
    }
}

#[cfg(windows)]
struct WindowsJob(windows_sys::Win32::Foundation::HANDLE);

// SAFETY: the exclusively owned job handle is only used by its owner and closed once.
#[cfg(windows)]
unsafe impl Send for WindowsJob {}

#[cfg(windows)]
impl WindowsJob {
    fn attach(child: &Child) -> std::io::Result<Self> {
        use windows_sys::Win32::System::JobObjects::*;
        // SAFETY: calls use initialized structs, correct buffer lengths and live process handles.
        unsafe {
            let handle = CreateJobObjectW(std::ptr::null(), std::ptr::null());
            if handle.is_null() {
                return Err(std::io::Error::last_os_error());
            }
            let job = Self(handle);
            let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
            info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            if SetInformationJobObject(
                handle,
                JobObjectExtendedLimitInformation,
                &info as *const _ as *const _,
                std::mem::size_of_val(&info) as u32,
            ) == 0
            {
                return Err(std::io::Error::last_os_error());
            }
            let process = child
                .raw_handle()
                .ok_or_else(|| std::io::Error::other("missing process handle"))?;
            if AssignProcessToJobObject(handle, process as _) == 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(job)
        }
    }
}

#[cfg(windows)]
impl Drop for WindowsJob {
    fn drop(&mut self) {
        // SAFETY: this unique live handle is closed exactly once.
        unsafe {
            windows_sys::Win32::Foundation::CloseHandle(self.0);
        }
    }
}

pub fn normalize_config(mut config: Config) -> Result<Config, AppServerError> {
    config.cwd = config.cwd.canonicalize()?;
    if !config.cwd.is_dir() {
        return Err(AppServerError::InvalidDirectory(
            config.cwd.display().to_string(),
        ));
    }
    // Strip the Win32 verbatim prefix for shell/server compatibility.
    #[cfg(windows)]
    {
        let text = config.cwd.to_string_lossy();
        config.cwd = if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
            PathBuf::from(format!(r"\\{rest}"))
        } else if let Some(rest) = text.strip_prefix(r"\\?\") {
            PathBuf::from(rest)
        } else {
            config.cwd.clone()
        };
    }
    Ok(config)
}

pub(crate) fn initialize(id: RpcId) -> Envelope {
    Envelope::request(
        id,
        "initialize",
        Some(json!({
            "clientInfo":{"name":"native-agent-tui","title":"Native Agent TUI","version":env!("CARGO_PKG_VERSION")},
            "capabilities":{"experimentalApi":true}
        })),
    )
}

pub(crate) fn thread_start(id: RpcId, config: &Config) -> Envelope {
    let mut params = json!({
        "cwd":config.cwd.to_string_lossy(),
        "approvalPolicy":config.approval_policy,
        "sandbox":config.sandbox,
        "allowProviderModelFallback":false,
        "experimentalRawEvents":false
    });
    if let Some(model) = &config.model {
        params["model"] = Value::String(model.clone());
    }
    Envelope::request(id, "thread/start", Some(params))
}

pub(crate) fn turn_start(id: RpcId, thread: &str, text: &str) -> Envelope {
    Envelope::request(
        id,
        "turn/start",
        Some(json!({
            "threadId":thread, "input":[{"type":"text","text":text}]
        })),
    )
}

pub(crate) fn interrupt(id: RpcId, thread: &str, turn: &str) -> Envelope {
    Envelope::request(
        id,
        "turn/interrupt",
        Some(json!({"threadId":thread,"turnId":turn})),
    )
}

pub(crate) fn preflight(id: RpcId, config: &Config) -> Envelope {
    let command: Vec<&str> = if cfg!(windows) {
        vec!["pwsh", "-NoProfile", "-NonInteractive", "-Command", "if (-not (Test-Path -LiteralPath . -PathType Container)) { exit 1 }; Write-Output native-agent-tui-shell-ok"]
    } else {
        vec!["sh", "-c", "test -d . && printf native-agent-tui-shell-ok"]
    };
    let sandbox = match config.sandbox.as_str() {
        "read-only" => json!({"type":"readOnly"}),
        "workspace-write" => {
            json!({"type":"workspaceWrite","writableRoots":[config.cwd.to_string_lossy()]})
        }
        "danger-full-access" => json!({"type":"dangerFullAccess"}),
        _ => unreachable!("CLI validates sandbox modes"),
    };
    Envelope::request(
        id,
        "command/exec",
        Some(json!({
        "command":command,"cwd":config.cwd.to_string_lossy(),"timeoutMs":10000,"sandboxPolicy":sandbox
        })),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(windows)]
    #[tokio::test]
    #[ignore = "requires the installed authenticated Codex app-server"]
    async fn live_windows_launch_applies_the_explicit_sandbox_override() {
        let config = normalize_config(Config {
            windows_sandbox: Some("unelevated".into()),
            ..Default::default()
        })
        .unwrap();
        let mut server = AppServer::spawn(&config).unwrap();
        let pipe = server.pipe.as_mut().unwrap();
        pipe.send(initialize(RpcId::Number(1))).unwrap();
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let reply = pipe.recv().await.unwrap();
                if reply.id == Some(RpcId::Number(1)) && reply.method.is_none() {
                    assert!(reply.error.is_none());
                    break;
                }
            }
        })
        .await
        .unwrap();
        pipe.send(Envelope::notification("initialized", None))
            .unwrap();
        pipe.send(Envelope::request(
            RpcId::Number(2),
            "config/read",
            Some(json!({"cwd":config.cwd.to_string_lossy(),"includeLayers":false})),
        ))
        .unwrap();
        let mode = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let reply = pipe.recv().await.unwrap();
                if reply.id == Some(RpcId::Number(2)) && reply.method.is_none() {
                    assert!(reply.error.is_none());
                    break reply
                        .result
                        .unwrap()
                        .pointer("/config/windows/sandbox")
                        .cloned();
                }
            }
        })
        .await
        .unwrap();
        server.shutdown().await.unwrap();
        assert_eq!(mode, Some(json!("unelevated")));
    }

    #[test]
    fn preflight_obeys_selected_sandbox_and_never_disables_it_implicitly() {
        for (mode, kind) in [
            ("read-only", "readOnly"),
            ("workspace-write", "workspaceWrite"),
            ("danger-full-access", "dangerFullAccess"),
        ] {
            let config = Config {
                sandbox: mode.into(),
                ..Default::default()
            };
            let request = preflight(RpcId::Number(1), &config);
            let params = request.params.unwrap();
            assert_eq!(params["sandboxPolicy"]["type"], kind);
            assert!(params.get("outputBytesCap").is_none());
            assert_eq!(request.method.as_deref(), Some("command/exec"));
        }
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn shutdown_terminates_the_owned_windows_descendant_even_after_parent_exits() {
        use windows_sys::Win32::Foundation::{CloseHandle, WAIT_OBJECT_0, WAIT_TIMEOUT};
        use windows_sys::Win32::System::Threading::{
            OpenProcess, WaitForSingleObject, PROCESS_SYNCHRONIZE,
        };
        struct Handle(windows_sys::Win32::Foundation::HANDLE);
        impl Drop for Handle {
            fn drop(&mut self) {
                // SAFETY: uniquely owns a live process handle obtained below.
                unsafe {
                    CloseHandle(self.0);
                }
            }
        }
        let mut command = Command::new("pwsh");
        command.args(["-NoProfile", "-NonInteractive", "-Command", r#"
            $descendant = Start-Process -FilePath pwsh -ArgumentList @('-NoProfile', '-NonInteractive', '-Command', '[Threading.Thread]::Sleep(-1)') -WindowStyle Hidden -PassThru
            @{method='test/descendant';params=@{pid=$descendant.Id}} | ConvertTo-Json -Compress
            [void][Console]::In.ReadLine()
        "#]);
        let mut server = AppServer::spawn_command(command).unwrap();
        let descendant = tokio::time::timeout(
            Duration::from_secs(10),
            server.pipe.as_mut().unwrap().recv(),
        )
        .await
        .unwrap()
        .unwrap();
        let pid = descendant.params.unwrap()["pid"].as_u64().unwrap() as u32;
        // Holding the process handle avoids confusing a reused PID with our descendant.
        // SAFETY: this PID was obtained from the process owned by this test.
        let handle = Handle(unsafe { OpenProcess(PROCESS_SYNCHRONIZE, 0, pid) });
        assert!(!handle.0.is_null());
        assert_eq!(unsafe { WaitForSingleObject(handle.0, 0) }, WAIT_TIMEOUT);
        server.shutdown().await.unwrap();
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                // SAFETY: the live handle is retained until the test finishes.
                match unsafe { WaitForSingleObject(handle.0, 0) } {
                    WAIT_OBJECT_0 => break,
                    WAIT_TIMEOUT => tokio::time::sleep(Duration::from_millis(20)).await,
                    result => panic!("unexpected wait result: {result}"),
                }
            }
        })
        .await
        .expect("owned descendant must exit after job closes");
    }
}
