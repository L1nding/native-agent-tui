//! Conversations as dsh reports them: the titled sessions of this workspace
//! and the readable messages of one of them. dsh owns the sessions; nothing is
//! stored here, these are only the bounded shapes Core accepts from it.

use serde_json::Value;

/// Rows kept from one list answer; the newest come first.
pub const MAX_CONVERSATIONS: usize = 500;
/// Messages kept from one history answer.
pub const MAX_MESSAGES: usize = 400;
const MAX_ID_BYTES: usize = 1024;
const MAX_TITLE_BYTES: usize = 512;
/// Text kept per message; longer text is cut and marked.
pub const MAX_TEXT_BYTES: usize = 64 * 1024;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ConversationSummary {
    /// The dsh session id; resuming names it.
    pub id: String,
    /// dsh's title; empty while the session has none.
    pub title: String,
    /// Zero when dsh gave no time.
    pub updated_at_ms: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SavedMessage {
    /// `user`, `assistant`, `reasoning` or `tool`.
    pub role: String,
    pub id: Option<String>,
    pub turn_id: Option<String>,
    pub text: String,
    pub truncated: bool,
    pub tool: Option<crate::tool_details::ConversationTool>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct History {
    pub messages: Vec<SavedMessage>,
    /// Older messages dsh or this bound left out.
    pub omitted: usize,
}

fn cut(text: &str, limit: usize) -> (String, bool) {
    if text.len() <= limit {
        return (text.to_owned(), false);
    }
    let mut end = limit;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    (text[..end].to_owned(), true)
}

/// Reads a `thread/list` result. Rows without a usable id are skipped; a
/// result without the list is malformed.
pub fn parse_list(result: &Value) -> Option<Vec<ConversationSummary>> {
    let rows = result.get("sessions")?.as_array()?;
    Some(
        rows.iter()
            .filter_map(|row| {
                let id = row.get("id")?.as_str()?;
                if id.is_empty() || id.len() > MAX_ID_BYTES {
                    return None;
                }
                let title = row.get("title").and_then(Value::as_str).unwrap_or("");
                Some(ConversationSummary {
                    id: id.to_owned(),
                    title: cut(title.trim(), MAX_TITLE_BYTES).0,
                    updated_at_ms: row
                        .get("updatedAt")
                        .or_else(|| row.get("createdAt"))
                        .and_then(Value::as_u64)
                        .unwrap_or(0),
                })
            })
            .take(MAX_CONVERSATIONS)
            .collect(),
    )
}

/// Reads a `thread/history` result, keeping the newest messages.
pub fn parse_history(result: &Value) -> Option<History> {
    let rows = result.get("messages")?.as_array()?;
    let mut messages: Vec<SavedMessage> = rows
        .iter()
        .filter_map(|row| {
            let role = row.get("role")?.as_str()?;
            let text = row.get("text")?.as_str()?;
            if !matches!(role, "user" | "assistant" | "reasoning" | "tool") || text.is_empty() {
                return None;
            }
            let (text, truncated) = cut(text, MAX_TEXT_BYTES);
            let identity = |key| {
                row.get(key)
                    .and_then(Value::as_str)
                    .filter(|id| !id.is_empty() && id.len() <= MAX_ID_BYTES)
                    .map(str::to_owned)
            };
            Some(SavedMessage {
                role: role.to_owned(),
                id: identity("id"),
                turn_id: identity("turnId"),
                text,
                truncated: truncated || row.get("truncated").and_then(Value::as_bool) == Some(true),
                tool: row.get("tool").and_then(|value| {
                    let mut tool: crate::tool_details::ConversationTool =
                        serde_json::from_value(value.clone()).ok()?;
                    if role != "tool"
                        || Some(tool.item_id.as_str()) != row.get("id").and_then(Value::as_str)
                    {
                        return None;
                    }
                    for text in [
                        &mut tool.name,
                        &mut tool.command,
                        &mut tool.cwd,
                        &mut tool.target,
                    ]
                    .into_iter()
                    .flatten()
                    {
                        *text = crate::state::display_text(&cut(text, 2048).0);
                    }
                    Some(tool)
                }),
            })
        })
        .collect();
    let dropped = messages.len().saturating_sub(MAX_MESSAGES);
    messages.drain(..dropped);
    let reported = result
        .get("omitted")
        .and_then(Value::as_u64)
        .and_then(|omitted| usize::try_from(omitted).ok())
        .unwrap_or(0);
    Some(History {
        messages,
        omitted: reported.saturating_add(dropped),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_list_keeps_titled_and_untitled_rows_and_skips_rows_without_an_id() {
        let listed = parse_list(&json!({"sessions":[
            {"id":"a","title":"  修复登录  ","updatedAt":20,"createdAt":10,"live":true},
            {"id":"b","createdAt":5},
            {"title":"no id"},
            {"id":""},
        ]}))
        .unwrap();
        assert_eq!(
            listed,
            vec![
                ConversationSummary {
                    id: "a".into(),
                    title: "修复登录".into(),
                    updated_at_ms: 20
                },
                ConversationSummary {
                    id: "b".into(),
                    title: String::new(),
                    updated_at_ms: 5
                },
            ]
        );
        assert_eq!(parse_list(&json!({})), None);
        assert_eq!(parse_list(&json!({"sessions":"x"})), None);
    }

    #[test]
    fn history_keeps_reasoning_and_tools_with_their_server_identity() {
        let history = parse_history(&json!({"messages":[
            {"role":"reasoning","id":"shared","turnId":"turn","text":"thought"},
            {"role":"tool","id":"tool-1","turnId":"turn","text":"▸ failed · cargo check"},
            {"role":"assistant","id":"shared","turnId":"turn","text":"answer"},
            {"role":"system","text":"unsupported"},
        ]}))
        .unwrap();
        assert_eq!(history.messages.len(), 3);
        assert_eq!(history.messages[0].role, "reasoning");
        assert_eq!(history.messages[1].role, "tool");
        assert_eq!(history.messages[1].id.as_deref(), Some("tool-1"));
        assert_eq!(history.messages[1].turn_id.as_deref(), Some("turn"));
        assert_eq!(history.messages[0].id, history.messages[2].id);
    }

    #[test]
    fn restored_tool_facts_require_matching_identity_and_bound_private_fields() {
        let tool = json!({"itemId":"tool","category":"shell","lifecycle":"failed",
            "command":"界".repeat(3000),"exitCode":1});
        let history = parse_history(&json!({"messages":[
            {"role":"tool","id":"tool","text":"▸ done · misleading", "tool":tool},
            {"role":"tool","id":"other","text":"▸ failed · x", "tool":tool},
            {"role":"assistant","id":"tool","text":"answer", "tool":tool},
            {"role":"tool","id":"legacy","text":"▸ done · x"},
        ]}))
        .unwrap();
        let restored = history.messages[0].tool.as_ref().unwrap();
        assert_eq!(
            restored.lifecycle,
            crate::tool_details::ToolLifecycle::Failed
        );
        assert!(restored.command.as_ref().unwrap().len() <= 2048);
        assert!(history.messages[1..]
            .iter()
            .all(|message| message.tool.is_none()));
        assert!(!format!("{restored:?}").contains('界'));
    }

    #[test]
    fn history_keeps_the_newest_messages_and_marks_cut_text() {
        let long = "界".repeat(MAX_TEXT_BYTES);
        let history = parse_history(&json!({"omitted":3,"messages":[
            {"role":"user","text":"hi","truncated":false},
            {"role":"system","text":"ignored"},
            {"role":"assistant","text":long},
            {"role":"assistant","text":""},
        ]}))
        .unwrap();
        assert_eq!(history.omitted, 3);
        assert_eq!(history.messages.len(), 2);
        assert_eq!(history.messages[0].role, "user");
        assert!(history.messages[1].truncated);
        assert!(history.messages[1].text.len() <= MAX_TEXT_BYTES);

        let many: Vec<_> = (0..MAX_MESSAGES + 2)
            .map(|index| json!({"role":"user","text":index.to_string()}))
            .collect();
        let history = parse_history(&json!({"messages":many})).unwrap();
        assert_eq!(history.messages.len(), MAX_MESSAGES);
        assert_eq!(history.messages[0].text, "2");
        assert_eq!(history.omitted, 2);
        assert_eq!(parse_history(&json!({"messages":1})), None);
    }
}
