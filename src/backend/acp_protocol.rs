//! Typed edge handling for the Agent Client Protocol (ACP).
//!
//! ACP is JSON-RPC over newline-delimited stdio. The bridge keeps the raw
//! values at this edge and exposes bounded, protocol-neutral update facts to
//! Core. Unknown update kinds remain explicit instead of being guessed.

use serde_json::{json, Value};
use thiserror::Error;

use crate::config::McpServerConfig;
use crate::protocol::{Envelope, RpcId};
mod tools;
pub use tools::{FileDiff, ToolKind};

pub const INITIALIZE: &str = "initialize";
pub const SESSION_NEW: &str = "session/new";
pub const SESSION_PROMPT: &str = "session/prompt";
pub const SESSION_UPDATE: &str = "session/update";
pub const SESSION_REQUEST_PERMISSION: &str = "session/request_permission";
pub const SESSION_CANCEL: &str = "session/cancel";
pub const SESSION_CLOSE: &str = "session/close";
pub const SESSION_SET_CONFIG_OPTION: &str = "session/set_config_option";

fn acp(mut envelope: Envelope) -> Envelope {
    envelope.jsonrpc = Some("2.0".into());
    envelope
}

const MAX_TEXT_BYTES: usize = 32 * 1024;
const MAX_STRUCTURED_RESULT_BYTES: usize = 64 * 1024;
// MCP may duplicate the same JSON as structuredContent and escaped text content.
const MAX_RESULT_WRAPPER_BYTES: usize = 4 * MAX_STRUCTURED_RESULT_BYTES;

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum AcpProtocolError {
    #[error("ACP message is missing {0}")]
    Missing(&'static str),
    #[error("ACP message contains invalid {0}")]
    Invalid(&'static str),
    #[error("ACP text exceeds {MAX_TEXT_BYTES} bytes")]
    TextTooLarge,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Update {
    AgentMessage {
        item_id: String,
        text: String,
    },
    AgentThought {
        item_id: String,
        text: String,
    },
    ToolCall {
        item_id: String,
        name: String,
        kind: ToolKind,
        diffs: Vec<FileDiff>,
        /// 保留给之后只带 toolCallId 的权限请求查回命令、理由和申请的权限。
        input: ToolInput,
        mcp: Option<Box<McpCall>>,
    },
    ToolCallUpdate {
        item_id: String,
        status: ToolStatus,
        content: ToolContent,
    },
    Usage {
        input_tokens: Option<u64>,
        output_tokens: Option<u64>,
        total_tokens: Option<u64>,
        context_window: Option<u64>,
    },
    Unknown {
        kind: String,
    },
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ToolContent {
    pub output: Option<String>,
    /// Complete JSON is separate from bounded display text.
    pub structured_output: Option<Value>,
    pub diffs: Vec<FileDiff>,
    pub exit_code: Option<i64>,
    pub duration_ms: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpCall {
    pub server: String,
    pub tool: String,
    pub arguments: Option<Value>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolStatus {
    InProgress,
    Completed,
    Failed,
    Cancelled,
    Unknown,
}

pub fn initialize_request(id: RpcId) -> Envelope {
    acp(Envelope::request(
        id,
        INITIALIZE,
        Some(json!({
            "protocolVersion": 1,
            "clientCapabilities": {},
            "clientInfo": {"name":"native-agent-tui","version":env!("CARGO_PKG_VERSION")}
        })),
    ))
}

pub fn session_new_request(id: RpcId, cwd: &str, mcp_servers: Value) -> Envelope {
    acp(Envelope::request(
        id,
        SESSION_NEW,
        Some(json!({"cwd": cwd, "mcpServers": mcp_servers})),
    ))
}

/// Reattaches a new agent process to an existing session without replaying it.
pub fn session_resume_request(
    id: RpcId,
    session_id: &str,
    cwd: &str,
    mcp_servers: Value,
) -> Envelope {
    acp(Envelope::request(
        id,
        "session/resume",
        Some(json!({"sessionId": session_id, "cwd": cwd, "mcpServers": mcp_servers})),
    ))
}

pub fn encode_mcp_servers(servers: &[McpServerConfig]) -> Value {
    Value::Array(
        servers
            .iter()
            .map(|server| match server {
                McpServerConfig::Stdio {
                    name,
                    command,
                    args,
                    env,
                } => json!({
                    "name": name,
                    "command": command,
                    "args": args,
                    "env": env.iter().map(|(name, value)| json!({"name": name, "value": value})).collect::<Vec<_>>(),
                }),
                McpServerConfig::Http {
                    name,
                    url,
                    headers,
                } => json!({
                    "type": "http",
                    "name": name,
                    "url": url,
                    "headers": headers.iter().map(|(name, value)| json!({"name": name, "value": value})).collect::<Vec<_>>(),
                }),
            })
            .collect(),
    )
}

pub fn session_prompt_request(id: RpcId, session_id: &str, text: &str) -> Envelope {
    acp(Envelope::request(
        id,
        SESSION_PROMPT,
        Some(json!({
            "sessionId": session_id,
            "prompt": [{"type":"text", "text": text}]
        })),
    ))
}

pub fn session_cancel(session_id: &str) -> Envelope {
    acp(Envelope::notification(
        SESSION_CANCEL,
        Some(json!({"sessionId": session_id})),
    ))
}

pub fn session_close(id: RpcId, session_id: &str) -> Envelope {
    acp(Envelope::request(
        id,
        SESSION_CLOSE,
        Some(json!({"sessionId": session_id})),
    ))
}

pub fn session_set_config_option(
    id: RpcId,
    session_id: &str,
    config_id: &str,
    value: Value,
) -> Envelope {
    acp(Envelope::request(
        id,
        SESSION_SET_CONFIG_OPTION,
        Some(json!({"sessionId": session_id, "configId": config_id, "value": value})),
    ))
}

/// ACP `model` 配置项中的一个可选值；`value` 原样回传给 agent。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelChoice {
    pub value: String,
    pub provider: Option<String>,
    pub model: String,
    pub name: String,
}

impl ModelChoice {
    pub fn label(&self) -> String {
        match &self.provider {
            Some(provider) => format!("{provider}/{}", self.model),
            None => self.model.clone(),
        }
    }
}

fn model_option(result: &Value) -> Option<&Value> {
    result
        .get("configOptions")?
        .as_array()?
        .iter()
        .find(|option| option.get("id").and_then(Value::as_str) == Some("model"))
}

fn model_choice(value: &str, name: &str) -> ModelChoice {
    // dsh 把选项值编码为 JSON 数组 ["provider","model"]；其他 agent 可能直接给模型名。
    let parsed: Option<Vec<String>> = serde_json::from_str(value).ok();
    let (provider, model) = match parsed.as_deref() {
        Some([provider, model]) => (Some(provider.clone()), model.clone()),
        _ => (None, value.to_owned()),
    };
    ModelChoice {
        value: value.to_owned(),
        provider,
        model,
        name: name.to_owned(),
    }
}

pub fn model_choices(result: &Value) -> Vec<ModelChoice> {
    let mut choices = Vec::new();
    let Some(options) = model_option(result)
        .and_then(|option| option.get("options"))
        .and_then(Value::as_array)
    else {
        return choices;
    };
    for entry in options {
        let nested = entry.get("options").and_then(Value::as_array);
        for item in nested.map_or_else(|| std::slice::from_ref(entry), Vec::as_slice) {
            if let Some(value) = item.get("value").and_then(Value::as_str) {
                let name = item.get("name").and_then(Value::as_str).unwrap_or(value);
                choices.push(model_choice(value, name));
            }
        }
    }
    choices
}

/// 单层 select 配置项的当前值和可选值，例如 `reasoning_effort`。
pub fn select_option(result: &Value, id: &str) -> Option<(String, Vec<String>)> {
    let option = result
        .get("configOptions")?
        .as_array()?
        .iter()
        .find(|option| option.get("id").and_then(Value::as_str) == Some(id))?;
    let current = option
        .get("currentValue")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned();
    let values = option
        .get("options")
        .and_then(Value::as_array)
        .map(|options| {
            options
                .iter()
                .filter_map(|item| item.get("value").and_then(Value::as_str))
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();
    Some((current, values))
}

pub fn current_model(result: &Value) -> Option<String> {
    let value = model_option(result)?.get("currentValue")?.as_str()?;
    Some(model_choice(value, value).label())
}

/// `provider/model` 精确匹配；只给模型名时必须唯一。
pub fn match_model(choices: &[ModelChoice], requested: &str) -> Result<ModelChoice, String> {
    if let Some(found) = choices
        .iter()
        .find(|choice| choice.label() == requested || choice.value == requested)
    {
        return Ok(found.clone());
    }
    let by_name: Vec<_> = choices
        .iter()
        .filter(|choice| choice.model == requested || choice.name == requested)
        .collect();
    let labels = |list: &mut dyn Iterator<Item = &ModelChoice>| {
        list.map(ModelChoice::label).collect::<Vec<_>>().join(", ")
    };
    match by_name.as_slice() {
        [only] => Ok((*only).clone()),
        [] => Err(format!(
            "ACP agent does not offer model {requested}; available: {}",
            labels(&mut choices.iter())
        )),
        _ => Err(format!(
            "Model {requested} is offered by several providers; use provider/model: {}",
            labels(&mut by_name.iter().copied())
        )),
    }
}

pub fn session_id(value: &Value) -> Option<String> {
    value
        .get("sessionId")
        .or_else(|| value.pointer("/session/sessionId"))
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty() && value.len() <= 1024)
        .map(str::to_owned)
}

pub fn stop_reason(value: &Value) -> Option<&str> {
    value.get("stopReason").and_then(Value::as_str)
}

pub fn update(params: &Value) -> Result<(String, Update), AcpProtocolError> {
    let session = params
        .get("sessionId")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty() && value.len() <= 1024)
        .ok_or(AcpProtocolError::Missing("sessionId"))?
        .to_owned();
    let payload = params.get("update").unwrap_or(params);
    let kind = payload
        .get("sessionUpdate")
        .or_else(|| payload.get("type"))
        .and_then(Value::as_str)
        .ok_or(AcpProtocolError::Missing("update.sessionUpdate"))?;
    let item_id = || {
        payload
            .get("itemId")
            .or_else(|| payload.get("toolCallId"))
            .or_else(|| payload.get("messageId"))
            .or_else(|| payload.pointer("/toolCall/toolCallId"))
            .or_else(|| payload.pointer("/content/id"))
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty() && value.len() <= 1024)
            .map(str::to_owned)
            .ok_or(AcpProtocolError::Missing("itemId"))
    };
    let update = match kind {
        "agent_message_chunk" => Update::AgentMessage {
            item_id: item_id()?,
            text: content_text(payload)?.ok_or(AcpProtocolError::Missing("content.text"))?,
        },
        "agent_thought_chunk" => Update::AgentThought {
            item_id: item_id()?,
            text: content_text(payload)?.ok_or(AcpProtocolError::Missing("content.text"))?,
        },
        "tool_call" => Update::ToolCall {
            item_id: item_id()?,
            input: tool_input(payload),
            mcp: mcp_call(payload),
            name: tools::bounded(
                payload
                    .get("title")
                    .and_then(Value::as_str)
                    .unwrap_or("ACP tool call"),
                512,
            ),
            kind: tools::kind(payload, &tool_input(payload)),
            diffs: tools::diffs(payload, tools::kind(payload, &tool_input(payload))),
        },
        "tool_call_update" => Update::ToolCallUpdate {
            item_id: item_id()?,
            status: if payload
                .pointer("/rawOutput/isError")
                .and_then(Value::as_bool)
                == Some(true)
            {
                ToolStatus::Failed
            } else {
                tool_status(payload.get("status").and_then(Value::as_str))
            },
            content: ToolContent {
                diffs: tools::diffs(payload, ToolKind::Generic),
                exit_code: payload
                    .get("exitCode")
                    .or_else(|| payload.pointer("/rawOutput/exitCode"))
                    .and_then(Value::as_i64),
                duration_ms: payload
                    .get("durationMs")
                    .or_else(|| payload.pointer("/rawOutput/durationMs"))
                    .and_then(Value::as_u64),
                // 工具输出（如读取整个文件）常超过上限；截断保留开头，不让整轮失败。
                output: payload
                    .get("content")
                    .and_then(truncated_text)
                    .or_else(|| payload.get("rawOutput").and_then(truncated_text)),
                structured_output: (tool_status(payload.get("status").and_then(Value::as_str))
                    == ToolStatus::Completed)
                    .then(|| {
                        if payload["rawOutput"]["isError"] == true {
                            return Some(json!({"isError": true}));
                        }
                        match payload.get("rawOutput").filter(|value| !value.is_null()) {
                            Some(raw) => bounded_json_result(raw),
                            None => payload.get("content").and_then(bounded_content_result),
                        }
                    })
                    .flatten(),
            },
        },
        "usage_update" => Update::Usage {
            input_tokens: number(payload, &["inputTokens", "input_tokens", "input"]),
            output_tokens: number(payload, &["outputTokens", "output_tokens", "output"]),
            total_tokens: number(payload, &["totalTokens", "total_tokens", "total", "used"]),
            context_window: number(payload, &["contextWindow", "context_window", "size"]),
        },
        other => Update::Unknown {
            kind: other.to_owned(),
        },
    };
    Ok((session, update))
}

fn mcp_call(payload: &Value) -> Option<Box<McpCall>> {
    // Tool identity comes from ACP's registry title. A shell command cannot become
    // an MCP fact by printing its name or a matching JSON envelope.
    if payload.pointer("/rawInput/command").is_some() {
        return None;
    }
    let name = payload["title"].as_str()?.strip_prefix("mcp__")?;
    let (server, tool) = name.split_once("__")?;
    if server.is_empty()
        || tool.is_empty()
        || server.len() > 256
        || tool.len() > 256
        || !server
            .bytes()
            .chain(tool.bytes())
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    {
        return None;
    }
    Some(Box::new(McpCall {
        server: server.into(),
        tool: tool.into(),
        arguments: payload
            .get("rawInput")
            .filter(|v| v.to_string().len() <= MAX_TEXT_BYTES)
            .cloned(),
    }))
}

fn content_text(value: &Value) -> Result<Option<String>, AcpProtocolError> {
    let Some(content) = value.get("content") else {
        return Ok(None);
    };
    text_from_value(content)
}

fn text_from_value(value: &Value) -> Result<Option<String>, AcpProtocolError> {
    if let Some(text) = value.as_str() {
        if text.len() > MAX_TEXT_BYTES {
            return Err(AcpProtocolError::TextTooLarge);
        }
        return Ok(Some(text.to_owned()));
    }
    if let Some(text) = value.get("text").and_then(Value::as_str) {
        if text.len() > MAX_TEXT_BYTES {
            return Err(AcpProtocolError::TextTooLarge);
        }
        return Ok(Some(text.to_owned()));
    }
    if let Some(content) = value.get("content") {
        return text_from_value(content);
    }
    let Some(items) = value.as_array() else {
        return Ok(None);
    };
    let mut combined = String::new();
    for item in items {
        if let Some(text) = text_from_value(item)? {
            if !combined.is_empty() {
                combined.push('\n');
            }
            combined.push_str(&text);
            if combined.len() > MAX_TEXT_BYTES {
                return Err(AcpProtocolError::TextTooLarge);
            }
        }
    }
    Ok((!combined.is_empty()).then_some(combined))
}

/// ACP 工具调用 `rawInput` 中可展示给审批者的字段，均已截断。
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ToolInput {
    pub command: Option<String>,
    pub reason: Option<String>,
    pub cwd: Option<String>,
    /// 非命令类工具（read/edit/glob 等）操作的路径或模式。
    pub target: Option<String>,
    /// 工具申请的沙箱权限，例如 danger-full-access；审批时必须可见。
    pub sandbox: Option<String>,
}

pub fn tool_input(tool: &Value) -> ToolInput {
    let input = tool.get("rawInput");
    let field = |keys: &[&str]| {
        keys.iter().find_map(|key| {
            let value = input?.get(*key)?;
            let text = match value {
                Value::String(text) => text.clone(),
                Value::Array(parts) => parts
                    .iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join(" "),
                _ => return None,
            };
            (!text.trim().is_empty()).then(|| tools::bounded(&text, MAX_TEXT_BYTES))
        })
    };
    ToolInput {
        command: field(&["command", "cmd"]),
        reason: field(&["justification", "description", "reason"]),
        cwd: field(&["workdir", "cwd"]),
        target: field(&["file_path", "path", "pattern", "url", "query"]),
        sandbox: field(&["sandbox_permissions"]),
    }
}

/// Preserve one complete JSON receipt independently of the display text budget.
fn bounded_json_result(value: &Value) -> Option<Value> {
    let parsed = if let Some(text) = value.as_str() {
        if text.len() > MAX_RESULT_WRAPPER_BYTES {
            return None;
        }
        serde_json::from_str::<Value>(text).ok()?
    } else {
        if serde_json::to_vec(value).ok()?.len() > MAX_RESULT_WRAPPER_BYTES {
            return None;
        }
        value.clone()
    };
    if parsed["isError"] == true {
        return Some(json!({"isError": true}));
    }
    if parsed.is_array() || parsed["type"] == "text" || parsed["type"] == "content" {
        return bounded_content_result(&parsed);
    }
    let receipt = if let Some(inner) = parsed.get("structuredContent").filter(|v| !v.is_null()) {
        inner.clone()
    } else if let Some(content) = parsed.get("content") {
        return bounded_content_result(content);
    } else {
        parsed
    };
    (serde_json::to_vec(&receipt).ok()?.len() <= MAX_STRUCTURED_RESULT_BYTES).then_some(receipt)
}

fn bounded_content_result(value: &Value) -> Option<Value> {
    fn content(value: &Value, depth: usize) -> Option<Value> {
        if depth > 4 {
            return None;
        }
        if value.is_string() {
            return bounded_json_result(value);
        }
        if let Some(text) = value.get("text") {
            return bounded_json_result(text);
        }
        if let Some(inner) = value.get("content") {
            return content(inner, depth + 1);
        }
        let items = value.as_array()?;
        if items.len() == 1 {
            return content(&items[0], depth + 1);
        }
        if items.len() > 8 {
            return None;
        }
        // MCP image delivery may include display-only diagnostics beside the sealed
        // receipt. Preserve exactly one complete receipt; never pick between JSONs.
        let mut candidates = items.iter().filter_map(|item| content(item, depth + 1));
        let receipt = candidates.next()?;
        (receipt["contract"] == "modeling-fact-receipt/v1" && candidates.next().is_none())
            .then_some(receipt)
    }
    content(value, 0)
}

/// 收集内容块中的文本，超过上限时按字符边界截断并标注。
fn truncated_text(value: &Value) -> Option<String> {
    fn collect(value: &Value, out: &mut String) {
        if out.len() > MAX_TEXT_BYTES {
            return;
        }
        if let Some(text) = value.as_str() {
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str(text);
        } else if let Some(text) = value.get("text").and_then(Value::as_str) {
            collect(&Value::String(text.to_owned()), out);
        } else if let Some(content) = value.get("content") {
            collect(content, out);
        } else if let Some(items) = value.as_array() {
            for item in items {
                collect(item, out);
            }
        }
    }
    let mut text = String::new();
    collect(value, &mut text);
    if text.len() > MAX_TEXT_BYTES {
        let mut end = MAX_TEXT_BYTES;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
        text.push_str("\n[output truncated]");
    }
    (!text.is_empty()).then_some(text)
}

fn number(value: &Value, keys: &[&str]) -> Option<u64> {
    keys.iter()
        .find_map(|key| value.get(*key).and_then(Value::as_u64))
}

fn tool_status(value: Option<&str>) -> ToolStatus {
    match value {
        Some("pending" | "in_progress" | "running") => ToolStatus::InProgress,
        Some("completed" | "success" | "succeeded") => ToolStatus::Completed,
        Some("failed" | "error") => ToolStatus::Failed,
        Some("cancelled" | "canceled") => ToolStatus::Cancelled,
        _ => ToolStatus::Unknown,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::McpServerConfig;

    #[test]
    fn dsh_tool_calls_expose_the_raw_command_reason_and_workdir() {
        let tool = json!({"sessionUpdate":"tool_call","toolCallId":"t","title":"pwsh","kind":"other",
            "rawInput":{"command":"Set-Content -Path probe.txt -Value 'hi'","description":"Write probe.txt",
                "justification":"需要写入文件","workdir":"D:\\ws"}});
        let input = tool_input(&tool);
        assert_eq!(
            input.command.as_deref(),
            Some("Set-Content -Path probe.txt -Value 'hi'")
        );
        assert_eq!(input.reason.as_deref(), Some("需要写入文件"));
        assert_eq!(input.cwd.as_deref(), Some("D:\\ws"));
        let Ok((_, Update::ToolCall { input, .. })) =
            update(&json!({"sessionId":"s","update":tool}))
        else {
            panic!()
        };
        assert_eq!(
            input.command.as_deref(),
            Some("Set-Content -Path probe.txt -Value 'hi'")
        );
        assert_eq!(tool_input(&json!({"title":"pwsh"})), ToolInput::default());
        let Ok((_, Update::ToolCall { name, input, .. })) =
            update(&json!({"sessionId":"s","update":{
            "sessionUpdate":"tool_call","toolCallId":"r","title":"read",
            "rawInput":{"file_path":"calc.py"}}}))
        else {
            panic!()
        };
        assert_eq!(name, "read");
        assert_eq!(input.target.as_deref(), Some("calc.py"));
    }

    #[test]
    fn model_selection_reads_dsh_grouped_options_and_requires_unique_names() {
        let result = json!({"sessionId":"s","configOptions":[
            {"id":"reasoning_effort","options":[{"value":"high"}]},
            {"id":"model","currentValue":"[\"deepseek-official\",\"deepseek-v4-flash\"]","options":[
                {"group":"deepseek-official","options":[
                    {"value":"[\"deepseek-official\",\"deepseek-v4-flash\"]","name":"deepseek-v4-flash"}]},
                {"group":"openai","options":[
                    {"value":"[\"openai\",\"gpt-6-astra\"]","name":"GPT-6 Astra"}]},
                {"group":"hi","options":[
                    {"value":"[\"hi\",\"gpt-6-astra\"]","name":"GPT-6 Astra"},
                    {"value":"[\"hi\",\"gpt-6-luna\"]","name":"GPT-6 Luna"}]}]}]});
        let choices = model_choices(&result);
        assert_eq!(choices.len(), 4);
        assert_eq!(
            current_model(&result).as_deref(),
            Some("deepseek-official/deepseek-v4-flash")
        );
        let exact = match_model(&choices, "hi/gpt-6-astra").unwrap();
        assert_eq!(exact.value, "[\"hi\",\"gpt-6-astra\"]");
        assert_eq!(
            match_model(&choices, "gpt-6-luna").unwrap().label(),
            "hi/gpt-6-luna"
        );
        let ambiguous = match_model(&choices, "gpt-6-astra").unwrap_err();
        assert!(
            ambiguous.contains("openai/gpt-6-astra, hi/gpt-6-astra"),
            "{ambiguous}"
        );
        let missing = match_model(&choices, "nope").unwrap_err();
        assert!(
            missing.contains("available: deepseek-official/deepseek-v4-flash"),
            "{missing}"
        );
    }

    #[test]
    fn builds_acp_lifecycle_requests_with_bounded_content() {
        let prompt = session_prompt_request(RpcId::Number(1), "s", "hello");
        assert_eq!(prompt.method.as_deref(), Some(SESSION_PROMPT));
        assert_eq!(prompt.jsonrpc.as_deref(), Some("2.0"));
        assert_eq!(prompt.params.as_ref().unwrap()["sessionId"], "s");
        assert_eq!(
            prompt.params.as_ref().unwrap()["prompt"][0]["text"],
            "hello"
        );
    }

    #[test]
    fn decodes_updates_without_exposing_raw_json_to_callers() {
        let (session, Update::AgentMessage { item_id, text }) = update(&json!({
            "sessionId":"s", "update":{"sessionUpdate":"agent_message_chunk","messageId":"i","content":{"type":"text","text":"hi"}}
        })).unwrap() else { panic!() };
        assert_eq!(
            (session, item_id, text),
            ("s".into(), "i".into(), "hi".into())
        );
        assert!(matches!(
            update(
                &json!({"sessionId":"s","update":{"sessionUpdate":"agent_message_chunk","messageId":"i","content":{"type":"text","text":"x".repeat(MAX_TEXT_BYTES + 1)}}})
            ),
            Err(AcpProtocolError::TextTooLarge)
        ));
        let large = "中".repeat(MAX_TEXT_BYTES);
        let Ok((
            _,
            Update::ToolCallUpdate {
                content: ToolContent { output, .. },
                ..
            },
        )) = update(&json!({"sessionId":"s",
            "update":{"sessionUpdate":"tool_call_update","toolCallId":"t","status":"completed",
                "content":[{"type":"content","content":{"type":"text","text":large}}]}}))
        else {
            panic!("large tool output must not fail the update")
        };
        let output = output.unwrap();
        assert!(output.ends_with("[output truncated]"));
        assert!(output.len() <= MAX_TEXT_BYTES + "\n[output truncated]".len());
    }

    #[test]
    fn encodes_typed_stdio_and_http_mcp_servers() {
        let value = encode_mcp_servers(&[
            McpServerConfig::Stdio {
                name: "local".into(),
                command: "mcp-server".into(),
                args: vec!["--stdio".into()],
                env: vec![("MODE".into(), "test".into())],
            },
            McpServerConfig::Http {
                name: "remote".into(),
                url: "https://mcp.invalid".into(),
                headers: vec![("X-Test".into(), "value".into())],
            },
        ]);
        assert_eq!(value[0]["command"], "mcp-server");
        assert_eq!(value[0]["env"][0]["name"], "MODE");
        assert_eq!(value[1]["type"], "http");
        assert_eq!(value[1]["headers"][0]["name"], "X-Test");
    }

    #[test]
    fn decodes_tool_content_arrays_without_retaining_raw_blocks() {
        let (
            _,
            Update::ToolCallUpdate {
                content: ToolContent { output, .. },
                ..
            },
        ) = update(&json!({
            "sessionId":"s",
            "update": {
                "sessionUpdate":"tool_call_update",
                "toolCallId":"tool",
                "status":"completed",
                "content":[{"type":"content","content":{"type":"text","text":"result"}}]
            }
        }))
        .unwrap()
        else {
            panic!()
        };
        assert_eq!(output.as_deref(), Some("result"));
    }
    #[test]
    fn mcp_identity_comes_from_registry_title_and_not_a_shell_command() {
        let call = json!({"title":"mcp__modeling_domain__prepare_change", "rawInput":{"plan":{}}});
        let metadata = mcp_call(&call).unwrap();
        assert_eq!(metadata.server, "modeling_domain");
        assert_eq!(metadata.tool, "prepare_change");
        assert_eq!(metadata.arguments, Some(json!({"plan":{}})));
        let shell = json!({"title":"mcp__modeling_domain__prepare_change", "rawInput":{"command":"echo fake"}});
        assert!(mcp_call(&shell).is_none());
        assert!(mcp_call(
            &json!({"title":"pwsh", "rawInput":{"command":"mcp__modeling_domain__prepare_change"}})
        )
        .is_none());
    }
}

#[cfg(test)]
mod structured_result_tests {
    use super::*;

    #[test]
    fn mixed_review_image_blocks_preserve_small_fact_receipt_without_binary_output() {
        let receipt = json!({"contract":"modeling-fact-receipt/v1", "kind":"artifact_validation",
            "fact_sha256":"a".repeat(64), "fact_bytes":1200, "fact_sequence":10,
            "run_id":"run", "context_sha256":"ctx", "ok":true,
            "binding":{"build_id":"review-build"}});
        let payload = json!({"sessionId":"s","update":{
        "sessionUpdate":"tool_call_update","toolCallId":"t","status":"completed",
        "content":[
            {"type":"content","content":{"type":"text","text":receipt.to_string()}},
            {"type":"content","content":{"type":"image","mimeType":"image/png",
                "data":"BINARY_IMAGE_DATA".repeat(100_000)}}
        ]}});
        let (
            _,
            Update::ToolCallUpdate {
                content:
                    ToolContent {
                        output,
                        structured_output,
                        ..
                    },
                ..
            },
        ) = update(&payload).unwrap()
        else {
            panic!()
        };
        assert_eq!(
            structured_output,
            Some(receipt.clone()),
            "Core must receive the sealed receipt independently of image/display blocks"
        );
        let text = output.unwrap();
        assert_eq!(serde_json::from_str::<Value>(&text).unwrap(), receipt);
        assert!(text.len() < 6000);
        assert!(!text.contains("BINARY_IMAGE_DATA"));
    }

    #[test]
    fn image_receipt_survives_canonical_and_diagnostic_blocks_but_ambiguous_json_is_rejected() {
        let receipt = json!({"contract":"modeling-fact-receipt/v1","kind":"artifact_validation"});
        let block = json!({"type":"text","text":receipt.to_string()});
        let image = json!({"type":"image","mimeType":"image/png","data":"not retained"});
        let diagnostic =
            json!({"type":"text","text":"[image unavailable: model does not declare image input]"});
        for sibling in [image, diagnostic] {
            let mixed = json!({"content":[block.clone(),sibling]});
            assert_eq!(bounded_json_result(&mixed), Some(receipt.clone()));
        }
        for sibling in [block.clone(), json!({"type":"text","text":"{\"ok\":true}"})] {
            assert!(bounded_content_result(&json!([block.clone(), sibling])).is_none());
        }
        assert!(
            bounded_json_result(&json!({"isError":true,"content":[block.clone()]})).unwrap()
                ["isError"]
                == true
        );
    }

    #[test]
    fn complete_mcp_json_survives_display_truncation_from_raw_or_content() {
        let receipt = json!({"contract":"modeling-domain/v2", "reason":"中".repeat(14_000)});
        for field in ["rawOutput", "content"] {
            let mut payload = json!({"sessionId":"s","update":{
                "sessionUpdate":"tool_call_update","toolCallId":"t","status":"completed"}});
            payload["update"][field] = if field == "rawOutput" {
                json!(receipt.to_string())
            } else {
                json!([{"type":"content","content":{"type":"text","text":receipt.to_string()}}])
            };
            let (
                _,
                Update::ToolCallUpdate {
                    content:
                        ToolContent {
                            output,
                            structured_output,
                            ..
                        },
                    ..
                },
            ) = update(&payload).unwrap()
            else {
                panic!()
            };
            assert!(output.unwrap().ends_with("[output truncated]"));
            assert_eq!(structured_output, Some(receipt.clone()));
        }
    }

    #[test]
    fn duplicated_standard_mcp_wrapper_preserves_one_complete_bounded_receipt() {
        let receipt = json!({"contract":"modeling-domain/v2", "reason":"中".repeat(14_000)});
        let wrapper = json!({"isError":false,"structuredContent":receipt,
            "content":[{"type":"text","text":receipt.to_string()}]});
        assert!(wrapper.to_string().len() > MAX_STRUCTURED_RESULT_BYTES);
        let block = json!({"type":"text","text":receipt.to_string()});
        let blocks = json!([block.clone()]);
        for raw in [
            wrapper.clone(),
            json!(wrapper.to_string()),
            blocks.clone(),
            json!(blocks.to_string()),
            block,
        ] {
            let (
                _,
                Update::ToolCallUpdate {
                    content:
                        ToolContent {
                            structured_output, ..
                        },
                    ..
                },
            ) = update(&json!({
                "sessionId":"s","update":{"sessionUpdate":"tool_call_update",
                "toolCallId":"t","status":"completed","rawOutput":raw}
            }))
            .unwrap()
            else {
                panic!()
            };
            assert_eq!(structured_output, Some(receipt.clone()));
        }
        let too_large = json!({"isError":false,
            "structuredContent":{"reason":"x".repeat(MAX_STRUCTURED_RESULT_BYTES)},
            "content":[{"type":"text","text":"{\"ok\":true}"}]});
        assert!(bounded_json_result(&too_large).is_none());
        assert!(bounded_json_result(&json!("x".repeat(MAX_RESULT_WRAPPER_BYTES + 1))).is_none());
    }

    #[test]
    fn explicit_mcp_error_cannot_fall_back_to_success_looking_content() {
        let (
            _,
            Update::ToolCallUpdate {
                content: ToolContent {
                    structured_output, ..
                },
                ..
            },
        ) = update(&json!({
            "sessionId":"s","update":{"sessionUpdate":"tool_call_update",
            "toolCallId":"t","status":"completed",
            "rawOutput":{"isError":true,"message":"x".repeat(MAX_STRUCTURED_RESULT_BYTES)},
            "content":[{"type":"text","text":"{\"ok\":true}"}]}
        }))
        .unwrap()
        else {
            panic!()
        };
        assert_eq!(structured_output, Some(json!({"isError": true})));
    }

    #[test]
    fn oversized_primary_receipt_cannot_be_replaced_by_short_content() {
        let (
            _,
            Update::ToolCallUpdate {
                content: ToolContent {
                    structured_output, ..
                },
                ..
            },
        ) = update(&json!({
            "sessionId":"s","update":{"sessionUpdate":"tool_call_update",
            "toolCallId":"t","status":"completed",
            "rawOutput":json!({"reason":"x".repeat(MAX_STRUCTURED_RESULT_BYTES)}).to_string(),
            "content":[{"type":"text","text":"{\"ok\":true}"}]}
        }))
        .unwrap()
        else {
            panic!()
        };
        assert!(structured_output.is_none());
    }

    #[test]
    fn oversized_malformed_and_failed_receipts_do_not_acquire_complete_json() {
        for (status, receipt) in [
            (
                "completed",
                json!({"reason":"x".repeat(MAX_STRUCTURED_RESULT_BYTES)}).to_string(),
            ),
            ("completed", "{broken".to_owned()),
            ("failed", "{\"ok\":true}".to_owned()),
        ] {
            let (_, Update::ToolCallUpdate { content: ToolContent { structured_output, .. }, .. }) = update(&json!({"sessionId":"s","update":{
                "sessionUpdate":"tool_call_update","toolCallId":"t","status":status,"rawOutput":receipt}})).unwrap() else { panic!() };
            assert!(structured_output.is_none());
        }
    }
}
