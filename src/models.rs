//! The model inventory offered by the backend, and the pending selection.
use serde_json::Value;

pub const MAX_MODELS: usize = 256;
pub const MAX_MODEL_BYTES: usize = 256;

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum ModelsAvailability {
    #[default]
    NotQueried,
    Loading,
    Available,
    Unsupported,
    TimedOut,
    RpcError,
    Malformed,
    TransportUnavailable,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelOption {
    /// Sent back unchanged when the model is selected.
    pub id: String,
    pub name: String,
}

/// One real request sent through a model to check its provider configuration.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum ModelTest {
    #[default]
    Idle,
    Running {
        model: String,
    },
    Passed {
        model: String,
        millis: u64,
    },
    Failed {
        model: String,
        /// The backend's own error text, bounded.
        message: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ModelsSnapshot {
    pub availability: ModelsAvailability,
    pub options: Vec<ModelOption>,
    pub truncated: bool,
    /// A selection sent to the backend that has not been confirmed yet.
    pub switching: Option<String>,
    /// The backend is restarting to read edited configuration.
    pub reloading: bool,
    pub test: ModelTest,
    /// Providers stored in the dsh profile, for editing; empty for other backends.
    pub providers: Vec<crate::backend::dsh_config::ProviderInfo>,
}

fn bounded(text: &str) -> Option<String> {
    let text = text.trim();
    (!text.is_empty() && text.len() <= MAX_MODEL_BYTES).then(|| text.to_owned())
}

/// Parses a `model/list` result. Only the identifier and display name are kept.
pub fn parse_result(value: &Value) -> Option<ModelsSnapshot> {
    let data = value.get("data")?.as_array()?;
    let mut snapshot = ModelsSnapshot {
        availability: ModelsAvailability::Available,
        truncated: data.len() > MAX_MODELS || value.get("nextCursor").is_some_and(Value::is_string),
        ..Default::default()
    };
    for entry in data.iter().take(MAX_MODELS) {
        let Some(id) = entry
            .get("model")
            .or_else(|| entry.get("id"))
            .and_then(Value::as_str)
            .and_then(bounded)
        else {
            continue;
        };
        if snapshot.options.iter().any(|option| option.id == id) {
            continue;
        }
        let name = entry
            .get("displayName")
            .and_then(Value::as_str)
            .and_then(bounded)
            .unwrap_or_else(|| id.clone());
        snapshot.options.push(ModelOption { id, name });
    }
    Some(snapshot)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn list_keeps_bounded_unique_identifiers_and_reports_truncation() {
        let snapshot = parse_result(&json!({"data":[
            {"id":"a-id","model":"a","displayName":"Model A","description":"PRIVATE"},
            {"model":"a","displayName":"Duplicate"},
            {"id":"b"},
            {"model":" "},
            {"model":"x".repeat(MAX_MODEL_BYTES + 1)},
            {"displayName":"no identifier"}
        ],"nextCursor":null}))
        .unwrap();
        assert_eq!(snapshot.availability, ModelsAvailability::Available);
        assert_eq!(
            snapshot.options,
            vec![
                ModelOption {
                    id: "a".into(),
                    name: "Model A".into()
                },
                ModelOption {
                    id: "b".into(),
                    name: "b".into()
                },
            ]
        );
        assert!(!snapshot.truncated);
        assert!(
            parse_result(&json!({"data":[],"nextCursor":"more"}))
                .unwrap()
                .truncated
        );
        let many: Vec<Value> = (0..MAX_MODELS + 1)
            .map(|index| json!({"model":format!("m{index}")}))
            .collect();
        let snapshot = parse_result(&json!({"data":many})).unwrap();
        assert_eq!(snapshot.options.len(), MAX_MODELS);
        assert!(snapshot.truncated);
        for malformed in [json!({}), json!({"data":"x"}), Value::Null] {
            assert!(parse_result(&malformed).is_none());
        }
    }
}
