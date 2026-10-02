use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

pub const MAX_LINE_BYTES: usize = 16 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(untagged)]
pub enum RpcId {
    Number(i64),
    String(String),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Envelope {
    #[serde(default)]
    pub id: Option<RpcId>,
    #[serde(default)]
    pub method: Option<String>,
    #[serde(default)]
    pub params: Option<Value>,
    #[serde(default)]
    pub result: Option<Value>,
    #[serde(default)]
    pub error: Option<Value>,
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
            result,
            error: None,
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
}

pub fn decode_line(line: &str) -> Result<Envelope, ProtocolError> {
    if line.len() > MAX_LINE_BYTES {
        return Err(ProtocolError::LineTooLarge);
    }

    let value: Value = serde_json::from_str(line)?;
    if !value.is_object() {
        return Err(ProtocolError::NotAnObject);
    }

    let envelope: Envelope = serde_json::from_value(value)?;
    if envelope.id.is_none() && envelope.method.is_none() {
        return Err(ProtocolError::MissingIdentity);
    }
    Ok(envelope)
}

pub fn encode_line(envelope: &Envelope) -> Result<String, ProtocolError> {
    let mut line = serde_json::to_string(envelope)?;
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
        assert_eq!(
            line,
            r#"{"id":null,"method":"initialized","params":null,"result":null,"error":null}"#
                .to_owned()
                + "\n"
        );
    }
}
