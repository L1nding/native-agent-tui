//! Choices remembered between sessions, such as the model picked with `/model`.
use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::journal::JournalSettings;
use crate::models::MAX_MODEL_BYTES;

const FILE: &str = "preferences.json";
const MAX_BYTES: u64 = 64 * 1024;

/// A choice from `/setting` that has a fixed list of values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Setting {
    Language,
    Theme,
}

impl Setting {
    pub const ALL: [Self; 2] = [Self::Language, Self::Theme];

    pub fn key(self) -> &'static str {
        match self {
            Self::Language => "language",
            Self::Theme => "theme",
        }
    }

    pub fn values(self) -> &'static [&'static str] {
        match self {
            Self::Language => &["zh", "en"],
            Self::Theme => &["dark", "light", "terminal", "mono"],
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Preferences {
    /// Last selected model per backend name (`deepseek-acp`).
    #[serde(default)]
    models: BTreeMap<String, String>,
    /// `Setting::key` to one of `Setting::values`.
    #[serde(default)]
    settings: BTreeMap<String, String>,
    /// Attention class name to `[quiet_ms, attention_ms]`.
    #[serde(default)]
    attention: BTreeMap<String, [u64; 2]>,
}

/// `.zf/preferences.json` in the user's home directory, or inside an explicit
/// `--journal-dir` so an isolated run never touches the user's own preferences.
pub fn path(journal: &JournalSettings) -> Option<PathBuf> {
    if let Some(root) = &journal.root {
        return Some(root.join(FILE));
    }
    let home = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
        .map(PathBuf::from)
        .filter(|home| home.is_absolute())?;
    Some(home.join(".zf").join(FILE))
}

/// Where preferences were kept before they moved to `.zf`: beside the managed
/// journal directory.
fn legacy_path(journal: &JournalSettings) -> Option<PathBuf> {
    if journal.root.is_some() {
        return None;
    }
    Some(crate::journal::legacy_directory()?.parent()?.join(FILE))
}

/// Moves preferences saved at the old location to `path`, once. An existing
/// file at `path` is never replaced; the old file is removed only after the
/// copy succeeded.
pub fn migrate(journal: &JournalSettings, path: &Path) {
    let Some(legacy) = legacy_path(journal) else {
        return;
    };
    move_legacy(&legacy, path);
}

fn move_legacy(legacy: &Path, path: &Path) {
    if path.exists() || !legacy.is_file() {
        return;
    }
    let copied = path
        .parent()
        .map_or(Ok(()), std::fs::create_dir_all)
        .and_then(|()| std::fs::copy(legacy, path));
    if copied.is_ok() {
        let _ = std::fs::remove_file(legacy);
    }
}

impl Preferences {
    /// A missing, oversized or malformed file is the same as no preferences.
    pub fn load(path: &Path) -> Self {
        let mut bytes = Vec::new();
        let read = std::fs::File::open(path)
            .and_then(|file| file.take(MAX_BYTES + 1).read_to_end(&mut bytes));
        if read.is_err() || bytes.len() as u64 > MAX_BYTES {
            return Self::default();
        }
        serde_json::from_slice(&bytes).unwrap_or_default()
    }

    pub fn model(&self, backend: &str) -> Option<&str> {
        self.models
            .get(backend)
            .map(String::as_str)
            .filter(|model| !model.trim().is_empty() && model.len() <= MAX_MODEL_BYTES)
    }

    /// A stored value that is no longer one of the allowed ones is ignored.
    pub fn setting(&self, setting: Setting) -> Option<&'static str> {
        let stored = self.settings.get(setting.key())?;
        setting
            .values()
            .iter()
            .copied()
            .find(|value| value == stored)
    }

    pub fn attention(&self, class: &str) -> Option<(u64, u64)> {
        self.attention
            .get(class)
            .map(|[quiet, attention]| (*quiet, *attention))
    }
}

/// Records `model` for `backend`, keeping every other stored choice.
pub fn save_model(path: &Path, backend: &str, model: &str) -> std::io::Result<()> {
    update(path, |preferences| {
        preferences.models.insert(backend.into(), model.into());
    })
}

pub fn save_setting(path: &Path, setting: Setting, value: &str) -> std::io::Result<()> {
    if !setting.values().contains(&value) {
        return Err(std::io::Error::other("unsupported setting value"));
    }
    update(path, |preferences| {
        preferences
            .settings
            .insert(setting.key().into(), value.into());
    })
}

pub fn save_attention(
    path: &Path,
    class: &str,
    quiet_ms: u64,
    attention_ms: u64,
) -> std::io::Result<()> {
    update(path, |preferences| {
        preferences
            .attention
            .insert(class.into(), [quiet_ms, attention_ms]);
    })
}

fn update(path: &Path, change: impl FnOnce(&mut Preferences)) -> std::io::Result<()> {
    let mut preferences = Preferences::load(path);
    change(&mut preferences);
    let bytes = serde_json::to_vec_pretty(&preferences).map_err(std::io::Error::other)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    // 先写临时文件再改名，读取方不会看到写了一半的内容。
    let temporary = path.with_extension(format!("json.{}.tmp", std::process::id()));
    let written = std::fs::File::create(&temporary)
        .and_then(|mut file| file.write_all(&bytes).and_then(|()| file.sync_all()))
        .and_then(|()| std::fs::rename(&temporary, path));
    if written.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    written
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_saved_model_is_kept_per_backend_and_bad_files_mean_no_preference() {
        let root = std::env::temp_dir().join(format!(
            "native-agent-tui-preferences-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let settings = JournalSettings {
            root: Some(root.clone()),
            ..Default::default()
        };
        let path = path(&settings).unwrap();
        assert_eq!(path, root.join(FILE));
        assert_eq!(Preferences::load(&path), Preferences::default());

        save_model(&path, "deepseek-acp", "hi/gpt-6-luna").unwrap();
        save_model(&path, "another-backend", "gpt-6.1-sol").unwrap();
        save_model(&path, "deepseek-acp", "hi/gpt-6.1-sol").unwrap();
        let loaded = Preferences::load(&path);
        assert_eq!(loaded.model("deepseek-acp"), Some("hi/gpt-6.1-sol"));
        assert_eq!(loaded.model("another-backend"), Some("gpt-6.1-sol"));
        assert_eq!(loaded.model("other"), None);

        save_setting(&path, Setting::Theme, "light").unwrap();
        save_setting(&path, Setting::Language, "en").unwrap();
        assert!(save_setting(&path, Setting::Language, "fr").is_err());
        save_attention(&path, "Model", 1000, 2000).unwrap();
        let loaded = Preferences::load(&path);
        assert_eq!(loaded.setting(Setting::Theme), Some("light"));
        assert_eq!(loaded.setting(Setting::Language), Some("en"));
        assert_eq!(loaded.attention("Model"), Some((1000, 2000)));
        assert_eq!(loaded.attention("Tool"), None);
        // 写设置不能丢掉已保存的模型。
        assert_eq!(loaded.model("another-backend"), Some("gpt-6.1-sol"));
        let leftovers: Vec<_> = std::fs::read_dir(&root)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(leftovers, vec![std::ffi::OsString::from(FILE)]);

        for bad in [
            "not json".to_owned(),
            r#"{"models":{"another-backend":" "}}"#.to_owned(),
            format!(
                r#"{{"models":{{"another-backend":"{}"}}}}"#,
                "x".repeat(MAX_MODEL_BYTES + 1)
            ),
            " ".repeat(MAX_BYTES as usize + 1),
        ] {
            std::fs::write(&path, bad).unwrap();
            assert_eq!(Preferences::load(&path).model("another-backend"), None);
        }
        // 默认位置在用户目录的 .zf 下；显式指定数据目录时没有旧位置可迁移。
        if let Some(default) = super::path(&JournalSettings::default()) {
            assert!(
                default.ends_with(Path::new(".zf").join(FILE)),
                "{default:?}"
            );
        }
        assert_eq!(legacy_path(&settings), None);

        // 旧位置的文件只迁移一次：新位置已有文件时不覆盖，旧文件原样保留。
        let legacy = root.join("old").join(FILE);
        let moved = root.join("new").join(".zf").join(FILE);
        std::fs::create_dir_all(legacy.parent().unwrap()).unwrap();
        save_model(&legacy, "another-backend", "from-old").unwrap();
        move_legacy(&legacy, &moved);
        assert!(!legacy.exists());
        assert_eq!(
            Preferences::load(&moved).model("another-backend"),
            Some("from-old")
        );
        save_model(&legacy, "another-backend", "stale").unwrap();
        move_legacy(&legacy, &moved);
        assert!(legacy.exists());
        assert_eq!(
            Preferences::load(&moved).model("another-backend"),
            Some("from-old")
        );
        move_legacy(
            &root.join("absent").join(FILE),
            &root.join("untouched").join(FILE),
        );
        assert!(!root.join("untouched").exists());
        std::fs::remove_dir_all(&root).unwrap();
    }
}
