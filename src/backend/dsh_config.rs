//! Adds a model provider to a DeepSeek Harness profile, the way dsh stores it:
//! `profiles/<profile>/cordis.patch.yml` holds `providers.<name>` under the
//! `llm-pi-ai` entry, and `.credentials.yaml` holds the key the provider's
//! `apiKeyEnv` names.
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use serde_yaml::{Mapping, Value};

pub const API_KINDS: [&str; 3] = [
    "openai-completions",
    "openai-responses",
    "anthropic-messages",
];
const MAX_FILE_BYTES: u64 = 1024 * 1024;
const ENTRY_ID: &str = "llm-pi-ai";
const ENTRY_NAME: &str = "@deepseek-ai/dsh-llm-pi-ai";

/// An API key. It is written to dsh's credential store and nowhere else.
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
    /// Empty keeps whatever credential the profile already has for this provider.
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
    /// `provider/model`, the label the model list will show after a restart.
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

/// `DSH_HOME`, or `.dsh` in the user's home directory.
pub fn home() -> Option<PathBuf> {
    std::env::var_os("DSH_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| {
            std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
                .map(|home| PathBuf::from(home).join(".dsh"))
        })
}

fn read(path: &Path) -> Result<String, String> {
    let mut text = String::new();
    std::fs::File::open(path)
        .and_then(|file| file.take(MAX_FILE_BYTES + 1).read_to_string(&mut text))
        .map_err(|_| format!("cannot read {}", path.display()))?;
    if text.len() as u64 > MAX_FILE_BYTES {
        return Err(format!("{} is too large to edit safely", path.display()));
    }
    Ok(text)
}

fn tagged(value: &Value) -> bool {
    match value {
        Value::Tagged(_) => true,
        Value::Sequence(items) => items.iter().any(tagged),
        Value::Mapping(entries) => entries
            .iter()
            .any(|(key, value)| tagged(key) || tagged(value)),
        _ => false,
    }
}

/// Parses a dsh YAML file that is safe to write back. A file with tagged
/// values (such as `!!js` expressions) could change meaning when rewritten,
/// so it is left for the user to edit by hand.
fn parse(path: &Path, text: &str) -> Result<Value, String> {
    let value: Value =
        serde_yaml::from_str(text).map_err(|_| format!("{} is not valid YAML", path.display()))?;
    // 解析器可能把 `!!js` 这类标签静默丢掉，所以还要看注释以外的原文。
    let tag_in_text = text.lines().any(|line| {
        let code = match line.find('#') {
            Some(0) => "",
            Some(at) if line[..at].ends_with(char::is_whitespace) => &line[..at],
            _ => line,
        };
        code.contains("!!")
    });
    if tag_in_text || tagged(&value) {
        return Err(format!(
            "{} uses YAML tags (such as !!js); edit it by hand",
            path.display()
        ));
    }
    Ok(value)
}

/// What the profile already stores for one provider; the key is never read.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ProviderInfo {
    pub name: String,
    pub base_url: String,
    pub api: String,
    /// Model id and its stored context length, when the profile gives one.
    pub context_windows: Vec<(String, u64)>,
}

/// Providers in the profile's own patch layer, for prefilling the edit form.
/// Anything unreadable simply yields no entries.
pub fn providers(home: &Path, profile: &str) -> Vec<ProviderInfo> {
    let path = home.join("profiles").join(profile).join("cordis.patch.yml");
    let Some(document) = read(&path)
        .ok()
        .and_then(|text| serde_yaml::from_str::<Value>(&text).ok())
    else {
        return Vec::new();
    };
    let text = |provider: &Value, field: &str| {
        provider
            .get(field)
            .and_then(Value::as_str)
            .filter(|value| value.len() <= 2048)
            .unwrap_or("")
            .to_owned()
    };
    document
        .as_sequence()
        .into_iter()
        .flatten()
        .filter(|entry| entry.get("id").and_then(Value::as_str) == Some(ENTRY_ID))
        .filter_map(|entry| entry.get("config")?.get("providers")?.as_mapping())
        .flatten()
        .filter_map(|(name, provider)| {
            Some(ProviderInfo {
                name: name.as_str().filter(|name| name.len() <= 64)?.to_owned(),
                base_url: text(provider, "baseURL"),
                api: text(provider, "api"),
                context_windows: provider
                    .get("models")
                    .and_then(Value::as_sequence)
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
        .collect()
}

fn write(path: &Path, text: &str) -> Result<(), String> {
    let temporary = path.with_extension(format!("{}.tmp", std::process::id()));
    let written = std::fs::File::create(&temporary)
        .and_then(|mut file| {
            file.write_all(text.as_bytes())
                .and_then(|()| file.sync_all())
        })
        .and_then(|()| std::fs::rename(&temporary, path));
    if written.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    written.map_err(|_| format!("cannot write {}", path.display()))
}

fn key(name: &str) -> Value {
    Value::String(name.into())
}

/// The mapping stored under `name`, created when it is missing or null.
fn child<'a>(parent: &'a mut Mapping, name: &str) -> Result<&'a mut Mapping, String> {
    let slot = parent
        .entry(key(name))
        .or_insert_with(|| Value::Mapping(Mapping::new()));
    if slot.is_null() {
        *slot = Value::Mapping(Mapping::new());
    }
    slot.as_mapping_mut()
        .ok_or_else(|| format!("`{name}` in the dsh profile is not a mapping"))
}

/// Adds or updates `spec` in `home/profiles/<profile>`. The patch file is
/// backed up first; nothing is written when any step before the write fails.
pub fn add_provider(home: &Path, profile: &str, spec: &ProviderSpec) -> Result<Added, String> {
    validate(spec).map_err(str::to_owned)?;
    if !plain(profile, 64)
        || !profile
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        || profile.starts_with('.')
    {
        return Err("the dsh profile name is not a plain directory name".into());
    }
    let patch_path = home.join("profiles").join(profile).join("cordis.patch.yml");
    let original = read(&patch_path)?;
    let mut document = parse(&patch_path, &original)?;
    if document.is_null() {
        document = Value::Sequence(Vec::new());
    }
    let entries = document
        .as_sequence_mut()
        .ok_or_else(|| format!("{} is not a patch list", patch_path.display()))?;
    let position = entries
        .iter()
        .position(|entry| entry.get("id").and_then(Value::as_str) == Some(ENTRY_ID));
    let index = position.unwrap_or_else(|| {
        let mut entry = Mapping::new();
        entry.insert(key("id"), key(ENTRY_ID));
        entry.insert(key("name"), key(ENTRY_NAME));
        entries.push(Value::Mapping(entry));
        entries.len() - 1
    });
    let entry = entries[index]
        .as_mapping_mut()
        .ok_or("the llm-pi-ai patch entry is not a mapping")?;
    let providers = child(child(entry, "config")?, "providers")?;
    let provider = child(providers, &spec.provider)?;

    let credential = provider
        .get("apiKeyEnv")
        .and_then(Value::as_str)
        .filter(|name| plain(name, 128))
        .map_or_else(|| credential_name(&spec.provider), str::to_owned);
    provider.insert(key("apiKeyEnv"), key(&credential));
    provider.insert(key("api"), key(&spec.api));
    provider.insert(key("baseURL"), key(&spec.base_url));
    let name = if spec.model_name.trim().is_empty() {
        spec.model.as_str()
    } else {
        spec.model_name.trim()
    };
    let models = provider
        .entry(key("models"))
        .or_insert_with(|| Value::Sequence(Vec::new()));
    if models.is_null() {
        *models = Value::Sequence(Vec::new());
    }
    let models = models
        .as_sequence_mut()
        .ok_or("`models` in the dsh profile is not a list")?;
    match models
        .iter_mut()
        .find(|model| model.get("id").and_then(Value::as_str) == Some(spec.model.as_str()))
        .and_then(Value::as_mapping_mut)
    {
        // 已有的模型只更新给出的字段，保留 maxTokens、reasoningEfforts 等其他内容。
        Some(existing) => {
            existing.insert(key("name"), key(name));
            if let Some(tokens) = spec.context_window {
                existing.insert(key("contextWindow"), Value::Number(tokens.into()));
            }
        }
        None => {
            let mut model = Mapping::new();
            model.insert(key("id"), key(&spec.model));
            model.insert(key("name"), key(name));
            if let Some(tokens) = spec.context_window {
                model.insert(key("contextWindow"), Value::Number(tokens.into()));
            }
            models.push(Value::Mapping(model));
        }
    }

    // 凭据先于配置准备好：解析失败时两个文件都不改。
    let credentials_path = home.join(".credentials.yaml");
    let credentials = if spec.api_key.0.is_empty() {
        None
    } else {
        let mut store = parse(&credentials_path, &read(&credentials_path)?)?;
        let refs = child(
            store
                .as_mapping_mut()
                .ok_or("the dsh credential store is not a mapping")?,
            "refs",
        )?;
        refs.insert(key(&credential), key(&spec.api_key.0));
        Some(serde_yaml::to_string(&store).map_err(|error| error.to_string())?)
    };

    let header: String = original
        .lines()
        .take_while(|line| line.starts_with('#'))
        .map(|line| format!("{line}\n"))
        .collect();
    let body = serde_yaml::to_string(&document).map_err(|error| error.to_string())?;
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs());
    std::fs::copy(
        &patch_path,
        patch_path.with_extension(format!("yml.bak-{stamp}")),
    )
    .map_err(|_| "cannot back up the dsh profile; nothing was changed".to_owned())?;
    if let Some(text) = &credentials {
        std::fs::copy(
            &credentials_path,
            credentials_path.with_extension("yaml.bak-native-agent-tui"),
        )
        .map_err(|_| "cannot back up the dsh credential store; nothing was changed".to_owned())?;
        write(&credentials_path, text)?;
    }
    write(&patch_path, &format!("{header}{body}"))?;
    Ok(Added {
        label: format!("{}/{}", spec.provider, spec.model),
        credential,
        key_stored: credentials.is_some(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec() -> ProviderSpec {
        ProviderSpec {
            provider: "my-api".into(),
            base_url: "https://example.invalid/v1".into(),
            api: "openai-responses".into(),
            api_key: Secret("PRIVATE_KEY_VALUE".into()),
            model: "model-x".into(),
            model_name: "Model X".into(),
            context_window: None,
        }
    }

    fn home(name: &str, patch: &str, credentials: Option<&str>) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "native-agent-tui-dsh-{}-{name}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("profiles/acp")).unwrap();
        std::fs::write(root.join("profiles/acp/cordis.patch.yml"), patch).unwrap();
        if let Some(credentials) = credentials {
            std::fs::write(root.join(".credentials.yaml"), credentials).unwrap();
        }
        root
    }

    fn load(path: PathBuf) -> Value {
        serde_yaml::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
    }

    #[test]
    fn a_new_provider_joins_existing_ones_and_its_key_goes_to_the_credential_store() {
        let patch = "# header one\n# `!!js` expressions allowed\n- id: llm-pi-ai\n  name: '@deepseek-ai/dsh-llm-pi-ai'\n  config:\n    providers:\n      hi:\n        apiKeyEnv: HI_API_KEY\n        baseURL: https://hi.invalid\n        models:\n          - id: gpt-6-luna\n            name: GPT-6 Luna\n            contextWindow: 272000\n- id: other\n  config:\n    keep: true\n";
        let root = home(
            "new",
            patch,
            Some("version: 1\nrefs:\n  HI_API_KEY: OLD_PRIVATE\nrecords:\n  a:\n    kind: b\n"),
        );
        let added = add_provider(&root, "acp", &spec()).unwrap();
        assert_eq!(
            added,
            Added {
                label: "my-api/model-x".into(),
                credential: "MY_API_API_KEY".into(),
                key_stored: true,
            }
        );
        assert!(!format!("{:?}", spec()).contains("PRIVATE_KEY_VALUE"));

        let text = std::fs::read_to_string(root.join("profiles/acp/cordis.patch.yml")).unwrap();
        assert!(
            text.starts_with("# header one\n# `!!js` expressions allowed\n"),
            "{text}"
        );
        assert!(!text.contains("PRIVATE_KEY_VALUE"), "{text}");
        let document: Value = serde_yaml::from_str(&text).unwrap();
        let providers = &document[0]["config"]["providers"];
        assert_eq!(providers["hi"]["baseURL"], "https://hi.invalid");
        assert_eq!(providers["hi"]["models"][0]["contextWindow"], 272000);
        assert_eq!(providers["my-api"]["apiKeyEnv"], "MY_API_API_KEY");
        assert_eq!(providers["my-api"]["api"], "openai-responses");
        assert_eq!(providers["my-api"]["baseURL"], "https://example.invalid/v1");
        assert_eq!(providers["my-api"]["models"][0]["id"], "model-x");
        assert_eq!(providers["my-api"]["models"][0]["name"], "Model X");
        assert_eq!(document[1]["config"]["keep"], true);
        assert_eq!(
            super::providers(&root, "acp"),
            vec![
                ProviderInfo {
                    name: "hi".into(),
                    base_url: "https://hi.invalid".into(),
                    api: String::new(),
                    context_windows: vec![("gpt-6-luna".into(), 272000)],
                },
                ProviderInfo {
                    name: "my-api".into(),
                    base_url: "https://example.invalid/v1".into(),
                    api: "openai-responses".into(),
                    context_windows: Vec::new(),
                },
            ]
        );
        assert!(super::providers(&root, "missing").is_empty());

        let store = load(root.join(".credentials.yaml"));
        assert_eq!(store["refs"]["HI_API_KEY"], "OLD_PRIVATE");
        assert_eq!(store["refs"]["MY_API_API_KEY"], "PRIVATE_KEY_VALUE");
        assert_eq!(store["records"]["a"]["kind"], "b");
        let backups = std::fs::read_dir(root.join("profiles/acp"))
            .unwrap()
            .filter(|entry| {
                entry
                    .as_ref()
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .contains(".bak-")
            })
            .count();
        assert_eq!(backups, 1);

        // 再加一个模型：同一 provider 下追加，已有模型只改名字，不带 key 时不动凭据。
        let second = ProviderSpec {
            api_key: Secret::default(),
            model: "model-y".into(),
            model_name: String::new(),
            ..spec()
        };
        let added = add_provider(&root, "acp", &second).unwrap();
        assert!(!added.key_stored);
        let renamed = ProviderSpec {
            api_key: Secret::default(),
            model_name: "Renamed".into(),
            ..spec()
        };
        add_provider(&root, "acp", &renamed).unwrap();
        let document = load(root.join("profiles/acp/cordis.patch.yml"));
        let models = document[0]["config"]["providers"]["my-api"]["models"]
            .as_sequence()
            .unwrap();
        assert_eq!(models.len(), 2);
        assert_eq!(models[0]["name"], "Renamed");
        assert_eq!(models[1]["id"], "model-y");
        assert_eq!(models[1]["name"], "model-y");
        assert!(models[0].get("contextWindow").is_none());

        // 上下文长度：给出时写入或改写，不给时保持原值；其他字段不受影响。
        let sized = ProviderSpec {
            api_key: Secret::default(),
            context_window: Some(128000),
            ..spec()
        };
        add_provider(&root, "acp", &sized).unwrap();
        let existing = ProviderSpec {
            provider: "hi".into(),
            base_url: "https://hi.invalid".into(),
            api_key: Secret::default(),
            model: "gpt-6-luna".into(),
            model_name: "GPT-6 Luna".into(),
            context_window: Some(400000),
            ..spec()
        };
        add_provider(&root, "acp", &existing).unwrap();
        add_provider(&root, "acp", &renamed).unwrap();
        let document = load(root.join("profiles/acp/cordis.patch.yml"));
        let providers = &document[0]["config"]["providers"];
        assert_eq!(providers["my-api"]["models"][0]["contextWindow"], 128000);
        assert_eq!(providers["hi"]["models"][0]["contextWindow"], 400000);
        assert_eq!(providers["hi"]["models"][0]["name"], "GPT-6 Luna");
        assert_eq!(
            super::providers(&root, "acp")[0].context_windows,
            vec![("gpt-6-luna".to_owned(), 400000)]
        );
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn an_empty_patch_list_gets_the_entry_and_unsafe_inputs_change_nothing() {
        let root = home("empty", "# note\n[]\n", Some("refs: {}\n"));
        add_provider(&root, "acp", &spec()).unwrap();
        let document = load(root.join("profiles/acp/cordis.patch.yml"));
        assert_eq!(document[0]["id"], "llm-pi-ai");
        assert_eq!(document[0]["name"], "@deepseek-ai/dsh-llm-pi-ai");
        assert_eq!(
            document[0]["config"]["providers"]["my-api"]["models"][0]["id"],
            "model-x"
        );
        std::fs::remove_dir_all(&root).unwrap();

        let tagged = "- id: llm-pi-ai\n  config: !!js 'load()'\n";
        let root = home("tagged", tagged, Some("refs: {}\n"));
        assert!(add_provider(&root, "acp", &spec())
            .unwrap_err()
            .contains("edit it by hand"));
        assert_eq!(
            std::fs::read_to_string(root.join("profiles/acp/cordis.patch.yml")).unwrap(),
            tagged
        );
        // 没有凭据文件时不能保存 key：配置文件也保持原样。
        let root_without_store = home("nostore", "[]\n", None);
        assert!(add_provider(&root_without_store, "acp", &spec()).is_err());
        assert_eq!(
            std::fs::read_to_string(root_without_store.join("profiles/acp/cordis.patch.yml"))
                .unwrap(),
            "[]\n"
        );
        for profile in ["../acp", "", ".hidden", "a/b"] {
            assert!(add_provider(&root, profile, &spec()).is_err(), "{profile}");
        }
        std::fs::remove_dir_all(&root).unwrap();
        std::fs::remove_dir_all(&root_without_store).unwrap();
    }

    #[test]
    fn fields_are_validated_before_anything_is_read() {
        type Change = fn(&mut ProviderSpec);
        let cases: [(&str, Change); 9] = [
            ("Context length", |s| s.context_window = Some(0)),
            ("Context length", |s| {
                s.context_window = Some(MAX_CONTEXT_WINDOW + 1)
            }),
            ("Provider name", |s| s.provider = "My API".into()),
            ("Provider name", |s| s.provider = "-x".into()),
            ("Base URL", |s| s.base_url = "example.invalid".into()),
            ("Base URL", |s| s.base_url = "https://".into()),
            ("API types", |s| s.api = "other".into()),
            ("Model id", |s| s.model = "has space".into()),
            ("API key", |s| s.api_key = Secret("a b".into())),
        ];
        for (expected, change) in cases {
            let mut spec = spec();
            change(&mut spec);
            let error = validate(&spec).unwrap_err();
            assert!(error.contains(expected), "{expected}: {error}");
        }
        assert_eq!(validate(&spec()), Ok(()));
        assert_eq!(credential_name("my-api.2"), "MY_API_2_API_KEY");
    }
}
