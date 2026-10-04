use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

use native_agent_tui::journal::{Journal, JournalSettings, StoredSnapshot, StoredTask};
use native_agent_tui::observation::{ChildFact, ObservationFacts, Observer};
use native_agent_tui::outbox::{Outbox, OutboxIntent};
use native_agent_tui::protocol::{ObservedTool, ObservedToolOutcome, ToolCategory};
use native_agent_tui::scheduler::{
    ExternalTurn, RootTaskSpec, Scheduler, TaskId, TaskKind, TaskState,
};
use native_agent_tui::state::{CoreSnapshot, SessionPhase};

static NEXT: AtomicU64 = AtomicU64::new(1);

fn observed_compaction_snapshot(now: tokio::time::Instant, session: &str) -> CoreSnapshot {
    let mut scheduler = Scheduler::default();
    scheduler
        .enqueue(vec![RootTaskSpec::input("PRIVATE_PROMPT".into())])
        .unwrap();
    let attempt = scheduler.dispatch().unwrap().attempt;
    scheduler
        .started_root(
            attempt,
            ExternalTurn {
                thread_id: "root-thread".into(),
                turn_id: "compaction-turn".into(),
                generation: 1,
            },
        )
        .unwrap();
    let mut observer = Observer::new_at(session.into(), Default::default(), now, Some(1000));
    observer
        .reconcile(
            ObservationFacts {
                phase: SessionPhase::Running,
                root_thread: Some("root-thread"),
                root_generation: 1,
                root_task: scheduler.task(attempt.task),
                children: Vec::<ChildFact<'_>>::new(),
                requests: &[],
                gate: None,
            },
            now,
        )
        .unwrap();
    observer
        .tool(
            &ObservedTool {
                thread_id: "root-thread".into(),
                turn_id: "compaction-turn".into(),
                item_id: "compaction-item".into(),
                outcome: Some(ObservedToolOutcome::Completed),
                category: ToolCategory::Compaction,
            },
            now,
        )
        .unwrap();
    CoreSnapshot {
        phase: SessionPhase::Running,
        scheduler: scheduler.snapshot(),
        observation: observer.snapshot_at(0, now),
        ..Default::default()
    }
}

fn snapshot_with_task_states(session: &str, states: &[TaskState]) -> StoredSnapshot {
    let now = tokio::time::Instant::now();
    let observer = Observer::new_at(session.into(), Default::default(), now, Some(1000));
    let core = CoreSnapshot {
        phase: SessionPhase::Running,
        observation: observer.snapshot_at(0, now),
        ..Default::default()
    };
    let mut snapshot = StoredSnapshot::capture(&core);
    snapshot.tasks = states
        .iter()
        .enumerate()
        .map(|(index, state)| StoredTask {
            id: TaskId((index + 1) as u64),
            kind: TaskKind::RootTurn,
            state: *state,
            attempt: 0,
            parent: None,
            dependencies: Vec::new(),
            external: None,
            pause_requested: false,
            cancel_requested: false,
            pending_requests: 0,
        })
        .collect();
    snapshot
}

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
async fn export_cli_previews_and_saves_retained_unknown_state_without_starting_codex() {
    let fixture = Fixture::new();
    let destination = Fixture::new();
    let cwd = Path::new(env!("CARGO_MANIFEST_DIR"));
    let now = tokio::time::Instant::now();
    let observer = Observer::new_at("export-session".into(), Default::default(), now, Some(1000));
    let core = CoreSnapshot {
        phase: SessionPhase::Running,
        observation: observer.snapshot_at(0, now),
        ..Default::default()
    };
    let journal = Journal::open(&fixture.settings(), cwd, StoredSnapshot::capture(&core)).unwrap();
    let mut terminal = StoredSnapshot::capture(&core);
    terminal.observation.snapshot_version = 1;
    terminal.close(SessionPhase::Unknown, true);
    journal.finish(terminal).await.unwrap();
    let before = fixture.contents();
    let preview = fixture.run(&["--export", "export-session"]);
    assert!(preview.status.success());
    assert!(!String::from_utf8(preview.stdout)
        .unwrap()
        .contains("export-session"));
    let path = destination.0.join("中文 export.jsonl");
    let output = fixture.run(&[
        "--export",
        "export-session",
        "--since",
        "1",
        "--output",
        path.to_str().unwrap(),
    ]);
    assert!(output.status.success(), "{output:?}");
    let bytes = fs::read(&path).unwrap();
    let records: Vec<serde_json::Value> = std::str::from_utf8(&bytes)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(records[0]["kind"], "export_manifest");
    assert_eq!(records[0]["high_watermark"], 1);
    assert_eq!(records[0]["execution_result"], "unknown");
    assert_eq!(records.len(), 4);
    assert!(records[1..]
        .iter()
        .all(|record| record["historical"] == true));
    assert_eq!(
        fixture
            .run(&[
                "--export",
                "export-session",
                "--output",
                path.to_str().unwrap()
            ])
            .status
            .code(),
        Some(2)
    );
    assert_eq!(fs::read(&path).unwrap(), bytes);
    assert_eq!(
        fixture.run(&["--history", "export-session"]).status.code(),
        Some(2)
    );
    assert_eq!(fixture.contents(), before);
}

#[tokio::test]
async fn replay_cli_never_executes_or_writes_and_keeps_read_success_separate_from_task_result() {
    let fixture = Fixture::new();
    let cwd = Path::new(env!("CARGO_MANIFEST_DIR"));
    let now = tokio::time::Instant::now();
    let core = observed_compaction_snapshot(now, "cli-session");
    let journal = Journal::open(&fixture.settings(), cwd, StoredSnapshot::capture(&core)).unwrap();
    let second_core = observed_compaction_snapshot(now, "cli-session-second");
    let second_journal = Journal::open(
        &fixture.settings(),
        cwd,
        StoredSnapshot::capture(&second_core),
    )
    .unwrap();
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
    let activity = lines
        .iter()
        .filter_map(|record| record["payload"]["observation"]["activities"].as_array())
        .flatten()
        .find(|activity| activity["tool_category"] == "compaction")
        .unwrap();
    assert_eq!(activity["execution_state"], "completed");
    assert_eq!(activity["identity"]["turn_id"], "compaction-turn");
    assert_eq!(activity["item_id"], "compaction-item");
    assert!(!serde_json::to_string(&lines)
        .unwrap()
        .contains("PRIVATE_PROMPT"));
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
    let text_replay = fixture.run(&["--replay", "cli-session"]);
    assert!(text_replay.status.success());
    let text_replay = String::from_utf8(text_replay.stdout).unwrap();
    assert!(text_replay.contains("Compactions retained: 1"));
    assert!(text_replay.contains("lifetime total unavailable"));
    assert!(text_replay.contains("Some(Compaction)"));
    let listing = fixture.run(&["--sessions"]);
    assert!(listing.status.success());
    let listing_text = String::from_utf8(listing.stdout).unwrap();
    assert!(listing_text.contains("cli-session"));
    assert!(listing_text.contains("cli-session-second"));
    let search = fixture.run(&["--search", "compaction"]);
    assert!(search.status.success(), "{search:?}");
    let search_text = String::from_utf8(search.stdout).unwrap();
    assert!(
        search_text.contains("Search results: 8 hits across 2 sessions"),
        "{search_text}"
    );
    assert!(search_text.contains("session#1 event"));
    assert!(search_text.contains("session#2 event"));
    assert!(!search_text.contains("PRIVATE_PROMPT"));
    assert!(search.stderr.is_empty());
    let filtered = fixture.run(&[
        "--search",
        "compaction",
        "--search-category",
        "compaction",
        "--search-thread",
        "root-thread",
        "--search-turn",
        "compaction-turn",
    ]);
    assert!(filtered.status.success(), "{filtered:?}");
    let filtered_text = String::from_utf8(filtered.stdout).unwrap();
    assert!(filtered_text.contains("Search results: 2 hits across 2 sessions"));
    assert_eq!(filtered_text.matches("session#1 event").count(), 1);
    assert_eq!(filtered_text.matches("session#2 event").count(), 1);
    for args in [
        vec!["--replay", "../cli-session"],
        vec!["--replay", "missing"],
        vec!["--replay", "cli-session", "--since", "2"],
        vec!["--run", "PRIVATE_PROMPT", "--since", "0", "--json-events"],
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
    drop(second_journal);
}

#[tokio::test]
async fn recovery_cli_reports_task_classes_and_never_writes_or_launches_codex() {
    let fixture = Fixture::new();
    let cwd = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut active = snapshot_with_task_states(
        "recovery-active",
        &[
            TaskState::Running,
            TaskState::Unknown,
            TaskState::Queued,
            TaskState::Blocked,
            TaskState::Succeeded,
        ],
    );
    let journal = Journal::open(&fixture.settings(), cwd, active.clone()).unwrap();
    active.observation.snapshot_version = 1;
    journal.finish(active).await.unwrap();
    let outbox_path = fixture
        .settings()
        .outbox_path(cwd, "recovery-active")
        .unwrap();
    let mut outbox = Outbox::open(outbox_path).unwrap();
    outbox
        .record_intent(OutboxIntent::new(
            1,
            "recovery-active",
            None,
            "rpc-1",
            "turn/start",
            b"PRIVATE_PROMPT",
        ))
        .unwrap();
    outbox.mark_unknown(1).unwrap();
    drop(outbox);
    let before = fixture.contents();

    let output = fixture.run(&["--recovery", "recovery-active"]);
    assert!(output.status.success(), "{output:?}");
    assert!(output.stderr.is_empty());
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(text.contains("needs_recovery=true"), "{text}");
    assert!(text.contains("requires_input=true"), "{text}");
    assert!(text.contains("can_resume=false"), "{text}");
    assert!(text.contains("outbox_available=true"), "{text}");
    assert!(
        text.contains("outbox#1 method=turn/start request_id=rpc-1")
            && text.contains("status=Unknown action=InspectExternalOutcome"),
        "{text}"
    );
    assert!(
        text.contains("tasks: active=1 unknown=1 queued=1 blocked=1 terminal=1"),
        "{text}"
    );
    assert!(text.contains("action UnknownAfterRestart"), "{text}");
    assert!(text.contains("action NeedsInput"), "{text}");
    assert!(text.contains("action ResolveBlock"), "{text}");
    assert!(text.contains("action Terminal"), "{text}");
    assert!(!text.contains("PRIVATE_PROMPT"));
    assert_eq!(fixture.contents(), before);
}

#[tokio::test]
async fn recovery_cli_marks_closed_terminal_session_safe_without_input() {
    let fixture = Fixture::new();
    let cwd = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut complete = snapshot_with_task_states("recovery-complete", &[TaskState::Succeeded]);
    complete.phase = SessionPhase::Completed;
    complete.close(SessionPhase::Completed, true);
    let journal = Journal::open(&fixture.settings(), cwd, complete.clone()).unwrap();
    complete.observation.snapshot_version = 1;
    journal.finish(complete).await.unwrap();
    let before = fixture.contents();

    let output = fixture.run(&["--recovery", "recovery-complete"]);
    assert!(output.status.success(), "{output:?}");
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(text.contains("needs_recovery=false"), "{text}");
    assert!(text.contains("requires_input=false"), "{text}");
    assert!(text.contains("can_resume=false"), "{text}");
    assert!(text.contains("tasks: active=0 unknown=0 queued=0 blocked=0 terminal=1"));
    assert_eq!(fixture.contents(), before);
}

#[tokio::test]
async fn recovery_cli_reports_torn_tail_without_reading_or_executing_it() {
    let fixture = Fixture::new();
    let cwd = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut active = snapshot_with_task_states("recovery-tail", &[TaskState::Running]);
    let journal = Journal::open(&fixture.settings(), cwd, active.clone()).unwrap();
    active.observation.snapshot_version = 1;
    journal.finish(active).await.unwrap();
    let log = fs::read_dir(&fixture.0)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| path.extension().and_then(|extension| extension.to_str()) == Some("jsonl"))
        .unwrap();
    let mut file = fs::OpenOptions::new().append(true).open(log).unwrap();
    file.write_all(br#"{"schema_version":2,"kind":"state""#)
        .unwrap();
    drop(file);
    let before = fixture.contents();

    let output = fixture.run(&["--recovery", "recovery-tail"]);
    assert!(output.status.success(), "{output:?}");
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(text.contains("uncommitted_tail=true"), "{text}");
    assert!(text.contains("needs_recovery=true"), "{text}");
    assert_eq!(fixture.contents(), before);
}
