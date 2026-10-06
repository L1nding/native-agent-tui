//! Typed edge handling for the Agent Client Protocol (ACP).
//!
//! ACP is JSON-RPC over newline-delimited stdio. The bridge keeps the raw
//! values at this edge and exposes bounded, protocol-neutral update facts to
//! Core. Unknown update kinds remain explicit instead of being guessed.

use serde_json::{json, Value};
use thiserror::Error;

use crate::protocol::{Envelope, RpcId};

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
        title: String,
        kind: Option<String>,
    },
    ToolCallUpdate {
        item_id: String,
        status: ToolStatus,
        output: Option<String>,
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
            title: payload
                .get("title")
                .and_then(Value::as_str)
                .unwrap_or("ACP tool call")
                .chars()
                .take(MAX_TEXT_BYTES)
                .collect(),
            kind: payload
                .get("kind")
                .and_then(Value::as_str)
                .map(str::to_owned),
        },
        "tool_call_update" => Update::ToolCallUpdate {
            item_id: item_id()?,
            status: tool_status(payload.get("status").and_then(Value::as_str)),
            output: content_text(payload)?
                .or_else(|| {
                    payload
                        .get("rawOutput")
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                })
                .map(|text| text.chars().take(MAX_TEXT_BYTES).collect()),
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

fn content_text(value: &Value) -> Result<Option<String>, AcpProtocolError> {
    let Some(content) = value.get("content") else {
        return Ok(None);
    };
    let Some(text) = content
        .as_str()
        .or_else(|| content.get("text").and_then(Value::as_str))
    else {
        return Ok(None);
    };
    if text.len() > MAX_TEXT_BYTES {
        return Err(AcpProtocolError::TextTooLarge);
    }
    Ok(Some(text.to_owned()))
}

fn number(value: &Value, keys: &[&str]) -> Option<u64> {
    keys.iter()
        .find_map(|key| value.get(*key).and_then(Value::as_u64))
}

fn tool_status(value: Option<&str>) -> ToolStatus {
    match value {
        Some("in_progress" | "running") => ToolStatus::InProgress,
        Some("completed" | "success" | "succeeded") => ToolStatus::Completed,
        Some("failed" | "error") => ToolStatus::Failed,
        Some("cancelled" | "canceled") => ToolStatus::Cancelled,
        _ => ToolStatus::Unknown,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
    }
}
