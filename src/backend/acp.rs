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

/// How long after a goal round ends a new one may still begin within the same turn.
const GOAL_IDLE: std::time::Duration = std::time::Duration::from_secs(5);

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

fn dsh_arguments(config: &Config) -> Vec<std::ffi::OsString> {
    let mut args = vec!["--profile".into(), config.acp_profile.clone().into()];
    if let Some(patch) = &config.domain_mcp_patch {
        args.extend(["--patch".into(), patch.as_os_str().to_owned()]);
    }
    args
}

fn launch_with_tail(
    config: &Config,
    tail: Option<StderrTail>,
) -> std::io::Result<(PipeTransport, Child, JoinHandle<()>)> {
    let mut command = Command::new(&config.dsh_executable);
    command.args(dsh_arguments(config)).current_dir(&config.cwd);
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
    Prompt {
        turn_id: String,
    },
    BackgroundCancel {
        core_id: RpcId,
    },
    /// The plugin's skill catalog, to be reshaped into the inventory Core reads.
    Skills {
        cwd: String,
    },
    Passthrough,
    /// A bridge setting whose reply carries nothing for Core.
    Ignored,
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
    /// dsh keeps working toward an active goal after its prompt returns. Those
    /// rounds stay in this turn so their output is neither lost nor attached to
    /// the user's next message.
    goal_active: bool,
    /// Modeling sessions advance by concrete approvals; dsh session goals would
    /// start autonomous rounds that hide Ctrl+Y, so the bridge suppresses them.
    no_goals: bool,
    /// A dsh host turn (prompted or a goal round) is between its start and end.
    host_turn_running: bool,
    background: BackgroundWork,
    /// The prompt already returned; the turn ends when the goal rounds stop.
    deferred_finish: Option<(RpcId, String)>,
    /// No new goal round started by then: end the deferred turn.
    goal_idle_deadline: Option<tokio::time::Instant>,
    pending: HashMap<RpcId, Pending>,
    permissions: HashMap<RpcId, Permission>,
    message_text: HashMap<String, String>,
    last_message_id: Option<String>,
    /// 正在接收的内容块：(messageId, 是否思考, 序号)。同一条消息里思考与正文
    /// 交替出现时各成一段，保留先后关系。
    block: Option<(String, bool, u32)>,
    /// 正在生成的预览段的 item id；提交后的权威内容到达前由它们占位。
    live_items: Vec<LiveItem>,
    live_attempt: u64,
    /// 这次请求已经提交：下一条权威更新到达时撤掉预览。
    live_settled: bool,
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
    /// 插件转来、Core 尚未回答的提问。
    asks: std::collections::HashSet<RpcId>,
    tool_presentations:
        HashMap<String, (acp_protocol::ToolKind, String, Vec<acp_protocol::FileDiff>)>,
}

#[derive(Default)]
struct BackgroundWork {
    runs: HashMap<String, String>,
    first_host_turn: Option<u64>,
    host_turn: Option<u64>,
    settled_turn: Option<u64>,
    closed_host_turn: u64,
    resume_pending: bool,
    canceling: bool,
    failure: Option<BackgroundFailure>,
    interrupted: bool,
    delegated: bool,
    step_offset: u64,
    host_step: u64,
}

enum BackgroundFailure {
    Provider(Option<&'static str>),
    OutputDelivery,
    Unknown,
}

struct LiveItem {
    id: String,
    thinking: bool,
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
            goal_active: false,
            no_goals: config.domain_setup.is_some(),
            host_turn_running: false,
            background: BackgroundWork::default(),
            deferred_finish: None,
            goal_idle_deadline: None,
            pending: HashMap::new(),
            permissions: HashMap::new(),
            message_text: HashMap::new(),
            last_message_id: None,
            block: None,
            live_items: Vec::new(),
            live_attempt: 0,
            live_settled: false,
            model: config.model.clone(),
            model_is_preference: config.model_is_preference,
            models: Vec::new(),
            reasoning: config.reasoning.clone(),
            current: None,
            respawn: None,
            replacement: None,
            startup_stderr: None,
            tool_inputs: HashMap::new(),
            mcp_calls: HashMap::new(),
            asks: Default::default(),
            tool_presentations: HashMap::new(),
        }
    }

    async fn run(mut self) {
        loop {
            let goal_deadline = self.goal_idle_deadline;
            tokio::select! {
                _ = tokio::time::sleep_until(goal_deadline.unwrap_or_else(tokio::time::Instant::now)),
                    if goal_deadline.is_some() =>
                {
                    self.goal_idle_deadline = None;
                    if !self.host_turn_running && self.finish_deferred().is_err() {
                        break;
                    }
                }
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
        self.background = BackgroundWork::default();
        self.suppress_goals(&session)?;
        self.message_text.clear();
        self.tool_inputs.clear();
        self.mcp_calls.clear();
        self.tool_presentations.clear();
        self.last_message_id = None;
        self.block = None;
        self.live_items.clear();
        self.live_settled = false;
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
                self.block = None;
                self.live_items.clear();
                self.live_settled = false;
                let turn_id = format!("acp-turn-{}", id_text(&id));
                self.turn_id = Some(turn_id.clone());
                self.turn_started = true;
                self.core.send(Envelope::notification(
                    "turn/started",
                    Some(json!({
                        "threadId": session,
                        "turn": {"id": self.turn_id.as_deref().unwrap_or_default()}
                    })),
                ))?;
                self.pending.insert(id.clone(), Pending::Prompt { turn_id });
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
                if !self.background.runs.is_empty() {
                    self.background.canceling = true;
                    let request = RpcId::String(format!("acp-background-cancel-{}", id_text(&id)));
                    self.pending.insert(
                        request.clone(),
                        Pending::BackgroundCancel {
                            core_id: id.clone(),
                        },
                    );
                    self.deferred_finish = Some((id, "interrupted".into()));
                    return self.acp.send(with_jsonrpc(Envelope::request(
                        request,
                        "zf/session/background/cancel",
                        Some(json!({"sessionId":self.session_id})),
                    )));
                }
                // The prompt already returned while goal rounds ran: end the turn here.
                if let Some((_, status)) = self.deferred_finish.as_mut() {
                    self.goal_idle_deadline = None;
                    *status = "interrupted".into();
                    self.finish_deferred()?;
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
            // 引导只对桥接当前绑定的轮次有效；dsh 在下一步边界把它交给模型。
            (Some("turn/steer"), Some(id)) => {
                let params = message.params.unwrap_or(Value::Null);
                let (Some(session), Some(text)) = (
                    self.session_id.clone().filter(|_| {
                        self.turn_id.is_some()
                            && params["turnId"].as_str() == self.turn_id.as_deref()
                    }),
                    params["text"].as_str(),
                ) else {
                    return self.core.send(Envelope::error_response(
                        id,
                        -32000,
                        "no turn is running",
                    ));
                };
                self.pending.insert(id.clone(), Pending::Passthrough);
                self.acp.send(with_jsonrpc(Envelope::request(
                    id,
                    "zf/session/steer",
                    Some(json!({"sessionId":session,"text":text})),
                )))?;
            }
            // dsh 自己的斜杠命令；只在没有进行中的轮次时执行。
            (Some(method @ ("backend/commands/list" | "backend/command/run")), Some(id)) => {
                let Some(session) = self.session_id.clone() else {
                    return self.core.send(Envelope::error_response(
                        id,
                        -32000,
                        "ACP session is not ready",
                    ));
                };
                let (target, params) = if method == "backend/commands/list" {
                    ("zf/commands/list", json!({"sessionId":session}))
                } else if self.turn_id.is_some() {
                    return self.core.send(Envelope::error_response(
                        id,
                        -32000,
                        "a turn is running",
                    ));
                } else {
                    let line = message
                        .params
                        .as_ref()
                        .and_then(|params| params["line"].as_str())
                        .unwrap_or_default();
                    ("zf/commands/run", json!({"sessionId":session,"line":line}))
                };
                self.pending.insert(id.clone(), Pending::Passthrough);
                self.acp
                    .send(with_jsonrpc(Envelope::request(id, target, Some(params))))?;
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
                    // 列出所有工作区的会话，每条标明是否属于这里；范围由界面切换。
                    Some(json!({"cwd": self.cwd, "all": true})),
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
            // Core 对插件提问的答复原样交还；结果的换算在插件里做。
            (None, Some(id)) if self.asks.remove(&id) => {
                self.acp.send(with_jsonrpc(Envelope {
                    jsonrpc: None,
                    id: Some(id.clone()),
                    method: None,
                    params: None,
                    result: message.result,
                    error: message.error,
                }))?;
                if let Some(session) = &self.session_id {
                    self.core.send(Envelope::notification(
                        "serverRequest/resolved",
                        Some(json!({"threadId": session, "requestId": id})),
                    ))?;
                }
            }
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
            (Some("zf/session/ask"), Some(id)) => {
                self.ask(id, message.params.unwrap_or(Value::Null))?;
            }
            (None, Some(id)) => self.response(id, message).await?,
            _ => {}
        }
        Ok(())
    }

    /// 撤掉生成中的预览。权威内容是 dsh 提交的整条消息，预览只是它到达前的占位。
    fn discard_live(&mut self, session: &str, turn: &str) -> Result<(), TransportError> {
        self.live_settled = false;
        if !self.live_items.is_empty() {
            self.live_attempt = self.live_attempt.saturating_add(1);
        }
        for item in std::mem::take(&mut self.live_items) {
            self.core.send(Envelope::notification(
                "item/discarded",
                Some(json!({"threadId":session,"turnId":turn,"itemId":item.id})),
            ))?;
        }
        Ok(())
    }

    fn replace_live(
        &mut self,
        session: &str,
        turn: &str,
        thinking: bool,
    ) -> Result<Vec<String>, TransportError> {
        if !self.live_settled {
            return Ok(Vec::new());
        }
        // Authority can omit a previewed section. Discard that section explicitly;
        // only a matching role can transfer its scroll identity.
        while self
            .live_items
            .first()
            .is_some_and(|item| item.thinking != thinking)
        {
            let item = self.live_items.remove(0);
            self.core.send(Envelope::notification(
                "item/discarded",
                Some(json!({"threadId":session,"turnId":turn,"itemId":item.id})),
            ))?;
        }
        let count = self
            .live_items
            .iter()
            .take_while(|item| item.thinking == thinking)
            .count();
        let replaced = self.live_items.drain(..count).map(|item| item.id).collect();
        if self.live_items.is_empty() {
            self.live_settled = false;
            self.live_attempt = self.live_attempt.saturating_add(1);
        }
        Ok(replaced)
    }

    /// 给内容块一个稳定身份。dsh 的一条消息里，思考块和正文块共用 messageId 并
    /// 按顺序到达；每次在两者之间切换就是新的一段，否则 Core 会把它们各并成一条，
    /// 丢掉“思考、正文、再思考”的先后。
    fn block_id(&mut self, message_id: String, thought: bool) -> String {
        let index = match &self.block {
            Some((current, kind, index)) if *current == message_id => {
                index + u32::from(*kind != thought)
            }
            _ => 0,
        };
        self.block = Some((message_id.clone(), thought, index));
        if index == 0 {
            message_id
        } else {
            format!("{message_id}#{index}")
        }
    }

    /// Agent 向用户提的问题（如 plan 评审）。dsh 的 ACP 没有应答者，由插件转来；
    /// 这里把它变成 Core 已有的用户输入请求，绑定当前会话和轮次。
    fn ask(&mut self, id: RpcId, params: Value) -> Result<(), TransportError> {
        let questions = params["questions"].as_array().filter(|questions| {
            !questions.is_empty() && questions.len() <= crate::interactions::MAX_INPUT_QUESTIONS
        });
        let (Some(session), Some(turn), Some(questions)) = (
            self.session_id
                .clone()
                .filter(|id| params["sessionId"] == id.as_str()),
            self.turn_id.clone(),
            questions,
        ) else {
            return self.acp.send(with_jsonrpc(Envelope::error_response(
                id,
                -32000,
                "no turn of this session is running",
            )));
        };
        // Core 的请求上限是 32 KiB；被评审的正文按题目数均分预算后截断。
        let budget = 20_000 / questions.len();
        let clip = |value: &Value, limit: usize| -> String {
            let text = value.as_str().unwrap_or_default();
            let mut end = text.len().min(limit);
            while !text.is_char_boundary(end) {
                end -= 1;
            }
            if end < text.len() {
                format!("{}\n[truncated]", &text[..end])
            } else {
                text.to_owned()
            }
        };
        let questions: Vec<Value> = questions
            .iter()
            .map(|question| {
                let options: Vec<Value> = question["options"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .take(12)
                    .map(|option| {
                        json!({"label":clip(&option["label"], 200),
                            "description":clip(&option["description"], 400)})
                    })
                    .collect();
                let mut item = json!({"id":question["id"],
                    "header":clip(&question["header"], 200),
                    "question":clip(&question["question"], budget)});
                if let Some(preview) = question.get("preview") {
                    item["preview"] = preview.clone();
                }
                if !options.is_empty() {
                    item["options"] = Value::Array(options);
                    item["multiSelect"] = json!(question["multiSelect"] == true);
                }
                item
            })
            .collect();
        let params = json!({"threadId":session,"turnId":turn,"questions":questions});
        // Bound the whole request, including repeated options and identity fields.
        // Reject here so dsh receives a terminal error rather than waiting for a
        // Core request that cannot be decoded. Never drop questions from a batch.
        if let Err(error) = crate::interactions::RequestView::decode(
            id.clone(),
            "item/tool/requestUserInput",
            &params,
        ) {
            return self.acp.send(with_jsonrpc(Envelope::error_response(
                id,
                -32602,
                format!("invalid user questions: {error}"),
            )));
        }
        self.asks.insert(id.clone());
        self.core.send(Envelope::request(
            id,
            "item/tool/requestUserInput",
            Some(params),
        ))
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
        let scoped = matches!(
            event["type"].as_str(),
            Some(
                "step"
                    | "stage"
                    | "delta"
                    | "delta-end"
                    | "steer"
                    | "retry"
                    | "todos"
                    | "goal-round"
            )
        );
        if scoped
            && (event["turn"].is_number() || self.background.host_turn.is_some())
            && event["turn"].as_u64() != self.background.host_turn
        {
            return Ok(());
        }
        if let Some(host) = event["turn"].as_u64() {
            let boundary = matches!(event["type"].as_str(), Some("host-turn" | "host-settled"));
            if !boundary
                && self
                    .background
                    .host_turn
                    .is_some_and(|current| host != current)
            {
                return Ok(());
            }
        }
        match event["type"].as_str() {
            Some("background-error") => return Err(TransportError::Overloaded),
            Some("subagent-run") => {
                let (Some(run), Some(id)) = (
                    event["runId"]
                        .as_str()
                        .filter(|s| !s.is_empty() && s.len() <= 128),
                    event["id"]
                        .as_str()
                        .filter(|s| !s.is_empty() && s.len() <= 1024),
                ) else {
                    return Ok(());
                };
                if self.turn_id.is_none() {
                    return Ok(());
                }
                match event["state"].as_str() {
                    Some("start")
                        if self.background.runs.len() >= 256
                            && !self.background.runs.contains_key(run) =>
                    {
                        return Err(TransportError::Overloaded);
                    }
                    Some("start") if self.background.runs.len() < 256 => {
                        self.background.delegated = true;
                        self.background
                            .runs
                            .entry(run.into())
                            .or_insert_with(|| id.into());
                    }
                    Some("end")
                        if self
                            .background
                            .runs
                            .get(run)
                            .is_some_and(|child| child == id) =>
                    {
                        self.background.runs.remove(run);
                        self.background.resume_pending |= !self.host_turn_running;
                    }
                    _ => return Ok(()),
                }
                self.background_notice()?;
                self.settle_background()?;
            }
            Some("host-settled") => {
                if !matches!(
                    event["state"].as_str(),
                    Some("completed" | "failed" | "interrupted")
                ) {
                    return Ok(());
                }
                let Some(host) = event["turn"].as_u64().filter(|turn| {
                    self.background
                        .first_host_turn
                        .is_some_and(|first| *turn >= first)
                        && self.background.host_turn.is_some_and(|last| *turn <= last)
                }) else {
                    return Ok(());
                };
                self.background.settled_turn =
                    Some(self.background.settled_turn.unwrap_or(0).max(host));
                if event["state"] == "failed" {
                    let failure = match event["failureCategory"].as_str() {
                        Some("provider") => BackgroundFailure::Provider(
                            [
                                "AUTH",
                                "QUOTA_EXCEEDED",
                                "RATE_LIMIT",
                                "SERVER",
                                "TIMEOUT",
                                "TRANSPORT",
                                "PI_AI_ERROR",
                                "CONTEXT_WINDOW_EXCEEDED",
                                "EMPTY_RESPONSE",
                            ]
                            .into_iter()
                            .find(|code| Some(*code) == event["failureCode"].as_str()),
                        ),
                        Some("output_delivery") => BackgroundFailure::OutputDelivery,
                        _ => BackgroundFailure::Unknown,
                    };
                    // 旧宿主缺少分类时保留已有明确失败；投递失败优先，避免误报 provider。
                    if matches!(
                        self.background.failure,
                        None | Some(BackgroundFailure::Unknown)
                    ) || matches!(failure, BackgroundFailure::OutputDelivery)
                    {
                        self.background.failure = Some(failure);
                    }
                }
                self.background.interrupted |= event["state"] == "interrupted";
                self.background_notice()?;
                if event["state"] == "interrupted" {
                    if let Some((_, status)) = self.deferred_finish.as_mut() {
                        *status = "interrupted".into();
                    }
                }
                self.settle_background()?;
            }
            Some("subagent") => {
                let (Some(id), Some(state), Some(summary)) = (
                    event["id"]
                        .as_str()
                        .filter(|id| !id.is_empty() && id.len() <= 1024),
                    event["state"]
                        .as_str()
                        .filter(|state| matches!(*state, "completed" | "failed" | "message")),
                    event["summary"].as_str().filter(|text| text.len() <= 4096),
                ) else {
                    return Ok(());
                };
                self.core.send(Envelope::notification(
                    "subagent/notice",
                    Some(json!({"threadId":session,"id":id,"state":state,"summary":summary})),
                ))?;
            }
            Some("step") => {
                let (Some(turn), Some(step), Some(state)) = (
                    self.turn_id.clone(),
                    event["step"]
                        .as_u64()
                        .filter(|step| *step <= 9_007_199_254_740_991),
                    event["state"]
                        .as_str()
                        .filter(|state| matches!(*state, "start" | "end")),
                ) else {
                    return Ok(());
                };
                self.background.host_step = self.background.host_step.max(step);
                let step = self.background.step_offset.saturating_add(step);
                if !self.turn_started {
                    if state != "start" {
                        return Ok(());
                    }
                    self.core.send(Envelope::notification(
                        "turn/started",
                        Some(json!({"threadId":session,"turn":{"id":turn}})),
                    ))?;
                    self.turn_started = true;
                }
                self.core.send(Envelope::notification(
                    "turn/step",
                    Some(json!({"threadId":session,"turnId":turn,"step":step,"state":state})),
                ))?;
                if state == "end" && self.live_settled {
                    self.discard_live(&session, &turn)?;
                }
            }
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
                let mut params = json!({"threadId":session,"turnId":turn,"stage":stage});
                if let Some(step) = event["step"]
                    .as_u64()
                    .filter(|step| *step <= 9_007_199_254_740_991)
                {
                    params["step"] = json!(self.background.step_offset.saturating_add(step));
                }
                self.core
                    .send(Envelope::notification("turn/stage", Some(params)))?;
            }
            Some("delta") => {
                let (Some(turn), Some(text)) = (
                    self.turn_id.clone(),
                    event["text"]
                        .as_str()
                        .filter(|text| !text.is_empty() && text.len() <= 64 * 1024),
                ) else {
                    return Ok(());
                };
                if self.live_settled {
                    self.discard_live(&session, &turn)?;
                }
                let thinking = event["kind"] == "thinking";
                let item = format!(
                    "live:{}:{}:{}",
                    self.live_attempt,
                    if thinking { "thinking" } else { "text" },
                    event["block"].as_u64().unwrap_or(0).min(999)
                );
                if !self.live_items.iter().any(|live| live.id == item) && self.live_items.len() < 64
                {
                    self.live_items.push(LiveItem {
                        id: item.clone(),
                        thinking,
                    });
                }
                if !self.turn_started {
                    self.core.send(Envelope::notification(
                        "turn/started",
                        Some(json!({"threadId":session,"turn":{"id":turn}})),
                    ))?;
                    self.turn_started = true;
                }
                self.core.send(Envelope::notification(
                    if thinking {
                        "item/reasoning/textDelta"
                    } else {
                        "item/agentMessage/delta"
                    },
                    Some(json!({"threadId":session,"turnId":turn,"itemId":item,"delta":text})),
                ))?;
            }
            Some("delta-end") => {
                let Some(turn) = self.turn_id.clone() else {
                    return Ok(());
                };
                if event["committed"] == true {
                    // 权威消息紧随其后；等它到了再换，避免中间出现空白。
                    self.live_settled = true;
                } else {
                    self.discard_live(&session, &turn)?;
                }
            }
            Some("ask") => {
                if let Some(id) = event["id"].as_str().filter(|id| id.len() <= 128) {
                    let id = RpcId::String(id.to_owned());
                    if event["state"] == "withdrawn" && self.asks.remove(&id) {
                        self.core.send(Envelope::notification(
                            "serverRequest/resolved",
                            Some(json!({"threadId": session, "requestId": id})),
                        ))?;
                    }
                }
            }
            Some("steer") => {
                let (Some(turn), Some(id)) = (
                    self.turn_id.clone(),
                    event["id"]
                        .as_str()
                        .filter(|id| !id.is_empty() && id.len() <= 128),
                ) else {
                    return Ok(());
                };
                if event["state"] == "delivered" {
                    self.core.send(Envelope::notification(
                        "turn/steer/delivered",
                        Some(json!({"threadId":session,"turnId":turn,"id":id})),
                    ))?;
                }
            }
            Some("host-turn") => match event["state"].as_str() {
                Some("start") => {
                    if let Some(host) = event["turn"].as_u64() {
                        if self.turn_id.is_none()
                            || host <= self.background.closed_host_turn
                            || self.background.host_turn.is_some_and(|last| host <= last)
                        {
                            return Ok(());
                        }
                        self.background.first_host_turn.get_or_insert(host);
                        self.background.step_offset = self
                            .background
                            .step_offset
                            .saturating_add(self.background.host_step);
                        self.background.host_step = 0;
                        self.background.host_turn = Some(host);
                        self.background.resume_pending = false;
                    }
                    self.host_turn_running = true;
                    self.goal_idle_deadline = None;
                    self.background_notice()?;
                }
                Some("end") => {
                    if event["turn"]
                        .as_u64()
                        .is_some_and(|host| self.background.host_turn != Some(host))
                    {
                        return Ok(());
                    }
                    self.host_turn_running = false;
                    self.background_notice()?;
                    if self.deferred_finish.is_some() {
                        if self.goal_active {
                            self.goal_idle_deadline = Some(tokio::time::Instant::now() + GOAL_IDLE);
                        } else {
                            self.finish_deferred()?;
                        }
                    }
                }
                _ => {}
            },
            Some("goal") => {
                match event["operation"].as_str() {
                    Some("create" | "resume") => self.goal_active = true,
                    Some(
                        "block" | "complete" | "clear" | "cancel" | "pause" | "delete"
                        | "suppressed",
                    ) => self.goal_active = false,
                    _ => {}
                }
                if !self.goal_active && !self.host_turn_running && self.deferred_finish.is_some() {
                    self.finish_deferred()?;
                }
            }
            Some("goal-round") => {
                let (Some(turn), Some(round)) = (self.turn_id.clone(), event["round"].as_u64())
                else {
                    return Ok(());
                };
                self.core.send(Envelope::notification(
                    "turn/notice",
                    Some(json!({"threadId":session,"turnId":turn,"kind":"goal",
                        "round":round,"maxRounds":event["max"].as_u64()})),
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
                "description":input.and_then(|input| input.reason.as_deref()),
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
        if matches!(kind, Pending::Ignored) {
            return Ok(());
        }
        if let Pending::Prompt { turn_id } = &kind {
            if self.turn_id.as_ref() != Some(turn_id) {
                return Ok(());
            }
        }
        if matches!(kind, Pending::BackgroundCancel { .. }) && message.error.is_some() {
            return Err(TransportError::Failed(
                "DSH background cancellation was not confirmed".into(),
            ));
        }
        if let (Some(error), Pending::Prompt { .. }) = (&message.error, &kind) {
            // ACP 的 prompt 错误响应表示这一轮已确定以错误结束，不是不确定结果；
            // 映射为 failed 终态，用户可以继续提交或重试。
            let text = error
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("ACP prompt failed")
                .to_owned();
            if !self.background.runs.is_empty() || self.background.host_turn.is_some() {
                self.background
                    .failure
                    .get_or_insert(BackgroundFailure::Unknown);
                self.deferred_finish = Some((id, "failed".into()));
                return self.finish_deferred();
            }
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
                self.background = BackgroundWork::default();
                self.suppress_goals(&session)?;
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
                self.background = BackgroundWork::default();
                self.suppress_goals(&session)?;
                let models = acp_protocol::model_choices(&result);
                self.models = models.clone();
                // An explicit `--reasoning` wins over the agent's default and is applied
                // through the same post-model restore path.
                let current = acp_protocol::select_option(&result, "reasoning_effort")
                    .map(|(current, _)| current)
                    .filter(|current| !current.is_empty());
                self.reasoning = self.reasoning.take().or(current);
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
            Pending::Prompt { .. } => {
                let status = match acp_protocol::stop_reason(&result) {
                    Some("end_turn" | "completed") => "completed",
                    Some("cancelled" | "canceled") => "interrupted",
                    Some(_) | None => "failed",
                };
                if self.background.host_turn.is_some()
                    || !self.background.runs.is_empty()
                    || self.background.canceling
                {
                    let status = if self.background.canceling {
                        "interrupted"
                    } else {
                        status
                    };
                    self.deferred_finish = Some((id, status.into()));
                    self.settle_background()?;
                } else if status == "completed" && self.goal_active && self.turn_id.is_some() {
                    // dsh continues the goal on its own; keep this turn open for those rounds.
                    self.deferred_finish = Some((id, status.into()));
                    if !self.host_turn_running {
                        self.goal_idle_deadline = Some(tokio::time::Instant::now() + GOAL_IDLE);
                    }
                } else {
                    self.finish_prompt(id, status, None)?;
                }
            }
            Pending::BackgroundCancel { core_id } => {
                if result["stopped"] != true {
                    return Err(TransportError::Failed(
                        "DSH background cancellation was not confirmed".into(),
                    ));
                }
                self.background.canceling = false;
                self.background.resume_pending = false;
                self.core
                    .send(Envelope::response(core_id, Some(json!({}))))?;
                self.finish_deferred()?;
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
            Pending::Ignored => {}
        }
        Ok(())
    }

    /// Ask the bridge to keep this modeling session free of dsh session goals.
    fn suppress_goals(&mut self, session: &str) -> Result<(), TransportError> {
        if !self.no_goals {
            return Ok(());
        }
        let id = RpcId::String(format!("acp-no-goals-{session}"));
        self.pending.insert(id.clone(), Pending::Ignored);
        self.acp.send(with_jsonrpc(Envelope::request(
            id,
            "zf/session/goals",
            Some(json!({"sessionId": session, "allowed": false})),
        )))
    }

    /// End a turn whose prompt returned while dsh was still continuing its goal.
    fn settle_background(&mut self) -> Result<(), TransportError> {
        if self.goal_active
            && self.background.failure.is_none()
            && !self.background.interrupted
            && !self.background.canceling
        {
            if !self.host_turn_running && self.deferred_finish.is_some() {
                self.goal_idle_deadline
                    .get_or_insert(tokio::time::Instant::now() + GOAL_IDLE);
            }
            return Ok(());
        }
        self.finish_deferred()
    }

    /// 等后台 children、宿主续行及其输出一起收尾，不触发额外 prompt。
    fn finish_deferred(&mut self) -> Result<(), TransportError> {
        if !self.background.runs.is_empty()
            || self.background.resume_pending
            || self.background.canceling
            || self.host_turn_running
            || self
                .background
                .host_turn
                .is_some_and(|host| self.background.settled_turn != Some(host))
        {
            return Ok(());
        }
        self.goal_idle_deadline = None;
        match self.deferred_finish.take() {
            Some((id, status)) => self.finish_prompt(
                id,
                if self.background.failure.is_some() {
                    "failed"
                } else if self.background.interrupted {
                    "interrupted"
                } else {
                    &status
                },
                self.background
                    .failure
                    .as_ref()
                    .map(|failure| match failure {
                        BackgroundFailure::Provider(Some(code)) => {
                            format!("模型服务请求失败（{code}）")
                        }
                        BackgroundFailure::Provider(None) => {
                            "模型服务请求失败（未知错误码）".into()
                        }
                        BackgroundFailure::OutputDelivery => "DSH 输出转换或投递失败".into(),
                        BackgroundFailure::Unknown => "DSH 后台续行或输出投递失败".into(),
                    }),
            ),
            None => Ok(()),
        }
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
        if let Some(session) = self.session_id.clone() {
            self.discard_live(&session, &turn)?;
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
        self.background_notice()?;
        self.core.send(Envelope::notification(
            "turn/completed",
            Some(json!({
                "threadId":self.session_id,
                "turn":{"id":turn,"status":status,"error":error.map(|message| json!({"message":message}))}
            })),
        ))?;
        self.turn_started = false;
        self.turn_id = None;
        // 后台取消可先于原 prompt 响应完成；退休其 RPC，避免迟到响应作用于下一轮。
        self.pending.retain(
            |_, pending| !matches!(pending, Pending::Prompt { turn_id } if turn_id == &turn),
        );
        self.background.closed_host_turn = self
            .background
            .host_turn
            .unwrap_or(self.background.closed_host_turn);
        self.background.first_host_turn = None;
        self.background.host_turn = None;
        self.background.settled_turn = None;
        self.background.resume_pending = false;
        self.background.failure = None;
        self.background.interrupted = false;
        self.background.delegated = false;
        self.background.step_offset = 0;
        self.background.host_step = 0;
        Ok(())
    }

    fn background_notice(&self) -> Result<(), TransportError> {
        if !self.background.delegated {
            return Ok(());
        }
        if let Some(turn) = &self.turn_id {
            let running = self.host_turn_running
                || self.background.resume_pending
                || self
                    .background
                    .host_turn
                    .is_some_and(|host| self.background.settled_turn != Some(host));
            self.core.send(Envelope::notification(
                "turn/notice",
                Some(json!({
                    "threadId":self.session_id,"turnId":turn,"kind":"background",
                    "pending":self.background.runs.len(),"running":running
                })),
            ))?;
        }
        Ok(())
    }

    async fn update(&mut self, params: Value) -> Result<(), TransportError> {
        if params["sessionId"].as_str() != self.session_id.as_deref() {
            return Ok(());
        }
        if let Some(host) = params["_meta"]["zfHostTurn"].as_u64() {
            if self
                .background
                .first_host_turn
                .is_none_or(|first| host < first)
                || self.background.host_turn.is_none_or(|last| host > last)
                || host <= self.background.closed_host_turn
            {
                return Ok(());
            }
        } else if self.background.host_turn.is_some()
            && params["update"]["sessionUpdate"] != "config_option_update"
        {
            return Ok(());
        }
        let (session, update) = match acp_protocol::update(&params) {
            Ok(value) => value,
            Err(error) => {
                if !self.background.runs.is_empty() || self.background.host_turn.is_some() {
                    return Err(TransportError::Failed(
                        "DSH ACP update failed during background work".into(),
                    ));
                }
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
                let item_id = self.block_id(item_id, false);
                let replaced = if self.last_message_id.as_deref() != Some(&item_id) { self.replace_live(&session, &turn, false)? } else { Vec::new() };
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
                self.core.send(Envelope::notification("item/agentMessage/delta", Some(json!({"threadId":session,"turnId":turn,"itemId":item_id,"delta":text,"replaces":replaced}))))?
            }
            Update::AgentThought { item_id, text } => {
                let previous = self.block.clone();
                let item_id = self.block_id(item_id, true);
                let replaced = if previous != self.block { self.replace_live(&session, &turn, true)? } else { Vec::new() };
                self.core.send(Envelope::notification("item/reasoning/textDelta", Some(json!({"threadId":session,"turnId":turn,"itemId":item_id,"delta":text,"replaces":replaced}))))?
            }
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

    #[test]
    fn domain_dsh_arguments_include_global_patch() {
        let config = Config {
            domain_mcp_patch: Some("domain patch.yml".into()),
            ..Default::default()
        };
        assert_eq!(
            super::dsh_arguments(&config),
            ["--profile", "zf-tui", "--patch", "domain patch.yml"].map(std::ffi::OsString::from)
        );
    }

    #[tokio::test]
    async fn domain_subagent_notices_arrive_between_turns_and_reject_foreign_sessions() {
        let (mut core, core_bridge) = pair();
        let (_, acp_bridge) = pair();
        let mut bridge = AcpBridge::new(core_bridge, acp_bridge, &Config::default());
        bridge.session_id = Some("s".into());
        for session in ["foreign", "s"] {
            bridge.plugin_event(json!({"sessionId":session,"event":{"type":"subagent","id":"child","state":"completed","summary":"区域判读结束"}})).unwrap();
        }
        let event = core.recv().await.unwrap();
        assert_eq!(event.method.as_deref(), Some("subagent/notice"));
        assert_eq!(
            event.params.unwrap(),
            json!({"threadId":"s","id":"child","state":"completed","summary":"区域判读结束"})
        );
    }

    #[tokio::test]
    async fn background_children_keep_parent_continuations_and_drained_outputs_in_one_execution() {
        for status in ["completed", "failed", "interrupted"] {
            let (mut core, core_bridge) = pair();
            let (mut acp, acp_bridge) = pair();
            let mut bridge = AcpBridge::new(core_bridge, acp_bridge, &Config::default());
            bridge.session_id = Some("s".into());
            bridge
                .handle_core(app_server::turn_start(RpcId::Number(7), "s", "work", None))
                .await
                .unwrap();
            assert_eq!(
                acp.recv().await.unwrap().method.as_deref(),
                Some("session/prompt")
            );
            let event = |value: Value| json!({"sessionId":"s","event":value});
            bridge
                .plugin_event(event(json!({"type":"host-turn","turn":1,"state":"start"})))
                .unwrap();
            bridge
                .plugin_event(event(
                    json!({"type":"subagent-run","id":"child","runId":"run-1","state":"start"}),
                ))
                .unwrap();
            bridge
                .plugin_event(event(
                    json!({"type":"step","turn":1,"step":1,"state":"start"}),
                ))
                .unwrap();
            assert_eq!(until(&mut core, "turn/step").await.1["step"], 1);
            bridge
                .plugin_event(event(
                    json!({"type":"step","turn":1,"step":1,"state":"end"}),
                ))
                .unwrap();
            until(&mut core, "turn/step").await;
            bridge
                .plugin_event(event(
                    json!({"type":"subagent-run","id":"child","runId":"run-1","state":"start"}),
                ))
                .unwrap();
            bridge
                .plugin_event(event(json!({"type":"host-turn","turn":1,"state":"end"})))
                .unwrap();
            bridge
                .plugin_event(event(
                    json!({"type":"host-settled","turn":1,"state":"completed"}),
                ))
                .unwrap();
            bridge
                .response(
                    RpcId::Number(7),
                    Envelope::response(RpcId::Number(7), Some(json!({"stopReason":"end_turn"}))),
                )
                .await
                .unwrap();
            assert_eq!(
                bridge.turn_id.as_deref(),
                Some("acp-turn-7"),
                "a background child is not a completed workflow"
            );
            assert_eq!(bridge.background.runs.len(), 1);
            bridge
                .plugin_event(event(
                    json!({"type":"subagent-run","id":"wrong","runId":"run-1","state":"end"}),
                ))
                .unwrap();
            assert_eq!(bridge.background.runs.len(), 1);
            bridge
                .plugin_event(event(
                    json!({"type":"subagent-run","id":"child","runId":"run-1","state":"end"}),
                ))
                .unwrap();
            assert!(
                bridge.turn_id.is_some(),
                "settlement must wait for the already-enqueued parent continuation"
            );
            bridge
                .plugin_event(event(json!({"type":"host-turn","turn":2,"state":"start"})))
                .unwrap();
            bridge
                .plugin_event(event(
                    json!({"type":"step","turn":2,"step":1,"state":"start"}),
                ))
                .unwrap();
            assert_eq!(until(&mut core, "turn/step").await.1["step"], 2);
            bridge.update(json!({"sessionId":"s","_meta":{"zfHostTurn":2},"update":{
                "sessionUpdate":"agent_message_chunk","messageId":"parent-result","content":{"type":"text","text":"child result registered"}
            }})).await.unwrap();
            bridge
                .plugin_event(event(json!({"type":"host-turn","turn":1,"state":"end"})))
                .unwrap();
            assert!(
                bridge.host_turn_running,
                "a late old boundary cannot end the continuation"
            );
            bridge
                .plugin_event(event(json!({"type":"host-turn","turn":2,"state":"end"})))
                .unwrap();
            assert!(
                bridge.turn_id.is_some(),
                "raw host turn/end is not an output drain"
            );
            bridge
                .plugin_event(event(
                    json!({"type":"host-settled","turn":2,"state":status}),
                ))
                .unwrap();
            let (methods, finished) = until(&mut core, "turn/completed").await;
            assert_eq!(finished["turn"]["id"], "acp-turn-7");
            assert_eq!(finished["turn"]["status"], status);
            assert!(methods.iter().any(|m| m == "item/agentMessage/delta"));
            assert!(methods.iter().any(|m| m == "item/completed"));
            assert!(bridge.turn_id.is_none());
            assert!(
                tokio::time::timeout(Duration::from_millis(20), acp.recv())
                    .await
                    .is_err(),
                "Native must not issue another prompt to consume the same child result"
            );
            bridge
                .handle_core(app_server::turn_start(RpcId::Number(8), "s", "next", None))
                .await
                .unwrap();
            acp.recv().await.unwrap();
            bridge
                .plugin_event(event(json!({"type":"host-turn","turn":2,"state":"start"})))
                .unwrap();
            bridge
                .update(json!({"sessionId":"s","_meta":{"zfHostTurn":2},"update":null}))
                .await
                .unwrap();
            assert_eq!(
                bridge.turn_id.as_deref(),
                Some("acp-turn-8"),
                "a malformed retired update cannot fail the new turn"
            );
            bridge.update(json!({"sessionId":"s","_meta":{"zfHostTurn":2},"update":{
                "sessionUpdate":"agent_message_chunk","messageId":"late","content":{"type":"text","text":"old output"}
            }})).await.unwrap();
            assert!(!bridge.message_text.contains_key("late"));
            bridge
                .plugin_event(event(json!({"type":"host-turn","turn":3,"state":"start"})))
                .unwrap();
            bridge.update(json!({"sessionId":"s","_meta":{"zfHostTurn":3},"update":{
                "sessionUpdate":"agent_message_chunk","messageId":"current","content":{"type":"text","text":"new output"}
            }})).await.unwrap();
            assert!(bridge.message_text.contains_key("current"));
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn two_background_children_resume_parent_over_mock_acp_without_another_prompt() {
        for (terminal, category, code, expected_error) in [
            ("completed", None, None, None),
            ("interrupted", None, None, None),
            ("failed", None, None, Some("DSH 后台续行或输出投递失败")),
            (
                "failed",
                Some("provider"),
                Some("PI_AI_ERROR"),
                Some("模型服务请求失败（PI_AI_ERROR）"),
            ),
            (
                "failed",
                Some("provider"),
                Some("RATE_LIMIT"),
                Some("模型服务请求失败（RATE_LIMIT）"),
            ),
            (
                "failed",
                Some("provider"),
                Some("PRIVATE_CODE"),
                Some("模型服务请求失败（未知错误码）"),
            ),
            (
                "failed",
                Some("output_delivery"),
                Some("PRIVATE_CODE"),
                Some("DSH 输出转换或投递失败"),
            ),
            (
                "failed",
                Some("PRIVATE_CATEGORY"),
                Some("PRIVATE_CODE"),
                Some("DSH 后台续行或输出投递失败"),
            ),
        ] {
            let (mut core, core_bridge) = pair();
            let (mut acp, acp_bridge) = pair();
            let config = Config::default();
            let task = tokio::spawn(AcpBridge::new(core_bridge, acp_bridge, &config).run());
            let prompt = prompted_turn(&mut core, &mut acp, &config).await;
            let event = |value: Value| {
                Envelope::notification(
                    "zf/session/event",
                    Some(json!({"sessionId":"s","event":value})),
                )
            };
            let message = |host: u64, id: &str, text: &str| {
                Envelope::notification(
                    "session/update",
                    Some(
                        json!({"sessionId":"s","_meta":{"zfHostTurn":host},"update":{
                            "sessionUpdate":"agent_message_chunk","messageId":id,"content":{"type":"text","text":text}
                        }}),
                    ),
                )
            };
            acp.send(event(json!({"type":"host-turn","turn":1,"state":"start"})))
                .unwrap();
            for id in ["a", "b"] {
                acp.send(event(
                    json!({"type":"subagent-run","runId":id,"id":id,"state":"start"}),
                ))
                .unwrap();
            }
            acp.send(message(1, "independent", "parent independent work"))
                .unwrap();
            acp.send(event(json!({"type":"host-turn","turn":1,"state":"end"})))
                .unwrap();
            acp.send(event(
                json!({"type":"host-settled","turn":1,"state":"completed"}),
            ))
            .unwrap();
            acp.send(Envelope::response(
                prompt,
                Some(json!({"stopReason":"end_turn"})),
            ))
            .unwrap();
            let mut output = Vec::new();
            let mut messages = Vec::new();
            let mut root_turn = String::new();
            for pending in [2, 1] {
                loop {
                    let frame = tokio::time::timeout(Duration::from_secs(2), core.recv())
                        .await
                        .unwrap()
                        .unwrap();
                    assert_ne!(
                        frame.method.as_deref(),
                        Some("turn/completed"),
                        "{terminal}: unfinished background work was declared complete"
                    );
                    let params = frame.params.unwrap_or(Value::Null);
                    if frame.method.as_deref() == Some("item/agentMessage/delta") {
                        messages.push(params["delta"].as_str().unwrap().to_owned());
                        root_turn = params["turnId"].as_str().unwrap().into();
                    }
                    if frame.method.as_deref() == Some("turn/notice")
                        && params["kind"] == "background"
                        && params["pending"] == pending
                        && params["running"] == false
                    {
                        break;
                    }
                }
                if pending == 2 {
                    acp.send(event(json!({"type":"host-turn","turn":2,"state":"start"})))
                        .unwrap();
                    acp.send(event(
                        json!({"type":"subagent-run","runId":"a","id":"a","state":"end"}),
                    ))
                    .unwrap();
                    acp.send(message(
                        2,
                        "resume-a",
                        "child A registered; parent continues independent work",
                    ))
                    .unwrap();
                    acp.send(event(json!({"type":"host-turn","turn":2,"state":"end"})))
                        .unwrap();
                    acp.send(event(
                        json!({"type":"host-settled","turn":2,"state":"completed"}),
                    ))
                    .unwrap();
                }
            }
            assert!(messages.iter().any(|m| m.starts_with("child A registered")));
            let cancel = if terminal == "interrupted" {
                core.send(Envelope::request(
                    RpcId::Number(9),
                    "turn/interrupt",
                    Some(json!({"threadId":"s","turnId":root_turn})),
                ))
                .unwrap();
                assert_eq!(
                    acp.recv().await.unwrap().method.as_deref(),
                    Some("session/cancel")
                );
                let cancel = acp.recv().await.unwrap();
                assert_eq!(
                    cancel.method.as_deref(),
                    Some("zf/session/background/cancel")
                );
                cancel.id
            } else {
                None
            };
            // 已退休宿主轮的边界、预览和坏帧均不得影响下一次 parent 续行。
            acp.send(event(json!({"type":"host-turn","turn":1,"state":"end"})))
                .unwrap();
            acp.send(event(json!({"type":"host-turn","turn":3,"state":"start"})))
                .unwrap();
            acp.send(event(
                json!({"type":"delta","turn":1,"kind":"text","block":0,"text":"LATE_PREVIEW"}),
            ))
            .unwrap();
            acp.send(event(
                json!({"type":"subagent-run","runId":"b","id":"b","state":"end"}),
            ))
            .unwrap();
            acp.send(event(
                json!({"type":"subagent-run","runId":"a","id":"a","state":"end"}),
            ))
            .unwrap();
            acp.send(message(3, "resume-b", "child B registered"))
                .unwrap();
            acp.send(event(json!({"type":"host-turn","turn":3,"state":"end"})))
                .unwrap();
            acp.send(event(
                json!({"type":"host-settled","turn":3,"state":terminal,
                    "failureCategory":category,"failureCode":code,"message":"PRIVATE_PROVIDER_SECRET"}),
            ))
            .unwrap();
            if let Some(id) = cancel {
                acp.send(Envelope::response(id, Some(json!({"stopped":true}))))
                    .unwrap();
            }
            loop {
                let frame = tokio::time::timeout(Duration::from_secs(2), core.recv())
                    .await
                    .unwrap()
                    .unwrap();
                let params = frame.params.unwrap_or(Value::Null);
                if frame.method.as_deref() == Some("item/agentMessage/delta") {
                    assert_eq!(params["turnId"], root_turn);
                    messages.push(params["delta"].as_str().unwrap().to_owned());
                }
                output.push(frame.method.clone());
                if frame.method.as_deref() == Some("turn/completed") {
                    assert_eq!(params["turn"]["id"], root_turn);
                    assert_eq!(params["turn"]["status"], terminal);
                    assert_eq!(params["turn"]["error"]["message"].as_str(), expected_error);
                    assert!(!params.to_string().contains("PRIVATE_"));
                    break;
                }
            }
            assert_eq!(
                messages,
                [
                    "parent independent work",
                    "child A registered; parent continues independent work",
                    "child B registered"
                ]
            );
            assert_eq!(
                output
                    .iter()
                    .filter(|method| method.as_deref() == Some("turn/completed"))
                    .count(),
                1
            );
            assert!(
                tokio::time::timeout(Duration::from_millis(30), acp.recv())
                    .await
                    .is_err(),
                "{terminal}: duplicate prompt or restart"
            );
            drop(core);
            tokio::time::timeout(Duration::from_secs(2), task)
                .await
                .unwrap()
                .unwrap();
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn canceled_background_prompt_response_cannot_finish_the_next_execution() {
        for old_error in [false, true] {
            for after_host_start in [false, true] {
                let (mut core, core_bridge) = pair();
                let (mut acp, acp_bridge) = pair();
                let config = Config::default();
                let task = tokio::spawn(AcpBridge::new(core_bridge, acp_bridge, &config).run());
                let old_prompt = prompted_turn(&mut core, &mut acp, &config).await;
                let event = |value: Value| {
                    Envelope::notification(
                        "zf/session/event",
                        Some(json!({"sessionId":"s","event":value})),
                    )
                };
                acp.send(event(json!({"type":"host-turn","turn":1,"state":"start"})))
                    .unwrap();
                acp.send(event(
                    json!({"type":"subagent-run","runId":"a","id":"a","state":"start"}),
                ))
                .unwrap();
                loop {
                    let (_, notice) = until(&mut core, "turn/notice").await;
                    if notice["kind"] == "background" && notice["pending"] == 1 {
                        break;
                    }
                }
                core.send(Envelope::request(
                    RpcId::Number(9),
                    "turn/interrupt",
                    Some(json!({"threadId":"s","turnId":"acp-turn-3"})),
                ))
                .unwrap();
                assert_eq!(
                    acp.recv().await.unwrap().method.as_deref(),
                    Some("session/cancel")
                );
                let cancel = acp.recv().await.unwrap();
                assert_eq!(
                    cancel.method.as_deref(),
                    Some("zf/session/background/cancel")
                );
                acp.send(event(
                    json!({"type":"subagent-run","runId":"a","id":"a","state":"end"}),
                ))
                .unwrap();
                acp.send(event(json!({"type":"host-turn","turn":1,"state":"end"})))
                    .unwrap();
                acp.send(event(
                    json!({"type":"host-settled","turn":1,"state":"interrupted"}),
                ))
                .unwrap();
                acp.send(Envelope::response(
                    cancel.id.unwrap(),
                    Some(json!({"stopped":true})),
                ))
                .unwrap();
                let (before, finished) = until(&mut core, "turn/completed").await;
                assert!(!before.iter().any(|method| method == "turn/completed"));
                assert_eq!(finished["turn"]["id"], "acp-turn-3");
                assert_eq!(finished["turn"]["status"], "interrupted");

                core.send(app_server::turn_start(RpcId::Number(10), "s", "next", None))
                    .unwrap();
                let next_prompt = acp.recv().await.unwrap();
                assert_eq!(next_prompt.method.as_deref(), Some("session/prompt"));
                let (_, started) = until(&mut core, "turn/started").await;
                assert_eq!(started["turn"]["id"], "acp-turn-10");
                if after_host_start {
                    acp.send(event(json!({"type":"host-turn","turn":2,"state":"start"})))
                        .unwrap();
                }
                acp.send(if old_error {
                    Envelope::error_response(old_prompt, -32000, "retired prompt failed")
                } else {
                    Envelope::response(old_prompt, Some(json!({"stopReason":"end_turn"})))
                })
                .unwrap();
                if !after_host_start {
                    acp.send(event(json!({"type":"host-turn","turn":2,"state":"start"})))
                        .unwrap();
                }
                acp.send(Envelope::notification(
                    "session/update",
                    Some(json!({
                        "sessionId":"s","_meta":{"zfHostTurn":2},"update":{
                            "sessionUpdate":"agent_message_chunk","messageId":"next-output",
                            "content":{"type":"text","text":"new execution output"}
                        }
                    })),
                ))
                .unwrap();
                acp.send(event(json!({"type":"host-turn","turn":2,"state":"end"})))
                    .unwrap();
                acp.send(event(
                    json!({"type":"host-settled","turn":2,"state":"completed"}),
                ))
                .unwrap();
                acp.send(Envelope::response(
                    next_prompt.id.unwrap(),
                    Some(json!({"stopReason":"end_turn"})),
                ))
                .unwrap();
                let mut output = Vec::new();
                loop {
                    let frame = tokio::time::timeout(Duration::from_secs(2), core.recv())
                        .await
                        .unwrap()
                        .unwrap();
                    let params = frame.params.unwrap_or(Value::Null);
                    if frame.method.as_deref() == Some("item/agentMessage/delta") {
                        assert_eq!(params["turnId"], "acp-turn-10");
                        output.push(params["delta"].as_str().unwrap().to_owned());
                    }
                    if frame.method.as_deref() == Some("turn/completed") {
                        assert_eq!(params["turn"]["id"], "acp-turn-10");
                        assert_eq!(
                            params["turn"]["status"], "completed",
                            "old_error={old_error}, after_host_start={after_host_start}"
                        );
                        assert!(params["turn"]["error"].is_null());
                        break;
                    }
                }
                assert_eq!(
                    output,
                    ["new execution output"],
                    "old_error={old_error}, after_host_start={after_host_start}"
                );
                assert!(
                    tokio::time::timeout(Duration::from_millis(30), core.recv())
                        .await
                        .is_err(),
                    "a retired response must not emit a second terminal"
                );
                assert!(
                    tokio::time::timeout(Duration::from_millis(30), acp.recv())
                        .await
                        .is_err(),
                    "each execution sends exactly one prompt"
                );
                drop(core);
                tokio::time::timeout(Duration::from_secs(2), task)
                    .await
                    .unwrap()
                    .unwrap();
            }
        }
    }

    #[tokio::test]
    async fn background_interrupt_waits_for_descendant_cancellation_and_parent_output_drain() {
        let (mut core, core_bridge) = pair();
        let (mut acp, acp_bridge) = pair();
        let mut bridge = AcpBridge::new(core_bridge, acp_bridge, &Config::default());
        bridge.session_id = Some("s".into());
        bridge
            .handle_core(app_server::turn_start(RpcId::Number(7), "s", "work", None))
            .await
            .unwrap();
        acp.recv().await.unwrap();
        let event = |value: Value| json!({"sessionId":"s","event":value});
        bridge
            .plugin_event(event(json!({"type":"host-turn","turn":1,"state":"start"})))
            .unwrap();
        bridge
            .plugin_event(event(
                json!({"type":"subagent-run","id":"child","runId":"run-1","state":"start"}),
            ))
            .unwrap();
        bridge
            .plugin_event(event(json!({"type":"host-turn","turn":1,"state":"end"})))
            .unwrap();
        bridge
            .plugin_event(event(
                json!({"type":"host-settled","turn":1,"state":"completed"}),
            ))
            .unwrap();
        bridge
            .response(
                RpcId::Number(7),
                Envelope::response(RpcId::Number(7), Some(json!({"stopReason":"end_turn"}))),
            )
            .await
            .unwrap();
        bridge
            .handle_core(Envelope::request(
                RpcId::Number(9),
                "turn/interrupt",
                Some(json!({"threadId":"s","turnId":"acp-turn-7"})),
            ))
            .await
            .unwrap();
        assert_eq!(
            acp.recv().await.unwrap().method.as_deref(),
            Some("session/cancel")
        );
        let cancel = acp.recv().await.unwrap();
        assert_eq!(
            cancel.method.as_deref(),
            Some("zf/session/background/cancel")
        );
        assert!(bridge.turn_id.is_some());
        bridge
            .plugin_event(event(json!({"type":"host-turn","turn":2,"state":"start"})))
            .unwrap();
        bridge
            .plugin_event(event(
                json!({"type":"subagent-run","id":"child","runId":"run-1","state":"end"}),
            ))
            .unwrap();
        bridge
            .response(
                cancel.id.clone().unwrap(),
                Envelope::response(cancel.id.unwrap(), Some(json!({"stopped":true}))),
            )
            .await
            .unwrap();
        assert!(bridge.turn_id.is_some());
        bridge
            .plugin_event(event(json!({"type":"host-turn","turn":2,"state":"end"})))
            .unwrap();
        bridge
            .plugin_event(event(
                json!({"type":"host-settled","turn":2,"state":"interrupted"}),
            ))
            .unwrap();
        let (_, finished) = until(&mut core, "turn/completed").await;
        assert_eq!(finished["turn"]["status"], "interrupted");
        bridge
            .plugin_event(event(json!({"type":"host-turn","turn":2,"state":"start"})))
            .unwrap();
        assert!(bridge.turn_id.is_none());
    }

    #[tokio::test]
    async fn domain_global_mcp_content_update_is_still_a_verified_receipt() {
        let (mut core, core_bridge) = pair();
        let (_, acp_bridge) = pair();
        let mut bridge = AcpBridge::new(core_bridge, acp_bridge, &Config::default());
        bridge.session_id = Some("s".into());
        bridge.turn_id = Some("t".into());
        bridge.turn_started = true;
        bridge.update(json!({"sessionId":"s","update":{"sessionUpdate":"tool_call","toolCallId":"c","title":"mcp__modeling_domain__inspect_change","rawInput":{}}})).await.unwrap();
        core.recv().await.unwrap();
        let receipt = json!({"contract":"modeling-fact-receipt/v1","run_id":"run","context_sha256":"ctx","kind":"capabilities","ok":true,"fact_sequence":1,"fact_sha256":"a".repeat(64),"fact_bytes":2,"binding":{}});
        bridge.update(json!({"sessionId":"s","update":{"sessionUpdate":"tool_call_update","toolCallId":"c","status":"completed","content":[{"type":"content","content":{"type":"text","text":receipt.to_string()}}]}})).await.unwrap();
        let item = core.recv().await.unwrap().params.unwrap()["item"].clone();
        let setup = crate::domain::DomainSetup {
            mode: crate::domain::DomainMode::Project,
            context_path: "context".into(),
            repository: ".".into(),
            python: "python".into(),
            run_id: "run".into(),
            context_sha256: "ctx".into(),
            floor_id: "floor".into(),
        };
        let notice = crate::protocol::ObservedTool {
            thread_id: "s".into(),
            turn_id: "t".into(),
            item_id: "c".into(),
            outcome: Some(crate::protocol::ObservedToolOutcome::Completed),
            category: crate::protocol::ToolCategory::Mcp,
            compaction: None,
        };
        assert_eq!(item["server"], "modeling_domain");
        assert_eq!(
            crate::domain::receipt::from_tool(&setup, &notice, &item)
                .unwrap()
                .unwrap()
                .fact_sequence,
            1
        );
    }

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
    async fn requested_reasoning_effort_is_selected_after_the_model() {
        let (core_client, core_bridge) = pair();
        let (mut acp_server, acp_bridge) = pair();
        let config = Config {
            model: Some("hi/gpt-6-astra".into()),
            reasoning: Some("low".into()),
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
                    {"id":"reasoning_effort","currentValue":"high","options":[{"value":"low"},{"value":"high"}]}]})),
            ))
            .unwrap();
        let select = acp_server.recv().await.unwrap();
        assert_eq!(select.params.unwrap()["configId"], "model");
        acp_server
            .send(Envelope::response(
                select.id.unwrap(),
                Some(json!({"configOptions":[
                    {"id":"reasoning_effort","currentValue":"high","options":[{"value":"low"},{"value":"high"}]}]})),
            ))
            .unwrap();
        let effort = acp_server.recv().await.unwrap();
        let params = effort.params.unwrap();
        assert_eq!(params["configId"], "reasoning_effort");
        assert_eq!(params["value"], "low");
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
    use serde_json::Value;

    /// Start a session and one prompted turn; returns the ACP prompt request id.
    async fn prompted_turn(
        core_client: &mut PipeTransport,
        acp_server: &mut PipeTransport,
        config: &Config,
    ) -> RpcId {
        core_client
            .send(app_server::thread_start(RpcId::Number(2), config))
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
        let prompt = acp_server.recv().await.unwrap();
        core_client.recv().await.unwrap();
        prompt.id.unwrap()
    }

    /// Core frames until one has `method`; returns the methods seen before it.
    async fn until(core_client: &mut PipeTransport, method: &str) -> (Vec<String>, Value) {
        let mut seen = Vec::new();
        loop {
            let frame = tokio::time::timeout(Duration::from_secs(10), core_client.recv())
                .await
                .expect("frame in time")
                .unwrap();
            let name = frame.method.clone().unwrap_or_default();
            if name == method {
                return (seen, frame.params.unwrap_or(Value::Null));
            }
            seen.push(name);
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn modeling_sessions_ask_the_bridge_to_forbid_session_goals() {
        for (modeling, expected) in [(true, true), (false, false)] {
            let (core_client, core_bridge) = pair();
            let (mut acp_server, acp_bridge) = pair();
            let mut config = Config::default();
            if modeling {
                config.domain_setup = Some(crate::domain::DomainSetup {
                    mode: crate::domain::DomainMode::Project,
                    context_path: "ctx.json".into(),
                    repository: ".".into(),
                    python: "python".into(),
                    run_id: "run".into(),
                    context_sha256: "c".repeat(64),
                    floor_id: "项目目标".into(),
                });
            }
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
            let next = tokio::time::timeout(Duration::from_millis(500), acp_server.recv()).await;
            let asked = matches!(&next, Ok(Ok(frame)) if frame.method.as_deref() == Some("zf/session/goals")
                && frame.params.as_ref().is_some_and(|p| p["allowed"] == false && p["sessionId"] == "s"));
            assert_eq!(asked, expected, "modeling={modeling}: {next:?}");
            drop(core_client);
            let _ = tokio::time::timeout(Duration::from_secs(2), bridge).await;
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn goal_rounds_after_the_prompt_returns_stay_in_the_same_turn() {
        let (mut core_client, core_bridge) = pair();
        let (mut acp_server, acp_bridge) = pair();
        let config = Config::default();
        let bridge = tokio::spawn(AcpBridge::new(core_bridge, acp_bridge, &config).run());
        let prompt = prompted_turn(&mut core_client, &mut acp_server, &config).await;
        let event = |event: Value| {
            Envelope::notification(
                "zf/session/event",
                Some(json!({"sessionId":"s","event":event})),
            )
        };
        acp_server
            .send(event(json!({"type":"host-turn","state":"start"})))
            .unwrap();
        acp_server
            .send(event(json!({"type":"goal","operation":"create"})))
            .unwrap();
        acp_server
            .send(event(json!({"type":"host-turn","state":"end"})))
            .unwrap();
        acp_server
            .send(Envelope::response(
                prompt,
                Some(json!({"stopReason":"end_turn"})),
            ))
            .unwrap();
        // dsh continues the goal by itself: same turn, marked as a goal round.
        acp_server
            .send(event(json!({"type":"host-turn","state":"start"})))
            .unwrap();
        acp_server
            .send(event(json!({"type":"goal-round","round":1,"max":30})))
            .unwrap();
        let (before, notice) = until(&mut core_client, "turn/notice").await;
        assert!(!before.iter().any(|m| m == "turn/completed"), "{before:?}");
        assert_eq!(
            (notice["kind"].as_str(), notice["round"].as_u64()),
            (Some("goal"), Some(1))
        );
        let turn = notice["turnId"].as_str().unwrap().to_owned();
        acp_server.send(Envelope::notification("session/update", Some(json!({"sessionId":"s","update":{
            "sessionUpdate":"agent_message_chunk","messageId":"g1","content":{"type":"text","text":"round output"}}})))).unwrap();
        let (before, delta) = until(&mut core_client, "item/agentMessage/delta").await;
        assert!(!before.iter().any(|m| m == "turn/completed"), "{before:?}");
        assert_eq!(
            delta["turnId"],
            turn.as_str(),
            "goal output is not lost or misattributed"
        );
        // A blocked goal ends with its round: the turn completes once.
        acp_server
            .send(event(json!({"type":"goal","operation":"block"})))
            .unwrap();
        acp_server
            .send(event(json!({"type":"host-turn","state":"end"})))
            .unwrap();
        let (_, completed) = until(&mut core_client, "turn/completed").await;
        assert_eq!(completed["turn"]["id"], turn.as_str());
        assert_eq!(completed["turn"]["status"], "completed");
        drop(core_client);
        let _ = tokio::time::timeout(Duration::from_secs(2), bridge).await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_active_goal_without_a_next_round_still_ends_the_turn() {
        let (mut core_client, core_bridge) = pair();
        let (mut acp_server, acp_bridge) = pair();
        let config = Config::default();
        let bridge = tokio::spawn(AcpBridge::new(core_bridge, acp_bridge, &config).run());
        let prompt = prompted_turn(&mut core_client, &mut acp_server, &config).await;
        acp_server
            .send(Envelope::notification(
                "zf/session/event",
                Some(json!({"sessionId":"s","event":{"type":"goal","operation":"create"}})),
            ))
            .unwrap();
        acp_server
            .send(Envelope::response(
                prompt,
                Some(json!({"stopReason":"end_turn"})),
            ))
            .unwrap();
        let started = std::time::Instant::now();
        let (_, completed) = until(&mut core_client, "turn/completed").await;
        assert_eq!(completed["turn"]["status"], "completed");
        assert!(
            started.elapsed() >= Duration::from_secs(4),
            "waited for a goal round"
        );
        drop(core_client);
        let _ = tokio::time::timeout(Duration::from_secs(2), bridge).await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn generated_text_is_a_preview_replaced_by_the_committed_message() {
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
        let event = |event: serde_json::Value| {
            Envelope::notification(
                "zf/session/event",
                Some(json!({"sessionId":"s","event":event})),
            )
        };
        macro_rules! next {
            () => {{
                let frame = core_client.recv().await.unwrap();
                let params = frame.params.unwrap();
                (
                    frame.method.unwrap(),
                    params["itemId"].as_str().unwrap_or_default().to_owned(),
                    params["delta"].as_str().unwrap_or_default().to_owned(),
                )
            }};
        }
        for delta in [
            json!({"type":"delta","kind":"thinking","block":0,"text":"weigh"}),
            json!({"type":"delta","kind":"text","block":1,"text":"Hel"}),
            json!({"type":"delta","kind":"text","block":1,"text":"lo"}),
            json!({"type":"delta-end","committed":true}),
        ] {
            acp_server.send(event(delta)).unwrap();
        }
        assert_eq!(
            next!(),
            (
                "item/reasoning/textDelta".into(),
                "live:0:thinking:0".into(),
                "weigh".into()
            )
        );
        assert_eq!(
            next!(),
            (
                "item/agentMessage/delta".into(),
                "live:0:text:1".into(),
                "Hel".into()
            )
        );
        assert_eq!(
            next!(),
            (
                "item/agentMessage/delta".into(),
                "live:0:text:1".into(),
                "lo".into()
            )
        );
        // 提交后预览留在原位，直到权威消息到达的那一刻才换掉。
        acp_server.send(Envelope::notification("session/update", Some(json!({"sessionId":"s","update":{
            "sessionUpdate":"agent_message_chunk","messageId":"m1","content":{"type":"text","text":"Hello"}}})))).unwrap();
        assert_eq!(
            next!(),
            (
                "item/discarded".into(),
                "live:0:thinking:0".into(),
                String::new()
            )
        );
        let replaced = core_client.recv().await.unwrap();
        assert_eq!(replaced.method.as_deref(), Some("item/agentMessage/delta"));
        let params = replaced.params.unwrap();
        assert_eq!(params["itemId"], "m1");
        assert_eq!(params["delta"], "Hello");
        assert_eq!(params["replaces"], json!(["live:0:text:1"]));
        // 被放弃的请求（例如随后重试）：预览立刻撤掉，没有消息来替换它。
        acp_server
            .send(event(
                json!({"type":"delta","kind":"text","block":0,"text":"partial"}),
            ))
            .unwrap();
        acp_server
            .send(event(json!({"type":"delta-end","committed":false})))
            .unwrap();
        assert_eq!(next!().1, "live:1:text:0");
        assert_eq!(
            next!(),
            (
                "item/discarded".into(),
                "live:1:text:0".into(),
                String::new()
            )
        );
        drop(core_client);
        let _ = tokio::time::timeout(Duration::from_secs(2), bridge).await;
    }

    #[tokio::test]
    async fn alternating_thought_and_text_blocks_of_one_message_keep_their_order() {
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
        let chunk = |kind: &str, message: &str, text: &str| {
            Envelope::notification(
                "session/update",
                Some(
                    json!({"sessionId":"s","update":{"sessionUpdate":kind,"messageId":message,
                    "content":{"type":"text","text":text}}}),
                ),
            )
        };
        for (block, kind) in [(0, "thinking"), (1, "text"), (2, "thinking"), (3, "text")] {
            acp_server.send(Envelope::notification("zf/session/event", Some(json!({
                "sessionId":"s", "event":{"type":"delta","block":block,"kind":kind,"text":"preview"}
            })))).unwrap();
        }
        for _ in 0..4 {
            let preview = core_client.recv().await.unwrap();
            assert!(preview.params.unwrap()["itemId"]
                .as_str()
                .unwrap()
                .starts_with("live:0:"));
        }
        acp_server
            .send(Envelope::notification(
                "zf/session/event",
                Some(json!({
                    "sessionId":"s", "event":{"type":"delta-end","committed":true}
                })),
            ))
            .unwrap();
        // dsh 把一条消息的各个内容块按顺序发来，共用一个 messageId。
        for (kind, message, text) in [
            ("agent_thought_chunk", "m1", "think A"),
            ("agent_thought_chunk", "m1", " more"),
            ("agent_message_chunk", "m1", "text B"),
            ("agent_thought_chunk", "m1", "think C"),
            ("agent_message_chunk", "m1", "text D"),
            ("agent_message_chunk", "m2", "next message"),
        ] {
            acp_server.send(chunk(kind, message, text)).unwrap();
        }
        let mut seen = Vec::new();
        let mut replacements = Vec::new();
        for _ in 0..6 {
            let event = core_client.recv().await.unwrap();
            let params = event.params.unwrap();
            replacements.push(params["replaces"].clone());
            seen.push((
                event.method.unwrap(),
                params["itemId"].as_str().unwrap().to_owned(),
                params["delta"].as_str().unwrap().to_owned(),
            ));
        }
        assert_eq!(
            replacements,
            [
                json!(["live:0:thinking:0"]),
                json!([]),
                json!(["live:0:text:1"]),
                json!(["live:0:thinking:2"]),
                json!(["live:0:text:3"]),
                json!([]),
            ]
        );
        let expected = [
            ("item/reasoning/textDelta", "m1", "think A"),
            ("item/reasoning/textDelta", "m1", " more"),
            ("item/agentMessage/delta", "m1#1", "text B"),
            ("item/reasoning/textDelta", "m1#2", "think C"),
            ("item/agentMessage/delta", "m1#3", "text D"),
            ("item/agentMessage/delta", "m2", "next message"),
        ];
        assert_eq!(
            seen.iter()
                .map(|(method, item, delta)| (method.as_str(), item.as_str(), delta.as_str()))
                .collect::<Vec<_>>(),
            expected
        );
        drop(core_client);
        let _ = tokio::time::timeout(Duration::from_secs(2), bridge).await;
    }

    #[tokio::test]
    async fn question_preview_reference_and_bound_answer_survive_acp_conversion() {
        let (mut core_client, core_bridge) = pair();
        let (mut acp_server, acp_bridge) = pair();
        let mut bridge = AcpBridge::new(core_bridge, acp_bridge, &Config::default());
        bridge.session_id = Some("s".into());
        bridge.turn_id = Some("t".into());
        let preview = json!({"html_path":"review/index.html","png_path":"review/review.png","data_sha256":"a".repeat(64)});
        let id = RpcId::String("preview".into());
        bridge
            .ask(
                id.clone(),
                json!({"sessionId":"s","questions":[{
                    "id":"candidate","header":"Pick","question":"Which?","preview":preview
                }]}),
            )
            .unwrap();
        let request = core_client.recv().await.unwrap();
        let params = request.params.unwrap();
        assert_eq!(params["questions"][0]["preview"], preview);
        let request = crate::interactions::RequestView::decode(
            id.clone(),
            "item/tool/requestUserInput",
            &params,
        )
        .unwrap();
        let result = request
            .input_result(&std::collections::BTreeMap::from([(
                "candidate".into(),
                vec!["Revise".into()],
            )]))
            .unwrap();
        assert_eq!(
            result["answers"]["candidate"]["preview_data_sha256"],
            preview["data_sha256"]
        );
        bridge
            .handle_core(Envelope::response(id.clone(), Some(result.clone())))
            .await
            .unwrap();
        let reply = acp_server.recv().await.unwrap();
        assert_eq!(reply.id, Some(id));
        assert_eq!(reply.result.unwrap(), result);
    }

    #[tokio::test]
    async fn batch_of_29_questions_reaches_core_without_losing_item_identity() {
        let (mut core_client, core_bridge) = pair();
        let (_acp_server, acp_bridge) = pair();
        let mut bridge = AcpBridge::new(core_bridge, acp_bridge, &Config::default());
        bridge.session_id = Some("s".into());
        bridge.turn_id = Some("t".into());
        let questions: Vec<serde_json::Value> = (0..29).map(|i| json!({
            "id":format!("component.hospital-floor-08.{i}"), "header":format!("构件 {i}"),
            "question":"方案 V1，来源姿态保持；投影校验阻塞。请选择保留、调整或暂缓。", "multiSelect":false,
            "options":[{"label":"保留","description":"校验通过后可申请批准"},
                {"label":"调整","description":"填写具体位移或尺寸依据"},
                {"label":"暂缓","description":"保持待确认"}]
        })).collect();
        let id = RpcId::String("batch".into());
        bridge
            .ask(
                id.clone(),
                json!({"sessionId":"s","questions":questions.clone()}),
            )
            .unwrap();
        let request = tokio::time::timeout(Duration::from_secs(1), core_client.recv())
            .await
            .unwrap()
            .unwrap();
        let params = request.params.unwrap();
        assert_eq!(params["questions"], json!(questions));
        assert!(crate::interactions::RequestView::decode(
            id,
            "item/tool/requestUserInput",
            &params
        )
        .is_ok());
    }

    #[tokio::test]
    async fn invalid_batch_questions_return_an_error_without_leaving_a_pending_request() {
        let (_core_client, core_bridge) = pair();
        let (mut acp_server, acp_bridge) = pair();
        let mut bridge = AcpBridge::new(core_bridge, acp_bridge, &Config::default());
        bridge.session_id = Some("s".into());
        bridge.turn_id = Some("t".into());
        let options: Vec<serde_json::Value> = (0..12)
            .map(|i| {
                json!({
                    "label":format!("{i}{}", "l".repeat(180)), "description":"d".repeat(380)
                })
            })
            .collect();
        let oversized: Vec<serde_json::Value> = (0..29).map(|i| json!({
            "id":format!("item-{i}"), "header":"Review", "question":"Choose", "options":options
        })).collect();
        let duplicate = vec![json!({"id":"same","header":"A","question":"Q"}); 2];
        for (i, questions) in [oversized, duplicate].into_iter().enumerate() {
            let id = RpcId::String(format!("invalid-{i}"));
            bridge
                .ask(id.clone(), json!({"sessionId":"s","questions":questions}))
                .unwrap();
            let response = tokio::time::timeout(Duration::from_secs(1), acp_server.recv())
                .await
                .unwrap()
                .unwrap();
            assert_eq!(response.id, Some(id));
            assert!(response.error.is_some());
            assert!(bridge.asks.is_empty());
        }
    }

    #[tokio::test]
    async fn questions_from_the_plugin_become_user_input_requests_and_answers_return() {
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
        let ask = |id: &str, session: &str, question: String| {
            Envelope::request(
                RpcId::String(id.into()),
                "zf/session/ask",
                Some(
                    json!({"sessionId":session,"questions":[{"id":"plan-review","header":"Plan review",
                    "question":question,"options":[{"label":"Approve","description":"Leave plan mode"},
                    {"label":"Keep planning","description":"Stay"}]},
                    {"id":"areas","header":"Areas","question":"Which areas?","multiSelect":true,
                    "options":[{"label":"Core","description":""},{"label":"UI","description":""}]}]}),
                ),
            )
        };
        // 没有进行中的轮次时没有人能回答：直接告诉 dsh，不留悬而未决的请求。
        acp_server
            .send(ask("zf-ask-0", "s", "early".into()))
            .unwrap();
        let refused = acp_server.recv().await.unwrap();
        assert_eq!(refused.id, Some(RpcId::String("zf-ask-0".into())));
        assert!(refused.error.is_some());
        core_client
            .send(app_server::turn_start(RpcId::Number(3), "s", "model", None))
            .unwrap();
        core_client.recv().await.unwrap();
        acp_server.recv().await.unwrap();
        core_client.recv().await.unwrap();
        acp_server
            .send(ask("zf-ask-x", "other", "foreign".into()))
            .unwrap();
        assert!(acp_server.recv().await.unwrap().error.is_some());
        acp_server
            .send(ask(
                "zf-ask-1",
                "s",
                format!("Approve?\n\n{}", "计划".repeat(20_000)),
            ))
            .unwrap();
        let request = core_client.recv().await.unwrap();
        assert_eq!(
            request.method.as_deref(),
            Some("item/tool/requestUserInput")
        );
        assert_eq!(request.id, Some(RpcId::String("zf-ask-1".into())));
        let params = request.params.unwrap();
        assert_eq!(params["threadId"], "s");
        assert!(params["turnId"]
            .as_str()
            .is_some_and(|turn| !turn.is_empty()));
        assert!(params.to_string().len() <= 32 * 1024);
        assert_eq!(params["questions"][0]["options"][0]["label"], "Approve");
        assert_eq!(params["questions"][0]["multiSelect"], false);
        assert_eq!(params["questions"][1]["multiSelect"], true);
        assert!(crate::interactions::RequestView::decode(
            RpcId::String("zf-ask-1".into()),
            "item/tool/requestUserInput",
            &params
        )
        .is_ok());
        core_client
            .send(Envelope::response(
                RpcId::String("zf-ask-1".into()),
                Some(json!({"answers":{"plan-review":{"answers":["Approve"]}}})),
            ))
            .unwrap();
        let answer = acp_server.recv().await.unwrap();
        assert_eq!(answer.id, Some(RpcId::String("zf-ask-1".into())));
        assert_eq!(
            answer.result.unwrap(),
            json!({"answers":{"plan-review":{"answers":["Approve"]}}})
        );
        let resolved = core_client.recv().await.unwrap();
        assert_eq!(resolved.method.as_deref(), Some("serverRequest/resolved"));
        assert_eq!(resolved.params.unwrap()["requestId"], "zf-ask-1");
        // dsh 撤回问题（例如轮次被中断）：Core 里的请求随之结束，迟到的答复不再转发。
        acp_server
            .send(ask("zf-ask-2", "s", "again".into()))
            .unwrap();
        core_client.recv().await.unwrap();
        acp_server
            .send(Envelope::notification(
                "zf/session/event",
                Some(json!({"sessionId":"s","event":{"type":"ask","id":"zf-ask-2","state":"withdrawn"}})),
            ))
            .unwrap();
        let resolved = core_client.recv().await.unwrap();
        assert_eq!(resolved.method.as_deref(), Some("serverRequest/resolved"));
        assert_eq!(resolved.params.unwrap()["requestId"], "zf-ask-2");
        drop(core_client);
        let _ = tokio::time::timeout(Duration::from_secs(2), bridge).await;
    }

    #[tokio::test]
    async fn backend_commands_are_listed_any_time_but_run_only_between_turns() {
        let (mut core_client, core_bridge) = pair();
        let (mut acp_server, acp_bridge) = pair();
        let config = Config::default();
        let bridge = tokio::spawn(AcpBridge::new(core_bridge, acp_bridge, &config).run());
        core_client
            .send(app_server::backend_commands_list(RpcId::Number(1)))
            .unwrap();
        assert_eq!(
            core_client.recv().await.unwrap().error.unwrap()["message"],
            "ACP session is not ready"
        );
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
            .send(app_server::backend_command_run(
                RpcId::Number(4),
                "/plan off",
            ))
            .unwrap();
        let forwarded = acp_server.recv().await.unwrap();
        assert_eq!(forwarded.method.as_deref(), Some("zf/commands/run"));
        assert_eq!(
            forwarded.params.unwrap(),
            json!({"sessionId":"s","line":"/plan off"})
        );
        acp_server
            .send(Envelope::response(
                RpcId::Number(4),
                Some(json!({"kind":"success","text":"Plan mode off."})),
            ))
            .unwrap();
        assert_eq!(
            core_client.recv().await.unwrap().result.unwrap()["text"],
            "Plan mode off."
        );
        core_client
            .send(app_server::turn_start(RpcId::Number(3), "s", "model", None))
            .unwrap();
        core_client.recv().await.unwrap();
        acp_server.recv().await.unwrap();
        core_client.recv().await.unwrap();
        // 轮次进行中：清单照常可取，执行由桥接直接拒绝。
        core_client
            .send(app_server::backend_command_run(
                RpcId::Number(5),
                "/compact",
            ))
            .unwrap();
        assert_eq!(
            core_client.recv().await.unwrap().error.unwrap()["message"],
            "a turn is running"
        );
        core_client
            .send(app_server::backend_commands_list(RpcId::Number(6)))
            .unwrap();
        let forwarded = acp_server.recv().await.unwrap();
        assert_eq!(forwarded.method.as_deref(), Some("zf/commands/list"));
        assert_eq!(forwarded.params.unwrap(), json!({"sessionId":"s"}));
        drop(core_client);
        let _ = tokio::time::timeout(Duration::from_secs(2), bridge).await;
    }

    #[tokio::test]
    async fn steering_is_forwarded_for_the_bound_turn_and_its_delivery_is_reported() {
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
        // 没有进行中的轮次：桥接直接拒绝，不打扰 dsh。
        core_client
            .send(app_server::turn_steer(
                RpcId::Number(9),
                "s",
                "none",
                "early",
            ))
            .unwrap();
        let refused = core_client.recv().await.unwrap();
        assert_eq!(refused.id, Some(RpcId::Number(9)));
        assert_eq!(refused.error.unwrap()["message"], "no turn is running");
        core_client
            .send(app_server::turn_start(RpcId::Number(3), "s", "model", None))
            .unwrap();
        let mut turn = None;
        for _ in 0..2 {
            let frame = core_client.recv().await.unwrap();
            for value in [frame.result.as_ref(), frame.params.as_ref()]
                .into_iter()
                .flatten()
            {
                if let Some(id) = value.pointer("/turn/id").and_then(|id| id.as_str()) {
                    turn = Some(id.to_owned());
                }
            }
        }
        let turn = turn.expect("the bridge names the turn");
        acp_server.recv().await.unwrap();
        core_client
            .send(app_server::turn_steer(
                RpcId::Number(10),
                "s",
                "stale-turn",
                "late",
            ))
            .unwrap();
        assert!(core_client.recv().await.unwrap().error.is_some());
        core_client
            .send(app_server::turn_steer(
                RpcId::Number(11),
                "s",
                &turn,
                "use 中文",
            ))
            .unwrap();
        let forwarded = acp_server.recv().await.unwrap();
        assert_eq!(forwarded.method.as_deref(), Some("zf/session/steer"));
        assert_eq!(
            forwarded.params.unwrap(),
            json!({"sessionId":"s","text":"use 中文"})
        );
        acp_server
            .send(Envelope::response(
                RpcId::Number(11),
                Some(json!({"messageId":"m1"})),
            ))
            .unwrap();
        let accepted = core_client.recv().await.unwrap();
        assert_eq!(accepted.result.unwrap()["messageId"], "m1");
        for (session, state) in [
            ("other", "delivered"),
            ("s", "invented"),
            ("s", "delivered"),
        ] {
            acp_server
                .send(Envelope::notification(
                    "zf/session/event",
                    Some(json!({"sessionId":session,"event":{"type":"steer","id":"m1","state":state}})),
                ))
                .unwrap();
        }
        let delivered = core_client.recv().await.unwrap();
        assert_eq!(delivered.method.as_deref(), Some("turn/steer/delivered"));
        assert_eq!(
            delivered.params.unwrap(),
            json!({"threadId":"s","turnId":turn,"id":"m1"})
        );
        drop(core_client);
        let _ = tokio::time::timeout(Duration::from_secs(2), bridge).await;
    }

    #[tokio::test]
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
        for (session, step, state) in [
            ("other", json!(1), "start"),
            ("s", json!(-1), "start"),
            ("s", json!(1), "wrong"),
            ("s", json!(1), "start"),
            ("s", json!(1), "end"),
        ] {
            acp_server
                .send(event(
                    session,
                    json!({"type":"step","step":step,"state":state}),
                ))
                .unwrap();
        }
        for state in ["start", "end"] {
            let relayed = core_client.recv().await.unwrap();
            assert_eq!(relayed.method.as_deref(), Some("turn/step"));
            let params = relayed.params.unwrap();
            assert_eq!(params["threadId"], "s");
            assert!(params["turnId"]
                .as_str()
                .is_some_and(|turn| !turn.is_empty()));
            assert_eq!(params["step"], 1);
            assert_eq!(params["state"], state);
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
