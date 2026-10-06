use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use thiserror::Error;
use tokio::io::AsyncReadExt;
use tokio::task::JoinHandle;

use crate::backend::acp::{self, AcpError};
use crate::backend::BackendKind;
use crate::compatibility::{self, CompatibilityError};
use crate::config::Config;
use crate::owned_process::{self, Child, Command, Input};
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
    Compatibility(#[from] CompatibilityError),
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
    _catalog: Option<Arc<DirectCatalog>>,
}

impl AppServer {
    pub(crate) async fn spawn(config: &Config) -> Result<Self, AppServerError> {
        if config.backend == BackendKind::DeepSeekAcp {
            let process = acp::spawn(config)?;
            return Ok(Self {
                pipe: Some(process.core_pipe),
                child: process.child,
                stderr: process.stderr,
                bridge: Some(process.bridge),
                _catalog: None,
            });
        }
        let version = query_output(
            config,
            &["--version"],
            4096,
            Duration::from_secs(5),
            "version",
        )
        .await?;
        compatibility::verify_version(&version)?;
        let catalog = Arc::new(DirectCatalog::prepare(config).await?);
        Self::with_catalog(config, catalog)
    }

    fn with_catalog(config: &Config, catalog: Arc<DirectCatalog>) -> Result<Self, AppServerError> {
        let mut command = Command::new(&config.executable);
        command.args(["-c", &catalog.config_override()]);
        #[cfg(windows)]
        if let Some(mode) = &config.windows_sandbox {
            command.args(["-c", &format!("windows.sandbox=\"{mode}\"")]);
        }
        command
            .args(["app-server", "--strict-config", "--listen", "stdio://"])
            .current_dir(&config.cwd);
        let mut server = Self::spawn_command(command)?;
        server._catalog = Some(catalog);
        Ok(server)
    }

    pub(crate) fn shell_peer(&self) -> Option<ShellPeer> {
        self._catalog.as_ref().map(|catalog| ShellPeer {
            catalog: Arc::clone(catalog),
        })
    }

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
            _catalog: None,
        })
    }

    pub(crate) async fn shutdown(&mut self) -> Result<(), AppServerError> {
        if let Some(pipe) = &mut self.pipe {
            pipe.close_writer().await;
        }
        if let Some(mut bridge) = self.bridge.take() {
            if tokio::time::timeout(Duration::from_secs(1), &mut bridge)
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

/// Retains the reviewed catalog until both app-server processes have closed.
pub(crate) struct ShellPeer {
    catalog: Arc<DirectCatalog>,
}

impl ShellPeer {
    pub(crate) async fn launch(
        self,
        config: &Config,
        cancelled: &tokio::sync::watch::Receiver<bool>,
    ) -> Result<Option<AppServer>, AppServerError> {
        if *cancelled.borrow() {
            return Ok(None);
        }
        // Finish and confirm query cleanup even when cancellation arrives mid-query.
        let version = query_output(
            config,
            &["--version"],
            4096,
            Duration::from_secs(5),
            "version",
        )
        .await?;
        compatibility::verify_version(&version)?;
        if *cancelled.borrow() {
            return Ok(None);
        }
        AppServer::with_catalog(config, self.catalog).map(Some)
    }
}

/// Queries retain process ownership through output, exit, and cleanup confirmation.
async fn query_output(
    config: &Config,
    arguments: &[&str],
    limit: u64,
    timeout: Duration,
    name: &'static str,
) -> Result<Vec<u8>, AppServerError> {
    let mut command = Command::new(&config.executable);
    command.args(arguments).current_dir(&config.cwd);
    let mut child = owned_process::spawn(command, Input::Null)?;
    let stdout = child.stdout.take().expect("owned query has stdout");
    let mut stderr = child.stderr.take().expect("owned query has stderr");
    let mut drains = tokio::task::JoinSet::new();
    drains.spawn(async move {
        let mut buffer = [0; 8192];
        while matches!(stderr.read(&mut buffer).await, Ok(n) if n > 0) {}
    });
    let mut bytes = Vec::new();
    let result = tokio::time::timeout(timeout, async {
        stdout.take(limit + 1).read_to_end(&mut bytes).await?;
        if bytes.len() as u64 > limit {
            return Err(std::io::Error::other(format!(
                "Codex {name} output exceeds its byte limit"
            )));
        }
        if !child.wait().await?.success() {
            return Err(std::io::Error::other(format!(
                "Codex {name} query failed; no app-server or model turn was started"
            )));
        }
        Ok(())
    })
    .await;
    let cleanup = tokio::time::timeout(Duration::from_secs(5), child.kill()).await;
    drains.abort_all();
    while drains.join_next().await.is_some() {}
    if !matches!(cleanup, Ok(Ok(()))) {
        return Err(AppServerError::StartupCleanup);
    }
    match result {
        Ok(result) => result?,
        Err(_) => {
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                format!("Codex {name} query timed out; no app-server or model turn was started"),
            )
            .into())
        }
    }
    Ok(bytes)
}

/// An execution-owned copy: never change the user's catalog or configuration.
pub(crate) struct DirectCatalog {
    directory: PathBuf,
}

impl DirectCatalog {
    async fn prepare(config: &Config) -> Result<Self, AppServerError> {
        let bytes = query_output(
            config,
            &["debug", "models"],
            16 * 1024 * 1024,
            Duration::from_secs(15),
            "model catalog",
        )
        .await?;
        Self::from_bytes(&bytes)
    }

    pub(crate) fn from_bytes(bytes: &[u8]) -> Result<Self, AppServerError> {
        let mut catalog: Value = serde_json::from_slice(bytes).map_err(std::io::Error::other)?;
        let models = catalog
            .get_mut("models")
            .and_then(Value::as_array_mut)
            .filter(|models| !models.is_empty())
            .ok_or_else(|| std::io::Error::other("Codex model catalog has no models"))?;
        for model in models {
            let object = model
                .as_object_mut()
                .ok_or_else(|| std::io::Error::other("invalid model catalog entry"))?;
            object.insert("tool_mode".into(), json!("direct"));
        }
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(std::io::Error::other)?
            .as_nanos();
        let directory = std::env::temp_dir().join(format!(
            "native-agent-tui-catalog-{}-{nonce}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let builder = std::fs::DirBuilder::new();
        #[cfg(unix)]
        let builder = {
            use std::os::unix::fs::DirBuilderExt;
            let mut builder = builder;
            builder.mode(0o700);
            builder
        };
        builder.create(&directory)?;
        let owned = Self { directory };
        std::fs::write(
            owned.path(),
            serde_json::to_vec(&catalog).map_err(std::io::Error::other)?,
        )?;
        Ok(owned)
    }

    fn path(&self) -> PathBuf {
        self.directory.join("models.json")
    }

    pub(crate) fn config_override(&self) -> String {
        format!(
            "model_catalog_json={}",
            serde_json::to_string(&self.path().to_string_lossy())
                .expect("path string can be encoded")
        )
    }
}

impl Drop for DirectCatalog {
    fn drop(&mut self) {
        // Only remove the exact file and empty directory created by this owner.
        let _ = std::fs::remove_file(self.path());
        let _ = std::fs::remove_dir(&self.directory);
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
    if config.backend.is_acp()
        && (config.acp_profile.trim().is_empty() || config.acp_profile.len() > 128)
    {
        return Err(AppServerError::InvalidConfiguration(
            "ACP profile must contain 1-128 non-whitespace bytes".into(),
        ));
    }
    if config.backend.is_acp() && config.dsh_executable.as_os_str().is_empty() {
        return Err(AppServerError::InvalidConfiguration(
            "ACP executable path cannot be empty".into(),
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
    fn direct_catalog_preserves_all_other_fields_and_removes_its_private_copy() {
        let original = json!({"models":[{"slug":"fixture-model","tool_mode":"code_mode_only","use_responses_lite":true,"extra":{"keep":true}}],"version":"fixture"});
        let bytes = serde_json::to_vec(&original).unwrap();
        let copy = DirectCatalog::from_bytes(&bytes).unwrap();
        let path = copy.path();
        let mut expected = original.clone();
        expected["models"][0]["tool_mode"] = json!("direct");
        let actual: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(actual, expected);
        assert_eq!(serde_json::from_slice::<Value>(&bytes).unwrap(), original);
        drop(copy);
        assert!(!path.exists());
        assert!(!path.parent().unwrap().exists());
        for invalid in [json!({}), json!({"models":[]}), json!({"models":[null]})] {
            assert!(DirectCatalog::from_bytes(&serde_json::to_vec(&invalid).unwrap()).is_err());
        }
    }

    #[cfg(windows)]
    #[tokio::test]
    #[ignore = "requires the pinned Codex 0.159.2 and Python; no model calls"]
    async fn live_codex_schemas_match_the_reviewed_protocol_manifest() {
        struct FixtureDirectory(PathBuf);
        impl Drop for FixtureDirectory {
            fn drop(&mut self) {
                // Delete only the exact newly created directory, never an alias.
                if self.0.canonicalize().ok().as_ref() == Some(&self.0) {
                    let _ = std::fs::remove_dir_all(&self.0);
                }
            }
        }
        let config = normalize_config(Config::default()).unwrap();
        let version = query_output(
            &config,
            &["--version"],
            4096,
            Duration::from_secs(5),
            "version",
        )
        .await
        .unwrap();
        compatibility::verify_version(&version).unwrap();
        let target = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .canonicalize()
            .unwrap();
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let directory = target.join(format!("protocol-schema-{}-{nonce}", std::process::id()));
        assert!(directory.is_absolute() && directory.starts_with(&target));
        std::fs::create_dir(&directory).unwrap();
        let directory = FixtureDirectory(directory);
        query_output(
            &config,
            &[
                "app-server",
                "generate-json-schema",
                "--experimental",
                "--out",
                directory.0.to_str().unwrap(),
            ],
            4096,
            Duration::from_secs(15),
            "schema export",
        )
        .await
        .unwrap();
        let python = Config {
            executable: std::env::var_os("NATIVE_AGENT_TUI_PYTHON")
                .unwrap_or_else(|| "python".into())
                .into(),
            ..config
        };
        let script =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("scripts/check_protocol_schema.py");
        query_output(
            &python,
            &[
                script.to_str().unwrap(),
                "--schema-dir",
                directory.0.to_str().unwrap(),
            ],
            4096,
            Duration::from_secs(10),
            "schema verification",
        )
        .await
        .unwrap();
        let resolved = directory.0.canonicalize().unwrap();
        assert!(resolved == directory.0 && resolved.starts_with(&target));
        std::fs::remove_dir_all(&resolved).unwrap();
    }

    #[cfg(windows)]
    #[tokio::test]
    #[ignore = "requires the installed authenticated Codex app-server"]
    async fn live_windows_launch_applies_the_explicit_sandbox_override() {
        let config = normalize_config(Config {
            windows_sandbox: Some("unelevated".into()),
            ..Default::default()
        })
        .unwrap();
        let mut server = AppServer::spawn(&config).await.unwrap();
        let pipe = server.pipe.as_mut().unwrap();
        pipe.send(initialize(RpcId::Number(1))).unwrap();
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let reply = pipe.recv().await.unwrap();
                if reply.id == Some(RpcId::Number(1)) && reply.method.is_none() {
                    assert!(reply.error.is_none());
                    compatibility::verify_initialize(reply.result.as_ref().unwrap()).unwrap();
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
