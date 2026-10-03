use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const MAX_SKILLS: usize = 256;
pub const MAX_NAME_BYTES: usize = 256;
pub const MAX_PATH_BYTES: usize = 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SkillAvailability {
    #[default]
    NotQueried,
    Available,
    Partial,
    Unsupported,
    TimedOut,
    RpcError,
    Malformed,
    TransportUnavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SkillFreshness {
    #[default]
    Unknown,
    Current,
    Stale,
    Queued,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillEntry {
    pub name: String,
    pub path: String,
    pub scope: String,
    pub enabled: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SkillsSnapshot {
    pub availability: SkillAvailability,
    pub freshness: SkillFreshness,
    pub skill_count: u32,
    pub enabled_count: u32,
    pub scan_error_count: u32,
    pub truncated: bool,
    pub entries: Vec<SkillEntry>,
}

/// Parses only the fields intended for the live inventory. Sensitive metadata and
/// raw scan errors are deliberately discarded at the protocol boundary.
pub fn parse_result(value: &Value, cwd: &str) -> Option<SkillsSnapshot> {
    let data = value.get("data")?.as_array()?;
    let mut snapshot = SkillsSnapshot {
        availability: SkillAvailability::Available,
        freshness: SkillFreshness::Current,
        ..SkillsSnapshot::default()
    };
    let mut found_cwd = false;
    let mut malformed = false;
    if data.len() > 32 {
        snapshot.truncated = true;
    }
    for directory in data.iter().take(32) {
        let Some(directory_cwd) = directory.get("cwd").and_then(Value::as_str) else {
            malformed = true;
            continue;
        };
        if directory_cwd != cwd {
            continue;
        }
        if found_cwd {
            // A single-cwd request must not turn duplicate response groups into
            // inflated counts or duplicate display entries.
            return None;
        }
        found_cwd = true;
        let errors = directory.get("errors").and_then(Value::as_array)?;
        let skills = directory.get("skills").and_then(Value::as_array)?;
        snapshot.scan_error_count = snapshot
            .scan_error_count
            .saturating_add(errors.len().min(u32::MAX as usize) as u32);
        if snapshot.entries.len().saturating_add(skills.len()) > MAX_SKILLS {
            snapshot.truncated = true;
        }
        for skill in skills {
            let (Some(name), Some(path), Some(scope), Some(enabled)) = (
                skill.get("name").and_then(Value::as_str),
                skill.get("path").and_then(Value::as_str),
                skill.get("scope").and_then(Value::as_str),
                skill.get("enabled").and_then(Value::as_bool),
            ) else {
                malformed = true;
                continue;
            };
            if !matches!(scope, "user" | "repo" | "system" | "admin") {
                malformed = true;
                continue;
            }
            snapshot.skill_count = snapshot.skill_count.saturating_add(1);
            snapshot.enabled_count = snapshot.enabled_count.saturating_add(u32::from(enabled));
            if name.len() > MAX_NAME_BYTES || path.len() > MAX_PATH_BYTES {
                snapshot.truncated = true;
                continue;
            }
            if snapshot.entries.len() >= MAX_SKILLS {
                continue;
            }
            snapshot.entries.push(SkillEntry {
                name: name.to_owned(),
                path: path.to_owned(),
                scope: scope.to_owned(),
                enabled,
            });
        }
    }
    if !found_cwd {
        return None;
    }
    if malformed || snapshot.scan_error_count > 0 || snapshot.truncated {
        snapshot.availability = SkillAvailability::Partial;
    }
    Some(snapshot)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StoredSkillsSummary {
    pub availability: SkillAvailability,
    pub freshness: SkillFreshness,
    pub skill_count: u32,
    pub enabled_count: u32,
    pub scan_error_count: u32,
    pub truncated: bool,
}

impl From<&SkillsSnapshot> for StoredSkillsSummary {
    fn from(value: &SkillsSnapshot) -> Self {
        Self {
            availability: value.availability,
            freshness: value.freshness,
            skill_count: value.skill_count,
            enabled_count: value.enabled_count,
            scan_error_count: value.scan_error_count,
            truncated: value.truncated,
        }
    }
}

impl StoredSkillsSummary {
    pub fn brief(&self) -> String {
        match self.availability {
            SkillAvailability::Available => format!(
                "server-confirmed | {:?} | {} skills observed, {} enabled | scan errors: {}{}",
                self.freshness,
                self.skill_count,
                self.enabled_count,
                self.scan_error_count,
                if self.truncated { " | truncated" } else { "" },
            ),
            SkillAvailability::Partial => format!(
                "server-confirmed partial | {:?} | at least {} skills observed, {} enabled | scan errors: {}{}",
                self.freshness,
                self.skill_count,
                self.enabled_count,
                self.scan_error_count,
                if self.truncated { " | truncated" } else { "" },
            ),
            reason => format!("{:?} / {:?} | skill counts unavailable", reason, self.freshness),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_only_bounded_display_fields_and_discards_sensitive_metadata() {
        let value = json!({"data":[{"cwd":"/workspace","errors":[{"path":"SECRET_PATH","message":"SECRET_ERROR"}],"skills":[{"name":"build","description":"SECRET_DESCRIPTION","enabled":true,"path":"/workspace/.agents/skills/build/SKILL.md","scope":"repo","interface":{"defaultPrompt":"SECRET_PROMPT"},"dependencies":{"tools":[{"type":"mcp","value":"SECRET_DEPENDENCY"}]}}]}]});
        let snapshot = parse_result(&value, "/workspace").unwrap();
        assert_eq!(snapshot.availability, SkillAvailability::Partial);
        assert_eq!(snapshot.skill_count, 1);
        assert_eq!(snapshot.enabled_count, 1);
        assert_eq!(snapshot.scan_error_count, 1);
        assert_eq!(snapshot.entries[0].name, "build");
        let stored = serde_json::to_string(&StoredSkillsSummary::from(&snapshot)).unwrap();
        assert!(!stored.contains("SECRET_"));
        assert!(!stored.contains("/workspace"));
    }

    #[test]
    fn mismatched_cwd_or_malformed_response_is_not_an_empty_success() {
        assert!(parse_result(&json!({"data":[]}), "/workspace").is_none());
        assert!(parse_result(
            &json!({"data":[{"cwd":"/other","errors":[],"skills":[]}]}),
            "/workspace"
        )
        .is_none());
        assert!(parse_result(
            &json!({"data":[{"cwd":"/workspace","skills":[]}]}),
            "/workspace"
        )
        .is_none());
        assert!(parse_result(
            &json!({"data":[{"cwd":"/workspace","errors":[]}]}),
            "/workspace"
        )
        .is_none());
    }

    #[test]
    fn duplicate_requested_cwd_groups_are_rejected() {
        let value: Value = serde_json::from_str(include_str!(
            "../tests/fixtures/codex-0.159.2/skills-list-repeated-cwd.json"
        ))
        .unwrap();
        assert!(parse_result(&value, "/workspace").is_none());
    }

    #[test]
    fn inventory_caps_retained_entries_and_keeps_the_total_count() {
        let skills: Vec<_> = (0..=MAX_SKILLS)
            .map(|index| {
                json!({"name":format!("skill-{index}"),"description":"private","enabled":index % 2 == 0,"path":format!("/workspace/{index}/SKILL.md"),"scope":"repo"})
            })
            .collect();
        let value = json!({"data":[{"cwd":"/workspace","errors":[],"skills":skills}]});
        let snapshot = parse_result(&value, "/workspace").unwrap();
        assert_eq!(snapshot.entries.len(), MAX_SKILLS);
        assert_eq!(snapshot.skill_count, (MAX_SKILLS + 1) as u32);
        assert_eq!(snapshot.availability, SkillAvailability::Partial);
        assert!(snapshot.truncated);
    }
}
