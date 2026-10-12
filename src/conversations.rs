//! Conversations kept so they can be listed by title and resumed later.
//!
//! The layout follows dsh's session store: one directory per workspace, one
//! directory per session, and inside it a versioned, append-only JSONL log.
//!
//! ```text
//! ~/.zf/sessions/--D-project-app--/<session id>/session.v1.jsonl
//! ```
//!
//! This store is separate from the journal: the journal stays a redacted
//! record of execution facts, while this one holds the text the user saw, in
//! the user's own directory, for the single purpose of resuming.
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::journal::JournalSettings;

const DIRECTORY: &str = "sessions";
const FILE: &str = "session.v1.jsonl";
const MAX_FILE_BYTES: u64 = 16 * 1024 * 1024;
/// A log larger than this is rewritten to its current state.
const COMPACT_BYTES: u64 = 4 * 1024 * 1024;
/// Text kept per conversation when it is loaded or compacted; older messages go first.
pub const MAX_TEXT_BYTES: usize = 512 * 1024;
const MAX_LISTED: usize = 200;
const TITLE_CHARS: usize = 80;

/// One line of a session log. Later lines win: a message line with a known
/// `id` replaces that message, and the last `meta` line is the one in effect.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum Record {
    /// Always the first line.
    Session {
        version: u32,
        id: String,
        cwd: String,
        at_ms: u64,
    },
    /// Where the conversation can be resumed.
    Meta {
        backend: String,
        thread_id: String,
        #[serde(default)]
        model: Option<String>,
        at_ms: u64,
    },
    Message {
        id: String,
        role: String,
        text: String,
        at_ms: u64,
    },
}

#[derive(Clone, PartialEq, Eq)]
pub struct SavedMessage {
    /// Unique within the conversation across every session that wrote to it.
    pub id: String,
    pub role: String,
    pub text: String,
}

/// A session log read back to its current state.
#[derive(Clone, PartialEq, Eq)]
pub struct SavedConversation {
    /// Stable name of this conversation; the session that started it.
    pub id: String,
    pub backend: String,
    /// The backend's own session identifier, needed to resume there.
    pub thread_id: String,
    pub model: Option<String>,
    pub cwd: String,
    pub updated_at_ms: u64,
    pub messages: Vec<SavedMessage>,
}

impl std::fmt::Debug for SavedConversation {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // 调试输出不带对话正文。
        formatter
            .debug_struct("SavedConversation")
            .field("id", &self.id)
            .field("backend", &self.backend)
            .field("messages", &self.messages.len())
            .finish_non_exhaustive()
    }
}

/// What the sessions page shows; it carries the title but not the messages.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConversationSummary {
    pub id: String,
    pub backend: String,
    pub title: String,
    pub updated_at_ms: u64,
    /// Messages the user sent.
    pub turns: usize,
}

impl SavedConversation {
    /// First line of the first thing the user asked, shortened.
    pub fn title(&self) -> String {
        let first = self
            .messages
            .iter()
            .find(|message| message.role == "You")
            .map_or("", |message| message.text.trim());
        let line = first.lines().next().unwrap_or("").trim();
        let mut title: String = line.chars().take(TITLE_CHARS).collect();
        if line.chars().count() > TITLE_CHARS {
            title.push('…');
        }
        title
    }

    pub fn summary(&self) -> ConversationSummary {
        ConversationSummary {
            id: self.id.clone(),
            backend: self.backend.clone(),
            title: self.title(),
            updated_at_ms: self.updated_at_ms,
            turns: self
                .messages
                .iter()
                .filter(|message| message.role == "You")
                .count(),
        }
    }

    /// Drops the oldest messages beyond the text limit; the first one names
    /// the conversation and always stays.
    fn bound(&mut self) {
        let mut bytes: usize = self.messages.iter().map(|message| message.text.len()).sum();
        while bytes > MAX_TEXT_BYTES && self.messages.len() > 2 {
            bytes -= self.messages.remove(1).text.len();
        }
    }
}

pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_millis() as u64)
}

/// Directory name for a workspace, as dsh writes it: separators become one
/// dash and the whole name sits between double dashes, so
/// `D:\project\app` is `--D-project-app--`.
pub fn workspace_name(cwd: &str) -> String {
    let mut name = String::from("--");
    let mut dash = true;
    for character in cwd.chars() {
        if matches!(character, '\\' | '/' | ':') || character.is_control() {
            if !dash {
                name.push('-');
                dash = true;
            }
        } else {
            // 其余在文件名里不合法的字符换成下划线。
            name.push(if matches!(character, '<' | '>' | '"' | '|' | '?' | '*') {
                '_'
            } else {
                character
            });
            dash = false;
        }
    }
    if !dash {
        name.push('-');
    }
    name.push('-');
    name
}

/// The workspace's directory: `.zf/sessions/--<cwd>--` in the user's home
/// directory, or under an explicit `--journal-dir` so an isolated run never
/// touches the user's own.
pub fn directory(journal: &JournalSettings, cwd: &str) -> Option<PathBuf> {
    let root = match &journal.root {
        Some(root) => root.join(DIRECTORY),
        None => std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
            .map(PathBuf::from)
            .filter(|home| home.is_absolute())?
            .join(".zf")
            .join(DIRECTORY),
    };
    Some(root.join(workspace_name(cwd)))
}

fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && !id.starts_with('.')
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

fn log_path(directory: &Path, id: &str) -> PathBuf {
    directory.join(id).join(FILE)
}

fn encode(records: &[Record]) -> std::io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    for record in records {
        serde_json::to_writer(&mut bytes, record).map_err(std::io::Error::other)?;
        bytes.push(b'\n');
    }
    Ok(bytes)
}

/// Appends `records` to the session's log, creating it when needed. A log
/// that has grown large is rewritten to its current state afterwards.
pub fn append(directory: &Path, id: &str, records: &[Record]) -> std::io::Result<()> {
    if !valid_id(id) {
        return Err(std::io::Error::other("invalid conversation id"));
    }
    if records.is_empty() {
        return Ok(());
    }
    let path = log_path(directory, id);
    std::fs::create_dir_all(path.parent().expect("a session log has a directory"))?;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)?;
    // 一次写入整批记录，行不会和别的写入交错。
    file.write_all(&encode(records)?)?;
    file.sync_all()?;
    if file.metadata()?.len() > COMPACT_BYTES {
        drop(file);
        compact(directory, id)?;
    }
    Ok(())
}

fn compact(directory: &Path, id: &str) -> std::io::Result<()> {
    let Some(conversation) = load(directory, id) else {
        return Ok(());
    };
    let at_ms = conversation.updated_at_ms;
    let mut records = vec![
        Record::Session {
            version: 1,
            id: conversation.id.clone(),
            cwd: conversation.cwd.clone(),
            at_ms,
        },
        Record::Meta {
            backend: conversation.backend.clone(),
            thread_id: conversation.thread_id.clone(),
            model: conversation.model.clone(),
            at_ms,
        },
    ];
    records.extend(
        conversation
            .messages
            .into_iter()
            .map(|message| Record::Message {
                id: message.id,
                role: message.role,
                text: message.text,
                at_ms,
            }),
    );
    let path = log_path(directory, id);
    let temporary = path.with_extension(format!("{}.tmp", std::process::id()));
    let written = std::fs::File::create(&temporary)
        .and_then(|mut file| {
            file.write_all(&encode(&records)?)
                .and_then(|()| file.sync_all())
        })
        .and_then(|()| std::fs::rename(&temporary, &path));
    if written.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    written
}

/// Reads a session log to its current state. A torn last line, as a crash
/// mid-write leaves, is ignored; a log without its first line is not a session.
pub fn load(directory: &Path, id: &str) -> Option<SavedConversation> {
    if !valid_id(id) {
        return None;
    }
    let file = std::fs::File::open(log_path(directory, id)).ok()?;
    if file.metadata().ok()?.len() > MAX_FILE_BYTES {
        return None;
    }
    let mut lines = BufReader::new(file.take(MAX_FILE_BYTES)).lines();
    let Ok(Record::Session {
        id: stored,
        cwd,
        at_ms,
        ..
    }) = serde_json::from_str(&lines.next()?.ok()?)
    else {
        return None;
    };
    if stored != id {
        return None;
    }
    let mut conversation = SavedConversation {
        id: stored,
        backend: String::new(),
        thread_id: String::new(),
        model: None,
        cwd,
        updated_at_ms: at_ms,
        messages: Vec::new(),
    };
    for line in lines.map_while(Result::ok) {
        let Ok(record) = serde_json::from_str::<Record>(&line) else {
            continue;
        };
        match record {
            Record::Session { .. } => {}
            Record::Meta {
                backend,
                thread_id,
                model,
                at_ms,
            } => {
                conversation.backend = backend;
                conversation.thread_id = thread_id;
                conversation.model = model;
                conversation.updated_at_ms = conversation.updated_at_ms.max(at_ms);
            }
            Record::Message {
                id,
                role,
                text,
                at_ms,
            } => {
                conversation.updated_at_ms = conversation.updated_at_ms.max(at_ms);
                match conversation
                    .messages
                    .iter_mut()
                    .find(|known| known.id == id)
                {
                    Some(known) => {
                        known.role = role;
                        known.text = text;
                    }
                    None => conversation.messages.push(SavedMessage { id, role, text }),
                }
            }
        }
    }
    conversation.bound();
    (!conversation.thread_id.is_empty()).then_some(conversation)
}

/// Conversations of this workspace directory, newest first. Unreadable or
/// empty sessions are skipped.
pub fn list(directory: &Path) -> Vec<ConversationSummary> {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return Vec::new();
    };
    let mut found: Vec<ConversationSummary> = entries
        .filter_map(Result::ok)
        .filter_map(|entry| entry.file_name().into_string().ok())
        .take(4 * MAX_LISTED)
        .filter_map(|id| load(directory, &id))
        .map(|conversation| conversation.summary())
        .filter(|summary| !summary.title.is_empty())
        .collect();
    found.sort_by_key(|summary| std::cmp::Reverse(summary.updated_at_ms));
    found.truncate(MAX_LISTED);
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    fn start(id: &str, cwd: &str, at_ms: u64, first: &str) -> Vec<Record> {
        vec![
            Record::Session {
                version: 1,
                id: id.into(),
                cwd: cwd.into(),
                at_ms,
            },
            Record::Meta {
                backend: "deepseek-acp".into(),
                thread_id: format!("thread-{id}"),
                model: Some("hi/gpt-6-luna".into()),
                at_ms,
            },
            message("s:user-1", "You", first, at_ms),
        ]
    }

    fn message(id: &str, role: &str, text: &str, at_ms: u64) -> Record {
        Record::Message {
            id: id.into(),
            role: role.into(),
            text: text.into(),
            at_ms,
        }
    }

    fn temporary(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "native-agent-tui-sessions-{}-{name}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        root
    }

    #[test]
    fn workspaces_are_named_the_way_dsh_names_them() {
        for (cwd, name) in [
            (
                r"D:\project\native-agent-tui",
                "--D-project-native-agent-tui--",
            ),
            (
                r"C:\Users\Admin\.dsh\ungrouped",
                "--C-Users-Admin-.dsh-ungrouped--",
            ),
            ("/home/me/work", "--home-me-work--"),
            (r"D:\project\app\", "--D-project-app--"),
            (r"D:\a b\c?d", "--D-a b-c_d--"),
        ] {
            assert_eq!(workspace_name(cwd), name, "{cwd}");
        }
        let root = temporary("layout");
        let settings = JournalSettings {
            root: Some(root.clone()),
            ..Default::default()
        };
        assert_eq!(
            directory(&settings, r"D:\project\app"),
            Some(root.join("sessions").join("--D-project-app--"))
        );
        // 默认位置在用户目录的 .zf 下。
        if let Some(default) = directory(&JournalSettings::default(), r"D:\project\app") {
            assert!(
                default.ends_with(Path::new(".zf").join("sessions").join("--D-project-app--")),
                "{default:?}"
            );
        }
    }

    #[test]
    fn a_session_log_is_appended_read_back_and_listed_by_title() {
        let root = temporary("log");
        let work = root.join("--work--");
        assert!(list(&work).is_empty());

        append(
            &work,
            "a-1",
            &start("a-1", "C:\\work", 10, "fix the parser"),
        )
        .unwrap();
        // 回复先写了一半，之后同一个 id 写入完整内容：后写的生效。
        append(
            &work,
            "a-1",
            &[message("s:agent-1", "Agent", "partial", 11)],
        )
        .unwrap();
        append(
            &work,
            "a-1",
            &[
                message("s:agent-1", "Agent", "the full reply", 12),
                message("t:user-1", "You", "second question", 20),
            ],
        )
        .unwrap();
        // 模型或后端会话变了，追加一条新的 meta。
        append(
            &work,
            "a-1",
            &[Record::Meta {
                backend: "deepseek-acp".into(),
                thread_id: "thread-moved".into(),
                model: None,
                at_ms: 21,
            }],
        )
        .unwrap();
        assert!(work.join("a-1").join("session.v1.jsonl").is_file());
        let text = std::fs::read_to_string(work.join("a-1").join("session.v1.jsonl")).unwrap();
        assert_eq!(text.lines().count(), 7);
        assert!(
            text.lines().all(|line| line.starts_with("{\"type\":\"")),
            "{text}"
        );

        let loaded = load(&work, "a-1").unwrap();
        assert_eq!(loaded.thread_id, "thread-moved");
        assert_eq!(loaded.model, None);
        assert_eq!(loaded.updated_at_ms, 21);
        assert_eq!(
            loaded
                .messages
                .iter()
                .map(|m| (m.id.as_str(), m.text.as_str()))
                .collect::<Vec<_>>(),
            [
                ("s:user-1", "fix the parser"),
                ("s:agent-1", "the full reply"),
                ("t:user-1", "second question"),
            ]
        );

        let long_first = format!("  {}\nsecond line", "题".repeat(100));
        append(&work, "b-2", &start("b-2", "C:\\work", 30, &long_first)).unwrap();
        // 另一个工作区的会话在自己的目录里，不会混进来。
        append(
            &root.join("--other--"),
            "c-3",
            &start("c-3", "C:\\other", 40, "elsewhere"),
        )
        .unwrap();
        let listed = list(&work);
        assert_eq!(
            listed.iter().map(|c| c.id.as_str()).collect::<Vec<_>>(),
            ["b-2", "a-1"]
        );
        assert_eq!(listed[0].title, format!("{}…", "题".repeat(80)));
        assert_eq!(listed[1].title, "fix the parser");
        assert_eq!(listed[1].turns, 2);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn damaged_or_unsafe_logs_are_not_sessions_and_large_ones_are_compacted() {
        let root = temporary("damaged");
        let work = root.join("--work--");
        append(
            &work,
            "torn-1",
            &start("torn-1", "C:\\work", 10, "survives a crash"),
        )
        .unwrap();
        // 崩溃留下的半行：之前的内容仍然可读。
        let path = work.join("torn-1").join(FILE);
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        file.write_all(b"{\"type\":\"message\",\"id\":\"s:agent-1\",\"ro")
            .unwrap();
        drop(file);
        let loaded = load(&work, "torn-1").unwrap();
        assert_eq!(loaded.messages.len(), 1);
        assert_eq!(loaded.title(), "survives a crash");

        std::fs::create_dir_all(work.join("headless")).unwrap();
        std::fs::write(
            work.join("headless").join(FILE),
            "{\"type\":\"message\",\"id\":\"x\",\"role\":\"You\",\"text\":\"no header\",\"atMs\":1}\n",
        )
        .unwrap();
        std::fs::create_dir_all(work.join("renamed")).unwrap();
        std::fs::copy(&path, work.join("renamed").join(FILE)).unwrap();
        for id in [
            "headless",
            "renamed",
            "missing",
            "../torn-1",
            "",
            ".hidden",
            "a/b",
        ] {
            assert_eq!(load(&work, id), None, "{id}");
        }
        assert!(append(&work, "../escape", &start("x", "C:\\work", 1, "x")).is_err());
        assert_eq!(list(&work).len(), 1);

        // 追加超过上限后日志被重写为当前状态：保留标题和最新内容，文件变小。
        append(
            &work,
            "big-4",
            &start("big-4", "C:\\work", 50, "keep this title"),
        )
        .unwrap();
        let chunk = "x".repeat(MAX_TEXT_BYTES / 4);
        for index in 0..40 {
            append(
                &work,
                "big-4",
                &[message(
                    &format!("s:agent-{index}"),
                    "Agent",
                    &format!("{index}{chunk}"),
                    60,
                )],
            )
            .unwrap();
        }
        let size = std::fs::metadata(work.join("big-4").join(FILE))
            .unwrap()
            .len();
        // 一共追加了约 5 MiB；重写后只剩当前状态。
        assert!(size < 2 * 1024 * 1024, "{size}");
        let kept = load(&work, "big-4").unwrap();
        assert_eq!(kept.title(), "keep this title");
        assert_eq!(kept.thread_id, "thread-big-4");
        assert!(kept.messages.last().unwrap().text.starts_with("39"));
        assert!(kept.messages.iter().map(|m| m.text.len()).sum::<usize>() <= MAX_TEXT_BYTES);
        std::fs::remove_dir_all(&root).unwrap();
    }
}
