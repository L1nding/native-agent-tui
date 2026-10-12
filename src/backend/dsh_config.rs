//! Model providers as dsh's settings service holds them. This module only
//! validates what the form entered and reads what dsh answers; dsh stores the
//! provider and, through its credential service, the key.
use serde_json::{json, Value};

pub const API_KINDS: [&str; 3] = [
    "openai-completions",
    "openai-responses",
    "anthropic-messages",
];
/// An API key. It is handed to dsh's credential service and kept nowhere else.
#[derive(Clone, PartialEq, Eq, Default)]
pub struct Secret(pub String);

impl std::fmt::Debug for Secret {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("Secret(<redacted>)")
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ProviderSpec {
    pub provider: String,
    pub base_url: String,
    pub api: String,
    /// Empty keeps whatever credential dsh already has for this provider.
    pub api_key: Secret,
    pub model: String,
    /// Empty uses the model identifier as its display name.
    pub model_name: String,
    /// The model's context length in tokens; `None` leaves the stored value alone.
    pub context_window: Option<u64>,
}

pub const MAX_CONTEXT_WINDOW: u64 = 100_000_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Added {
    /// `provider/model`, the label the model list shows.
    pub label: String,
    pub credential: String,
    pub key_stored: bool,
}

fn plain(text: &str, max: usize) -> bool {
    !text.is_empty()
        && text.len() <= max
        && !text.chars().any(|c| c.is_whitespace() || c.is_control())
}

/// The credential name dsh looks up for `provider`, e.g. `MY_API_API_KEY`.
pub fn credential_name(provider: &str) -> String {
    let name: String = provider
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_uppercase()
            } else {
                '_'
            }
        })
        .collect();
    format!("{name}_API_KEY")
}

/// Rejects anything that could not be a provider entry; the message is shown in the form.
pub fn validate(spec: &ProviderSpec) -> Result<(), &'static str> {
    let provider = spec.provider.as_str();
    if !plain(provider, 32)
        || !provider.starts_with(|c: char| c.is_ascii_lowercase() || c.is_ascii_digit())
        || !provider
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '-' | '_'))
    {
        return Err("Provider name: 1-32 of a-z, 0-9, - or _, starting with a letter or digit.");
    }
    if !plain(&spec.base_url, 2048)
        || !(spec.base_url.starts_with("https://") || spec.base_url.starts_with("http://"))
        || spec.base_url.ends_with("://")
    {
        return Err("Base URL must start with https:// or http:// and contain no spaces.");
    }
    if !API_KINDS.contains(&spec.api.as_str()) {
        return Err("Choose one of the listed API types.");
    }
    if !plain(&spec.model, 128) {
        return Err("Model id: 1-128 characters without spaces.");
    }
    if spec.model_name.len() > 128 || spec.model_name.chars().any(char::is_control) {
        return Err("Display name: at most 128 characters.");
    }
    if !spec.api_key.0.is_empty() && !plain(&spec.api_key.0, 512) {
        return Err("API key: at most 512 characters without spaces.");
    }
    if spec
        .context_window
        .is_some_and(|tokens| tokens == 0 || tokens > MAX_CONTEXT_WINDOW)
    {
        return Err("Context length: a whole number of tokens from 1 to 100000000.");
    }
    Ok(())
}

/// What dsh holds for one provider; the key is never read.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ProviderInfo {
    pub name: String,
    pub base_url: String,
    pub api: String,
    /// Model id and its context length, when dsh gives one.
    pub context_windows: Vec<(String, u64)>,
}

/// Parameters of a `provider/save` request. The key is included only when
/// one was entered; an absent key leaves dsh's stored credential alone.
pub fn save_params(spec: &ProviderSpec) -> Value {
    let mut params = json!({
        "provider": spec.provider,
        "baseURL": spec.base_url,
        "api": spec.api,
        "model": spec.model,
        "credential": credential_name(&spec.provider),
    });
    if !spec.model_name.is_empty() {
        params["modelName"] = json!(spec.model_name);
    }
    if let Some(tokens) = spec.context_window {
        params["contextWindow"] = json!(tokens);
    }
    if !spec.api_key.0.is_empty() {
        params["apiKey"] = json!(spec.api_key.0);
    }
    params
}

/// Reads the answer to `provider/save`.
pub fn parse_saved(result: &Value) -> Option<Added> {
    Some(Added {
        label: result.get("label")?.as_str()?.to_owned(),
        credential: result.get("credential")?.as_str()?.to_owned(),
        key_stored: result.get("keyStored")?.as_bool()?,
    })
}

/// Reads the answer to `provider/list`, for prefilling the edit form. Entries
/// that are not providers are skipped; an answer without the list is malformed.
pub fn parse_providers(result: &Value) -> Option<Vec<ProviderInfo>> {
    let rows = result.get("providers")?.as_array()?;
    Some(
        rows.iter()
            .filter_map(|row| {
                let name = row.get("name")?.as_str()?;
                if !plain(name, 64) {
                    return None;
                }
                let field = |key: &str| {
                    row.get(key)
                        .and_then(Value::as_str)
                        .filter(|text| text.len() <= 2048)
                        .unwrap_or("")
                        .to_owned()
                };
                Some(ProviderInfo {
                    name: name.to_owned(),
                    base_url: field("baseURL"),
                    api: field("api"),
                    context_windows: row
                        .get("models")
                        .and_then(Value::as_array)
                        .into_iter()
                        .flatten()
                        .filter_map(|model| {
                            Some((
                                model.get("id")?.as_str()?.to_owned(),
                                model.get("contextWindow")?.as_u64()?,
                            ))
                        })
                        .take(256)
                        .collect(),
                })
            })
            .take(64)
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec() -> ProviderSpec {
        ProviderSpec {
            provider: "my-api".into(),
            base_url: "https://api.example.com/v1".into(),
            api: "openai-completions".into(),
            api_key: Secret("sk-test-1".into()),
            model: "big-model".into(),
            model_name: "Big".into(),
            context_window: Some(200_000),
        }
    }

    #[test]
    fn a_save_request_names_the_credential_and_carries_the_key_only_when_entered() {
        let params = save_params(&spec());
        assert_eq!(
            params,
            json!({
                "provider":"my-api","baseURL":"https://api.example.com/v1","api":"openai-completions",
                "model":"big-model","credential":"MY_API_API_KEY","modelName":"Big",
                "contextWindow":200_000,"apiKey":"sk-test-1"
            })
        );
        let bare = save_params(&ProviderSpec {
            api_key: Secret::default(),
            model_name: String::new(),
            context_window: None,
            ..spec()
        });
        for absent in ["apiKey", "modelName", "contextWindow"] {
            assert!(bare.get(absent).is_none(), "{absent}");
        }
        assert!(!format!("{:?}", spec()).contains("sk-test-1"));
    }

    #[test]
    fn invalid_entries_are_rejected_before_anything_is_sent() {
        assert!(validate(&spec()).is_ok());
        for bad in [
            ProviderSpec {
                provider: "My Api".into(),
                ..spec()
            },
            ProviderSpec {
                base_url: "ftp://x".into(),
                ..spec()
            },
            ProviderSpec {
                api: "other".into(),
                ..spec()
            },
            ProviderSpec {
                model: "a b".into(),
                ..spec()
            },
            ProviderSpec {
                api_key: Secret("a b".into()),
                ..spec()
            },
            ProviderSpec {
                context_window: Some(0),
                ..spec()
            },
            ProviderSpec {
                context_window: Some(MAX_CONTEXT_WINDOW + 1),
                ..spec()
            },
        ] {
            assert!(validate(&bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn answers_from_dsh_are_read_and_malformed_ones_are_not_guessed() {
        assert_eq!(
            parse_saved(
                &json!({"label":"my-api/big-model","credential":"MY_API_API_KEY","keyStored":true})
            ),
            Some(Added {
                label: "my-api/big-model".into(),
                credential: "MY_API_API_KEY".into(),
                key_stored: true
            })
        );
        assert_eq!(parse_saved(&json!({"label":"x"})), None);
        let listed = parse_providers(&json!({"writable":true,"providers":[
            {"name":"hi","baseURL":"https://hi.example/v1","api":"openai-responses","apiKeyEnv":"HI_API_KEY",
             "models":[{"id":"luna","name":"Luna","contextWindow":272000},{"id":"no-window"}]},
            {"name":"bad name"},
            {"baseURL":"https://nameless"},
        ]}))
        .unwrap();
        assert_eq!(
            listed,
            vec![ProviderInfo {
                name: "hi".into(),
                base_url: "https://hi.example/v1".into(),
                api: "openai-responses".into(),
                context_windows: vec![("luna".into(), 272000)],
            }]
        );
        assert_eq!(parse_providers(&json!({})), None);
    }
}
