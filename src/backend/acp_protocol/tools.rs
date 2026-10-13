//! Bounded ACP tool presentation facts; no lifecycle inference from output text.
use serde_json::Value;

use super::{ToolInput, MAX_TEXT_BYTES};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolKind {
    Shell,
    File,
    Generic,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileDiff {
    pub path: String,
    pub patch: String,
}

pub fn kind(payload: &Value, input: &ToolInput) -> ToolKind {
    match payload.get("kind").and_then(Value::as_str) {
        Some("edit" | "delete" | "move") => return ToolKind::File,
        Some("execute") => return ToolKind::Shell,
        _ => {}
    }
    match payload.get("title").and_then(Value::as_str) {
        Some("edit" | "write") => ToolKind::File,
        Some("str_replace_editor")
            if payload.pointer("/rawInput/command").and_then(Value::as_str) != Some("view") =>
        {
            ToolKind::File
        }
        Some("str_replace_editor") => ToolKind::Generic,
        _ if input.command.is_some() => ToolKind::Shell,
        _ => ToolKind::Generic,
    }
}

pub fn diffs(payload: &Value, kind: ToolKind) -> Vec<FileDiff> {
    let mut out = Vec::new();
    let mut bytes = 0;
    if let Some(content) = payload.get("content").and_then(Value::as_array) {
        for value in content
            .iter()
            .filter(|value| value["type"] == "diff")
            .take(32)
        {
            if let Some(diff) = file_diff(
                value.get("path"),
                value.get("oldText"),
                value.get("newText"),
            ) {
                bytes += diff.path.len() + diff.patch.len();
                if bytes > MAX_TEXT_BYTES {
                    break;
                }
                out.push(diff);
            }
        }
    }
    if out.is_empty() && kind == ToolKind::File {
        let input = &payload["rawInput"];
        let old = input.get("old_string").or_else(|| input.get("old_str"));
        let new = input.get("new_string").or_else(|| input.get("new_str"));
        if old.is_some() {
            if let Some(diff) = file_diff(
                input.get("file_path").or_else(|| input.get("path")),
                old,
                new.or(Some(&Value::Null)),
            ) {
                out.push(diff);
            }
        }
    }
    out
}

fn file_diff(path: Option<&Value>, old: Option<&Value>, new: Option<&Value>) -> Option<FileDiff> {
    let path = path?.as_str()?;
    if path.is_empty() || path.len() > 2048 {
        return None;
    }
    fn text(value: Option<&Value>) -> Option<&str> {
        match value? {
            Value::Null => Some(""),
            Value::String(text) => Some(text.as_str()),
            _ => None,
        }
    }
    let old = text(old)?;
    let new = text(new)?;
    if old.len() + new.len() > MAX_TEXT_BYTES {
        return None;
    }
    let old: Vec<_> = old.split_inclusive('\n').collect();
    let new: Vec<_> = new.split_inclusive('\n').collect();
    let prefix = old.iter().zip(&new).take_while(|(a, b)| a == b).count();
    let suffix = old[prefix..]
        .iter()
        .rev()
        .zip(new[prefix..].iter().rev())
        .take_while(|(a, b)| a == b)
        .count();
    let mut patch = format!(
        "@@ -{},{} +{},{} @@\n",
        prefix + 1,
        old.len() - prefix - suffix,
        prefix + 1,
        new.len() - prefix - suffix
    );
    for line in &old[prefix..old.len() - suffix] {
        patch.push('-');
        patch.push_str(line);
        if !line.ends_with('\n') {
            patch.push_str("\n\\ No newline at end of file\n");
        }
    }
    for line in &new[prefix..new.len() - suffix] {
        patch.push('+');
        patch.push_str(line);
        if !line.ends_with('\n') {
            patch.push_str("\n\\ No newline at end of file\n");
        }
    }
    (patch.len() + path.len() <= MAX_TEXT_BYTES).then(|| FileDiff {
        path: path.to_owned(),
        patch,
    })
}

pub fn bounded(text: &str, max: usize) -> String {
    let mut end = text.len().min(max);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn declared_and_registered_file_tools_do_not_become_shell_commands() {
        for payload in [
            json!({"kind":"edit"}),
            json!({"title":"str_replace_editor", "rawInput":{"command":"str_replace"}}),
        ] {
            assert_eq!(
                kind(
                    &payload,
                    &ToolInput {
                        command: Some("str_replace".into()),
                        ..Default::default()
                    }
                ),
                ToolKind::File
            );
        }
        assert_eq!(
            kind(
                &json!({"title":"str_replace_editor","rawInput":{"command":"view"}}),
                &ToolInput::default()
            ),
            ToolKind::Generic
        );
        assert_eq!(
            kind(&json!({"title":"read"}), &ToolInput::default()),
            ToolKind::Generic
        );
    }

    #[test]
    fn diff_keeps_changed_lines_unicode_and_final_newline_without_copying_common_text() {
        let diffs = diffs(
            &json!({"content":[{"type":"diff","path":"src/中文.rs","oldText":"same\n-old\nend\n","newText":"same\n+new\nend\n"}]}),
            ToolKind::Generic,
        );
        assert_eq!(diffs[0].patch, "@@ -2,1 +2,1 @@\n--old\n++new\n");
        let no_newline = file_diff(
            Some(&json!("a")),
            Some(&json!("line\n")),
            Some(&json!("line")),
        )
        .unwrap();
        assert!(no_newline.patch.contains("\\ No newline at end of file"));
        assert!(file_diff(
            Some(&json!("a")),
            Some(&json!("界".repeat(MAX_TEXT_BYTES))),
            Some(&json!("x"))
        )
        .is_none());
        assert_eq!(bounded("界👋suffix", 6), "界");
    }
}
