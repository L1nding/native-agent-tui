use crate::agents::AgentInfo;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

mod file_changes;
pub(crate) use file_changes::{
    decode_file_change_snapshot, FileChangeFingerprint, FileChangeSnapshotSource,
    ObservedFileChangeKind, ObservedFileChangeSnapshot,
};

pub const MAX_LINE_BYTES: usize = 16 * 1024 * 1024;
pub const WAIT_TOOL: &str = "wait_for_subagent_completion";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObservedToolOutcome {
    Completed,
    CompletedUnknown,
    Failed,
    Interrupted,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ToolCategory {
    Shell,
    File,
    Mcp,
    Dynamic,
    Delegation,
    Web,
    Image,
    Sleep,
    Review,
    Compaction,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObservedTool {
    pub thread_id: String,
    pub turn_id: String,
    pub item_id: String,
    pub outcome: Option<ObservedToolOutcome>,
    pub category: ToolCategory,
    pub compaction: Option<ObservedCompaction>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ObservedToolDetails {
    pub command: Option<String>,
    pub cwd: Option<String>,
    pub parameters: Option<String>,
    pub result: Option<String>,
    pub output: Option<String>,
    pub exit_code: Option<i64>,
    pub duration_ms: Option<u64>,
}

const DETAIL_BYTES: usize = 64 * 1024;

fn bounded_text(value: Option<&Value>) -> Option<String> {
    let text = value?.as_str()?;
    let mut end = text.len().min(DETAIL_BYTES);
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    Some(text[..end].to_owned())
}

fn bounded_json(value: Option<&Value>) -> Option<String> {
    let value = value?;
    if value.is_null() {
        return None;
    }
    let text = serde_json::to_string(value).ok()?;
    let mut end = text.len().min(DETAIL_BYTES);
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    Some(text[..end].to_owned())
}

pub fn decode_observed_tool_details(item: &Value, category: ToolCategory) -> ObservedToolDetails {
    let command = (category == ToolCategory::Shell)
        .then(|| bounded_text(item.get("command")))
        .flatten();
    let cwd = (category == ToolCategory::Shell)
        .then(|| bounded_text(item.get("cwd")))
        .flatten();
    let parameters = match category {
        ToolCategory::Mcp | ToolCategory::Dynamic => bounded_json(item.get("arguments")),
        _ => None,
    };
    let result = match category {
        ToolCategory::Shell => None,
        ToolCategory::File => item.get("changes").and_then(|changes| {
            let changes = changes.as_array()?;
            let paths: Vec<&str> = changes
                .iter()
                .filter_map(|change| change.get("path").and_then(Value::as_str))
                .take(3)
                .collect();
            let mut text = format!(
                "{} file{}",
                changes.len(),
                if changes.len() == 1 { "" } else { "s" }
            );
            if !paths.is_empty() {
                text.push_str(": ");
                text.push_str(&paths.join(", "));
                if changes.len() > paths.len() {
                    text.push_str(&format!(" (+{})", changes.len() - paths.len()));
                }
            }
            bounded_text(Some(&Value::String(text)))
        }),
        ToolCategory::Mcp => bounded_json(item.get("result").filter(|v| !v.is_null()))
            .or_else(|| bounded_json(item.get("error"))),
        ToolCategory::Dynamic => bounded_json(item.get("contentItems")),
        _ => bounded_text(item.get("error")),
    };
    let output = (category == ToolCategory::Shell)
        .then(|| bounded_text(item.get("aggregatedOutput")))
        .flatten();
    let exit_code = item.get("exitCode").and_then(Value::as_i64);
    let duration_ms = item.get("durationMs").and_then(Value::as_u64);
    ObservedToolDetails {
        command,
        cwd,
        parameters,
        result,
        output,
        exit_code,
        duration_ms,
    }
}

/// Numeric facts accepted from a context compaction item. Textual item fields
/// (summary, prompt, errors, paths) are intentionally ignored at the edge.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ObservedCompaction {
    pub input_tokens: Option<u64>,
    pub cached_input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub total_tokens: Option<u64>,
    pub context_window: Option<u64>,
}

fn number(params: &Value, paths: &[&str]) -> Option<u64> {
    paths.iter().find_map(|path| {
        let value = if let Some(path) = path.strip_prefix('/') {
            params.pointer(&format!("/{path}"))
        } else {
            params.get(*path)
        }?;
        value.as_u64()
    })
}

fn decode_compaction(item: &Value) -> ObservedCompaction {
    let usage = item
        .get("usage")
        .or_else(|| item.get("tokenUsage"))
        .unwrap_or(item);
    ObservedCompaction {
        input_tokens: number(usage, &["inputTokens", "input_tokens", "input"]),
        cached_input_tokens: number(
            usage,
            &["cachedInputTokens", "cached_input_tokens", "cachedInput"],
        ),
        output_tokens: number(usage, &["outputTokens", "output_tokens", "output"]),
        total_tokens: number(usage, &["totalTokens", "total_tokens", "total"]),
        context_window: number(
            item,
            &["contextWindow", "context_window", "modelContextWindow"],
        )
        .or_else(|| {
            number(
                usage,
                &["contextWindow", "context_window", "modelContextWindow"],
            )
        }),
    }
}

pub fn decode_observed_tool(
    method: &str,
    params: &Value,
) -> Option<Result<ObservedTool, ProtocolError>> {
    if !matches!(method, "item/started" | "item/completed") {
        return None;
    }
    let item = &params["item"];
    let item_type = item["type"].as_str()?;
    let category = match item_type {
        "commandExecution" => ToolCategory::Shell,
        "fileChange" => ToolCategory::File,
        "mcpToolCall" => ToolCategory::Mcp,
        "dynamicToolCall" => ToolCategory::Dynamic,
        "collabAgentToolCall" => ToolCategory::Delegation,
        "webSearch" => ToolCategory::Web,
        "imageView" | "imageGeneration" => ToolCategory::Image,
        "sleep" => ToolCategory::Sleep,
        "enteredReviewMode" | "exitedReviewMode" => ToolCategory::Review,
        "contextCompaction" => ToolCategory::Compaction,
        _ => return None,
    };
    let id = |value: &Value| {
        value
            .as_str()
            .filter(|id| !id.is_empty() && id.len() <= 1024)
            .map(str::to_owned)
            .ok_or(ProtocolError::InvalidEnvelope(
                "invalid observation item identity",
            ))
    };
    Some((|| {
        let thread_id = id(&params["threadId"])?;
        let turn_id = id(&params["turnId"])?;
        let item_id = id(&item["id"])?;
        let outcome = if method == "item/started" {
            None
        } else {
            Some(match item["status"].as_str() {
                Some("completed") if completion_outcome(item) == Some(true) => {
                    ObservedToolOutcome::Completed
                }
                Some("completed") if completion_outcome(item) == Some(false) => {
                    ObservedToolOutcome::Failed
                }
                Some("failed" | "declined") => ObservedToolOutcome::Failed,
                Some("completed") => ObservedToolOutcome::Unknown,
                Some("interrupted" | "cancelled") => ObservedToolOutcome::Interrupted,
                None if item_type == "contextCompaction" && statusless_flags_valid(item) => {
                    ObservedToolOutcome::Completed
                }
                None if statusless_item_valid(item_type, item) && statusless_flags_valid(item) => {
                    ObservedToolOutcome::CompletedUnknown
                }
                _ => ObservedToolOutcome::Unknown,
            })
        };
        Ok(ObservedTool {
            thread_id,
            turn_id,
            item_id,
            outcome,
            category,
            compaction: (category == ToolCategory::Compaction).then(|| decode_compaction(item)),
        })
    })())
}

fn statusless_item_valid(item_type: &str, item: &Value) -> bool {
    match item_type {
        "webSearch" => item.get("query").and_then(Value::as_str).is_some(),
        "imageView" => item.get("path").and_then(Value::as_str).is_some(),
        "sleep" => item.get("durationMs").and_then(Value::as_u64).is_some(),
        "enteredReviewMode" | "exitedReviewMode" => {
            item.get("review").and_then(Value::as_str).is_some()
        }
        "contextCompaction" => true,
        _ => false,
    }
}

fn statusless_flags_valid(item: &Value) -> bool {
    !item
        .as_object()
        .is_some_and(|object| object.contains_key("status"))
        && item.get("error").is_none_or(Value::is_null)
        && item.get("failure").is_none_or(Value::is_null)
        && item
            .get("success")
            .is_none_or(|success| success.as_bool() == Some(true))
        && item
            .get("exitCode")
            .is_none_or(|code| code.as_i64() == Some(0))
}

/// `Some(true)` is a confirmed success, `Some(false)` is a typed failure, and
/// `None` means a malformed field prevents a safe conclusion.
fn completion_outcome(item: &Value) -> Option<bool> {
    let success = match item.get("success") {
        None | Some(Value::Null) => None,
        Some(Value::Bool(value)) => Some(*value),
        Some(_) => return None,
    };
    let exit_ok = match item.get("exitCode") {
        None | Some(Value::Null) => None,
        Some(value) => Some(value.as_i64()? == 0),
    };
    if item.get("error").is_some_and(|value| !value.is_null())
        || item.get("failure").is_some_and(|value| !value.is_null())
        || success == Some(false)
        || exit_ok == Some(false)
    {
        return Some(false);
    }
    Some(true)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObservedOutputKind {
    Reasoning,
    Tool(ToolCategory),
}
pub struct ObservedOutput {
    pub thread_id: String,
    pub turn_id: String,
    pub item_id: String,
    pub bytes: usize,
    pub kind: ObservedOutputKind,
    pub text: String,
}
pub fn decode_observed_output(
    method: &str,
    params: &Value,
) -> Option<Result<ObservedOutput, ProtocolError>> {
    let kind = match method {
        "item/reasoning/textDelta" | "item/reasoning/summaryTextDelta" => {
            ObservedOutputKind::Reasoning
        }
        "item/commandExecution/outputDelta" => ObservedOutputKind::Tool(ToolCategory::Shell),
        "item/fileChange/outputDelta" => ObservedOutputKind::Tool(ToolCategory::File),
        _ => return None,
    };
    let id = |value: &Value| {
        value
            .as_str()
            .filter(|id| !id.is_empty() && id.len() <= 1024)
            .map(str::to_owned)
            .ok_or(ProtocolError::InvalidEnvelope(
                "invalid observation output identity",
            ))
    };
    Some((|| {
        Ok(ObservedOutput {
            thread_id: id(&params["threadId"])?,
            turn_id: id(&params["turnId"])?,
            item_id: id(&params["itemId"])?,
            bytes: params["delta"]
                .as_str()
                .ok_or(ProtocolError::InvalidEnvelope(
                    "observation delta is not text",
                ))?
                .len(),
            kind,
            text: params["delta"].as_str().unwrap_or_default().to_owned(),
        })
    })())
}

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

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(untagged)]
pub enum RpcId {
    Number(i64),
    String(String),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Envelope {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub jsonrpc: Option<String>,
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
            jsonrpc: None,
            id: Some(id),
            method: Some(method.into()),
            params,
            result: None,
            error: None,
        }
    }

    pub fn notification(method: impl Into<String>, params: Option<Value>) -> Self {
        Self {
            jsonrpc: None,
            id: None,
            method: Some(method.into()),
            params,
            result: None,
            error: None,
        }
    }

    pub fn response(id: RpcId, result: Option<Value>) -> Self {
        Self {
            jsonrpc: None,
            id: Some(id),
            method: None,
            params: None,
            result: Some(result.unwrap_or(Value::Null)),
            error: None,
        }
    }

    pub fn error_response(id: RpcId, code: i64, message: impl Into<String>) -> Self {
        Self {
            jsonrpc: None,
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

/// 摘要行只显示 shell 包装内的脚本，例如 `pwsh.exe -Command "x"` 显示为 `x`；
/// 无法确定包装格式时原样返回。审批详情仍显示完整命令。
pub fn shell_script(command: &str) -> &str {
    let command = command.trim();
    let (program, rest) = match command.strip_prefix('"') {
        Some(quoted) => match quoted.find('"') {
            Some(end) => (&quoted[..end], &quoted[end + 1..]),
            None => return command,
        },
        None => command
            .split_once(char::is_whitespace)
            .unwrap_or((command, "")),
    };
    let name = program
        .rsplit(['\\', '/'])
        .next()
        .unwrap_or(program)
        .to_ascii_lowercase();
    let name = name.strip_suffix(".exe").unwrap_or(&name);
    if !matches!(name, "pwsh" | "powershell" | "cmd" | "bash" | "sh" | "zsh") {
        return command;
    }
    let mut rest = rest.trim_start();
    loop {
        let Some((flag, tail)) = rest.split_once(char::is_whitespace) else {
            return command;
        };
        let flag = flag.to_ascii_lowercase();
        if matches!(flag.as_str(), "-command" | "-c" | "/c" | "-lc") {
            rest = tail.trim();
            break;
        }
        if !flag.starts_with('-') && !flag.starts_with('/') {
            return command;
        }
        rest = tail.trim_start();
    }
    ['"', '\'']
        .into_iter()
        .find_map(|quote| {
            rest.strip_prefix(quote)
                .and_then(|inner| inner.strip_suffix(quote))
                .filter(|inner| !inner.is_empty() && !inner.contains(quote))
        })
        .unwrap_or(rest)
}

#[cfg(test)]
mod tests {
    #[test]
    fn file_change_details_name_the_first_paths() {
        let item = serde_json::json!({"changes":[
            {"path":"calc.py"},{"path":"src/a.rs"},{"path":"src/b.rs"},{"path":"src/c.rs"}]});
        let details = super::decode_observed_tool_details(&item, super::ToolCategory::File);
        assert_eq!(
            details.result.as_deref(),
            Some("4 files: calc.py, src/a.rs, src/b.rs (+1)")
        );
        let one = serde_json::json!({"changes":[{"path":"calc.py"}]});
        let details = super::decode_observed_tool_details(&one, super::ToolCategory::File);
        assert_eq!(details.result.as_deref(), Some("1 file: calc.py"));
    }

    #[test]
    fn shell_script_unwraps_common_shell_wrappers_only() {
        assert_eq!(
            super::shell_script(
                r#""C:\Program Files\PowerShell\pwsh.exe" -NoProfile -Command "Set-Content x 'hi'""#
            ),
            "Set-Content x 'hi'"
        );
        assert_eq!(super::shell_script("bash -lc 'cargo test'"), "cargo test");
        // 内部还有同种引号时无法安全去壳，保留原样。
        assert_eq!(
            super::shell_script("bash -lc 'echo '\"'x'\"''"),
            "'echo '\"'x'\"''"
        );
        assert_eq!(super::shell_script("cmd.exe /c dir"), "dir");
        assert_eq!(super::shell_script("cargo test --all"), "cargo test --all");
        assert_eq!(super::shell_script("pwsh"), "pwsh");
        assert_eq!(super::shell_script(r#""unterminated"#), r#""unterminated"#);
    }

    use super::{decode_line, encode_line, Envelope, ProtocolError, RpcId};

    #[test]
    fn tool_observation_excludes_arguments_and_does_not_guess_missing_results() {
        use super::{
            decode_observed_output, decode_observed_tool, ObservedOutputKind, ObservedToolOutcome,
        };
        for (item, expected) in [
            (
                serde_json::json!({"type":"commandExecution","id":"tool","status":"completed","exitCode":0,"command":"PRIVATE"}),
                ObservedToolOutcome::Completed,
            ),
            (
                serde_json::json!({"type":"commandExecution","id":"tool","status":"completed","exitCode":1}),
                ObservedToolOutcome::Failed,
            ),
            (
                serde_json::json!({"type":"mcpToolCall","id":"tool","status":"completed","error":{"message":"PRIVATE"}}),
                ObservedToolOutcome::Failed,
            ),
            (
                serde_json::json!({"type":"fileChange","id":"tool"}),
                ObservedToolOutcome::Unknown,
            ),
            (
                serde_json::json!({"type":"commandExecution","id":"tool","status":"interrupted"}),
                ObservedToolOutcome::Interrupted,
            ),
        ] {
            let notice = decode_observed_tool(
                "item/completed",
                &serde_json::json!({"threadId":"t","turnId":"u","item":item}),
            )
            .unwrap()
            .unwrap();
            assert_eq!(notice.outcome, Some(expected));
            assert!(!format!("{notice:?}").contains("PRIVATE"));
        }
        let output = decode_observed_output(
            "item/reasoning/textDelta",
            &serde_json::json!({"threadId":"t","turnId":"u","itemId":"i","delta":"中"}),
        )
        .unwrap()
        .unwrap();
        assert_eq!(output.bytes, 3);
        assert_eq!(output.kind, ObservedOutputKind::Reasoning);
    }

    #[test]
    fn schema_statusless_compaction_is_confirmed_only_by_item_completed() {
        use super::{decode_observed_tool, ObservedToolOutcome, ToolCategory};
        let params = serde_json::json!({
            "threadId":"t", "turnId":"u",
            "item":{"id":"compact-1", "type":"contextCompaction"}
        });
        let started = decode_observed_tool("item/started", &params)
            .unwrap()
            .unwrap();
        assert_eq!(started.category, ToolCategory::Compaction);
        assert_eq!(started.outcome, None);
        let completed = decode_observed_tool("item/completed", &params)
            .unwrap()
            .unwrap();
        assert_eq!(completed.outcome, Some(ObservedToolOutcome::Completed));

        for contradiction in [
            serde_json::json!({"id":"compact-1","type":"contextCompaction","status":"running"}),
            serde_json::json!({"id":"compact-1","type":"contextCompaction","error":{"message":"failed"}}),
            serde_json::json!({"id":"compact-1","type":"contextCompaction","success":false}),
            serde_json::json!({"id":"compact-1","type":"contextCompaction","exitCode":1}),
            serde_json::json!({"id":"compact-1","type":"contextCompaction","exitCode":"1"}),
            serde_json::json!({"id":"compact-1","type":"contextCompaction","success":"false"}),
            serde_json::json!({"id":"compact-1","type":"contextCompaction","error":"failed"}),
            serde_json::json!({"id":"compact-1","type":"contextCompaction","status":null}),
        ] {
            let notice = decode_observed_tool(
                "item/completed",
                &serde_json::json!({"threadId":"t","turnId":"u","item":contradiction}),
            )
            .unwrap()
            .unwrap();
            assert_ne!(notice.outcome, Some(ObservedToolOutcome::Completed));
        }

        let ordinary = decode_observed_tool(
            "item/completed",
            &serde_json::json!({"threadId":"t","turnId":"u","item":{"id":"tool","type":"commandExecution"}}),
        )
        .unwrap()
        .unwrap();
        assert_eq!(ordinary.outcome, Some(ObservedToolOutcome::Unknown));
    }

    #[test]
    fn completed_tool_requires_well_typed_success_fields() {
        use super::{decode_observed_tool, ObservedToolOutcome};
        let invalid = [
            serde_json::json!({"id":"tool","type":"commandExecution","status":"completed","exitCode":"1"}),
            serde_json::json!({"id":"tool","type":"commandExecution","status":"completed","success":"false"}),
            serde_json::json!({"id":"tool","type":"imageGeneration","status":"completed","failure":{"message":"failed"}}),
        ];
        for item in invalid {
            let notice = decode_observed_tool(
                "item/completed",
                &serde_json::json!({"threadId":"t","turnId":"u","item":item}),
            )
            .unwrap()
            .unwrap();
            assert_ne!(notice.outcome, Some(ObservedToolOutcome::Completed));
        }

        for item in [
            serde_json::json!({"id":"tool","type":"commandExecution","status":"completed"}),
            serde_json::json!({"id":"tool","type":"commandExecution","status":"completed","exitCode":null,"success":null}),
            serde_json::json!({"id":"tool","type":"commandExecution","status":"completed","exitCode":0,"success":true}),
        ] {
            let notice = decode_observed_tool(
                "item/completed",
                &serde_json::json!({"threadId":"t","turnId":"u","item":item}),
            )
            .unwrap()
            .unwrap();
            assert_eq!(notice.outcome, Some(ObservedToolOutcome::Completed));
        }
    }

    #[test]
    fn statusless_completion_is_limited_to_schema_items_with_confirmed_fields() {
        use super::{decode_observed_tool, ObservedToolOutcome};
        for item in [
            serde_json::json!({"id":"web","type":"webSearch","query":"safe"}),
            serde_json::json!({"id":"image-view","type":"imageView","path":"/tmp/image.png"}),
            serde_json::json!({"id":"sleep","type":"sleep","durationMs":10}),
            serde_json::json!({"id":"review","type":"enteredReviewMode","review":"review"}),
        ] {
            let notice = decode_observed_tool(
                "item/completed",
                &serde_json::json!({"threadId":"t","turnId":"u","item":item}),
            )
            .unwrap()
            .unwrap();
            assert_eq!(notice.outcome, Some(ObservedToolOutcome::CompletedUnknown));
        }

        for item in [
            serde_json::json!({"id":"image","type":"imageGeneration","result":"ok"}),
            serde_json::json!({"id":"web","type":"webSearch","query":"safe","success":null}),
            serde_json::json!({"id":"sleep","type":"sleep","durationMs":10,"exitCode":null}),
        ] {
            let notice = decode_observed_tool(
                "item/completed",
                &serde_json::json!({"threadId":"t","turnId":"u","item":item}),
            )
            .unwrap()
            .unwrap();
            assert_ne!(notice.outcome, Some(ObservedToolOutcome::CompletedUnknown));
        }
    }

    #[test]
    fn tool_details_keep_aggregate_shell_output_out_of_result_and_use_schema_fields() {
        use super::{decode_observed_tool_details, ToolCategory};

        let shell = decode_observed_tool_details(
            &serde_json::json!({"command":"echo","aggregatedOutput":"done"}),
            ToolCategory::Shell,
        );
        assert_eq!(shell.result, None);
        assert_eq!(shell.output.as_deref(), Some("done"));

        let dynamic = decode_observed_tool_details(
            &serde_json::json!({"arguments":{"input":"x"},"contentItems":[{"type":"text","text":"result"}]}),
            ToolCategory::Dynamic,
        );
        assert_eq!(dynamic.parameters.as_deref(), Some(r#"{"input":"x"}"#));
        assert!(dynamic.result.as_deref().unwrap().contains("result"));

        let mcp = decode_observed_tool_details(
            &serde_json::json!({"result":null,"error":{"message":"failed"}}),
            ToolCategory::Mcp,
        );
        assert!(mcp.result.as_deref().unwrap().contains("failed"));
    }

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
    fn compaction_decoder_keeps_numeric_usage_and_drops_private_text() {
        let params = serde_json::json!({
            "threadId":"t", "turnId":"u",
            "item":{
                "id":"compact-1", "type":"contextCompaction",
                "usage":{"inputTokens":12,"cachedInputTokens":3,"outputTokens":4,"totalTokens":16},
                "contextWindow":128,
                "summary":"PRIVATE_SUMMARY", "path":"PRIVATE_PATH",
                "error":{"message":"PRIVATE_ERROR"}
            }
        });
        let notice = super::decode_observed_tool("item/completed", &params)
            .unwrap()
            .unwrap();
        assert_eq!(notice.compaction.unwrap().total_tokens, Some(16));
        let debug = format!("{notice:?}");
        assert!(!debug.contains("PRIVATE_"));
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
