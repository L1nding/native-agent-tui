use crate::agents::AgentInfo;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

pub const MAX_LINE_BYTES: usize = 16 * 1024 * 1024;
pub const WAIT_TOOL: &str = "wait_for_subagent_completion";

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WaitCall {
    pub thread_id: String,
    pub turn_id: String,
    pub call_id: String,
    tool: String,
    namespace: Option<String>,
    pub arguments: WaitArguments,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WaitArguments {
    pub targets: Vec<String>,
}

pub fn decode_wait_call(params: Value) -> Result<WaitCall, ProtocolError> {
    if params.to_string().len() > 32 * 1024 {
        return Err(ProtocolError::InvalidEnvelope(
            "wait parameters exceed 32 KiB",
        ));
    }
    let call: WaitCall = serde_json::from_value(params)?;
    if call.tool != WAIT_TOOL || call.namespace.is_some() {
        return Err(ProtocolError::InvalidEnvelope("unsupported dynamic tool"));
    }
    if [&call.thread_id, &call.turn_id, &call.call_id]
        .iter()
        .any(|id| id.is_empty() || id.len() > 1024)
        || call.arguments.targets.len() > 64
        || call
            .arguments
            .targets
            .iter()
            .any(|id| id.is_empty() || id.len() > 1024)
    {
        return Err(ProtocolError::InvalidEnvelope(
            "invalid wait identity or target count",
        ));
    }
    Ok(call)
}

pub fn decode_agent_info(params: &Value) -> Result<Option<AgentInfo>, ProtocolError> {
    let thread = &params["thread"];
    let source = thread.pointer("/source/subAgent/thread_spawn");
    if thread.pointer("/source/subAgent").is_some() && source.is_none() {
        return Ok(None);
    }
    let direct_parent = thread["parentThreadId"].as_str();
    let source_parent = source.and_then(|source| source["parent_thread_id"].as_str());
    if direct_parent
        .zip(source_parent)
        .is_some_and(|(a, b)| a != b)
    {
        return Err(ProtocolError::InvalidEnvelope(
            "conflicting agent parent identity",
        ));
    }
    let Some(parent) = direct_parent.or(source_parent) else {
        return Ok(None);
    };
    let id = thread["id"]
        .as_str()
        .ok_or(ProtocolError::InvalidEnvelope("agent thread.id is missing"))?;
    Ok(Some(AgentInfo {
        id: id.into(),
        parent_id: parent.into(),
        path: source
            .and_then(|source| source["agent_path"].as_str())
            .map(str::to_owned),
        nickname: thread["agentNickname"]
            .as_str()
            .or_else(|| source.and_then(|source| source["agent_nickname"].as_str()))
            .map(str::to_owned),
        role: thread["agentRole"].as_str().map(str::to_owned),
        model: thread["model"].as_str().map(str::to_owned),
        confirmed: true,
    }))
}

pub fn dynamic_tool_result(success: bool, data: Value) -> Value {
    serde_json::json!({"success":success,"contentItems":[{"type":"inputText","text":data.to_string()}]})
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(untagged)]
pub enum RpcId {
    Number(i64),
    String(String),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Envelope {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<RpcId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub method: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
    #[serde(
        default,
        deserialize_with = "present_value",
        skip_serializing_if = "Option::is_none"
    )]
    pub result: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<Value>,
}

fn present_value<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<Value>, D::Error> {
    Value::deserialize(deserializer).map(Some)
}

impl Envelope {
    pub fn request(id: RpcId, method: impl Into<String>, params: Option<Value>) -> Self {
        Self {
            id: Some(id),
            method: Some(method.into()),
            params,
            result: None,
            error: None,
        }
    }

    pub fn notification(method: impl Into<String>, params: Option<Value>) -> Self {
        Self {
            id: None,
            method: Some(method.into()),
            params,
            result: None,
            error: None,
        }
    }

    pub fn response(id: RpcId, result: Option<Value>) -> Self {
        Self {
            id: Some(id),
            method: None,
            params: None,
            result: Some(result.unwrap_or(Value::Null)),
            error: None,
        }
    }

    pub fn error_response(id: RpcId, code: i64, message: impl Into<String>) -> Self {
        Self {
            id: Some(id),
            method: None,
            params: None,
            result: None,
            error: Some(serde_json::json!({
                "code": code,
                "message": message.into(),
            })),
        }
    }
}

#[derive(Debug, Error)]
pub enum ProtocolError {
    #[error("JSONL frame exceeds {MAX_LINE_BYTES} bytes")]
    LineTooLarge,
    #[error("invalid JSON: {0}")]
    InvalidJson(#[from] serde_json::Error),
    #[error("JSON-RPC envelope must be an object")]
    NotAnObject,
    #[error("JSON-RPC envelope has neither an id nor a method")]
    MissingIdentity,
    #[error("invalid JSON-RPC envelope: {0}")]
    InvalidEnvelope(&'static str),
}

fn validate(value: &Value) -> Result<(), ProtocolError> {
    let object = value.as_object().ok_or(ProtocolError::NotAnObject)?;
    if object
        .get("jsonrpc")
        .is_some_and(|version| version != "2.0")
    {
        return Err(ProtocolError::InvalidEnvelope(
            "unsupported jsonrpc version",
        ));
    }
    let has_id = object.contains_key("id");
    let has_method = object.contains_key("method");
    if !has_id && !has_method {
        return Err(ProtocolError::MissingIdentity);
    }
    if has_id && serde_json::from_value::<RpcId>(object["id"].clone()).is_err() {
        return Err(ProtocolError::InvalidEnvelope(
            "id must be a string or integer",
        ));
    }
    if object
        .get("id")
        .and_then(Value::as_str)
        .is_some_and(|id| id.len() > 1024)
    {
        return Err(ProtocolError::InvalidEnvelope("RPC id exceeds 1024 bytes"));
    }
    if has_method {
        if object["method"]
            .as_str()
            .is_none_or(|method| method.trim().is_empty())
        {
            return Err(ProtocolError::InvalidEnvelope(
                "method must be a nonempty string",
            ));
        }
        if object.contains_key("result") || object.contains_key("error") {
            return Err(ProtocolError::InvalidEnvelope(
                "request contains response fields",
            ));
        }
        if object
            .get("params")
            .is_some_and(|params| !params.is_null() && !params.is_object() && !params.is_array())
        {
            return Err(ProtocolError::InvalidEnvelope(
                "params must be an object or array",
            ));
        }
    } else {
        if object.contains_key("params")
            || object.contains_key("result") == object.contains_key("error")
        {
            return Err(ProtocolError::InvalidEnvelope(
                "response needs exactly one result or error",
            ));
        }
        if let Some(error) = object.get("error") {
            if error.get("code").and_then(Value::as_i64).is_none()
                || error.get("message").and_then(Value::as_str).is_none()
            {
                return Err(ProtocolError::InvalidEnvelope(
                    "error needs an integer code and string message",
                ));
            }
        }
    }
    Ok(())
}

pub fn decode_line(line: &str) -> Result<Envelope, ProtocolError> {
    if line.len() > MAX_LINE_BYTES {
        return Err(ProtocolError::LineTooLarge);
    }

    let value: Value = serde_json::from_str(line)?;
    validate(&value)?;
    Ok(serde_json::from_value(value)?)
}

pub fn encode_line(envelope: &Envelope) -> Result<String, ProtocolError> {
    let value = serde_json::to_value(envelope)?;
    validate(&value)?;
    let mut line = serde_json::to_string(&value)?;
    if line.len() > MAX_LINE_BYTES {
        return Err(ProtocolError::LineTooLarge);
    }
    line.push('\n');
    Ok(line)
}

#[cfg(test)]
mod tests {
    use super::{decode_line, encode_line, Envelope, ProtocolError, RpcId};

    #[test]
    fn decodes_numeric_and_string_ids() {
        assert_eq!(
            decode_line(r#"{"id":7,"result":{}}"#).unwrap().id,
            Some(RpcId::Number(7))
        );
        assert_eq!(
            decode_line(r#"{"id":"seven","result":{}}"#).unwrap().id,
            Some(RpcId::String("seven".into()))
        );
    }

    #[test]
    fn rejects_frames_without_identity() {
        assert!(matches!(
            decode_line(r#"{"params":{}}"#),
            Err(ProtocolError::MissingIdentity)
        ));
    }

    #[test]
    fn encodes_a_notification_as_one_jsonl_frame() {
        let line = encode_line(&Envelope::notification("initialized", None)).unwrap();
        assert_eq!(line, r#"{"method":"initialized"}"#.to_owned() + "\n");
    }

    #[test]
    fn null_results_round_trip_and_unknown_fields_are_allowed() {
        let response =
            decode_line(r#"{"jsonrpc":"2.0","id":1,"result":null,"future":true}"#).unwrap();
        assert_eq!(response.result, Some(serde_json::Value::Null));
        assert_eq!(
            decode_line(&encode_line(&response).unwrap()).unwrap(),
            response
        );
        assert_eq!(
            Envelope::response(RpcId::Number(1), None).result,
            response.result
        );
    }

    #[test]
    fn rejects_ambiguous_or_malformed_envelopes() {
        for line in [
            r#"{"id":1}"#,
            r#"{"id":null,"result":null}"#,
            r#"{"id":true,"result":null}"#,
            r#"{"method":""}"#,
            r#"{"method":null}"#,
            r#"{"method":"x","params":false}"#,
            r#"{"id":1,"method":"x","result":null}"#,
            r#"{"id":1,"result":null,"error":null}"#,
            r#"{"id":1,"error":{"message":"failed"}}"#,
            r#"{"id":1,"error":{"code":-1,"message":false}}"#,
            r#"{"id":1,"result":null,"jsonrpc":"1.0"}"#,
        ] {
            assert!(decode_line(line).is_err(), "accepted {line}");
        }
    }

    #[test]
    fn refuses_oversized_outbound_frames() {
        let envelope = Envelope::response(
            RpcId::Number(1),
            Some(serde_json::Value::String("x".repeat(super::MAX_LINE_BYTES))),
        );
        assert!(matches!(
            encode_line(&envelope),
            Err(ProtocolError::LineTooLarge)
        ));
    }

    #[test]
    fn oversized_rpc_ids_cannot_enter_pending_requests_or_completed_wait_caches() {
        let frame =
            serde_json::json!({"id":"x".repeat(1025),"method":"item/tool/call","params":{}});
        assert!(matches!(
            decode_line(&frame.to_string()),
            Err(ProtocolError::InvalidEnvelope(_))
        ));
        assert!(encode_line(&Envelope::response(RpcId::String("x".repeat(1025)), None)).is_err());
    }
}
