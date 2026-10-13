//! DeepSeek Harness ACP process and the Core-facing envelope bridge.

use serde_json::{json, Value};
use std::collections::HashMap;
use thiserror::Error;
use tokio::io::AsyncReadExt;
use tokio::task::JoinHandle;

use crate::backend::acp_protocol::{self, ToolStatus, Update};
use crate::config::Config;
use crate::owned_process::{self, Child, Command, Input};
use crate::protocol::{Envelope, RpcId};
use crate::transport::{PipeTransport, TransportError};

#[derive(Debug, Error)]
pub enum AcpError {
    #[error("could not launch DeepSeek Harness ACP: {0}")]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Transport(#[from] TransportError),
}

pub(crate) struct AcpProcess {
    pub core_pipe: PipeTransport,
    pub child: Child,
    pub stderr: JoinHandle<()>,
    pub bridge: JoinHandle<()>,
}

/// The end of what dsh wrote to stderr, kept only to explain a failed start.
type StderrTail = std::sync::Arc<std::sync::Mutex<Vec<u8>>>;
const STDERR_TAIL_BYTES: usize = 8192;

/// The one line of dsh's error output worth showing: its last `Error:` line,
/// or else its last line that is not a stack frame. Control characters are
/// dropped and the length is bounded; nothing else of the output is kept.
fn startup_failure(tail: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(tail);
    let lines: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect();
    let line = lines
        .iter()
        .rev()
        .find(|line| line.starts_with("Error:"))
        .or_else(|| {
            lines
                .iter()
                .rev()
                .find(|line| !line.starts_with("at ") && !line.starts_with('^'))
        })?;
    let line = line.strip_prefix("Error:").unwrap_or(line).trim();
    let clean: String = line.chars().filter(|c| !c.is_control()).take(300).collect();
    (!clean.is_empty()).then_some(clean)
}

/// One dsh process with its transport and stderr drain.
fn launch(config: &Config) -> std::io::Result<(PipeTransport, Child, JoinHandle<()>)> {
    launch_with_tail(config, None)
}

fn launch_with_tail(
    config: &Config,
    tail: Option<StderrTail>,
) -> std::io::Result<(PipeTransport, Child, JoinHandle<()>)> {
    let mut command = Command::new(&config.dsh_executable);
    command
        .args(["--profile", config.acp_profile.as_str()])
        .current_dir(&config.cwd);
    let mut child = owned_process::spawn(command, Input::Pipe)?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| std::io::Error::other("missing ACP stdout"))?;
    let stdin = child
        .stdin
        .take()
        .ok_or_else(|| std::io::Error::other("missing ACP stdin"))?;
    let mut stderr = child
        .stderr
        .take()
        .ok_or_else(|| std::io::Error::other("missing ACP stderr"))?;
    let stderr = tokio::spawn(async move {
        // Drain in fixed chunks; only a bounded tail is ever kept.
        let mut buffer = [0; 8192];
        while let Ok(read) = stderr.read(&mut buffer).await {
            if read == 0 {
                break;
            }
            if let Some(tail) = &tail {
                let mut kept = tail.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
                kept.extend_from_slice(&buffer[..read]);
                let excess = kept.len().saturating_sub(STDERR_TAIL_BYTES);
                kept.drain(..excess);
            }
        }
    });
    Ok((PipeTransport::new(stdout, stdin), child, stderr))
}

/// A dsh process started by a configuration reload. The bridge owns it; the
/// first process stays with `AppServer` and exits when its input closes.
struct Replacement {
    child: Child,
    stderr: JoinHandle<()>,
    stderr_tail: StderrTail,
}

impl Replacement {
    /// The transport writer is already closed; wait briefly, then end the tree.
    async fn stop(mut self) {
        let exited =
            tokio::time::timeout(std::time::Duration::from_secs(2), self.child.wait()).await;
        #[cfg(windows)]
        let _ = self.child.shutdown_tree().await;
        if exited.is_err() {
            let _ = self.child.kill().await;
        }
        self.stderr.abort();
    }
}

/// Waits for the response to `id`, declining anything the agent asks meanwhile.
async fn test_reply(pipe: &mut PipeTransport, id: &RpcId) -> Result<Value, String> {
    loop {
        let message = pipe
            .recv()
            .await
            .map_err(|_| "dsh closed the connection".to_owned())?;
        match (message.method.as_deref(), &message.id) {
            (None, Some(reply)) if reply == id => {
                return match message.error {
                    Some(error) => Err(error
                        .get("message")
                        .and_then(Value::as_str)
                        .unwrap_or("request failed")
                        .chars()
                        .take(400)
                        .collect()),
                    None => Ok(message.result.unwrap_or(Value::Null)),
                };
            }
            // 测试不批准任何操作。
            (Some(acp_protocol::SESSION_REQUEST_PERMISSION), Some(request)) => {
                let _ = pipe.send(with_jsonrpc(Envelope::response(
                    request.clone(),
                    Some(json!({"outcome":{"outcome":"cancelled"}})),
                )));
            }
            _ => {}
        }
    }
}

/// Sends one short prompt through `model` in a session of its own. It proves
/// that the provider's address, API type and credential work together.
async fn exercise_model(pipe: &mut PipeTransport, cwd: &str, model: &str) -> Result<(), String> {
    let failed = |_| "dsh closed the connection".to_owned();
    let id = |name: &str| RpcId::String(format!("model-test-{name}"));
    pipe.send(acp_protocol::initialize_request(id("initialize")))
        .map_err(failed)?;
    test_reply(pipe, &id("initialize")).await?;
    pipe.send(acp_protocol::session_new_request(
        id("session"),
        cwd,
        json!([]),
    ))
    .map_err(failed)?;
    let created = test_reply(pipe, &id("session")).await?;
    let session = acp_protocol::session_id(&created).ok_or("dsh returned no session")?;
    let choice = acp_protocol::match_model(&acp_protocol::model_choices(&created), model)?;
    pipe.send(acp_protocol::session_set_config_option(
        id("model"),
        &session,
        "model",
        Value::String(choice.value),
    ))
    .map_err(failed)?;
    test_reply(pipe, &id("model")).await?;
    pipe.send(acp_protocol::session_prompt_request(
        id("prompt"),
        &session,
        "Reply with the single word OK.",
    ))
    .map_err(failed)?;
    let finished = test_reply(pipe, &id("prompt")).await?;
    let _ = pipe.send(acp_protocol::session_close(id("close"), &session));
    match acp_protocol::stop_reason(&finished) {
        Some("end_turn" | "completed") => Ok(()),
        reason => Err(format!(
            "the model did not finish its reply ({})",
            reason.unwrap_or("no stop reason")
        )),
    }
}

/// Tests `model` with a separate dsh process, so the running session and its
/// conversation are untouched. Returns the elapsed milliseconds.
pub(crate) async fn test_model(mut config: Config, model: String) -> Result<u64, String> {
    // The test needs a dsh session of its own. It lives in a directory kept for
    // that purpose, so it never shows up among the workspace's sessions.
    let scratch = std::env::temp_dir().join("native-agent-tui-model-test");
    std::fs::create_dir_all(&scratch)
        .map_err(|error| format!("the test directory could not be created: {error}"))?;
    config.cwd = scratch;
    let (mut pipe, child, stderr) = launch(&config).map_err(|error| error.to_string())?;
    let started = std::time::Instant::now();
    let cwd = config.cwd.display().to_string();
    let outcome = tokio::time::timeout(
        std::time::Duration::from_secs(90),
        exercise_model(&mut pipe, &cwd, &model),
    )
    .await
    .unwrap_or_else(|_| Err("no reply within 90 seconds".into()));
    let elapsed = started.elapsed().as_millis() as u64;
    let _ = pipe.close_writer().await;
    Replacement {
        child,
        stderr,
        stderr_tail: StderrTail::default(),
    }
    .stop()
    .await;
    outcome.map(|()| elapsed)
}

/// Starts another agent process for a reload; `None` when it has no child to own.
type Respawn = Box<dyn FnMut() -> std::io::Result<(PipeTransport, Option<Replacement>)> + Send>;

pub(crate) fn spawn(config: &Config) -> Result<AcpProcess, AcpError> {
    let tail = StderrTail::default();
    let (process_pipe, child, stderr) = launch_with_tail(config, Some(tail.clone()))?;

    // The Core sees the same bounded JSONL transport it always used. A
    // duplex pair keeps ACP translation out of Core and preserves writer
    // ownership, queue limits, and EOF semantics at both boundaries.
    let (core_io, bridge_io) = tokio::io::duplex(64 * 1024);
    let (core_read, core_write) = tokio::io::split(core_io);
    let (bridge_read, bridge_write) = tokio::io::split(bridge_io);
    let core_pipe = PipeTransport::new(core_read, core_write);
    let bridge_pipe = PipeTransport::new(bridge_read, bridge_write);
    let mut bridge = AcpBridge::new(bridge_pipe, process_pipe, config);
    bridge.startup_stderr = Some(tail);
    let respawn_config = config.clone();
    bridge.respawn = Some(Box::new(move || {
        let stderr_tail = StderrTail::default();
        let (pipe, child, stderr) = launch_with_tail(&respawn_config, Some(stderr_tail.clone()))?;
        Ok((
            pipe,
            Some(Replacement {
                child,
                stderr,
                stderr_tail,
            }),
        ))
    }));
    let bridge = tokio::spawn(bridge.run());
    Ok(AcpProcess {
        core_pipe,
        child,
        stderr,
        bridge,
    })
}

#[derive(Debug, Clone)]
enum Pending {
    Initialize,
    SessionNew,
    /// 建会话后按 `--model` 选择模型；确认后才回复 Core 的 thread/start。
    SelectModel {
        core_id: RpcId,
        session: String,
        label: String,
        reasoning: Option<String>,
        session_changed: bool,
    },
    /// 切换模型后 agent 可能把推理强度重置为空；恢复切换前的值。
    RestoreReasoning {
        core_id: RpcId,
        session: String,
        label: String,
        session_changed: bool,
    },
    /// 重载配置：新进程已启动，等它的 initialize 响应。
    ReloadInitialize {
        core_id: RpcId,
    },
    /// 新进程恢复原会话；失败时改为新建会话。
    ReloadResume {
        core_id: RpcId,
    },
    ReloadNew {
        core_id: RpcId,
    },
    /// Create a second session in the current dsh process.
    NewSession,
    /// The new session exists; release the old active session before confirming it.
    NewSessionRelease {
        core_id: RpcId,
        session: String,
        result: Value,
    },
    /// 在当前进程里改为继续另一个已保存的会话；失败时原会话不变。
    Switch {
        core_id: RpcId,
        session: String,
    },
    /// 目标已恢复，等旧会话释放后才确认切换，允许之后切回来。
    SwitchRelease {
        core_id: RpcId,
        session: String,
        result: Value,
    },
    Prompt,
    /// The plugin's skill catalog, to be reshaped into the inventory Core reads.
    Skills {
        cwd: String,
    },
    Passthrough,
}

/// Reshapes the plugin's skill catalog into the single-workspace inventory
/// Core reads. dsh names where a skill came from; Core groups by who owns it.
/// A skill without a file gets a `dsh:` locator so it is still listed.
fn skills_inventory(result: &Value, cwd: &str) -> Value {
    let Some(listed) = result.get("skills").and_then(Value::as_array) else {
        // 没有清单字段：交给 Core 按格式错误处理，而不是当成“没有技能”。
        return json!({});
    };
    let skills: Vec<Value> = listed
        .iter()
        .filter_map(|skill| {
            let name = skill.get("name").and_then(Value::as_str)?;
            let source = skill.get("source").and_then(Value::as_str).unwrap_or("");
            let scope = if source.starts_with("project") {
                "repo"
            } else if source.starts_with("user") {
                "user"
            } else {
                "system"
            };
            let invocable = |field: &str| skill.get(field).and_then(Value::as_bool) == Some(true);
            Some(json!({
                "name": name,
                "path": skill
                    .get("path")
                    .and_then(Value::as_str)
                    .map_or_else(|| format!("dsh:{source}/{name}"), str::to_owned),
                "scope": scope,
                "enabled": invocable("modelInvocable") || invocable("userInvocable"),
            }))
        })
        .collect();
    json!({"data":[{"cwd":cwd,"errors":[],"skills":skills}]})
}

struct Permission {
    /// (optionId, kind)；kind 是 ACP 标准的 allow_once / reject_once 等。
    options: Vec<(String, Option<String>)>,
}

struct AcpBridge {
    core: PipeTransport,
    acp: PipeTransport,
    cwd: String,
    mcp_servers: Value,
    session_id: Option<String>,
    turn_id: Option<String>,
    turn_started: bool,
    pending: HashMap<RpcId, Pending>,
    permissions: HashMap<RpcId, Permission>,
    message_text: HashMap<String, String>,
    last_message_id: Option<String>,
    model: Option<String>,
    /// `model` 来自保存的偏好：agent 不再提供它时沿用 agent 当前模型，不让启动失败。
    model_is_preference: bool,
    /// session/new 给出的可选模型；`model/list` 与 `model/select` 只读这份清单。
    models: Vec<acp_protocol::ModelChoice>,
    /// 最近一次确认的推理强度；切换模型后用它恢复。
    reasoning: Option<String>,
    /// 最近一次确认的模型标签；重载后用它恢复选择。
    current: Option<String>,
    respawn: Option<Respawn>,
    replacement: Option<Replacement>,
    /// dsh 的错误输出末尾；只在会话建立之前进程就退出时用来说明原因。
    startup_stderr: Option<StderrTail>,
    /// 本轮工具调用的输入，按 toolCallId 索引；权限请求可能只带 id。
    tool_inputs: HashMap<String, acp_protocol::ToolInput>,
    mcp_calls: HashMap<String, acp_protocol::McpCall>,
    tool_presentations:
        HashMap<String, (acp_protocol::ToolKind, String, Vec<acp_protocol::FileDiff>)>,
}

impl AcpBridge {
    fn new(core: PipeTransport, acp: PipeTransport, config: &Config) -> Self {
        Self {
            core,
            acp,
            cwd: config.cwd.display().to_string(),
            // MCP remains a typed seam even before CLI configuration exposes
            // individual servers. An empty list is valid ACP input.
            mcp_servers: acp_protocol::encode_mcp_servers(&config.mcp_servers),
            session_id: None,
            turn_id: None,
            turn_started: false,
            pending: HashMap::new(),
            permissions: HashMap::new(),
            message_text: HashMap::new(),
            last_message_id: None,
            model: config.model.clone(),
            model_is_preference: config.model_is_preference,
            models: Vec::new(),
            reasoning: None,
            current: None,
            respawn: None,
            replacement: None,
            startup_stderr: None,
            tool_inputs: HashMap::new(),
            mcp_calls: HashMap::new(),
            tool_presentations: HashMap::new(),
        }
    }

    async fn run(mut self) {
        loop {
            tokio::select! {
                message = self.core.recv() => {
                    match message {
                        Ok(message) => {
                            if self.handle_core(message).await.is_err() {
                                self.explain_failed_start().await;
                                break;
                            }
                        }
                        Err(_) => {
                            if let Some(session) = &self.session_id {
                                let _ = self.acp.send(acp_protocol::session_close(RpcId::String("shutdown".into()), session));
                            }
                            break;
                        }
                    }
                }
                message = self.acp.recv() => {
                    match message {
                        Ok(message) => {
                            if self.handle_acp(message).await.is_err() {
                                self.explain_failed_start().await;
                                break;
                            }
                        }
                        Err(_) => {
                            self.explain_failed_start().await;
                            break;
                        }
                    }
                }
            }
        }
        // send 仅入队；退出前排空 Core 响应，避免 Drop 中止 writer 丢失错误原因。
        self.core.close_writer().await;
        // 只关闭一次：输入结束后 agent 自行退出，重载启动的进程由这里收尾。
        let _ = self.acp.close_writer().await;
        if let Some(replacement) = self.replacement.take() {
            replacement.stop().await;
        }
    }

    /// 启动或重载握手期间退出时，向 Core 原始请求返回当前进程的原因。
    async fn explain_failed_start(&mut self) {
        let waiting: Vec<(RpcId, RpcId, bool)> = self
            .pending
            .iter()
            .filter_map(|(id, kind)| match kind {
                Pending::Initialize | Pending::SessionNew | Pending::NewSession => {
                    Some((id.clone(), id.clone(), false))
                }
                Pending::NewSessionRelease { core_id, .. } => {
                    Some((id.clone(), core_id.clone(), false))
                }
                Pending::ReloadInitialize { core_id }
                | Pending::ReloadResume { core_id }
                | Pending::ReloadNew { core_id } => Some((id.clone(), core_id.clone(), true)),
                _ => None,
            })
            .collect();
        if waiting.is_empty() {
            return;
        }
        // The error text can arrive a moment after the output pipe closes.
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        let said = self.startup_stderr.as_ref().and_then(|tail| {
            startup_failure(&tail.lock().unwrap_or_else(|poisoned| poisoned.into_inner()))
        });
        for (id, core_id, reload) in waiting {
            let stage = if reload {
                "while restarting the session"
            } else {
                "before a session started"
            };
            let message = match &said {
                Some(said) => format!("dsh exited {stage}: {said}"),
                None => format!("dsh exited {stage} and reported no error"),
            };
            self.pending.remove(&id);
            let _ = self
                .core
                .send(Envelope::error_response(core_id, -32000, message));
        }
        // The bridge ends right after this; the answer must be written out first.
        self.core.close_writer().await;
    }

    /// Replies to Core's thread/start, model/select or backend/reload with the
    /// session and the model now in effect.
    fn confirm(
        &mut self,
        core_id: RpcId,
        session: &str,
        model: Option<String>,
    ) -> Result<(), TransportError> {
        self.current = model.clone();
        self.core.send(Envelope::response(
            core_id,
            Some(json!({"thread":{"id":session},"model":model})),
        ))
    }

    /// Starts a fresh agent process so edited configuration is read, then
    /// resumes the session in it. Nothing changes when the new process cannot start.
    async fn reload(&mut self, core_id: RpcId) -> Result<(), TransportError> {
        let refusal = if self.session_id.is_none() {
            Some("ACP session is not ready".to_owned())
        } else if self.turn_id.is_some() {
            Some("ACP session has an active prompt".to_owned())
        } else {
            None
        };
        let started = match (refusal, self.respawn.as_mut()) {
            (Some(message), _) => Err(message),
            (None, None) => Err("this ACP connection cannot be restarted".to_owned()),
            (None, Some(respawn)) => respawn().map_err(|error| error.to_string()),
        };
        let (pipe, replacement) = match started {
            Ok(started) => started,
            Err(message) => {
                return self.core.send(Envelope::error_response(
                    core_id,
                    -32000,
                    format!("dsh was not restarted: {message}"),
                ));
            }
        };
        if let Some(replacement) = &replacement {
            self.startup_stderr = Some(replacement.stderr_tail.clone());
        }
        let mut previous = std::mem::replace(&mut self.acp, pipe);
        let _ = previous.close_writer().await;
        if let Some(old) = std::mem::replace(&mut self.replacement, replacement) {
            tokio::spawn(old.stop());
        }
        // 旧进程的未决请求不会再有响应。
        self.pending.clear();
        self.permissions.clear();
        let request_id = RpcId::String(format!("acp-reload-{}", id_text(&core_id)));
        self.pending
            .insert(request_id.clone(), Pending::ReloadInitialize { core_id });
        self.acp.send(acp_protocol::initialize_request(request_id))
    }

    fn switched(
        &mut self,
        core_id: RpcId,
        session: String,
        result: &Value,
    ) -> Result<(), TransportError> {
        self.session_id = Some(session.clone());
        self.message_text.clear();
        self.tool_inputs.clear();
        self.mcp_calls.clear();
        self.tool_presentations.clear();
        self.last_message_id = None;
        self.reloaded(core_id, session, result)
    }

    /// The reloaded session's options are known: keep the model chosen before
    /// the reload when the agent still offers it.
    fn reloaded(
        &mut self,
        core_id: RpcId,
        session: String,
        result: &Value,
    ) -> Result<(), TransportError> {
        self.models = acp_protocol::model_choices(result);
        let offered = acp_protocol::current_model(result);
        let wanted = self
            .current
            .clone()
            .filter(|wanted| Some(wanted) != offered.as_ref())
            .and_then(|wanted| acp_protocol::match_model(&self.models, &wanted).ok());
        let Some(choice) = wanted else {
            return self.confirm(core_id, &session, offered);
        };
        let request_id = RpcId::String(format!("acp-model-{}", id_text(&core_id)));
        self.pending.insert(
            request_id.clone(),
            Pending::SelectModel {
                core_id,
                session: session.clone(),
                label: choice.label(),
                reasoning: self.reasoning.clone(),
                session_changed: true,
            },
        );
        self.acp.send(acp_protocol::session_set_config_option(
            request_id,
            &session,
            "model",
            Value::String(choice.value),
        ))
    }

    async fn handle_core(&mut self, message: Envelope) -> Result<(), TransportError> {
        match (message.method.as_deref(), message.id.clone()) {
            (Some(acp_protocol::INITIALIZE), Some(id)) => {
                self.pending.insert(id.clone(), Pending::Initialize);
                self.acp.send(acp_protocol::initialize_request(id))?;
            }
            (Some("thread/start"), Some(id)) => {
                self.pending.insert(id.clone(), Pending::SessionNew);
                self.acp.send(acp_protocol::session_new_request(
                    id,
                    &self.cwd,
                    self.mcp_servers.clone(),
                ))?;
            }
            (Some("thread/new"), Some(id)) => {
                if self.turn_id.is_some() || self.session_id.is_none() {
                    return self.core.send(Envelope::error_response(
                        id,
                        -32000,
                        "ACP session is not ready for a new conversation",
                    ));
                }
                self.pending.insert(id.clone(), Pending::NewSession);
                self.acp.send(acp_protocol::session_new_request(
                    id,
                    &self.cwd,
                    self.mcp_servers.clone(),
                ))?;
            }
            (Some("turn/start"), Some(id)) => {
                if self.turn_id.is_some() {
                    self.core.send(Envelope::error_response(
                        id,
                        -32000,
                        "ACP session already has an active prompt",
                    ))?;
                    return Ok(());
                }
                let Some(session) = self.session_id.clone() else {
                    self.core.send(Envelope::error_response(
                        id,
                        -32001,
                        "ACP session is not ready",
                    ))?;
                    return Ok(());
                };
                let text = message
                    .params
                    .as_ref()
                    .and_then(|params| params.pointer("/input/0/text"))
                    .and_then(Value::as_str)
                    .unwrap_or("");
                self.message_text.clear();
                self.tool_inputs.clear();
                self.mcp_calls.clear();
                self.tool_presentations.clear();
                self.last_message_id = None;
                self.turn_id = Some(format!("acp-turn-{}", id_text(&id)));
                self.turn_started = true;
                self.core.send(Envelope::notification(
                    "turn/started",
                    Some(json!({
                        "threadId": session,
                        "turn": {"id": self.turn_id.as_deref().unwrap_or_default()}
                    })),
                ))?;
                self.pending.insert(id.clone(), Pending::Prompt);
                self.acp.send(acp_protocol::session_prompt_request(
                    id.clone(),
                    &session,
                    text,
                ))?;
                // ACP 的 prompt 响应要等整轮结束；Core 的 turn/start 只确认已开始，
                // 必须立即回复，否则长任务会触发 RPC 期限而被误判为 Unknown。
                // 终态只由 finish_prompt 发出的 turn/completed 表达。
                self.core.send(Envelope::response(
                    id,
                    Some(json!({"turn":{"id":self.turn_id.as_deref().unwrap_or_default()}})),
                ))?;
            }
            (Some("turn/interrupt"), Some(id)) => {
                if let Some(session) = &self.session_id {
                    self.acp.send(acp_protocol::session_cancel(session))?;
                }
                // ACP cancel is a notification. The synthetic acknowledgement
                // only releases Core's pending RPC; terminal state comes from
                // the prompt response or a later disconnect/update.
                self.core.send(Envelope::response(id, Some(json!({}))))?;
            }
            (Some("command/exec"), Some(id)) => {
                self.core.send(Envelope::response(
                    id,
                    Some(json!({
                        "exitCode": 0, "stdout": "native-agent-tui-shell-ok", "stderr": ""
                    })),
                ))?;
            }
            (Some("backend/reload"), Some(id)) => self.reload(id).await?,
            (Some("thread/resume"), Some(id)) => {
                let session = message
                    .params
                    .as_ref()
                    .and_then(|params| params.get("threadId"))
                    .and_then(Value::as_str)
                    .filter(|session| !session.is_empty() && session.len() <= 1024)
                    .map(str::to_owned);
                let refusal = match (&session, self.turn_id.is_some()) {
                    (None, _) => Some("no session to resume was named"),
                    (_, true) => Some("ACP session has an active prompt"),
                    _ => None,
                };
                if let Some(reason) = refusal {
                    return self.core.send(Envelope::error_response(id, -32000, reason));
                }
                let session = session.unwrap_or_default();
                if self.session_id.as_ref() == Some(&session) {
                    return self.confirm(id, &session, self.current.clone());
                }
                let request_id = RpcId::String(format!("acp-switch-{}", id_text(&id)));
                self.pending.insert(
                    request_id.clone(),
                    Pending::Switch {
                        core_id: id,
                        session: session.clone(),
                    },
                );
                self.acp.send(acp_protocol::session_resume_request(
                    request_id,
                    &session,
                    &self.cwd,
                    self.mcp_servers.clone(),
                ))?;
            }
            // dsh's permission presets belong to a session; the bridge names ours.
            (Some(method @ ("permission/describe" | "permission/set")), Some(id)) => {
                let Some(session) = self.session_id.clone() else {
                    return self.core.send(Envelope::error_response(
                        id,
                        -32000,
                        "ACP session is not ready",
                    ));
                };
                let mut params = match message.params {
                    Some(Value::Object(map)) => Value::Object(map),
                    _ => json!({}),
                };
                params["sessionId"] = json!(session);
                let target = if method == "permission/set" {
                    "zf/permission/set"
                } else {
                    "zf/permission/describe"
                };
                self.pending.insert(id.clone(), Pending::Passthrough);
                self.acp
                    .send(with_jsonrpc(Envelope::request(id, target, Some(params))))?;
            }
            // The bridge plugin answers these from dsh's settings service; the
            // parameters, which may hold an API key, are forwarded untouched.
            (Some(method @ ("provider/save" | "provider/list")), Some(id)) => {
                let target = if method == "provider/save" {
                    "zf/provider/save"
                } else {
                    "zf/provider/list"
                };
                self.pending.insert(id.clone(), Pending::Passthrough);
                self.acp
                    .send(with_jsonrpc(Envelope::request(id, target, message.params)))?;
            }
            // The bridge plugin answers these from dsh's session services.
            (Some("thread/list"), Some(id)) => {
                self.pending.insert(id.clone(), Pending::Passthrough);
                self.acp.send(with_jsonrpc(Envelope::request(
                    id,
                    "zf/session/list",
                    Some(json!({"cwd": self.cwd})),
                )))?;
            }
            (Some("thread/history"), Some(id)) => {
                let session = message
                    .params
                    .as_ref()
                    .and_then(|params| params.get("threadId"))
                    .and_then(Value::as_str)
                    .unwrap_or("");
                self.pending.insert(id.clone(), Pending::Passthrough);
                self.acp.send(with_jsonrpc(Envelope::request(
                    id,
                    "zf/session/history",
                    Some(json!({"sessionId": session})),
                )))?;
            }
            (Some("model/list"), Some(id)) => {
                let data: Vec<Value> = self
                    .models
                    .iter()
                    .map(|choice| json!({"model": choice.label(), "displayName": choice.name}))
                    .collect();
                self.core
                    .send(Envelope::response(id, Some(json!({"data": data}))))?;
            }
            (Some("model/select"), Some(id)) => {
                let requested = message
                    .params
                    .as_ref()
                    .and_then(|params| params.get("model"))
                    .and_then(Value::as_str)
                    .unwrap_or("");
                let choice = match (&self.session_id, self.turn_id.is_some()) {
                    (None, _) => Err("ACP session is not ready".to_owned()),
                    (_, true) => Err("ACP session has an active prompt".to_owned()),
                    (Some(_), false) => acp_protocol::match_model(&self.models, requested),
                };
                match choice {
                    Ok(choice) => {
                        let session = self.session_id.clone().unwrap_or_default();
                        let request_id = RpcId::String(format!("acp-model-{}", id_text(&id)));
                        self.pending.insert(
                            request_id.clone(),
                            Pending::SelectModel {
                                core_id: id,
                                session: session.clone(),
                                label: choice.label(),
                                reasoning: self.reasoning.clone(),
                                session_changed: false,
                            },
                        );
                        self.acp.send(acp_protocol::session_set_config_option(
                            request_id,
                            &session,
                            "model",
                            Value::String(choice.value),
                        ))?;
                    }
                    Err(message) => {
                        self.core
                            .send(Envelope::error_response(id, -32602, &message))?;
                    }
                }
            }
            (Some("skills/list"), Some(id)) => {
                // Core asks for one workspace and matches the answer by this exact text.
                let cwd = message
                    .params
                    .as_ref()
                    .and_then(|params| params.pointer("/cwds/0"))
                    .and_then(Value::as_str)
                    .unwrap_or(&self.cwd)
                    .to_owned();
                self.acp.send(with_jsonrpc(Envelope::request(
                    id.clone(),
                    "zf/skills/list",
                    Some(json!({"cwd": cwd})),
                )))?;
                self.pending.insert(id, Pending::Skills { cwd });
            }
            (Some(acp_protocol::SESSION_CLOSE), Some(id)) => {
                self.acp.send(with_jsonrpc(message))?;
                self.pending.insert(id, Pending::Passthrough);
            }
            (Some(acp_protocol::SESSION_SET_CONFIG_OPTION), Some(id)) => {
                self.pending.insert(id, Pending::Passthrough);
                self.acp.send(with_jsonrpc(message))?;
            }
            (Some("thread/read"), Some(id)) => {
                self.core.send(Envelope::error_response(
                    id,
                    -32601,
                    "ACP child thread metadata is unavailable",
                ))?;
            }
            (Some("initialized"), None) => {}
            (None, Some(id)) if self.permissions.contains_key(&id) => {
                let permission = self.permissions.remove(&id).unwrap();
                let response = if let Some(error) = message.error {
                    Envelope {
                        jsonrpc: None,
                        id: Some(id.clone()),
                        method: None,
                        params: None,
                        result: None,
                        error: Some(error),
                    }
                } else {
                    let decision = message
                        .result
                        .as_ref()
                        .and_then(|value| value.get("decision"))
                        .and_then(Value::as_str)
                        .unwrap_or("decline");
                    // 找不到与决定一致的选项时回复 cancelled，绝不回退到批准类选项。
                    let outcome = match (
                        decision,
                        choose_permission_option(decision, &permission.options),
                    ) {
                        ("cancel", _) | (_, None) => json!({"outcome":"cancelled"}),
                        (_, Some(option)) => json!({"outcome":"selected","optionId":option}),
                    };
                    Envelope::response(id.clone(), Some(json!({"outcome": outcome})))
                };
                self.acp.send(with_jsonrpc(response))?;
                // ACP 没有单独的“请求已解决”通知：答复交给 dsh 就是解决。告诉 Core，
                // 否则这条请求会一直显示为待处理，挡住同一轮里后来的请求。
                if let Some(session) = &self.session_id {
                    self.core.send(Envelope::notification(
                        "serverRequest/resolved",
                        Some(json!({"threadId": session, "requestId": id})),
                    ))?;
                }
            }
            _ => {}
        }
        Ok(())
    }

    async fn handle_acp(&mut self, message: Envelope) -> Result<(), TransportError> {
        match (message.method.as_deref(), message.id.clone()) {
            (Some(acp_protocol::SESSION_UPDATE), None) => {
                self.update(message.params.unwrap_or(Value::Null)).await?;
            }
            (Some(acp_protocol::SESSION_REQUEST_PERMISSION), Some(id)) => {
                self.permission(id, message.params.unwrap_or(Value::Null))?;
            }
            (Some("zf/session/event"), None) => {
                self.plugin_event(message.params.unwrap_or(Value::Null))?;
            }
            (None, Some(id)) => self.response(id, message).await?,
            _ => {}
        }
        Ok(())
    }

    /// dsh 的 ACP 只转发已提交的整条消息；模型所处阶段和 todo 列表由
    /// zf-tui-bridge 插件补发。只接受当前会话的事件，载荷在这里再次限界。
    fn plugin_event(&mut self, params: Value) -> Result<(), TransportError> {
        let Some(session) = self
            .session_id
            .clone()
            .filter(|id| params["sessionId"] == id.as_str())
        else {
            return Ok(());
        };
        let event = &params["event"];
        match event["type"].as_str() {
            Some("stage") => {
                let (Some(turn), Some(stage)) = (
                    self.turn_id.clone(),
                    event["stage"]
                        .as_str()
                        .filter(|stage| matches!(*stage, "waiting" | "thinking" | "response")),
                ) else {
                    return Ok(());
                };
                if !self.turn_started {
                    self.core.send(Envelope::notification(
                        "turn/started",
                        Some(json!({"threadId":session,"turn":{"id":turn}})),
                    ))?;
                    self.turn_started = true;
                }
                self.core.send(Envelope::notification(
                    "turn/stage",
                    Some(json!({"threadId":session,"turnId":turn,"stage":stage})),
                ))?;
            }
            Some("retry") => {
                let (Some(turn), Some(retry)) = (self.turn_id.clone(), event["retry"].as_u64())
                else {
                    return Ok(());
                };
                let code = event["code"].as_str().filter(|code| {
                    !code.is_empty()
                        && code.len() <= 64
                        && code
                            .bytes()
                            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'))
                });
                self.core.send(Envelope::notification(
                    "turn/notice",
                    Some(json!({"threadId":session,"turnId":turn,"kind":"retry",
                        "retry":retry,"maxRetries":event["maxRetries"].as_u64(),
                        "delayMs":event["delayMs"].as_u64(),"code":code})),
                ))?;
            }
            Some("compaction") => {
                let (Some(id), Some(state)) = (
                    event["id"]
                        .as_str()
                        .filter(|id| !id.is_empty() && id.len() <= 128),
                    event["state"]
                        .as_str()
                        .filter(|state| matches!(*state, "start" | "end" | "failed")),
                ) else {
                    return Ok(());
                };
                // 手动压缩发生在轮次之外；此时没有 turn，提示仍属于这个会话。
                self.core.send(Envelope::notification(
                    "turn/notice",
                    Some(
                        json!({"threadId":session,"turnId":self.turn_id,"kind":"compaction",
                        "id":id,"state":state}),
                    ),
                ))?;
            }
            Some("todos") => {
                let Some(todos) = event["todos"].as_array() else {
                    return Ok(());
                };
                let todos: Vec<Value> = todos
                    .iter()
                    .take(crate::state::MAX_TODOS)
                    .filter_map(|todo| {
                        let status = todo["status"].as_str().filter(|status| {
                            matches!(*status, "pending" | "in_progress" | "completed")
                        })?;
                        let content: String = todo["content"]
                            .as_str()?
                            .chars()
                            .take(crate::state::MAX_TODO_CHARS)
                            .collect();
                        Some(json!({"content":content,"status":status}))
                    })
                    .collect();
                self.core.send(Envelope::notification(
                    "thread/todos/updated",
                    Some(json!({"threadId":session,"todos":todos})),
                ))?;
            }
            _ => {}
        }
        Ok(())
    }

    fn acp_tool_item(
        &self,
        item_id: &str,
        status: &str,
        content: acp_protocol::ToolContent,
    ) -> Value {
        let acp_protocol::ToolContent {
            output,
            structured_output,
            diffs,
            exit_code,
            duration_ms,
        } = content;
        let diffs = diffs.as_slice();
        if let Some(call) = self.mcp_calls.get(item_id) {
            let result = structured_output
                .or_else(|| output.as_ref().map(|output| Value::String(output.clone())));
            json!({"id":item_id,"type":"mcpToolCall","server":call.server,"tool":call.tool,"status":status,"result":result,
                "aggregatedOutput":output,"exitCode":exit_code,"durationMs":duration_ms})
        } else {
            let input = self.tool_inputs.get(item_id);
            let presentation = self.tool_presentations.get(item_id);
            let kind = if !diffs.is_empty() {
                acp_protocol::ToolKind::File
            } else {
                presentation.map_or(acp_protocol::ToolKind::Generic, |value| value.0)
            };
            let name = presentation.map_or("ACP tool call", |value| value.1.as_str());
            let diffs = if diffs.is_empty() {
                presentation.map_or(diffs, |value| value.2.as_slice())
            } else {
                diffs
            };
            let patch = (!diffs.is_empty()).then(|| {
                diffs
                    .iter()
                    .map(|diff| format!("--- {}\n+++ {}\n{}", diff.path, diff.path, diff.patch))
                    .collect::<Vec<_>>()
                    .join("\n")
            });
            let mut item = json!({"id":item_id,"status":status,"tool":name,
                "cwd":input.and_then(|input| input.cwd.as_deref()),
                "target":input.and_then(|input| input.target.as_deref()).or_else(|| diffs.first().map(|diff| diff.path.as_str())),
                "exitCode":exit_code,"durationMs":duration_ms,"aggregatedOutput":patch.or(output)});
            match kind {
                acp_protocol::ToolKind::Shell => {
                    item["type"] = json!("commandExecution");
                    item["command"] = json!(input
                        .and_then(|input| input.command.as_deref())
                        .unwrap_or(name));
                }
                acp_protocol::ToolKind::File => {
                    item["type"] = json!("fileChange");
                    if !diffs.is_empty() {
                        item["changes"] = json!(diffs.iter().map(|diff|
                        json!({"path":diff.path,"kind":{"type":"update"},"diff":diff.patch})).collect::<Vec<_>>());
                    }
                }
                acp_protocol::ToolKind::Generic => item["type"] = json!("dynamicToolCall"),
            }
            item
        }
    }

    async fn response(&mut self, id: RpcId, message: Envelope) -> Result<(), TransportError> {
        let Some(kind) = self.pending.remove(&id) else {
            return Ok(());
        };
        if let (Some(error), Pending::Prompt) = (&message.error, &kind) {
            // ACP 的 prompt 错误响应表示这一轮已确定以错误结束，不是不确定结果；
            // 映射为 failed 终态，用户可以继续提交或重试。
            let text = error
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("ACP prompt failed")
                .to_owned();
            return self.finish_prompt(id, "failed", Some(text));
        }
        if let (
            Some(error),
            Pending::SelectModel {
                core_id,
                session_changed,
                ..
            }
            | Pending::RestoreReasoning {
                core_id,
                session_changed,
                ..
            },
        ) = (&message.error, &kind)
        {
            let text = error
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("ACP model selection failed");
            let mut reply = Envelope::error_response(
                core_id.clone(),
                -32602,
                format!("ACP model selection failed: {text}"),
            );
            if *session_changed {
                reply.error.as_mut().unwrap()["data"] = json!({"sessionUnavailable": true});
            }
            self.core.send(reply)?;
            return Ok(());
        }
        if let (Some(_), Pending::ReloadResume { core_id }) = (&message.error, &kind) {
            // 新进程无法恢复原会话：改为新建，Core 会得知会话已更换。
            let request_id = RpcId::String(format!("acp-reload-new-{}", id_text(core_id)));
            self.pending.insert(
                request_id.clone(),
                Pending::ReloadNew {
                    core_id: core_id.clone(),
                },
            );
            return self.acp.send(acp_protocol::session_new_request(
                request_id,
                &self.cwd,
                self.mcp_servers.clone(),
            ));
        }
        if let (
            Some(error),
            Pending::SwitchRelease { core_id, .. } | Pending::NewSessionRelease { core_id, .. },
        ) = (&message.error, &kind)
        {
            self.core.send(Envelope {
                jsonrpc: None,
                id: Some(core_id.clone()),
                method: None,
                params: None,
                result: None,
                error: Some(json!({"code":-32000,"message":format!(
                    "Conversation change failed while releasing the previous session: {}",
                    error.get("message").and_then(Value::as_str).unwrap_or("request failed")
                ),"data":{"sessionUnavailable":true}})),
            })?;
            return Err(TransportError::Closed);
        }
        if let (Some(error), Pending::Switch { core_id, .. }) = (&message.error, &kind) {
            // dsh 把具体原因放在 data.details，例如会话正被另一个进程占用。
            let detail = error
                .pointer("/data/details")
                .or_else(|| error.get("message"))
                .and_then(Value::as_str)
                .unwrap_or("request failed");
            let text = if detail.contains("owned by an active") {
                "it is open in another terminal; continue it there or close that one first"
                    .to_owned()
            } else {
                detail.chars().take(300).collect()
            };
            return self
                .core
                .send(Envelope::error_response(core_id.clone(), -32000, text));
        }
        if let (
            Some(error),
            Pending::ReloadInitialize { core_id } | Pending::ReloadNew { core_id },
        ) = (&message.error, &kind)
        {
            let text = error
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("request failed");
            return self.core.send(Envelope::error_response(
                core_id.clone(),
                -32000,
                format!("dsh restarted but no session is available: {text}"),
            ));
        }
        if let Some(error) = message.error {
            self.core.send(Envelope {
                jsonrpc: None,
                id: Some(id),
                method: None,
                params: None,
                result: None,
                error: Some(error),
            })?;
            return Ok(());
        }
        let result = message.result.unwrap_or(Value::Null);
        match kind {
            Pending::ReloadInitialize { core_id } => {
                let session = self.session_id.clone().unwrap_or_default();
                let request_id = RpcId::String(format!("acp-reload-resume-{}", id_text(&core_id)));
                self.pending
                    .insert(request_id.clone(), Pending::ReloadResume { core_id });
                self.acp.send(acp_protocol::session_resume_request(
                    request_id,
                    &session,
                    &self.cwd,
                    self.mcp_servers.clone(),
                ))?;
            }
            Pending::ReloadResume { core_id } => {
                let session = self.session_id.clone().unwrap_or_default();
                self.reloaded(core_id, session, &result)?;
            }
            Pending::Switch { core_id, session } => {
                if let Some(previous) = self.session_id.clone() {
                    let request_id =
                        RpcId::String(format!("acp-switch-close-{}", id_text(&core_id)));
                    self.pending.insert(
                        request_id.clone(),
                        Pending::SwitchRelease {
                            core_id,
                            session,
                            result,
                        },
                    );
                    return self
                        .acp
                        .send(acp_protocol::session_close(request_id, &previous));
                }
                self.switched(core_id, session, &result)?;
            }
            Pending::SwitchRelease {
                core_id,
                session,
                result,
            } => {
                self.switched(core_id, session, &result)?;
            }
            Pending::ReloadNew { core_id } => {
                let Some(session) = acp_protocol::session_id(&result) else {
                    return self.core.send(Envelope::error_response(
                        core_id,
                        -32000,
                        "dsh restarted but session/new returned no sessionId",
                    ));
                };
                self.session_id = Some(session.clone());
                self.reloaded(core_id, session, &result)?;
            }
            Pending::Initialize => {
                self.core.send(Envelope::response(id, Some(json!({
                    "userAgent":"deepseek-acp", "codexHome":"unavailable", "platformFamily":"unavailable", "platformOs":"unavailable"
                }))))?;
            }
            Pending::NewSession => {
                let Some(session) = acp_protocol::session_id(&result) else {
                    return self.core.send(Envelope::error_response(
                        id,
                        -32602,
                        "ACP session/new response has no sessionId",
                    ));
                };
                if let Some(previous) = self.session_id.clone() {
                    let request_id = RpcId::String(format!("acp-new-close-{}", id_text(&id)));
                    self.pending.insert(
                        request_id.clone(),
                        Pending::NewSessionRelease {
                            core_id: id,
                            session,
                            result,
                        },
                    );
                    self.acp
                        .send(acp_protocol::session_close(request_id, &previous))?;
                } else {
                    self.switched(id, session, &result)?;
                }
            }
            Pending::NewSessionRelease {
                core_id,
                session,
                result,
            } => {
                self.switched(core_id, session, &result)?;
            }
            Pending::SessionNew => {
                let Some(session) = acp_protocol::session_id(&result) else {
                    self.core.send(Envelope::error_response(
                        id,
                        -32602,
                        "ACP session/new response has no sessionId",
                    ))?;
                    return Ok(());
                };
                self.session_id = Some(session.clone());
                let models = acp_protocol::model_choices(&result);
                self.models = models.clone();
                self.reasoning = acp_protocol::select_option(&result, "reasoning_effort")
                    .map(|(current, _)| current)
                    .filter(|current| !current.is_empty());
                let Some(requested) = self.model.clone() else {
                    let current = acp_protocol::current_model(&result);
                    return self.confirm(id, &session, current);
                };
                match acp_protocol::match_model(&models, &requested) {
                    Ok(choice) => {
                        let request_id = RpcId::String(format!("acp-model-{}", id_text(&id)));
                        self.pending.insert(
                            request_id.clone(),
                            Pending::SelectModel {
                                core_id: id,
                                session: session.clone(),
                                label: choice.label(),
                                reasoning: self.reasoning.clone(),
                                session_changed: false,
                            },
                        );
                        self.acp.send(acp_protocol::session_set_config_option(
                            request_id,
                            &session,
                            "model",
                            Value::String(choice.value),
                        ))?;
                    }
                    Err(_) if self.model_is_preference => {
                        let current = acp_protocol::current_model(&result);
                        self.confirm(id, &session, current)?;
                    }
                    Err(message) => {
                        self.core
                            .send(Envelope::error_response(id, -32602, &message))?;
                    }
                }
            }
            Pending::SelectModel {
                core_id,
                session,
                label,
                reasoning,
                session_changed,
            } => {
                let restore = reasoning.filter(|previous| {
                    acp_protocol::select_option(&result, "reasoning_effort").is_some_and(
                        |(current, values)| &current != previous && values.contains(previous),
                    )
                });
                if let Some(previous) = restore {
                    let request_id = RpcId::String(format!("acp-reasoning-{}", id_text(&core_id)));
                    self.pending.insert(
                        request_id.clone(),
                        Pending::RestoreReasoning {
                            core_id,
                            session: session.clone(),
                            label,
                            session_changed,
                        },
                    );
                    self.acp.send(acp_protocol::session_set_config_option(
                        request_id,
                        &session,
                        "reasoning_effort",
                        Value::String(previous),
                    ))?;
                } else {
                    self.confirm(core_id, &session, Some(label))?;
                }
            }
            Pending::RestoreReasoning {
                core_id,
                session,
                label,
                ..
            } => self.confirm(core_id, &session, Some(label))?,
            Pending::Prompt => {
                let status = match acp_protocol::stop_reason(&result) {
                    Some("end_turn" | "completed") => "completed",
                    Some("cancelled" | "canceled") => "interrupted",
                    Some(_) | None => "failed",
                };
                self.finish_prompt(id, status, None)?;
            }
            Pending::Skills { cwd } => {
                self.core.send(Envelope::response(
                    id,
                    Some(skills_inventory(&result, &cwd)),
                ))?;
            }
            Pending::Passthrough => {
                self.core.send(Envelope::response(id, Some(result)))?;
            }
        }
        Ok(())
    }

    fn finish_prompt(
        &mut self,
        id: RpcId,
        status: &str,
        error: Option<String>,
    ) -> Result<(), TransportError> {
        let turn = self
            .turn_id
            .clone()
            .unwrap_or_else(|| format!("acp-turn-{}", id_text(&id)));
        if !self.turn_started {
            self.core.send(Envelope::notification(
                "turn/started",
                Some(json!({"threadId":self.session_id,"turn":{"id":turn}})),
            ))?;
            self.turn_started = true;
        }
        if let Some(item_id) = self.last_message_id.take() {
            if let Some(text) = self.message_text.remove(&item_id) {
                self.core.send(Envelope::notification(
                    "item/completed",
                    Some(json!({
                        "threadId": self.session_id,
                        "turnId": turn,
                        "item": {"id": item_id, "type": "agentMessage", "text": text}
                    })),
                ))?;
            }
        }
        self.core.send(Envelope::notification(
            "turn/completed",
            Some(json!({
                "threadId":self.session_id,
                "turn":{"id":turn,"status":status,"error":error.map(|message| json!({"message":message}))}
            })),
        ))?;
        self.turn_started = false;
        self.turn_id = None;
        Ok(())
    }

    async fn update(&mut self, params: Value) -> Result<(), TransportError> {
        let (session, update) = match acp_protocol::update(&params) {
            Ok(value) => value,
            Err(error) => {
                if let (Some(session), Some(turn)) = (self.session_id.clone(), self.turn_id.clone())
                {
                    self.core.send(Envelope::notification(
                        "turn/completed",
                        Some(json!({
                            "threadId": session,
                            "turn": {"id": turn, "status": "failed", "error": {"message": error.to_string()}}
                        })),
                    ))?;
                    self.turn_started = false;
                    self.turn_id = None;
                }
                return Ok(());
            }
        };
        if self.session_id.as_deref() != Some(session.as_str()) {
            return Ok(());
        }
        // dsh 的设置改了（例如保存了供应商）：可选模型跟着换，并告诉 Core。
        if let Some(changed) = params.get("update").filter(|update| {
            update.get("sessionUpdate").and_then(Value::as_str) == Some("config_option_update")
        }) {
            let models = acp_protocol::model_choices(changed);
            if !models.is_empty() && models != self.models {
                self.models = models;
                self.core
                    .send(Envelope::notification("model/listChanged", Some(json!({}))))?;
            }
            return Ok(());
        }
        let Some(turn) = self.turn_id.clone() else {
            return Ok(());
        };
        if !self.turn_started {
            self.core.send(Envelope::notification(
                "turn/started",
                Some(json!({"threadId":session,"turn":{"id":turn}})),
            ))?;
            self.turn_started = true;
        }
        match update {
            Update::AgentMessage { item_id, text } => {
                self.last_message_id = Some(item_id.clone());
                if !self.message_text.contains_key(&item_id) && self.message_text.len() >= 64 {
                    self.message_text.clear();
                }
                let entry = self.message_text.entry(item_id.clone()).or_default();
                entry.push_str(&text);
                if entry.len() > crate::state::MESSAGE_BYTES {
                    let mut end = crate::state::MESSAGE_BYTES;
                    while end > 0 && !entry.is_char_boundary(end) {
                        end -= 1;
                    }
                    entry.truncate(end);
                }
                self.core.send(Envelope::notification("item/agentMessage/delta", Some(json!({"threadId":session,"turnId":turn,"itemId":item_id,"delta":text}))))?
            }
            Update::AgentThought { item_id, text } => self.core.send(Envelope::notification("item/reasoning/textDelta", Some(json!({"threadId":session,"turnId":turn,"itemId":item_id,"delta":text}))))?,
            Update::ToolCall {
                item_id,
                name,
                kind,
                diffs,
                input,
                mcp,
            } => {
                if self.tool_inputs.len() < 256 {
                    self.tool_inputs.insert(item_id.clone(), input);
                }
                if self.tool_presentations.len() < 256 {
                    self.tool_presentations.insert(item_id.clone(), (kind, name, diffs));
                }
                let item = if let Some(call) = mcp {
                    let item = json!({"id":item_id,"type":"mcpToolCall","server":call.server,"tool":call.tool,"arguments":call.arguments,"status":"inProgress"});
                    if self.mcp_calls.len() < 256 { self.mcp_calls.insert(item_id.clone(), *call); }
                    item
                } else { self.acp_tool_item(&item_id, "inProgress", acp_protocol::ToolContent::default()) };
                self.core.send(Envelope::notification("item/started", Some(json!({"threadId":session,"turnId":turn,"item":item}))))?;
            }
            Update::ToolCallUpdate { item_id, status, mut content } => {
                match status {
                    ToolStatus::Completed | ToolStatus::Failed | ToolStatus::Cancelled => {
                        let status = match status {
                            ToolStatus::Completed => "completed",
                            ToolStatus::Failed => "failed",
                            ToolStatus::Cancelled => "cancelled",
                            _ => unreachable!(),
                        };
                        let item = self.acp_tool_item(&item_id, status, content);
                        self.core.send(Envelope::notification("item/completed", Some(json!({"threadId":session,"turnId":turn,"item":item}))))?;
                    }
                    ToolStatus::InProgress | ToolStatus::Unknown => {
                        content.structured_output = None;
                        let item = self.acp_tool_item(&item_id, "inProgress", content);
                        self.core.send(Envelope::notification("item/started", Some(json!({"threadId":session,"turnId":turn,"item":item}))))?;
                    }
                }
            }
            Update::Usage { input_tokens, output_tokens, total_tokens, context_window } => self.core.send(Envelope::notification("thread/tokenUsage/updated", Some(json!({"threadId":session,"turnId":turn,"usage":{"inputTokens":input_tokens,"outputTokens":output_tokens,"totalTokens":total_tokens,"contextWindow":context_window}}))))?,
            Update::Unknown { .. } => {}
        }
        Ok(())
    }

    fn permission(&mut self, id: RpcId, params: Value) -> Result<(), TransportError> {
        let session = params
            .get("sessionId")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if self.session_id.as_deref() != Some(session) {
            self.acp.send(with_jsonrpc(Envelope::error_response(
                id,
                -32602,
                "ACP permission request does not belong to the active session",
            )))?;
            return Ok(());
        }
        let turn = self
            .turn_id
            .clone()
            .unwrap_or_else(|| "acp-turn-unknown".into());
        let tool = params.get("toolCall").cloned().unwrap_or(Value::Null);
        let title = tool
            .get("title")
            .and_then(Value::as_str)
            .unwrap_or("ACP permission request");
        let options = params
            .get("options")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(|item| {
                        let id = item
                            .get("optionId")
                            .or_else(|| item.get("id"))
                            .and_then(Value::as_str)?;
                        let kind = item.get("kind").and_then(Value::as_str);
                        Some((id.to_owned(), kind.map(str::to_owned)))
                    })
                    .collect()
            })
            .unwrap_or_default();
        if self.permissions.len() >= 64 {
            self.acp.send(with_jsonrpc(Envelope::error_response(
                id,
                -32000,
                "too many pending ACP permission requests",
            )))?;
            return Ok(());
        }
        self.permissions.insert(id.clone(), Permission { options });
        // 审批者必须看到要执行的内容：优先请求自带的 rawInput；dsh 只给
        // toolCallId 时，从同一轮先前的 tool_call 查回命令、理由和申请的权限。
        let mut input = acp_protocol::tool_input(&tool);
        if input.command.is_none() {
            if let Some(known) = tool
                .get("toolCallId")
                .and_then(Value::as_str)
                .and_then(|id| self.tool_inputs.get(id))
            {
                input = known.clone();
            }
        }
        let reason = match (&input.reason, &input.sandbox) {
            (Some(reason), Some(sandbox)) => format!("{reason} (requests {sandbox})"),
            (None, Some(sandbox)) => format!("Requests {sandbox}"),
            (Some(reason), None) => reason.clone(),
            (None, None) => "DeepSeek ACP permission request".into(),
        };
        let mut request = json!({
            "threadId": session,
            "turnId": turn,
            "command": input.command.as_deref().unwrap_or(title),
            "reason": reason,
            "kind": "command",
            "availableDecisions": ["accept", "decline", "cancel"],
        });
        if let Some(cwd) = input.cwd {
            request["cwd"] = Value::String(cwd);
        }
        self.core.send(Envelope::request(
            id,
            "item/commandExecution/requestApproval",
            Some(request),
        ))?;
        Ok(())
    }
}

fn with_jsonrpc(mut envelope: Envelope) -> Envelope {
    envelope.jsonrpc = Some("2.0".into());
    envelope
}

fn id_text(id: &RpcId) -> String {
    match id {
        RpcId::Number(number) => number.to_string(),
        RpcId::String(value) => value.clone(),
    }
}

/// 按决定选择 ACP 权限选项：优先标准 kind（once 优先于 always），其次按 optionId
/// 关键词。拒绝只会选拒绝类选项；没有匹配时返回 None，由调用方回复 cancelled。
fn choose_permission_option(
    decision: &str,
    options: &[(String, Option<String>)],
) -> Option<String> {
    let (kind_prefix, allowed, forbidden): (&str, &[&str], &[&str]) = match decision {
        "accept" => (
            "allow",
            &["allow", "accept", "yes"],
            &["reject", "deny", "decline"],
        ),
        "decline" => (
            "reject",
            &["reject", "deny", "decline"],
            &["allow", "accept"],
        ),
        _ => return None,
    };
    let by_kind = |suffix: &str| {
        options.iter().find(|(_, kind)| {
            kind.as_deref()
                .is_some_and(|kind| kind.starts_with(kind_prefix) && kind.ends_with(suffix))
        })
    };
    by_kind("once")
        .or_else(|| by_kind(""))
        .or_else(|| {
            options.iter().find(|(id, kind)| {
                let id = id.to_ascii_lowercase();
                kind.is_none()
                    && allowed.iter().any(|word| id.contains(word))
                    && !forbidden.iter().any(|word| id.contains(word))
            })
        })
        .map(|(id, _)| id.clone())
}

#[cfg(test)]
mod tests {
    use super::{choose_permission_option, AcpBridge};
    use crate::app_server;
    use crate::config::Config;
    use crate::protocol::{Envelope, RpcId};
    use crate::transport::PipeTransport;
    use serde_json::json;
    use std::time::Duration;

    fn pair() -> (PipeTransport, PipeTransport) {
        let (a, b) = tokio::io::duplex(64 * 1024);
        let (ar, aw) = tokio::io::split(a);
        let (br, bw) = tokio::io::split(b);
        (PipeTransport::new(ar, aw), PipeTransport::new(br, bw))
    }

    #[test]
    fn a_failed_start_is_explained_by_the_last_error_line_only() {
        let output = b"file:///C:/x/lib/index.js:974\n\t\tif (x) throw new Error(`nope`);\n\t\t      ^\n\nError: dsh: profile \"zf\" does not exist; create it with 'dsh plugin --profile zf add <package>'\n    at loadProfile (file:///C:/x/lib/index.js:974:34)\n    at main (file:///C:/x/bin.js:1:1)\n\nNode.js v24.16.0\n";
        assert_eq!(
            super::startup_failure(output).as_deref(),
            Some("dsh: profile \"zf\" does not exist; create it with 'dsh plugin --profile zf add <package>'")
        );
        // 没有 Error 行时取最后一行非堆栈内容；控制字符去掉，长度有上限。
        assert_eq!(
            super::startup_failure(
                b"warming up\n\x1b[31mcannot bind port\x07\n    at listen (x.js:1:1)\n"
            )
            .as_deref(),
            Some("[31mcannot bind port")
        );
        let long = format!("Error: {}", "x".repeat(1000));
        assert_eq!(super::startup_failure(long.as_bytes()).unwrap().len(), 300);
        assert_eq!(super::startup_failure(b""), None);
        assert_eq!(super::startup_failure(b"\n   \n"), None);
    }

    #[tokio::test]
    async fn an_agent_that_exits_during_the_handshake_fails_the_handshake_with_its_own_words() {
        let (mut core_client, core_bridge) = pair();
        let (mut acp_server, acp_bridge) = pair();
        let mut bridge = AcpBridge::new(core_bridge, acp_bridge, &Config::default());
        bridge.startup_stderr = Some(std::sync::Arc::new(std::sync::Mutex::new(
            b"Error: dsh: profile \"zf\" does not exist\n    at loadProfile (x.js:1:1)\n".to_vec(),
        )));
        let bridge = tokio::spawn(bridge.run());
        core_client
            .send(Envelope::request(
                RpcId::Number(1),
                "initialize",
                Some(json!({"protocolVersion": 1})),
            ))
            .unwrap();
        acp_server.recv().await.unwrap();
        drop(acp_server);
        let answer = tokio::time::timeout(Duration::from_secs(3), core_client.recv())
            .await
            .expect("the handshake was left unanswered")
            .unwrap();
        assert_eq!(answer.id, Some(RpcId::Number(1)));
        assert_eq!(
            answer.error.unwrap()["message"],
            "dsh exited before a session started: dsh: profile \"zf\" does not exist"
        );
        let _ = tokio::time::timeout(Duration::from_secs(2), bridge).await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_reload_that_exits_during_the_handshake_answers_the_original_request() {
        for stage in 0..3 {
            let (mut core_client, core_bridge) = pair();
            let (_first, acp_bridge) = pair();
            let (mut restarted, restarted_bridge) = pair();
            let mut bridge = AcpBridge::new(core_bridge, acp_bridge, &Config::default());
            bridge.session_id = Some("saved-session".into());
            bridge.startup_stderr = Some(std::sync::Arc::new(std::sync::Mutex::new(
                b"Error: dsh: profile no longer exists\n".to_vec(),
            )));
            let mut spare = Some(restarted_bridge);
            bridge.respawn = Some(Box::new(move || Ok((spare.take().unwrap(), None))));
            let bridge = tokio::spawn(bridge.run());
            core_client
                .send(crate::app_server::backend_reload(RpcId::Number(7)))
                .unwrap();
            let initialize = restarted.recv().await.unwrap();
            if stage >= 1 {
                restarted
                    .send(Envelope::response(initialize.id.unwrap(), Some(json!({}))))
                    .unwrap();
                let resume = restarted.recv().await.unwrap();
                if stage == 2 {
                    restarted
                        .send(Envelope::error_response(
                            resume.id.unwrap(),
                            -32002,
                            "unknown session",
                        ))
                        .unwrap();
                    let new_session = restarted.recv().await.unwrap();
                    assert_eq!(new_session.method.as_deref(), Some("session/new"));
                }
            }
            drop(restarted);
            let answer = tokio::time::timeout(Duration::from_secs(3), core_client.recv())
                .await
                .expect("the reload was left unanswered")
                .expect("the reload lost its failure response");
            assert_eq!(answer.id, Some(RpcId::Number(7)));
            assert_eq!(
                answer.error.unwrap()["message"],
                "dsh exited while restarting the session: dsh: profile no longer exists"
            );
            tokio::time::timeout(Duration::from_secs(2), bridge)
                .await
                .unwrap()
                .unwrap();
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_answered_permission_is_reported_resolved_so_a_later_one_can_be_answered() {
        let (mut core_client, core_bridge) = pair();
        let (mut acp_server, acp_bridge) = pair();
        let bridge =
            tokio::spawn(AcpBridge::new(core_bridge, acp_bridge, &Config::default()).run());
        core_client
            .send(Envelope::request(
                RpcId::Number(1),
                "initialize",
                Some(json!({"protocolVersion": 1})),
            ))
            .unwrap();
        acp_server.recv().await.unwrap();
        acp_server
            .send(Envelope::response(
                RpcId::Number(1),
                Some(json!({"protocolVersion": 1})),
            ))
            .unwrap();
        core_client.recv().await.unwrap();
        core_client
            .send(app_server::thread_start(
                RpcId::Number(2),
                &Config::default(),
            ))
            .unwrap();
        acp_server.recv().await.unwrap();
        acp_server
            .send(Envelope::response(
                RpcId::Number(2),
                Some(json!({"sessionId":"s"})),
            ))
            .unwrap();
        core_client.recv().await.unwrap();

        // 同一轮里先后两个审批：第一个答复后必须被报告为已解决。
        for (request, decision, expected) in [
            (
                70,
                "accept",
                json!({"outcome":"selected","optionId":"allow-once"}),
            ),
            (
                71,
                "decline",
                json!({"outcome":"selected","optionId":"reject-once"}),
            ),
        ] {
            acp_server
                .send(Envelope::request(
                    RpcId::Number(request),
                    "session/request_permission",
                    Some(json!({"sessionId":"s","toolCall":{"toolCallId":"t","title":"Run","rawInput":{"command":"echo hi"}},
                        "options":[{"optionId":"allow-once","kind":"allow_once"},{"optionId":"reject-once","kind":"reject_once"}]})),
                ))
                .unwrap();
            let asked = tokio::time::timeout(Duration::from_secs(2), core_client.recv())
                .await
                .expect("the permission request did not reach Core")
                .unwrap();
            assert_eq!(asked.id, Some(RpcId::Number(request)));
            core_client
                .send(Envelope::response(
                    RpcId::Number(request),
                    Some(json!({"decision": decision})),
                ))
                .unwrap();
            let answered = acp_server.recv().await.unwrap();
            assert_eq!(answered.id, Some(RpcId::Number(request)));
            assert_eq!(answered.result.unwrap()["outcome"], expected);
            let resolved = core_client.recv().await.unwrap();
            assert_eq!(resolved.method.as_deref(), Some("serverRequest/resolved"));
            assert_eq!(
                resolved.params.unwrap(),
                json!({"threadId":"s","requestId":request})
            );
        }

        drop(core_client);
        let _ = tokio::time::timeout(Duration::from_secs(2), bridge).await;
    }

    #[test]
    fn permission_decisions_prefer_matching_acp_option_ids() {
        let dsh = [
            ("allow-once".to_owned(), Some("allow_once".to_owned())),
            ("allow-always".to_owned(), Some("allow_always".to_owned())),
            ("reject-once".to_owned(), Some("reject_once".to_owned())),
        ];
        assert_eq!(
            choose_permission_option("accept", &dsh).as_deref(),
            Some("allow-once")
        );
        // dsh 的拒绝选项叫 reject-once；曾因只匹配 deny/decline 而回退到 allow-once。
        assert_eq!(
            choose_permission_option("decline", &dsh).as_deref(),
            Some("reject-once")
        );
        assert_eq!(choose_permission_option("cancel", &dsh), None);
        let untyped = [("allow_once".to_owned(), None), ("deny".to_owned(), None)];
        assert_eq!(
            choose_permission_option("decline", &untyped).as_deref(),
            Some("deny")
        );
        // 没有拒绝类选项时不能选中批准选项。
        let allow_only = [("allow-once".to_owned(), Some("allow_once".to_owned()))];
        assert_eq!(choose_permission_option("decline", &allow_only), None);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn session_list_and_history_are_asked_of_the_bridge_plugin_and_answers_return_as_given() {
        let (mut core_client, core_bridge) = pair();
        let (mut acp_server, acp_bridge) = pair();
        let config = Config::default();
        let bridge = tokio::spawn(AcpBridge::new(core_bridge, acp_bridge, &config).run());

        core_client
            .send(app_server::thread_list(RpcId::Number(7), &config.cwd))
            .unwrap();
        let list = tokio::time::timeout(Duration::from_secs(2), acp_server.recv())
            .await
            .expect("bridge did not forward the list")
            .unwrap();
        assert_eq!(list.jsonrpc.as_deref(), Some("2.0"));
        assert_eq!(list.method.as_deref(), Some("zf/session/list"));
        assert_eq!(
            list.params.unwrap()["cwd"],
            config.cwd.display().to_string()
        );
        acp_server
            .send(Envelope::response(
                RpcId::Number(7),
                Some(json!({"sessions":[{"id":"s","title":"t"}]})),
            ))
            .unwrap();
        let answer = core_client.recv().await.unwrap();
        assert_eq!(answer.id, Some(RpcId::Number(7)));
        assert_eq!(answer.result.unwrap()["sessions"][0]["title"], "t");

        core_client
            .send(app_server::thread_history(RpcId::Number(8), "s"))
            .unwrap();
        let history = acp_server.recv().await.unwrap();
        assert_eq!(history.method.as_deref(), Some("zf/session/history"));
        assert_eq!(history.params.unwrap()["sessionId"], "s");
        // 插件的拒绝原样回到 Core，不被改写成成功。
        acp_server
            .send(Envelope::error_response(
                RpcId::Number(8),
                -32603,
                "session \"s\" not found",
            ))
            .unwrap();
        let refused = core_client.recv().await.unwrap();
        assert_eq!(refused.id, Some(RpcId::Number(8)));
        assert!(refused.result.is_none());
        assert_eq!(refused.error.unwrap()["code"], -32603);

        drop(core_client);
        let _ = tokio::time::timeout(Duration::from_secs(2), bridge).await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_provider_is_saved_through_the_plugin_and_changed_models_reach_the_core() {
        let (mut core_client, core_bridge) = pair();
        let (mut acp_server, acp_bridge) = pair();
        let bridge =
            tokio::spawn(AcpBridge::new(core_bridge, acp_bridge, &Config::default()).run());
        let options = |models: &[&str]| {
            json!([{"id":"model","currentValue":"[\"hi\",\"luna\"]","options": models
                .iter()
                .map(|model| json!({"value":format!("[\"hi\",\"{model}\"]"),"name":model}))
                .collect::<Vec<_>>()}])
        };
        core_client
            .send(Envelope::request(
                RpcId::Number(1),
                "initialize",
                Some(json!({"protocolVersion": 1})),
            ))
            .unwrap();
        acp_server.recv().await.unwrap();
        acp_server
            .send(Envelope::response(
                RpcId::Number(1),
                Some(json!({"protocolVersion": 1})),
            ))
            .unwrap();
        core_client.recv().await.unwrap();
        core_client
            .send(app_server::thread_start(
                RpcId::Number(2),
                &Config::default(),
            ))
            .unwrap();
        acp_server.recv().await.unwrap();
        acp_server
            .send(Envelope::response(
                RpcId::Number(2),
                Some(json!({"sessionId":"s","configOptions":options(&["luna"])})),
            ))
            .unwrap();
        core_client.recv().await.unwrap();

        let spec = crate::backend::dsh_config::ProviderSpec {
            provider: "my-api".into(),
            base_url: "https://api.example.com/v1".into(),
            api: "openai-completions".into(),
            api_key: crate::backend::dsh_config::Secret("sk-test-1".into()),
            model: "big".into(),
            ..Default::default()
        };
        core_client
            .send(app_server::provider_save(RpcId::Number(3), &spec))
            .unwrap();
        let save = tokio::time::timeout(Duration::from_secs(2), acp_server.recv())
            .await
            .expect("bridge did not forward the save")
            .unwrap();
        assert_eq!(save.method.as_deref(), Some("zf/provider/save"));
        let params = save.params.unwrap();
        assert_eq!(params["provider"], "my-api");
        assert_eq!(params["credential"], "MY_API_API_KEY");
        assert_eq!(params["apiKey"], "sk-test-1");
        // dsh 先推送新的可选模型，再答复保存。
        acp_server
            .send(Envelope::notification(
                "session/update",
                Some(json!({"sessionId":"s","update":{"sessionUpdate":"config_option_update","configOptions":options(&["luna","big"])}})),
            ))
            .unwrap();
        acp_server
            .send(Envelope::response(
                RpcId::Number(3),
                Some(json!({"label":"my-api/big","credential":"MY_API_API_KEY","keyStored":true})),
            ))
            .unwrap();
        let changed = core_client.recv().await.unwrap();
        assert_eq!(changed.method.as_deref(), Some("model/listChanged"));
        let saved = core_client.recv().await.unwrap();
        assert_eq!(saved.result.unwrap()["label"], "my-api/big");

        core_client
            .send(app_server::model_list(RpcId::Number(4)))
            .unwrap();
        let listed = core_client.recv().await.unwrap().result.unwrap();
        assert_eq!(listed["data"].as_array().unwrap().len(), 2);
        // 内容没变的推送不再打扰 Core；别的会话的推送不算数。
        for session in ["s", "other"] {
            acp_server
                .send(Envelope::notification(
                    "session/update",
                    Some(json!({"sessionId":session,"update":{"sessionUpdate":"config_option_update","configOptions":options(&["luna","big"])}})),
                ))
                .unwrap();
        }
        // 权限预设属于会话：桥接层补上自己的会话 id，其余参数原样转发。
        core_client
            .send(app_server::permission_set(
                RpcId::Number(9),
                "read-only",
                true,
            ))
            .unwrap();
        let set = acp_server.recv().await.unwrap();
        assert_eq!(set.method.as_deref(), Some("zf/permission/set"));
        assert_eq!(
            set.params.unwrap(),
            json!({"preset":"read-only","remember":true,"sessionId":"s"})
        );
        acp_server
            .send(Envelope::response(
                RpcId::Number(9),
                Some(json!({"current":"read-only","presets":[]})),
            ))
            .unwrap();
        assert_eq!(
            core_client.recv().await.unwrap().result.unwrap()["current"],
            "read-only"
        );
        core_client
            .send(app_server::permission_describe(RpcId::Number(10)))
            .unwrap();
        let describe = acp_server.recv().await.unwrap();
        assert_eq!(describe.method.as_deref(), Some("zf/permission/describe"));
        assert_eq!(describe.params.unwrap(), json!({"sessionId":"s"}));
        acp_server
            .send(Envelope::error_response(
                RpcId::Number(10),
                -32603,
                "this dsh profile mounts no permission presets",
            ))
            .unwrap();
        assert!(core_client.recv().await.unwrap().error.is_some());

        core_client
            .send(app_server::provider_list(RpcId::Number(5)))
            .unwrap();
        assert_eq!(
            acp_server.recv().await.unwrap().method.as_deref(),
            Some("zf/provider/list")
        );
        acp_server
            .send(Envelope::response(
                RpcId::Number(5),
                Some(json!({"providers":[]})),
            ))
            .unwrap();
        assert_eq!(core_client.recv().await.unwrap().id, Some(RpcId::Number(5)));

        drop(core_client);
        let _ = tokio::time::timeout(Duration::from_secs(2), bridge).await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn skills_come_from_the_plugin_catalog_in_the_inventory_shape_core_reads() {
        let (mut core_client, core_bridge) = pair();
        let (mut acp_server, acp_bridge) = pair();
        let bridge =
            tokio::spawn(AcpBridge::new(core_bridge, acp_bridge, &Config::default()).run());
        let cwd = std::path::Path::new("C:/workspace");

        core_client
            .send(app_server::skills_list(RpcId::Number(4), cwd, false))
            .unwrap();
        let request = tokio::time::timeout(Duration::from_secs(2), acp_server.recv())
            .await
            .expect("bridge did not ask for skills")
            .unwrap();
        assert_eq!(request.method.as_deref(), Some("zf/skills/list"));
        assert_eq!(request.params.unwrap()["cwd"], "C:/workspace");
        acp_server
            .send(Envelope::response(
                RpcId::Number(4),
                Some(json!({"skills":[
                    {"name":"review","description":"d","source":"project-dsh","path":"C:/workspace/.dsh/skills/review/SKILL.md","modelInvocable":true,"userInvocable":true},
                    {"name":"mine","source":"user-agents","path":"C:/home/skills/mine/SKILL.md","modelInvocable":false,"userInvocable":true},
                    {"name":"virtual","source":"bundled","modelInvocable":false,"userInvocable":false},
                    {"source":"bundled"},
                ]})),
            ))
            .unwrap();
        let answer = core_client.recv().await.unwrap().result.unwrap();
        let parsed = crate::skills::parse_result(&answer, "C:/workspace").unwrap();
        assert_eq!(
            parsed.availability,
            crate::skills::SkillAvailability::Available
        );
        assert_eq!((parsed.skill_count, parsed.enabled_count), (3, 2));
        let listed: Vec<(&str, &str, &str, bool)> = parsed
            .entries
            .iter()
            .map(|skill| {
                (
                    skill.name.as_str(),
                    skill.scope.as_str(),
                    skill.path.as_str(),
                    skill.enabled,
                )
            })
            .collect();
        assert!(listed.contains(&(
            "review",
            "repo",
            "C:/workspace/.dsh/skills/review/SKILL.md",
            true
        )));
        assert!(listed.contains(&("mine", "user", "C:/home/skills/mine/SKILL.md", true)));
        assert!(listed.contains(&("virtual", "system", "dsh:bundled/virtual", false)));

        // 插件没有技能注册表时的拒绝原样到达；缺清单字段的答复不算“没有技能”。
        core_client
            .send(app_server::skills_list(RpcId::Number(5), cwd, true))
            .unwrap();
        acp_server.recv().await.unwrap();
        acp_server
            .send(Envelope::error_response(
                RpcId::Number(5),
                -32603,
                "this dsh profile mounts no skill registry",
            ))
            .unwrap();
        assert_eq!(
            core_client.recv().await.unwrap().error.unwrap()["code"],
            -32603
        );
        core_client
            .send(app_server::skills_list(RpcId::Number(6), cwd, true))
            .unwrap();
        acp_server.recv().await.unwrap();
        acp_server
            .send(Envelope::response(RpcId::Number(6), Some(json!({}))))
            .unwrap();
        let malformed = core_client.recv().await.unwrap().result.unwrap();
        assert!(crate::skills::parse_result(&malformed, "C:/workspace").is_none());

        drop(core_client);
        let _ = tokio::time::timeout(Duration::from_secs(2), bridge).await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn bridge_maps_acp_lifecycle_and_permission_without_model_calls() {
        let (mut core_client, core_bridge) = pair();
        let (mut acp_server, acp_bridge) = pair();
        let bridge =
            tokio::spawn(AcpBridge::new(core_bridge, acp_bridge, &Config::default()).run());

        core_client
            .send(Envelope::request(
                RpcId::Number(1),
                "initialize",
                Some(json!({"protocolVersion": 1})),
            ))
            .unwrap();
        let initialize = tokio::time::timeout(Duration::from_secs(2), acp_server.recv())
            .await
            .expect("bridge did not forward initialize")
            .unwrap();
        assert_eq!(initialize.jsonrpc.as_deref(), Some("2.0"));
        assert_eq!(initialize.method.as_deref(), Some("initialize"));
        acp_server
            .send(Envelope::response(
                RpcId::Number(1),
                Some(json!({"protocolVersion": 1})),
            ))
            .unwrap();
        assert_eq!(core_client.recv().await.unwrap().id, Some(RpcId::Number(1)));

        core_client
            .send(app_server::thread_start(
                RpcId::Number(2),
                &Config::default(),
            ))
            .unwrap();
        assert_eq!(
            acp_server.recv().await.unwrap().method.as_deref(),
            Some("session/new")
        );
        acp_server
            .send(Envelope::response(
                RpcId::Number(2),
                Some(json!({"sessionId": "session-1"})),
            ))
            .unwrap();
        assert_eq!(
            core_client.recv().await.unwrap().result.unwrap()["thread"]["id"],
            "session-1"
        );

        core_client
            .send(app_server::turn_start(
                RpcId::Number(3),
                "session-1",
                "hello",
                None,
            ))
            .unwrap();
        assert_eq!(
            core_client.recv().await.unwrap().method.as_deref(),
            Some("turn/started")
        );
        assert_eq!(
            acp_server.recv().await.unwrap().method.as_deref(),
            Some("session/prompt")
        );
        // turn/start 在 prompt 发出后立即确认，不等整轮结束。
        let ack = core_client.recv().await.unwrap();
        assert_eq!(ack.id, Some(RpcId::Number(3)));
        assert_eq!(ack.result.unwrap()["turn"]["id"], "acp-turn-3");
        acp_server
            .send(Envelope::notification(
                "session/update",
                Some(json!({
                    "sessionId":"session-1",
                    "update":{"sessionUpdate":"agent_message_chunk","messageId":"m","content":{"type":"text","text":"hello"}}
                })),
            ))
            .unwrap();
        acp_server
            .send(Envelope::request(
                RpcId::String("permission".into()),
                "session/request_permission",
                Some(json!({
                    "sessionId":"session-1",
                    "toolCall":{"title":"run tests"},
                    "options":[{"optionId":"allow_once"},{"optionId":"reject_once"}]
                })),
            ))
            .unwrap();
        let delta = core_client.recv().await.unwrap();
        assert_eq!(delta.method.as_deref(), Some("item/agentMessage/delta"));
        let permission = core_client.recv().await.unwrap();
        assert_eq!(permission.id, Some(RpcId::String("permission".into())));
        core_client
            .send(Envelope::response(
                RpcId::String("permission".into()),
                Some(json!({"decision":"accept"})),
            ))
            .unwrap();
        let permission_response = acp_server.recv().await.unwrap();
        assert_eq!(permission_response.jsonrpc.as_deref(), Some("2.0"));
        assert_eq!(
            permission_response.result.unwrap()["outcome"]["optionId"],
            "allow_once"
        );
        // 答复交给 agent 后，Core 被告知这条请求已解决。
        let resolved = core_client.recv().await.unwrap();
        assert_eq!(resolved.method.as_deref(), Some("serverRequest/resolved"));
        assert_eq!(resolved.params.unwrap()["requestId"], "permission");
        acp_server
            .send(Envelope::response(
                RpcId::Number(3),
                Some(json!({"stopReason":"end_turn"})),
            ))
            .unwrap();
        assert_eq!(
            core_client.recv().await.unwrap().method.as_deref(),
            Some("item/completed")
        );
        assert_eq!(
            core_client.recv().await.unwrap().method.as_deref(),
            Some("turn/completed")
        );

        drop(core_client);
        let _ = tokio::time::timeout(Duration::from_secs(2), bridge).await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn prompt_error_response_becomes_a_failed_turn_not_an_unknown_rpc() {
        let (mut core_client, core_bridge) = pair();
        let (mut acp_server, acp_bridge) = pair();
        let bridge =
            tokio::spawn(AcpBridge::new(core_bridge, acp_bridge, &Config::default()).run());
        core_client
            .send(app_server::thread_start(
                RpcId::Number(2),
                &Config::default(),
            ))
            .unwrap();
        acp_server.recv().await.unwrap();
        acp_server
            .send(Envelope::response(
                RpcId::Number(2),
                Some(json!({"sessionId": "session-1"})),
            ))
            .unwrap();
        core_client.recv().await.unwrap();
        core_client
            .send(app_server::turn_start(
                RpcId::Number(3),
                "session-1",
                "hello",
                None,
            ))
            .unwrap();
        assert_eq!(
            core_client.recv().await.unwrap().method.as_deref(),
            Some("turn/started")
        );
        acp_server.recv().await.unwrap();
        let response = core_client.recv().await.unwrap();
        assert_eq!(response.id, Some(RpcId::Number(3)));
        assert!(response.error.is_none());
        acp_server
            .send(Envelope::error_response(
                RpcId::Number(3),
                -32603,
                "Internal error: turn failed: Insufficient Balance",
            ))
            .unwrap();
        let completed = core_client.recv().await.unwrap();
        assert_eq!(completed.method.as_deref(), Some("turn/completed"));
        let turn = &completed.params.unwrap()["turn"];
        assert_eq!(turn["status"], "failed");
        assert_eq!(
            turn["error"]["message"],
            "Internal error: turn failed: Insufficient Balance"
        );
        drop(core_client);
        let _ = tokio::time::timeout(Duration::from_secs(2), bridge).await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn requested_model_is_selected_and_previous_reasoning_effort_restored() {
        let (mut core_client, core_bridge) = pair();
        let (mut acp_server, acp_bridge) = pair();
        let config = Config {
            model: Some("hi/gpt-6-astra".into()),
            ..Config::default()
        };
        let bridge = tokio::spawn(AcpBridge::new(core_bridge, acp_bridge, &config).run());
        core_client
            .send(app_server::thread_start(RpcId::Number(2), &config))
            .unwrap();
        acp_server.recv().await.unwrap();
        acp_server
            .send(Envelope::response(
                RpcId::Number(2),
                Some(json!({"sessionId":"s","configOptions":[
                    {"id":"model","currentValue":"[\"deepseek-official\",\"flash\"]","options":[
                        {"group":"hi","options":[{"value":"[\"hi\",\"gpt-6-astra\"]","name":"GPT-6 Astra"}]}]},
                    {"id":"reasoning_effort","currentValue":"high","options":[{"value":"off"},{"value":"high"}]}]})),
            ))
            .unwrap();
        let select = acp_server.recv().await.unwrap();
        assert_eq!(select.method.as_deref(), Some("session/set_config_option"));
        let params = select.params.unwrap();
        assert_eq!(params["configId"], "model");
        assert_eq!(params["value"], "[\"hi\",\"gpt-6-astra\"]");
        acp_server
            .send(Envelope::response(
                select.id.unwrap(),
                Some(json!({"configOptions":[
                    {"id":"reasoning_effort","currentValue":"","options":[{"value":""},{"value":"high"}]}]})),
            ))
            .unwrap();
        let restore = acp_server.recv().await.unwrap();
        let params = restore.params.unwrap();
        assert_eq!(params["configId"], "reasoning_effort");
        assert_eq!(params["value"], "high");
        acp_server
            .send(Envelope::response(restore.id.unwrap(), Some(json!({}))))
            .unwrap();
        let started = core_client.recv().await.unwrap();
        assert_eq!(started.id, Some(RpcId::Number(2)));
        let result = started.result.unwrap();
        assert_eq!(result["thread"]["id"], "s");
        assert_eq!(result["model"], "hi/gpt-6-astra");
        drop(core_client);
        let _ = tokio::time::timeout(Duration::from_secs(2), bridge).await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn session_models_are_listed_and_switched_on_request() {
        let (mut core_client, core_bridge) = pair();
        let (mut acp_server, acp_bridge) = pair();
        let config = Config::default();
        let bridge = tokio::spawn(AcpBridge::new(core_bridge, acp_bridge, &config).run());
        core_client
            .send(app_server::thread_start(RpcId::Number(2), &config))
            .unwrap();
        acp_server.recv().await.unwrap();
        acp_server
            .send(Envelope::response(
                RpcId::Number(2),
                Some(json!({"sessionId":"s","configOptions":[
                    {"id":"model","currentValue":"[\"deepseek-official\",\"flash\"]","options":[
                        {"group":"official","options":[{"value":"[\"deepseek-official\",\"flash\"]","name":"Flash"}]},
                        {"group":"hi","options":[{"value":"[\"hi\",\"gpt-6-luna\"]","name":"GPT-6 Luna"}]}]}]})),
            ))
            .unwrap();
        let started = core_client.recv().await.unwrap().result.unwrap();
        assert_eq!(started["model"], "deepseek-official/flash");

        core_client
            .send(app_server::model_list(RpcId::Number(3)))
            .unwrap();
        let listed = core_client.recv().await.unwrap().result.unwrap();
        assert_eq!(
            listed["data"],
            json!([
                {"model":"deepseek-official/flash","displayName":"Flash"},
                {"model":"hi/gpt-6-luna","displayName":"GPT-6 Luna"}
            ])
        );

        core_client
            .send(app_server::model_select(RpcId::Number(4), "missing"))
            .unwrap();
        let rejected = core_client.recv().await.unwrap();
        assert_eq!(rejected.id, Some(RpcId::Number(4)));
        assert!(rejected.error.unwrap()["message"]
            .as_str()
            .unwrap()
            .contains("available: deepseek-official/flash, hi/gpt-6-luna"));

        core_client
            .send(app_server::model_select(RpcId::Number(5), "hi/gpt-6-luna"))
            .unwrap();
        let select = acp_server.recv().await.unwrap();
        assert_eq!(select.method.as_deref(), Some("session/set_config_option"));
        let params = select.params.unwrap();
        assert_eq!(params["configId"], "model");
        assert_eq!(params["value"], "[\"hi\",\"gpt-6-luna\"]");
        acp_server
            .send(Envelope::response(select.id.unwrap(), Some(json!({}))))
            .unwrap();
        let switched = core_client.recv().await.unwrap();
        assert_eq!(switched.id, Some(RpcId::Number(5)));
        assert_eq!(switched.result.unwrap()["model"], "hi/gpt-6-luna");
        drop(core_client);
        let _ = tokio::time::timeout(Duration::from_secs(2), bridge).await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn reload_moves_the_session_to_a_new_process_and_keeps_the_chosen_model() {
        let options = |extra: &str| {
            let mut hi = vec![json!({"value":"[\"hi\",\"luna\"]","name":"Luna"})];
            if !extra.is_empty() {
                hi.push(json!({"value":format!("[\"hi\",\"{extra}\"]"),"name":extra}));
            }
            json!([{"id":"model","currentValue":"[\"official\",\"flash\"]","options":[
                {"group":"official","options":[{"value":"[\"official\",\"flash\"]","name":"Flash"}]},
                {"group":"hi","options":hi}]}])
        };
        let (mut core_client, core_bridge) = pair();
        let (mut first, acp_bridge) = pair();
        // 每次重载交出一条新的连接，代表新启动的进程。
        let (second, second_bridge) = pair();
        let (third, third_bridge) = pair();
        let mut spare = vec![third_bridge, second_bridge];
        let config = Config {
            model: Some("hi/luna".into()),
            ..Config::default()
        };
        let mut bridge = AcpBridge::new(core_bridge, acp_bridge, &config);
        bridge.respawn = Some(Box::new(move || {
            spare
                .pop()
                .map(|pipe| (pipe, None))
                .ok_or_else(|| std::io::Error::other("no more processes"))
        }));
        let bridge = tokio::spawn(bridge.run());

        core_client
            .send(app_server::backend_reload(RpcId::Number(1)))
            .unwrap();
        let early = core_client.recv().await.unwrap();
        assert!(early.error.unwrap()["message"]
            .as_str()
            .unwrap()
            .starts_with("dsh was not restarted: ACP session is not ready"));

        core_client
            .send(app_server::thread_start(RpcId::Number(2), &config))
            .unwrap();
        first.recv().await.unwrap();
        first
            .send(Envelope::response(
                RpcId::Number(2),
                Some(json!({"sessionId":"s","configOptions":options("")})),
            ))
            .unwrap();
        let select = first.recv().await.unwrap();
        first
            .send(Envelope::response(select.id.unwrap(), Some(json!({}))))
            .unwrap();
        assert_eq!(
            core_client.recv().await.unwrap().result.unwrap()["model"],
            "hi/luna"
        );

        // 重载：新进程 initialize → session/resume → 恢复之前选的模型。
        let mut second = second;
        core_client
            .send(app_server::backend_reload(RpcId::Number(3)))
            .unwrap();
        let initialize = second.recv().await.unwrap();
        assert_eq!(initialize.method.as_deref(), Some("initialize"));
        second
            .send(Envelope::response(initialize.id.unwrap(), Some(json!({}))))
            .unwrap();
        let resume = second.recv().await.unwrap();
        assert_eq!(resume.method.as_deref(), Some("session/resume"));
        assert_eq!(resume.params.as_ref().unwrap()["sessionId"], "s");
        second
            .send(Envelope::response(
                resume.id.unwrap(),
                Some(json!({"configOptions":options("sol")})),
            ))
            .unwrap();
        let select = second.recv().await.unwrap();
        assert_eq!(select.method.as_deref(), Some("session/set_config_option"));
        assert_eq!(
            select.params.as_ref().unwrap()["value"],
            "[\"hi\",\"luna\"]"
        );
        second
            .send(Envelope::response(select.id.unwrap(), Some(json!({}))))
            .unwrap();
        let reloaded = core_client.recv().await.unwrap();
        assert_eq!(reloaded.id, Some(RpcId::Number(3)));
        let result = reloaded.result.unwrap();
        assert_eq!(result["thread"]["id"], "s");
        assert_eq!(result["model"], "hi/luna");
        // 旧进程的输入已关闭；新清单里有重载后才出现的模型。
        assert!(first.recv().await.is_err());
        core_client
            .send(app_server::model_list(RpcId::Number(4)))
            .unwrap();
        let listed = core_client.recv().await.unwrap().result.unwrap();
        assert_eq!(listed["data"][2]["model"], "hi/sol");

        // 新进程不能恢复会话时改为新建，并把新的会话 id 告诉 Core。
        let mut third = third;
        core_client
            .send(app_server::backend_reload(RpcId::Number(5)))
            .unwrap();
        let initialize = third.recv().await.unwrap();
        third
            .send(Envelope::response(initialize.id.unwrap(), Some(json!({}))))
            .unwrap();
        let resume = third.recv().await.unwrap();
        third
            .send(Envelope::error_response(
                resume.id.unwrap(),
                -32002,
                "unknown session",
            ))
            .unwrap();
        let created = third.recv().await.unwrap();
        assert_eq!(created.method.as_deref(), Some("session/new"));
        third
            .send(Envelope::response(
                created.id.unwrap(),
                Some(json!({"sessionId":"s2","configOptions":options("")})),
            ))
            .unwrap();
        let select = third.recv().await.unwrap();
        assert_eq!(select.params.as_ref().unwrap()["sessionId"], "s2");
        third
            .send(Envelope::response(select.id.unwrap(), Some(json!({}))))
            .unwrap();
        let result = core_client.recv().await.unwrap().result.unwrap();
        assert_eq!(result["thread"]["id"], "s2");
        assert_eq!(result["model"], "hi/luna");

        // 在当前进程里恢复另一个已保存的会话；被占用时原会话不变。
        core_client
            .send(app_server::thread_resume(RpcId::Number(20), "busy"))
            .unwrap();
        let resume = third.recv().await.unwrap();
        assert_eq!(resume.method.as_deref(), Some("session/resume"));
        assert_eq!(resume.params.as_ref().unwrap()["sessionId"], "busy");
        third
            .send(Envelope {
                jsonrpc: None,
                id: resume.id,
                method: None,
                params: None,
                result: None,
                error: Some(json!({"code":-32603,"message":"Internal error","data":{
                    "details":"session \"busy\" is already owned by an active write handle"}})),
            })
            .unwrap();
        let refused = core_client.recv().await.unwrap();
        assert_eq!(refused.id, Some(RpcId::Number(20)));
        assert!(refused.error.unwrap()["message"]
            .as_str()
            .unwrap()
            .contains("open in another terminal"));
        core_client
            .send(app_server::thread_resume(RpcId::Number(21), "older"))
            .unwrap();
        let resume = third.recv().await.unwrap();
        assert_eq!(resume.params.as_ref().unwrap()["sessionId"], "older");
        third
            .send(Envelope::response(
                resume.id.unwrap(),
                Some(json!({"configOptions":options("")})),
            ))
            .unwrap();
        let close = third.recv().await.unwrap();
        assert_eq!(close.method.as_deref(), Some("session/close"));
        assert_eq!(close.params.as_ref().unwrap()["sessionId"], "s2");
        third
            .send(Envelope::response(close.id.unwrap(), Some(json!({}))))
            .unwrap();
        let select = third.recv().await.unwrap();
        assert_eq!(select.params.as_ref().unwrap()["sessionId"], "older");
        third
            .send(Envelope::response(select.id.unwrap(), Some(json!({}))))
            .unwrap();
        let resumed = core_client.recv().await.unwrap().result.unwrap();
        assert_eq!(resumed["thread"]["id"], "older");
        assert_eq!(resumed["model"], "hi/luna");

        // 没有可启动的进程时，现有进程和会话保持不动。
        core_client
            .send(app_server::backend_reload(RpcId::Number(6)))
            .unwrap();
        let refused = core_client.recv().await.unwrap();
        assert!(refused.error.unwrap()["message"]
            .as_str()
            .unwrap()
            .contains("no more processes"));
        core_client
            .send(app_server::model_list(RpcId::Number(7)))
            .unwrap();
        assert!(core_client.recv().await.unwrap().result.is_some());
        drop(core_client);
        let _ = tokio::time::timeout(Duration::from_secs(2), bridge).await;
    }

    #[tokio::test]
    async fn a_new_conversation_releases_the_previous_session_and_can_switch_back() {
        let (mut core, core_bridge) = pair();
        let (mut server, acp_bridge) = pair();
        let config = Config::default();
        let bridge = tokio::spawn(AcpBridge::new(core_bridge, acp_bridge, &config).run());
        core.send(app_server::thread_start(RpcId::Number(1), &config))
            .unwrap();
        let start = server.recv().await.unwrap();
        server
            .send(Envelope::response(
                start.id.unwrap(),
                Some(json!({"sessionId":"a"})),
            ))
            .unwrap();
        core.recv().await.unwrap();
        // Failed creation never closes A.
        core.send(app_server::thread_new(RpcId::Number(2), &config))
            .unwrap();
        let create = server.recv().await.unwrap();
        assert_eq!(create.method.as_deref(), Some("session/new"));
        server
            .send(Envelope::error_response(
                create.id.unwrap(),
                -32000,
                "creation refused",
            ))
            .unwrap();
        assert!(core.recv().await.unwrap().error.is_some());
        // Creation and close must both be confirmed before Core learns B's id.
        core.send(app_server::thread_new(RpcId::Number(3), &config))
            .unwrap();
        let create = server.recv().await.unwrap();
        assert_eq!(create.method.as_deref(), Some("session/new"));
        server
            .send(Envelope::response(
                create.id.unwrap(),
                Some(json!({"sessionId":"b"})),
            ))
            .unwrap();
        let close = server.recv().await.unwrap();
        assert_eq!(close.method.as_deref(), Some("session/close"));
        assert_eq!(close.params.as_ref().unwrap()["sessionId"], "a");
        assert!(tokio::time::timeout(Duration::from_millis(50), core.recv())
            .await
            .is_err());
        server
            .send(Envelope::response(close.id.unwrap(), Some(json!({}))))
            .unwrap();
        assert_eq!(
            core.recv().await.unwrap().result.unwrap()["thread"]["id"],
            "b"
        );
        core.send(app_server::thread_resume(RpcId::Number(4), "a"))
            .unwrap();
        let resume = server.recv().await.unwrap();
        assert_eq!(resume.method.as_deref(), Some("session/resume"));
        assert_eq!(resume.params.as_ref().unwrap()["sessionId"], "a");
        server
            .send(Envelope::response(resume.id.unwrap(), Some(json!({}))))
            .unwrap();
        let close = server.recv().await.unwrap();
        assert_eq!(close.method.as_deref(), Some("session/close"));
        assert_eq!(close.params.as_ref().unwrap()["sessionId"], "b");
        server
            .send(Envelope::response(close.id.unwrap(), Some(json!({}))))
            .unwrap();
        assert_eq!(
            core.recv().await.unwrap().result.unwrap()["thread"]["id"],
            "a"
        );
        drop(core);
        bridge.await.unwrap();
    }

    #[tokio::test]
    async fn switching_back_releases_old_sessions_and_a_refused_target_keeps_the_current_session() {
        let (mut core_client, core_bridge) = pair();
        let (mut server, acp_bridge) = pair();
        let config = Config::default();
        let bridge = tokio::spawn(AcpBridge::new(core_bridge, acp_bridge, &config).run());
        let agent = tokio::spawn(async move {
            let mut active = std::collections::HashSet::new();
            while let Ok(request) = server.recv().await {
                let Some(id) = request.id else { continue };
                let params = request.params.unwrap_or_default();
                let session = params["sessionId"].as_str().unwrap_or_default();
                let reply = match request.method.as_deref() {
                    Some("session/new") => {
                        active.insert("a".to_owned());
                        Envelope::response(id, Some(json!({"sessionId":"a"})))
                    }
                    Some("session/resume") if session == "missing" => {
                        Envelope::error_response(id, -32602, "session is not resumable")
                    }
                    Some("session/resume") if !active.insert(session.to_owned()) => {
                        Envelope::error_response(id, -32602, "session is already active")
                    }
                    Some("session/resume") => Envelope::response(id, Some(json!({}))),
                    Some("session/close") => {
                        assert!(
                            active.remove(session),
                            "closed a session that was not active"
                        );
                        Envelope::response(id, Some(json!({})))
                    }
                    other => panic!("unexpected request: {other:?}"),
                };
                server.send(reply).unwrap();
            }
            active
        });
        core_client
            .send(crate::app_server::thread_start(RpcId::Number(1), &config))
            .unwrap();
        core_client.recv().await.unwrap();
        for (index, session) in ["b", "a", "b", "b", "missing", "a"].into_iter().enumerate() {
            let id = RpcId::Number(index as i64 + 2);
            core_client
                .send(crate::app_server::thread_resume(id.clone(), session))
                .unwrap();
            let answer = tokio::time::timeout(Duration::from_secs(2), core_client.recv())
                .await
                .unwrap()
                .unwrap();
            assert_eq!(answer.id, Some(id));
            if session == "missing" {
                assert!(answer.error.unwrap()["message"]
                    .as_str()
                    .unwrap()
                    .contains("not resumable"));
            } else {
                assert_eq!(answer.result.unwrap()["thread"]["id"], session);
            }
        }
        drop(core_client);
        bridge.await.unwrap();
        assert!(agent.await.unwrap().is_empty());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_model_test_sends_one_prompt_in_its_own_session_and_reports_the_agent_error() {
        // 脚本化的 agent：按顺序回应一次测试；`fail` 决定 prompt 的结局。
        async fn agent(mut server: PipeTransport, fail: Option<&'static str>) -> Vec<String> {
            let mut seen = Vec::new();
            while let Ok(message) = server.recv().await {
                let method = message.method.clone().unwrap_or_default();
                seen.push(method.clone());
                let Some(id) = message.id else { continue };
                let reply = match method.as_str() {
                    "session/new" => json!({"sessionId":"test-session","configOptions":[
                        {"id":"model","currentValue":"[\"official\",\"flash\"]","options":[
                            {"value":"[\"official\",\"flash\"]","name":"Flash"},
                            {"value":"[\"my\",\"model-x\"]","name":"Model X"}]}]}),
                    "session/prompt" => {
                        // 测试期间的权限请求必须被拒绝，而不是卡住。
                        server
                            .send(Envelope::request(
                                RpcId::Number(900),
                                "session/request_permission",
                                Some(json!({"sessionId":"test-session"})),
                            ))
                            .unwrap();
                        let answer = server.recv().await.unwrap();
                        assert_eq!(answer.id, Some(RpcId::Number(900)));
                        assert_eq!(answer.result.unwrap()["outcome"]["outcome"], "cancelled");
                        if let Some(message) = fail {
                            server
                                .send(Envelope::error_response(id, -32603, message))
                                .unwrap();
                            continue;
                        }
                        json!({"stopReason":"end_turn"})
                    }
                    _ => json!({}),
                };
                server.send(Envelope::response(id, Some(reply))).unwrap();
            }
            seen
        }

        let (mut client, server) = pair();
        let script = tokio::spawn(agent(server, None));
        assert_eq!(
            super::exercise_model(&mut client, "C:\\work", "my/model-x").await,
            Ok(())
        );
        client.close_writer().await;
        assert_eq!(
            script.await.unwrap(),
            [
                "initialize",
                "session/new",
                "session/set_config_option",
                "session/prompt",
                "session/close"
            ]
        );

        let (mut client, server) = pair();
        let script = tokio::spawn(agent(server, Some("401 invalid api key")));
        assert_eq!(
            super::exercise_model(&mut client, "C:\\work", "my/model-x").await,
            Err("401 invalid api key".into())
        );
        client.close_writer().await;
        script.await.unwrap();

        // 清单里没有的模型不会发出 prompt。
        let (mut client, server) = pair();
        let script = tokio::spawn(agent(server, None));
        let error = super::exercise_model(&mut client, "C:\\work", "my/missing")
            .await
            .unwrap_err();
        assert!(error.contains("does not offer model my/missing"), "{error}");
        client.close_writer().await;
        assert!(!script.await.unwrap().contains(&"session/prompt".to_owned()));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn unknown_model_fails_thread_start_and_lists_choices() {
        let (mut core_client, core_bridge) = pair();
        let (mut acp_server, acp_bridge) = pair();
        let config = Config {
            model: Some("missing".into()),
            ..Config::default()
        };
        let bridge = tokio::spawn(AcpBridge::new(core_bridge, acp_bridge, &config).run());
        core_client
            .send(app_server::thread_start(RpcId::Number(2), &config))
            .unwrap();
        acp_server.recv().await.unwrap();
        acp_server
            .send(Envelope::response(
                RpcId::Number(2),
                Some(
                    json!({"sessionId":"s","configOptions":[{"id":"model","options":[
                    {"value":"[\"hi\",\"gpt-6-luna\"]","name":"GPT-6 Luna"}]}]}),
                ),
            ))
            .unwrap();
        let response = core_client.recv().await.unwrap();
        let error = response.error.unwrap();
        assert!(
            error["message"]
                .as_str()
                .unwrap()
                .contains("available: hi/gpt-6-luna"),
            "{error}"
        );
        drop(core_client);
        let _ = tokio::time::timeout(Duration::from_secs(2), bridge).await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_remembered_model_that_is_no_longer_offered_keeps_the_agent_default() {
        let (mut core_client, core_bridge) = pair();
        let (mut acp_server, acp_bridge) = pair();
        let config = Config {
            model: Some("missing".into()),
            model_is_preference: true,
            ..Config::default()
        };
        let bridge = tokio::spawn(AcpBridge::new(core_bridge, acp_bridge, &config).run());
        core_client
            .send(app_server::thread_start(RpcId::Number(2), &config))
            .unwrap();
        acp_server.recv().await.unwrap();
        acp_server
            .send(Envelope::response(
                RpcId::Number(2),
                Some(json!({"sessionId":"s","configOptions":[
                    {"id":"model","currentValue":"[\"hi\",\"gpt-6-luna\"]","options":[
                        {"value":"[\"hi\",\"gpt-6-luna\"]","name":"GPT-6 Luna"}]}]})),
            ))
            .unwrap();
        let response = core_client.recv().await.unwrap();
        assert!(response.error.is_none());
        let result = response.result.unwrap();
        assert_eq!(result["thread"]["id"], "s");
        assert_eq!(result["model"], "hi/gpt-6-luna");
        drop(core_client);
        let _ = tokio::time::timeout(Duration::from_secs(2), bridge).await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn id_only_permission_shows_the_earlier_command_and_requested_sandbox() {
        let (mut core_client, core_bridge) = pair();
        let (mut acp_server, acp_bridge) = pair();
        let config = Config::default();
        let bridge = tokio::spawn(AcpBridge::new(core_bridge, acp_bridge, &config).run());
        core_client
            .send(app_server::thread_start(RpcId::Number(2), &config))
            .unwrap();
        acp_server.recv().await.unwrap();
        acp_server
            .send(Envelope::response(
                RpcId::Number(2),
                Some(json!({"sessionId": "s"})),
            ))
            .unwrap();
        core_client.recv().await.unwrap();
        core_client
            .send(app_server::turn_start(RpcId::Number(3), "s", "run", None))
            .unwrap();
        core_client.recv().await.unwrap(); // turn/started
        acp_server.recv().await.unwrap(); // session/prompt
        core_client.recv().await.unwrap(); // turn/start ack
        acp_server
            .send(Envelope::notification(
                "session/update",
                Some(json!({"sessionId":"s","update":{"sessionUpdate":"tool_call",
                    "toolCallId":"call-1","title":"pwsh","kind":"other","status":"in_progress",
                    "rawInput":{"command":"Start-Sleep -Seconds 1","justification":"执行用户指定的命令",
                        "sandbox_permissions":"danger-full-access","workdir":"D:\\ws"}}})),
            ))
            .unwrap();
        assert_eq!(
            core_client.recv().await.unwrap().method.as_deref(),
            Some("item/started")
        );
        acp_server
            .send(Envelope::request(
                RpcId::Number(9),
                "session/request_permission",
                Some(json!({"sessionId":"s","toolCall":{"toolCallId":"call-1"},
                    "options":[{"optionId":"allow-once"},{"optionId":"reject-once"}]})),
            ))
            .unwrap();
        let request = core_client.recv().await.unwrap();
        let params = request.params.unwrap();
        assert_eq!(params["command"], "Start-Sleep -Seconds 1");
        assert_eq!(
            params["reason"],
            "执行用户指定的命令 (requests danger-full-access)"
        );
        assert_eq!(params["cwd"], "D:\\ws");
        drop(core_client);
        let _ = tokio::time::timeout(Duration::from_secs(2), bridge).await;
    }
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn plugin_events_reach_core_only_for_the_current_session_and_stay_bounded() {
        let (mut core_client, core_bridge) = pair();
        let (mut acp_server, acp_bridge) = pair();
        let config = Config::default();
        let bridge = tokio::spawn(AcpBridge::new(core_bridge, acp_bridge, &config).run());
        core_client
            .send(app_server::thread_start(RpcId::Number(2), &config))
            .unwrap();
        acp_server.recv().await.unwrap();
        acp_server
            .send(Envelope::response(
                RpcId::Number(2),
                Some(json!({"sessionId":"s"})),
            ))
            .unwrap();
        core_client.recv().await.unwrap();
        let event = |session: &str, event: serde_json::Value| {
            Envelope::notification(
                "zf/session/event",
                Some(json!({"sessionId":session,"event":event})),
            )
        };
        // 没有进行中的轮次时，阶段没有归属，不转发；todo 列表属于会话，照常转发。
        acp_server
            .send(event("s", json!({"type":"stage","stage":"waiting"})))
            .unwrap();
        acp_server
            .send(event(
                "s",
                json!({"type":"todos","todos":[
                {"content":"scan","status":"completed"},
                {"content":"中".repeat(2000),"status":"in_progress"},
                {"content":"bad","status":"done"},
                {"content":7,"status":"pending"}]}),
            ))
            .unwrap();
        let todos = core_client.recv().await.unwrap();
        assert_eq!(todos.method.as_deref(), Some("thread/todos/updated"));
        let todos = todos.params.unwrap();
        assert_eq!(todos["threadId"], "s");
        assert_eq!(todos["todos"].as_array().unwrap().len(), 2);
        assert_eq!(
            todos["todos"][0],
            json!({"content":"scan","status":"completed"})
        );
        assert_eq!(
            todos["todos"][1]["content"]
                .as_str()
                .unwrap()
                .chars()
                .count(),
            crate::state::MAX_TODO_CHARS
        );
        core_client
            .send(app_server::turn_start(RpcId::Number(3), "s", "model", None))
            .unwrap();
        core_client.recv().await.unwrap();
        acp_server.recv().await.unwrap();
        core_client.recv().await.unwrap();
        for (session, stage) in [
            ("other", "thinking"),
            ("s", "invented"),
            ("s", "thinking"),
            ("s", "response"),
        ] {
            acp_server
                .send(event(session, json!({"type":"stage","stage":stage})))
                .unwrap();
        }
        acp_server
            .send(event("other", json!({"type":"todos","todos":[]})))
            .unwrap();
        acp_server
            .send(event("s", json!({"type":"unknown"})))
            .unwrap();
        for stage in ["thinking", "response"] {
            let relayed = core_client.recv().await.unwrap();
            assert_eq!(relayed.method.as_deref(), Some("turn/stage"));
            let params = relayed.params.unwrap();
            assert_eq!(params["threadId"], "s");
            assert!(params["turnId"]
                .as_str()
                .is_some_and(|turn| !turn.is_empty()));
            assert_eq!(params["stage"], stage);
        }
        // 重试只带计数和失败代码；压缩带身份和结果。供应商的报错文字不进入 Core。
        acp_server
            .send(event(
                "s",
                json!({"type":"retry","retry":1,"maxRetries":2,"delayMs":500,
                "code":"rate_limit","message":"PRIVATE"}),
            ))
            .unwrap();
        acp_server
            .send(event(
                "s",
                json!({"type":"retry","retry":2,"code":"has spaces PRIVATE"}),
            ))
            .unwrap();
        acp_server
            .send(event(
                "s",
                json!({"type":"compaction","id":"c1","state":"failed"}),
            ))
            .unwrap();
        acp_server
            .send(event(
                "s",
                json!({"type":"compaction","id":"c1","state":"invented"}),
            ))
            .unwrap();
        acp_server
            .send(event("other", json!({"type":"retry","retry":9})))
            .unwrap();
        let mut notices = Vec::new();
        for _ in 0..3 {
            let notice = core_client.recv().await.unwrap();
            assert_eq!(notice.method.as_deref(), Some("turn/notice"));
            notices.push(notice.params.unwrap());
        }
        assert!(!serde_json::to_string(&notices).unwrap().contains("PRIVATE"));
        assert_eq!(
            (
                &notices[0]["kind"],
                &notices[0]["retry"],
                &notices[0]["maxRetries"]
            ),
            (&json!("retry"), &json!(1), &json!(2))
        );
        assert_eq!(
            (&notices[0]["delayMs"], &notices[0]["code"]),
            (&json!(500), &json!("rate_limit"))
        );
        assert_eq!(
            (&notices[1]["retry"], &notices[1]["code"]),
            (&json!(2), &json!(null))
        );
        assert_eq!(
            (&notices[2]["kind"], &notices[2]["id"], &notices[2]["state"]),
            (&json!("compaction"), &json!("c1"), &json!("failed"))
        );
        // 其余事件都被丢弃：下一条到达 Core 的是之后的普通更新。
        acp_server.send(Envelope::notification("session/update", Some(json!({"sessionId":"s", "update":{
            "sessionUpdate":"agent_message_chunk", "messageId":"m", "content":{"type":"text","text":"ok"}}})))).unwrap();
        assert_eq!(
            core_client.recv().await.unwrap().method.as_deref(),
            Some("item/agentMessage/delta")
        );
        drop(core_client);
        let _ = tokio::time::timeout(Duration::from_secs(2), bridge).await;
    }

    #[tokio::test]
    async fn mcp_calls_retain_typed_identity_arguments_and_structured_results() {
        let (mut core_client, core_bridge) = pair();
        let (mut acp_server, acp_bridge) = pair();
        let config = Config::default();
        let bridge = tokio::spawn(AcpBridge::new(core_bridge, acp_bridge, &config).run());
        core_client
            .send(app_server::thread_start(RpcId::Number(2), &config))
            .unwrap();
        acp_server.recv().await.unwrap();
        acp_server
            .send(Envelope::response(
                RpcId::Number(2),
                Some(json!({"sessionId":"s"})),
            ))
            .unwrap();
        core_client.recv().await.unwrap();
        core_client
            .send(app_server::turn_start(RpcId::Number(3), "s", "model", None))
            .unwrap();
        core_client.recv().await.unwrap();
        acp_server.recv().await.unwrap();
        core_client.recv().await.unwrap();
        acp_server.send(Envelope::notification("session/update", Some(json!({"sessionId":"s", "update":{
            "sessionUpdate":"tool_call", "toolCallId":"call", "title":"mcp__modeling_domain__prepare_change", "rawInput":{"plan":{}}, "kind":"other"}})))).unwrap();
        let event = core_client.recv().await.unwrap().params.unwrap();
        assert_eq!(event["item"]["type"], "mcpToolCall");
        assert_eq!(event["item"]["server"], "modeling_domain");
        assert_eq!(event["item"]["arguments"], json!({"plan":{}}));
        let result = json!({"contract":"modeling-domain/v2","kind":"proposal","ok":true,"reason":"中".repeat(14_000)});
        acp_server.send(Envelope::notification("session/update", Some(json!({"sessionId":"s", "update":{
            "sessionUpdate":"tool_call_update", "toolCallId":"call", "status":"completed", "rawOutput":result.to_string()}})))).unwrap();
        let event = core_client.recv().await.unwrap().params.unwrap();
        assert_eq!(event["item"]["type"], "mcpToolCall");
        assert_eq!(event["item"]["tool"], "prepare_change");
        assert_eq!(event["item"]["result"], result);
        acp_server.send(Envelope::notification("session/update", Some(json!({"sessionId":"s", "update":{
            "sessionUpdate":"tool_call_update", "toolCallId":"unregistered", "status":"completed", "rawOutput":result.to_string()}})))).unwrap();
        let event = core_client.recv().await.unwrap().params.unwrap();
        assert_eq!(event["item"]["type"], "dynamicToolCall");
        assert!(event["item"].get("result").is_none());
        assert!(event["item"]["aggregatedOutput"]
            .as_str()
            .unwrap()
            .ends_with("[output truncated]"));
        drop(core_client);
        let _ = tokio::time::timeout(Duration::from_secs(2), bridge).await;
    }

    #[tokio::test]
    async fn acp_tool_presentations_keep_command_directory_output_exit_and_diff_facts() {
        let (mut core_client, core_bridge) = pair();
        let (_, acp_bridge) = pair();
        let mut bridge = AcpBridge::new(core_bridge, acp_bridge, &Config::default());
        bridge.session_id = Some("s".into());
        bridge.turn_id = Some("turn".into());
        bridge.turn_started = true;
        bridge
            .update(
                json!({"sessionId":"s","update":{"sessionUpdate":"tool_call","toolCallId":"shell",
            "title":"pwsh","kind":"other","rawInput":{"command":"echo probe","workdir":"C:/ws"}}}),
            )
            .await
            .unwrap();
        let item = core_client.recv().await.unwrap().params.unwrap()["item"].clone();
        assert_eq!(item["type"], "commandExecution");
        assert_eq!(item["command"], "echo probe");
        assert_eq!(item["cwd"], "C:/ws");
        bridge.update(json!({"sessionId":"s","update":{"sessionUpdate":"tool_call_update","toolCallId":"shell",
            "status":"completed","exitCode":7,"durationMs":12,"content":[{"type":"content","content":{"type":"text","text":"line1\nline2"}}]}})).await.unwrap();
        let params = core_client.recv().await.unwrap().params.unwrap();
        let item = &params["item"];
        assert_eq!(item["command"], "echo probe");
        assert_eq!(item["cwd"], "C:/ws");
        assert_eq!(item["exitCode"], 7);
        assert_eq!(item["durationMs"], 12);
        assert_eq!(item["aggregatedOutput"], "line1\nline2");
        let notice = crate::protocol::decode_observed_tool("item/completed", &params)
            .unwrap()
            .unwrap();
        assert_eq!(
            notice.outcome,
            Some(crate::protocol::ObservedToolOutcome::Failed)
        );
        bridge.update(json!({"sessionId":"s","update":{"sessionUpdate":"tool_call","toolCallId":"edit",
            "title":"edit","kind":"other","rawInput":{"file_path":"src/a.rs","old_string":"same\nold\n","new_string":"same\nnew\n"}}})).await.unwrap();
        let params = core_client.recv().await.unwrap().params.unwrap();
        assert_eq!(params["item"]["type"], "fileChange");
        assert_eq!(params["item"]["target"], "src/a.rs");
        let fields = crate::protocol::decode_observed_tool_details(
            &params["item"],
            crate::protocol::ToolCategory::File,
        );
        assert_eq!(
            fields.diff,
            Some(crate::protocol::DiffStats {
                added: 1,
                removed: 1
            })
        );
        assert!(fields.output.unwrap().contains("-old\n+new"));
        bridge.update(json!({"sessionId":"s","update":{"sessionUpdate":"tool_call_update","toolCallId":"edit","status":"failed"}})).await.unwrap();
        let params = core_client.recv().await.unwrap().params.unwrap();
        assert_eq!(params["item"]["status"], "failed");
        assert_eq!(params["item"]["target"], "src/a.rs");
        assert!(params["item"]["exitCode"].is_null());
        bridge
            .update(
                json!({"sessionId":"s","update":{"sessionUpdate":"tool_call","toolCallId":"read",
            "title":"read","kind":"read","rawInput":{"file_path":"a.txt"}}}),
            )
            .await
            .unwrap();
        let params = core_client.recv().await.unwrap().params.unwrap();
        assert_eq!(params["item"]["type"], "dynamicToolCall");
        assert_eq!(params["item"]["target"], "a.txt");
        assert!(params["item"].get("command").is_none());
    }
}
