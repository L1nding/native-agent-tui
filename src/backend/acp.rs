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

pub(crate) fn spawn(config: &Config) -> Result<AcpProcess, AcpError> {
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
        let mut buffer = [0; 8192];
        while matches!(stderr.read(&mut buffer).await, Ok(n) if n > 0) {}
    });

    // The Core sees the same bounded JSONL transport it uses for Codex. A
    // duplex pair keeps ACP translation out of Core and preserves writer
    // ownership, queue limits, and EOF semantics at both boundaries.
    let process_pipe = PipeTransport::new(stdout, stdin);
    let (core_io, bridge_io) = tokio::io::duplex(64 * 1024);
    let (core_read, core_write) = tokio::io::split(core_io);
    let (bridge_read, bridge_write) = tokio::io::split(bridge_io);
    let core_pipe = PipeTransport::new(core_read, core_write);
    let bridge_pipe = PipeTransport::new(bridge_read, bridge_write);
    let bridge = tokio::spawn(AcpBridge::new(bridge_pipe, process_pipe, config).run());
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
    },
    /// 切换模型后 agent 可能把推理强度重置为空；恢复切换前的值。
    RestoreReasoning {
        core_id: RpcId,
        session: String,
        label: String,
    },
    Prompt,
    Passthrough,
}

struct Permission {
    options: Vec<String>,
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
        }
    }

    async fn run(mut self) {
        loop {
            tokio::select! {
                message = self.core.recv() => {
                    match message {
                        Ok(message) => {
                            if self.handle_core(message).await.is_err() { break; }
                        }
                        Err(_) => {
                            if let Some(session) = &self.session_id {
                                let _ = self.acp.send(acp_protocol::session_close(RpcId::String("shutdown".into()), session));
                            }
                            let _ = self.acp.close_writer().await;
                            break;
                        }
                    }
                }
                message = self.acp.recv() => {
                    match message {
                        Ok(message) => {
                            if self.handle_acp(message).await.is_err() { break; }
                        }
                        Err(_) => break,
                    }
                }
            }
        }
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
            (Some("skills/list"), Some(id)) => {
                self.core.send(Envelope::error_response(
                    id,
                    -32601,
                    "ACP skills/list is unavailable",
                ))?;
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
                        id: Some(id),
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
                    let outcome = if decision == "cancel" {
                        json!({"outcome":"cancelled"})
                    } else {
                        let option = choose_permission_option(decision, &permission.options);
                        json!({"outcome":"selected","optionId":option})
                    };
                    Envelope::response(id, Some(json!({"outcome": outcome})))
                };
                self.acp.send(with_jsonrpc(response))?;
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
            (None, Some(id)) => self.response(id, message).await?,
            _ => {}
        }
        Ok(())
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
            Pending::SelectModel { core_id, .. } | Pending::RestoreReasoning { core_id, .. },
        ) = (&message.error, &kind)
        {
            let text = error
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("ACP model selection failed");
            self.core.send(Envelope::error_response(
                core_id.clone(),
                -32602,
                format!("ACP model selection failed: {text}"),
            ))?;
            return Ok(());
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
            Pending::Initialize => {
                self.core.send(Envelope::response(id, Some(json!({
                    "userAgent":"deepseek-acp", "codexHome":"unavailable", "platformFamily":"unavailable", "platformOs":"unavailable"
                }))))?;
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
                let Some(requested) = self.model.clone() else {
                    let current = acp_protocol::current_model(&result);
                    self.core.send(Envelope::response(
                        id,
                        Some(json!({"thread":{"id":session},"model":current})),
                    ))?;
                    return Ok(());
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
                                reasoning: acp_protocol::select_option(&result, "reasoning_effort")
                                    .map(|(current, _)| current)
                                    .filter(|current| !current.is_empty()),
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
            Pending::SelectModel {
                core_id,
                session,
                label,
                reasoning,
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
                        },
                    );
                    self.acp.send(acp_protocol::session_set_config_option(
                        request_id,
                        &session,
                        "reasoning_effort",
                        Value::String(previous),
                    ))?;
                } else {
                    self.core.send(Envelope::response(
                        core_id,
                        Some(json!({"thread":{"id":session},"model":label})),
                    ))?;
                }
            }
            Pending::RestoreReasoning {
                core_id,
                session,
                label,
            } => {
                self.core.send(Envelope::response(
                    core_id,
                    Some(json!({"thread":{"id":session},"model":label})),
                ))?;
            }
            Pending::Prompt => {
                let status = match acp_protocol::stop_reason(&result) {
                    Some("end_turn" | "completed") => "completed",
                    Some("cancelled" | "canceled") => "interrupted",
                    Some(_) | None => "failed",
                };
                self.finish_prompt(id, status, None)?;
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
            Update::ToolCall { item_id, title, kind } => self.core.send(Envelope::notification("item/started", Some(json!({"threadId":session,"turnId":turn,"item":{"id":item_id,"type":"commandExecution","command":title,"kind":kind}}))))?,
            Update::ToolCallUpdate { item_id, status, output } => {
                match status {
                    ToolStatus::Completed | ToolStatus::Failed | ToolStatus::Cancelled => {
                        let status = match status {
                            ToolStatus::Completed => "completed",
                            ToolStatus::Failed => "failed",
                            ToolStatus::Cancelled => "cancelled",
                            _ => unreachable!(),
                        };
                        self.core.send(Envelope::notification("item/completed", Some(json!({"threadId":session,"turnId":turn,"item":{"id":item_id,"type":"commandExecution","status":status,"aggregatedOutput":output}}))))?;
                    }
                    ToolStatus::InProgress | ToolStatus::Unknown => {
                        self.core.send(Envelope::notification("item/started", Some(json!({"threadId":session,"turnId":turn,"item":{"id":item_id,"type":"commandExecution","status":"inProgress","aggregatedOutput":output}}))))?;
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
                        item.get("optionId")
                            .or_else(|| item.get("id"))
                            .and_then(Value::as_str)
                            .map(str::to_owned)
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
        // 审批者必须看到要执行的内容：优先 rawInput 的命令、理由和目录。
        let input = acp_protocol::tool_input(&tool);
        let mut request = json!({
            "threadId": session,
            "turnId": turn,
            "command": input.command.as_deref().unwrap_or(title),
            "reason": input.reason.as_deref().unwrap_or("DeepSeek ACP permission request"),
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

fn choose_permission_option(decision: &str, options: &[String]) -> String {
    let needle = match decision {
        "accept" => ["allow", "accept", "yes"],
        "cancel" => ["cancel", "abort", "stop"],
        _ => ["deny", "decline", "no"],
    };
    options
        .iter()
        .find(|option| {
            needle
                .iter()
                .any(|part| option.to_ascii_lowercase().contains(part))
        })
        .cloned()
        .or_else(|| options.first().cloned())
        .unwrap_or_else(|| decision.to_owned())
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
    fn permission_decisions_prefer_matching_acp_option_ids() {
        assert_eq!(
            choose_permission_option("accept", &["allow_once".into(), "deny".into()]),
            "allow_once"
        );
        assert_eq!(
            choose_permission_option("decline", &["allow_once".into(), "deny".into()]),
            "deny"
        );
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
}
