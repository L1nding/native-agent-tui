//! dsh's permission presets. Each bundles a sandbox mode with an approval
//! policy, and a session runs under exactly one. dsh enforces them; this is
//! only the bounded shape Core accepts and the interface shows.

use serde_json::Value;

const MAX_PRESETS: usize = 32;
const MAX_NAME_BYTES: usize = 64;
const MAX_DESCRIPTION_BYTES: usize = 512;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PermissionPreset {
    /// What `permission/set` names.
    pub value: String,
    pub name: String,
    pub description: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PermissionSnapshot {
    /// Preset the session runs under, as dsh last reported it; empty until read.
    pub current: String,
    /// Empty until read, or when the dsh profile has no presets.
    pub presets: Vec<PermissionPreset>,
    /// A switch dsh has not confirmed yet.
    pub switching: Option<String>,
}

fn plain(text: &str, max: usize) -> bool {
    !text.is_empty() && text.len() <= max && !text.chars().any(char::is_control)
}

/// Reads the answer to `permission/describe` or `permission/set`: the preset
/// in effect and the ones that can be chosen. Malformed rows are skipped; an
/// answer without a current preset is malformed.
pub fn parse(result: &Value) -> Option<(String, Vec<PermissionPreset>)> {
    let current = result.get("current")?.as_str()?;
    if !plain(current, MAX_NAME_BYTES) {
        return None;
    }
    let presets = result
        .get("presets")?
        .as_array()?
        .iter()
        .filter_map(|row| {
            let value = row.get("value")?.as_str()?;
            if !plain(value, MAX_NAME_BYTES) {
                return None;
            }
            let name = row
                .get("name")
                .and_then(Value::as_str)
                .filter(|name| plain(name, MAX_NAME_BYTES))
                .unwrap_or(value);
            let description = row
                .get("description")
                .and_then(Value::as_str)
                .filter(|text| text.len() <= MAX_DESCRIPTION_BYTES)
                .unwrap_or("");
            Some(PermissionPreset {
                value: value.to_owned(),
                name: name.to_owned(),
                description: description.to_owned(),
            })
        })
        .take(MAX_PRESETS)
        .collect();
    Some((current.to_owned(), presets))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_preset_in_effect_and_the_choices_are_read_and_bad_rows_are_skipped() {
        let (current, presets) = parse(
            &json!({"current":"workspace-write","default":"workspace-write","presets":[
                {"value":"read-only","name":"Read only","description":"No writes."},
                {"value":"workspace-write"},
                {"name":"no value"},
                {"value":"bad\nname"},
            ]}),
        )
        .unwrap();
        assert_eq!(current, "workspace-write");
        assert_eq!(
            presets,
            vec![
                PermissionPreset {
                    value: "read-only".into(),
                    name: "Read only".into(),
                    description: "No writes.".into()
                },
                PermissionPreset {
                    value: "workspace-write".into(),
                    name: "workspace-write".into(),
                    description: String::new()
                },
            ]
        );
        assert_eq!(parse(&json!({"presets":[]})), None);
        assert_eq!(parse(&json!({"current":"","presets":[]})), None);
        assert_eq!(parse(&json!({"current":"x"})), None);
    }
}
