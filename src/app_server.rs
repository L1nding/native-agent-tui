use std::path::PathBuf;
use std::time::Duration;

use serde_json::{json, Value};
use thiserror::Error;
#[cfg(test)]
use tokio::io::AsyncReadExt;
use tokio::task::JoinHandle;

use crate::backend::acp::{self, AcpError};
use crate::config::Config;
use crate::owned_process::Child;
#[cfg(test)]
use crate::owned_process::{self, Command, Input};
use crate::protocol::{Envelope, RpcId, WAIT_TOOL};
use crate::transport::{PipeTransport, TransportError};

#[derive(Debug, Error)]
pub enum AppServerError {
    #[error("could not launch app-server: {0}")]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Transport(#[from] TransportError),
    #[error("invalid working directory: {0}")]
    InvalidDirectory(String),
    #[error("invalid backend configuration: {0}")]
    InvalidConfiguration(String),
    #[error(transparent)]
    Acp(#[from] AcpError),
    #[error(
        "startup query process cleanup could not be confirmed; inspect retained session evidence"
    )]
    StartupCleanup,
}

/// Owns the process, stderr drain, reader and single writer for their entire lifetime.
pub(crate) struct AppServer {
    pub pipe: Option<PipeTransport>,
    child: Child,
    stderr: JoinHandle<()>,
    bridge: Option<JoinHandle<()>>,
}

impl AppServer {
    /// Boot the dsh profile; its bridge plugin answers on the child's stdio.
    pub(crate) async fn spawn(config: &Config) -> Result<Self, AppServerError> {
        let process = acp::spawn(config)?;
        Ok(Self {
            pipe: Some(process.core_pipe),
            child: process.child,
            stderr: process.stderr,
            bridge: Some(process.bridge),
        })
    }

    #[cfg(test)]
    pub(crate) fn spawn_command(command: Command) -> Result<Self, AppServerError> {
        let mut child = owned_process::spawn(command, Input::Pipe)?;
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
            bridge: None,
        })
    }

    pub(crate) async fn shutdown(&mut self) -> Result<(), AppServerError> {
        if let Some(pipe) = &mut self.pipe {
            pipe.close_writer().await;
        }
        if let Some(mut bridge) = self.bridge.take() {
            // 桥接可能还持有重载时启动的进程；留出它正常收尾的时间。
            if tokio::time::timeout(Duration::from_secs(4), &mut bridge)
                .await
                .is_err()
            {
                bridge.abort();
            }
        }
        let exited = tokio::time::timeout(Duration::from_secs(2), self.child.wait()).await;
        #[cfg(windows)]
        self.child.shutdown_tree().await?;
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
        let _ = self.child.start_kill();
        self.stderr.abort();
        if let Some(bridge) = &self.bridge {
            bridge.abort();
        }
    }
}

pub fn normalize_config(mut config: Config) -> Result<Config, AppServerError> {
    config.attention.validate().map_err(|error| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, error.to_string())
    })?;
    config.cwd = config.cwd.canonicalize()?;
    if !config.cwd.is_dir() {
        return Err(AppServerError::InvalidDirectory(
            config.cwd.display().to_string(),
        ));
    }
    if config.acp_profile.trim().is_empty() || config.acp_profile.len() > 128 {
        return Err(AppServerError::InvalidConfiguration(
            "the dsh profile must contain 1-128 non-whitespace bytes".into(),
        ));
    }
    if config.dsh_executable.as_os_str().is_empty() {
        return Err(AppServerError::InvalidConfiguration(
            "the dsh executable path cannot be empty".into(),
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
        "allowProviderModelFallback":false,
        "experimentalRawEvents":false
    });
    params["dynamicTools"] = json!([{
        "type":"function", "name":WAIT_TOOL, "deferLoading":false,
        "description":"Root coordinator: wait for specific current turns of direct child agents, without a timeout or polling. Targets are child thread IDs, registered paths or nicknames; an empty list captures all known direct children. Returns terminal outcomes when all complete or any fails/is interrupted. Children should return their work directly instead of invoking this root tool.",
        "inputSchema":{"type":"object","properties":{"targets":{"type":"array","items":{"type":"string"},"maxItems":64}},"required":["targets"],"additionalProperties":false}
    }]);
    params["config"] = json!({
        "features.code_mode":false,"features.code_mode_only":false,
        "features.multi_agent_v2.enabled":true,"features.multi_agent_v2.wait_agent_enabled":false,
        "features.multi_agent_v2.expose_spawn_agent_model_overrides":true
    });
    if let Some(model) = &config.model {
        params["model"] = Value::String(model.clone());
    }
    Envelope::request(id, "thread/start", Some(params))
}

/// Starts a second dsh session in the running ACP process. The bridge closes
/// the previous active session after this one has been created.
pub(crate) fn thread_new(id: RpcId, config: &Config) -> Envelope {
    let mut request = thread_start(id, config);
    request.method = Some("thread/new".into());
    request
}

pub(crate) fn turn_start(id: RpcId, thread: &str, text: &str, model: Option<&str>) -> Envelope {
    let mut params = json!({
        "threadId":thread, "input":[{"type":"text","text":text}]
    });
    if let Some(model) = model {
        params["model"] = Value::String(model.into());
    }
    Envelope::request(id, "turn/start", Some(params))
}

/// Understood by the ACP bridge only: guide the running turn. dsh hands the
/// text to the model at the next step; the turn is not restarted.
pub(crate) fn turn_steer(id: RpcId, thread: &str, turn: &str, text: &str) -> Envelope {
    Envelope::request(
        id,
        "turn/steer",
        Some(json!({"threadId":thread,"turnId":turn,"text":text})),
    )
}

/// dsh's own slash commands for the current session; understood by the ACP bridge.
pub(crate) fn backend_commands_list(id: RpcId) -> Envelope {
    Envelope::request(id, "backend/commands/list", Some(json!({})))
}

/// Runs one of dsh's slash commands, e.g. `/compact`, on the idle session.
pub(crate) fn backend_command_run(id: RpcId, line: &str) -> Envelope {
    Envelope::request(id, "backend/command/run", Some(json!({"line":line})))
}

pub(crate) fn model_list(id: RpcId) -> Envelope {
    Envelope::request(id, "model/list", Some(json!({"limit":100})))
}

/// Understood by the ACP bridge only: restart dsh and resume the session so
/// edited provider configuration is read.
pub(crate) fn backend_reload(id: RpcId) -> Envelope {
    Envelope::request(id, "backend/reload", Some(json!({})))
}

/// dsh's permission presets and the one this session runs under.
pub(crate) fn permission_describe(id: RpcId) -> Envelope {
    Envelope::request(id, "permission/describe", Some(json!({})))
}

/// Switches this session's permission preset. `remember` also makes it dsh's
/// default for new sessions.
pub(crate) fn permission_set(id: RpcId, preset: &str, remember: bool) -> Envelope {
    Envelope::request(
        id,
        "permission/set",
        Some(json!({"preset":preset,"remember":remember})),
    )
}

/// Adds or changes a provider and one of its models through dsh's settings.
pub(crate) fn provider_save(
    id: RpcId,
    spec: &crate::backend::dsh_config::ProviderSpec,
) -> Envelope {
    Envelope::request(
        id,
        "provider/save",
        Some(crate::backend::dsh_config::save_params(spec)),
    )
}

/// Providers dsh holds, for the edit form.
pub(crate) fn provider_list(id: RpcId) -> Envelope {
    Envelope::request(id, "provider/list", Some(json!({})))
}

/// dsh's sessions of one workspace; answered by the bridge plugin.
pub(crate) fn thread_list(id: RpcId, cwd: &std::path::Path) -> Envelope {
    Envelope::request(
        id,
        "thread/list",
        Some(json!({"cwd":cwd.display().to_string()})),
    )
}

/// Readable messages of one dsh session; answered by the bridge plugin.
pub(crate) fn thread_history(id: RpcId, thread: &str) -> Envelope {
    Envelope::request(id, "thread/history", Some(json!({"threadId":thread})))
}

/// Understood by the ACP bridge only: continue another saved session in the
/// running agent process.
pub(crate) fn thread_resume(id: RpcId, thread: &str) -> Envelope {
    Envelope::request(id, "thread/resume", Some(json!({"threadId":thread})))
}

/// Answered by the ACP bridge, which confirms the switch with dsh.
pub(crate) fn model_select(id: RpcId, model: &str) -> Envelope {
    Envelope::request(id, "model/select", Some(json!({"model":model})))
}

pub(crate) fn interrupt(id: RpcId, thread: &str, turn: &str) -> Envelope {
    Envelope::request(
        id,
        "turn/interrupt",
        Some(json!({"threadId":thread,"turnId":turn})),
    )
}

pub(crate) fn skills_list(id: RpcId, cwd: &std::path::Path, force_reload: bool) -> Envelope {
    Envelope::request(
        id,
        "skills/list",
        Some(json!({"cwds":[cwd.to_string_lossy()],"forceReload":force_reload})),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skills_list_carries_explicit_reload_intent_and_single_cwd() {
        let cwd = std::path::Path::new("C:/workspace");
        let initial = skills_list(RpcId::Number(1), cwd, false);
        assert_eq!(initial.method.as_deref(), Some("skills/list"));
        assert_eq!(
            initial.params.as_ref().unwrap()["cwds"],
            json!([cwd.to_string_lossy()])
        );
        assert_eq!(initial.params.as_ref().unwrap()["forceReload"], false);
        let explicit = skills_list(RpcId::Number(2), cwd, true);
        assert_eq!(explicit.params.as_ref().unwrap()["forceReload"], true);
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
