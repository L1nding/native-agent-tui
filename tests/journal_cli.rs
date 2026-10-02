use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

use native_agent_tui::journal::{Journal, JournalSettings, StoredSnapshot};
use native_agent_tui::observation::Observer;
use native_agent_tui::state::{CoreSnapshot, SessionPhase};

static NEXT: AtomicU64 = AtomicU64::new(1);

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join(format!(
                "journal-cli-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }
    fn settings(&self) -> JournalSettings {
        JournalSettings {
            root: Some(self.0.clone()),
            ..Default::default()
        }
    }
    fn run(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_native-agent-tui"))
            .current_dir(env!("CARGO_MANIFEST_DIR"))
            .arg("--journal-dir")
            .arg(&self.0)
            .arg("--codex")
            .arg(self.0.join("must-never-execute"))
            .args(args)
            .output()
            .unwrap()
    }
    fn contents(&self) -> BTreeMap<PathBuf, Vec<u8>> {
        fs::read_dir(&self.0)
            .unwrap()
            .map(|e| {
                let path = e.unwrap().path();
                // Windows excludes byte reads through the active lease lock.
                let bytes = if fs::metadata(&path).unwrap().len() == 0 {
                    Vec::new()
                } else {
                    fs::read(&path).unwrap()
                };
                (path, bytes)
            })
            .collect()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let path = self.0.canonicalize().unwrap();
        let target = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .canonicalize()
            .unwrap();
        assert!(
            path.starts_with(target)
                && path
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with("journal-cli-")
        );
        fs::remove_dir_all(path).unwrap();
    }
}

#[tokio::test]
async fn replay_cli_never_executes_or_writes_and_keeps_read_success_separate_from_task_result() {
    let fixture = Fixture::new();
    let cwd = Path::new(env!("CARGO_MANIFEST_DIR"));
    let now = tokio::time::Instant::now();
    let observer = Observer::new_at("cli-session".into(), Default::default(), now, Some(1000));
    let core = CoreSnapshot {
        phase: SessionPhase::Running,
        observation: observer.snapshot_at(0, now),
        ..Default::default()
    };
    let journal = Journal::open(&fixture.settings(), cwd, StoredSnapshot::capture(&core)).unwrap();
    let active_before = fixture.contents();
    let active = fixture.run(&["--replay", "cli-session", "--json-events"]);
    assert!(active.status.success(), "{:?}", active);
    assert_eq!(fixture.contents(), active_before);
    let lines: Vec<serde_json::Value> = String::from_utf8(active.stdout)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(lines.last().unwrap()["payload"]["session_closed"], false);
    assert_eq!(
        lines.last().unwrap()["payload"]["execution_result"],
        serde_json::Value::Null
    );
    assert_eq!(lines.last().unwrap()["payload"]["needs_recovery"], true);
    let mut terminal = StoredSnapshot::capture(&core);
    terminal.observation.snapshot_version = 1;
    terminal.close(SessionPhase::Unknown, true);
    journal.finish(terminal).await.unwrap();
    let before = fixture.contents();
    let output = fixture.run(&["--replay", "cli-session", "--since", "1", "--json-events"]);
    assert!(output.status.success(), "{:?}", output);
    assert!(output.stderr.is_empty());
    let lines: Vec<serde_json::Value> = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(lines.len(), 3);
    assert_eq!(lines[0]["kind"], "snapshot");
    assert_eq!(lines[2]["kind"], "replay_end");
    assert_eq!(lines[2]["payload"]["execution_result"], "unknown");
    assert_eq!(lines[2]["payload"]["live_attached"], false);
    assert!(fixture.run(&["--replay", "cli-session"]).status.success());
    let listing = fixture.run(&["--sessions"]);
    assert!(listing.status.success());
    assert!(String::from_utf8(listing.stdout)
        .unwrap()
        .contains("cli-session"));
    for args in [
        vec!["--replay", "../cli-session"],
        vec!["--replay", "missing"],
        vec!["--replay", "cli-session", "--since", "2"],
        vec!["--run", "PRIVATE_PROMPT", "--json-events"],
    ] {
        let rejected = fixture.run(&args);
        assert_eq!(rejected.status.code(), Some(2));
        assert!(rejected.stdout.is_empty());
    }
    assert_eq!(
        fixture.contents(),
        before,
        "Replay and session listing must leave history untouched"
    );
}
