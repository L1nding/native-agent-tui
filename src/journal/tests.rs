use super::*;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::observation::{AttentionLevel, Freshness, ObservationFacts, Observer};
use crate::scheduler::{RootTaskSpec, Scheduler};

static NEXT: AtomicU64 = AtomicU64::new(1);

pub(crate) struct Fixture {
    pub(crate) root: PathBuf,
}
impl Fixture {
    pub(crate) fn new() -> Self {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join(format!(
                "journal-test-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
        fs::create_dir_all(&root).unwrap();
        Self { root }
    }
    pub(crate) fn settings(&self) -> JournalSettings {
        JournalSettings {
            root: Some(self.root.clone()),
            ..Default::default()
        }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let target = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .canonicalize()
            .unwrap();
        if let Ok(root) = self.root.canonicalize() {
            assert!(
                root.starts_with(target)
                    && root
                        .file_name()
                        .unwrap()
                        .to_string_lossy()
                        .starts_with("journal-test-")
            );
            fs::remove_dir_all(root).unwrap();
        }
    }
}

fn core_snapshot(session: &str) -> CoreSnapshot {
    let now = tokio::time::Instant::now();
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
                turn_id: "turn".into(),
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
                children: vec![],
                requests: &[],
                gate: None,
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

fn snapshot(session: &str) -> StoredSnapshot {
    StoredSnapshot::capture(&core_snapshot(session))
}

fn completed(session: &str) -> StoredSnapshot {
    let mut snapshot = snapshot(session);
    snapshot.phase = SessionPhase::Stopped;
    snapshot.tasks[0].state = TaskState::Succeeded;
    snapshot.observation.activities[0].execution_state = ExecutionState::Completed;
    snapshot.observation.activities[0].kind = crate::observation::ActivityKind::Completed;
    snapshot.observation.activities[0].freshness = Freshness::Final;
    snapshot.observation.activities[0].attention.level = AttentionLevel::Ended;
    snapshot.close(SessionPhase::Completed, true);
    snapshot
}

#[test]
fn cleanup_and_journal_uncertainty_cannot_be_retained_as_safe_closed_history() {
    let mut snapshot = completed("uncertain-close");
    assert!(!snapshot.needs_recovery());
    snapshot.close(SessionPhase::Completed, false);
    assert_eq!(snapshot.execution_result, Some(SessionPhase::Completed));
    assert!(snapshot.needs_recovery());
    snapshot.cleanup_confirmed = Some(true);
    snapshot.issue = Some(PersistenceIssue::JournalUnavailable);
    assert!(snapshot.needs_recovery());
    snapshot.issue = None;
    snapshot.close(SessionPhase::Running, true);
    assert_eq!(snapshot.execution_result, Some(SessionPhase::Unknown));
    assert_eq!(snapshot.issue, Some(PersistenceIssue::ExecutionUncertain));
    assert!(snapshot.needs_recovery());
}

#[test]
fn workflow_close_keeps_task_terminals_and_reports_unresolved_roots_as_unknown() {
    for (state, expected) in [
        (TaskState::Succeeded, SessionPhase::Completed),
        (TaskState::Failed, SessionPhase::Failed),
        (TaskState::Cancelled, SessionPhase::Failed),
        (TaskState::Blocked, SessionPhase::Failed),
        (TaskState::Unknown, SessionPhase::Unknown),
        (TaskState::Ready, SessionPhase::Unknown),
        (TaskState::Paused, SessionPhase::Unknown),
        (TaskState::Running, SessionPhase::Unknown),
    ] {
        let mut snapshot = completed("workflow-result");
        let mut first = snapshot.tasks[0].clone();
        first.id = TaskId(2);
        first.state = state;
        snapshot.tasks.insert(0, first);
        snapshot.close(SessionPhase::Completed, true);
        assert_eq!(snapshot.execution_result, Some(expected), "{state:?}");
        assert_eq!(snapshot.tasks[0].state, state);
        assert_eq!(snapshot.tasks[1].state, TaskState::Succeeded);
        assert_eq!(snapshot.needs_recovery(), expected == SessionPhase::Unknown);
    }
}

fn store(fixture: &Fixture, state: StoredSnapshot) -> Store {
    let record = Record::snapshot(0, state);
    let mut store = Store::create(
        fixture.settings(),
        Path::new(env!("CARGO_MANIFEST_DIR")),
        &record,
    )
    .unwrap();
    store.persist(&record, &record.encode().unwrap()).unwrap();
    store
}

fn replay(fixture: &Fixture, session: &str, since: u64) -> Result<Replay, JournalError> {
    Replay::open(
        &fixture.settings(),
        Path::new(env!("CARGO_MANIFEST_DIR")),
        session,
        since,
    )
}

fn jsonl(replay: Replay) -> Vec<Record> {
    let mut bytes = Vec::new();
    replay.write_jsonl(&mut bytes).unwrap();
    String::from_utf8(bytes)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

#[test]
fn replay_uses_a_cursor_baseline_and_a_pinned_committed_prefix() {
    let fixture = Fixture::new();
    let mut store = store(&fixture, snapshot("prefix"));
    let mut first = snapshot("prefix");
    first.observation.snapshot_version = 1;
    let first = Record::snapshot(1, first);
    store.persist(&first, &first.encode().unwrap()).unwrap();
    let captured = replay(&fixture, "prefix", 1).unwrap();
    let mut terminal = completed("prefix");
    terminal.observation.snapshot_version = 2;
    let terminal = Record::snapshot(2, terminal);
    store
        .persist(&terminal, &terminal.encode().unwrap())
        .unwrap();
    let before = jsonl(captured);
    assert_eq!(before.len(), 3);
    assert_eq!(before[0].kind, RecordKind::Snapshot);
    assert!(before.iter().all(|record| record.event_seq == 1));
    let Payload::ReplayEnd(end) = &before[2].payload else {
        panic!()
    };
    assert!(!end.live_attached && !end.session_closed && end.needs_recovery);
    let after = jsonl(replay(&fixture, "prefix", 1).unwrap());
    assert_eq!(
        after
            .iter()
            .map(|record| record.event_seq)
            .collect::<Vec<_>>(),
        [1, 2, 2, 2]
    );
    assert_eq!(after[1].kind, RecordKind::State);
    assert_eq!(after[2].kind, RecordKind::Snapshot);
    assert!(after.iter().all(|record| record.historical == Some(true)));
    assert_eq!(
        after[0].state().unwrap().observation.activities[0].freshness,
        Freshness::Unknown
    );
    let Payload::ReplayEnd(end) = &after[3].payload else {
        panic!()
    };
    assert!(end.session_closed && !end.needs_recovery);
    assert_eq!(end.execution_result, Some(SessionPhase::Completed));
}

#[test]
fn missing_terminal_and_uncommitted_or_torn_tail_never_become_completion() {
    let fixture = Fixture::new();
    let mut store = store(&fixture, snapshot("crash"));
    store.log.write_all(b"{\"partial\":").unwrap();
    store.log.sync_all().unwrap();
    let records = jsonl(replay(&fixture, "crash", 0).unwrap());
    let Payload::ReplayEnd(end) = &records.last().unwrap().payload else {
        panic!()
    };
    assert!(end.needs_recovery && end.uncommitted_tail && !end.session_closed);
    assert_eq!(end.execution_result, None);
    let mut cursor = store.cursor.clone();
    cursor.committed_seq = 1;
    cursor.committed_bytes = store.log.metadata().unwrap().len();
    atomic_metadata(&store.root.join(format!("{}.cursor", store.stem)), &cursor).unwrap();
    assert!(matches!(
        replay(&fixture, "crash", 0),
        Err(JournalError::Corrupt)
    ));
}

#[test]
fn failed_commit_keeps_the_last_durable_watermark_and_reports_an_uncommitted_tail() {
    let fixture = Fixture::new();
    let mut store = store(&fixture, snapshot("commit-failure"));
    let record = Record::snapshot(1, snapshot("commit-failure"));
    fs::create_dir(store.root.join(format!("{}.cursor.new", store.stem))).unwrap();
    assert_eq!(
        store.persist(&record, &record.encode().unwrap()),
        Err(JournalError::Io)
    );
    let replay = replay(&fixture, "commit-failure", 0).unwrap();
    assert_eq!(replay.info.committed_seq, 0);
    assert!(replay.uncommitted_tail && replay.info.needs_recovery);
}

#[test]
fn invalid_cursor_identity_workspace_schema_and_order_are_rejected_explicitly() {
    let fixture = Fixture::new();
    let mut store = store(&fixture, snapshot("valid"));
    assert!(matches!(
        replay(&fixture, "../valid", 0),
        Err(JournalError::Identity)
    ));
    assert!(matches!(
        replay(&fixture, "other", 0),
        Err(JournalError::NotFound)
    ));
    assert!(matches!(
        replay(&fixture, "valid", 1),
        Err(JournalError::Cursor {
            requested: 1,
            high: 0
        })
    ));
    let other_workspace = fixture.root.join("workspace");
    fs::create_dir(&other_workspace).unwrap();
    assert!(matches!(
        Replay::open(&fixture.settings(), &other_workspace, "valid", 0),
        Err(JournalError::NotFound)
    ));
    for mutation in 0..4 {
        let mut record = Record::snapshot(0, snapshot("valid"));
        match mutation {
            0 => record.schema_version += 1,
            1 => record.session_id = "wrong".into(),
            2 => record.event_seq = 5,
            _ => record.snapshot_version = 10,
        }
        let bytes = record.encode().unwrap();
        store.log.set_len(0).unwrap();
        store.log.seek(SeekFrom::Start(0)).unwrap();
        store.log.write_all(&bytes).unwrap();
        store.log.sync_all().unwrap();
        let mut info = store.cursor.clone();
        info.committed_bytes = bytes.len() as u64;
        atomic_metadata(&store.root.join(format!("{}.cursor", store.stem)), &info).unwrap();
        assert!(matches!(
            replay(&fixture, "valid", 0),
            Err(JournalError::Schema | JournalError::Corrupt)
        ));
    }
}

#[test]
fn serialization_excludes_task_titles_messages_configuration_and_private_requests() {
    let fixture = Fixture::new();
    let state = snapshot("privacy");
    let core = CoreSnapshot {
        phase: SessionPhase::Failed,
        observation: state.observation.clone(),
        model: Some("PRIVATE_MODEL".into()), cwd: "PRIVATE_PATH".into(), last_error: Some("PRIVATE_TOKEN".into()),
        messages: vec![crate::state::ConversationItem { id: "message".into(), thread_id: "root-thread".into(), turn_id: "turn".into(), role: "Agent".into(), text: "PRIVATE_OUTPUT".into(), complete: true, truncated: false }],
        requests: vec![crate::interactions::RequestView::decode(crate::protocol::RpcId::Number(7), "item/commandExecution/requestApproval", &serde_json::json!({"threadId":"root-thread","turnId":"turn","command":"PRIVATE_COMMAND"})).unwrap()],
        ..Default::default()
    };
    let _store = store(&fixture, StoredSnapshot::capture(&core));
    let mut bytes = Vec::new();
    replay(&fixture, "privacy", 0)
        .unwrap()
        .write_jsonl(&mut bytes)
        .unwrap();
    assert!(!String::from_utf8(bytes).unwrap().contains("PRIVATE_"));
    assert_eq!(
        replay(&fixture, "privacy", 0).unwrap().latest_state().issue,
        Some(PersistenceIssue::ExecutionFailed)
    );
}

#[test]
fn retention_protects_leased_and_uncertain_sessions_and_records_removed_history() {
    let fixture = Fixture::new();
    let old = completed("old");
    let mut record = Record::snapshot(0, old);
    record.recorded_at = Some(0);
    let mut old_store = Store::create(
        fixture.settings(),
        Path::new(env!("CARGO_MANIFEST_DIR")),
        &record,
    )
    .unwrap();
    old_store
        .persist(&record, &record.encode().unwrap())
        .unwrap();
    let mut active = store(&fixture, snapshot("active"));
    assert!(
        replay(&fixture, "old", 0).is_ok(),
        "A live lease must protect even closed records"
    );
    drop(old_store);
    let record = Record::snapshot(1, snapshot("active"));
    active.persist(&record, &record.encode().unwrap()).unwrap();
    assert!(matches!(
        replay(&fixture, "old", 0),
        Err(JournalError::Removed)
    ));
    assert_eq!(
        sessions(&fixture.settings(), Path::new(env!("CARGO_MANIFEST_DIR")))
            .unwrap()
            .len(),
        1
    );
    let mut uncertain = snapshot("uncertain");
    uncertain.close(SessionPhase::Unknown, true);
    let mut record = Record::snapshot(0, uncertain);
    record.recorded_at = Some(0);
    let mut uncertain_store = Store::create(
        fixture.settings(),
        Path::new(env!("CARGO_MANIFEST_DIR")),
        &record,
    )
    .unwrap();
    uncertain_store
        .persist(&record, &record.encode().unwrap())
        .unwrap();
    drop(uncertain_store);
    let record = Record::snapshot(2, snapshot("active"));
    active.persist(&record, &record.encode().unwrap()).unwrap();
    assert!(
        replay(&fixture, "uncertain", 0)
            .unwrap()
            .info
            .needs_recovery
    );
}

#[test]
fn disk_budget_fails_instead_of_removing_active_control_records() {
    let fixture = Fixture::new();
    let active = store(&fixture, snapshot("active"));
    drop(active);
    let mut settings = fixture.settings();
    settings.max_bytes = 8192;
    let record = Record::snapshot(0, snapshot("new"));
    let mut new = Store::create(settings, Path::new(env!("CARGO_MANIFEST_DIR")), &record).unwrap();
    assert_eq!(
        new.persist(&record, &record.encode().unwrap()),
        Err(JournalError::Budget)
    );
    assert!(replay(&fixture, "active", 0).unwrap().info.needs_recovery);
}

#[tokio::test]
async fn writer_failure_is_visible_and_normal_finish_waits_for_durable_close() {
    let fixture = Fixture::new();
    let journal = Journal::open(
        &fixture.settings(),
        Path::new(env!("CARGO_MANIFEST_DIR")),
        snapshot("normal"),
    )
    .unwrap();
    let view = journal.finish(completed("normal")).await.unwrap();
    assert_eq!(view.committed_seq, 1);
    assert!(replay(&fixture, "normal", 0).unwrap().info.session_closed);
    let mut journal = Journal::open(
        &fixture.settings(),
        Path::new(env!("CARGO_MANIFEST_DIR")),
        snapshot("error"),
    )
    .unwrap();
    let stem = format!(
        "{}_error",
        workspace_id(Path::new(env!("CARGO_MANIFEST_DIR"))).unwrap()
    );
    fs::create_dir(fixture.root.join(format!("{stem}.cursor.new"))).unwrap();
    journal.append(snapshot("error")).unwrap();
    journal
        .status
        .wait_for(|status| status.closed)
        .await
        .unwrap();
    assert_eq!(journal.status.borrow().error, Some(JournalError::Io));
    assert_eq!(
        journal.finish(completed("error")).await.unwrap_err(),
        JournalError::Io
    );
    assert_eq!(replay(&fixture, "error", 0).unwrap().info.committed_seq, 0);
}

#[tokio::test]
async fn queue_saturation_is_explicit_and_does_not_block_the_execution_owner() {
    let fixture = Fixture::new();
    let mut journal = Journal::open(
        &fixture.settings(),
        Path::new(env!("CARGO_MANIFEST_DIR")),
        snapshot("queue"),
    )
    .unwrap();
    let lock = catalog_lock(&fixture.root).unwrap();
    lock.lock().unwrap();
    let mut failure = None;
    for _ in 0..QUEUE_RECORDS + 2 {
        if let Err(error) = journal.append(snapshot("queue")) {
            failure = Some(error);
            break;
        }
    }
    assert_eq!(failure, Some(JournalError::Overloaded));
    assert_eq!(journal.status.borrow().committed_seq, 0);
    drop(lock);
    let view = journal.finish(completed("queue")).await.unwrap();
    assert!(view.committed_seq >= QUEUE_RECORDS as u64);
    assert!(replay(&fixture, "queue", 0).unwrap().info.session_closed);
}

#[test]
fn replay_writer_failure_and_large_records_are_explicit() {
    struct Broken;
    impl Write for Broken {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            Err(io::ErrorKind::BrokenPipe.into())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let fixture = Fixture::new();
    let _store = store(&fixture, snapshot("broken-pipe"));
    assert_eq!(
        replay(&fixture, "broken-pipe", 0)
            .unwrap()
            .write_jsonl(&mut Broken),
        Err(JournalError::Output)
    );
    let mut large = snapshot("large");
    large.observation.clock_epoch = "x".repeat(RECORD_BYTES);
    assert_eq!(
        Record::snapshot(0, large).encode(),
        Err(JournalError::RecordLimit)
    );
}

#[test]
fn schema_one_history_stays_readable_and_is_not_mixed_with_schema_two_records() {
    let fixture = Fixture::new();
    let mut store = store(&fixture, snapshot("legacy"));
    let mut record = Record::snapshot(0, snapshot("legacy"));
    record.schema_version = 1;
    let mut legacy_value = serde_json::to_value(&record).unwrap();
    let payload = legacy_value["payload"].as_object_mut().unwrap();
    payload.remove("usage");
    payload.remove("token_budget");
    payload.remove("skills");
    let bytes = [serde_json::to_vec(&legacy_value).unwrap(), vec![b'\n']].concat();
    store.log.set_len(0).unwrap();
    store.log.seek(SeekFrom::Start(0)).unwrap();
    store.log.write_all(&bytes).unwrap();
    store.log.sync_all().unwrap();
    let mut info = store.cursor.clone();
    info.schema_version = 1;
    info.committed_bytes = bytes.len() as u64;
    atomic_metadata(&store.root.join(format!("{}.cursor", store.stem)), &info).unwrap();
    let legacy = replay(&fixture, "legacy", 0).unwrap();
    assert!(legacy.latest_state().last_headless_action.is_none());
    assert!(legacy.latest_state().skills.is_none());
    assert!(jsonl(legacy)
        .iter()
        .all(|record| record.schema_version == 1));
    info.schema_version = SCHEMA_VERSION;
    atomic_metadata(&store.root.join(format!("{}.cursor", store.stem)), &info).unwrap();
    assert!(matches!(
        replay(&fixture, "legacy", 0),
        Err(JournalError::Corrupt)
    ));
}

#[test]
fn confirmed_usage_and_budget_replay_without_starting_execution_or_exposing_private_content() {
    let fixture = Fixture::new();
    let mut core = core_snapshot("usage-budget");
    core.usage = crate::state::UsageSummary {
        input_tokens: Some(120),
        cached_input_tokens: Some(30),
        output_tokens: Some(45),
        reasoning_tokens: Some(12),
        total_tokens: Some(165),
        context_window: Some(32_000),
        source: crate::state::FactSource::ServerConfirmed,
    };
    core.token_budget = crate::state::TokenBudgetSnapshot {
        confirmed_total_tokens: Some(165),
        confirmed_complete: true,
        limit: Some(200),
        stop_triggered: false,
    };
    core.model = Some("PRIVATE_MODEL".into());
    core.last_error = Some("PRIVATE_SECRET".into());
    core.skills = crate::skills::SkillsSnapshot {
        availability: crate::skills::SkillAvailability::Available,
        freshness: crate::skills::SkillFreshness::Current,
        skill_count: 1,
        enabled_count: 1,
        scan_error_count: 0,
        truncated: false,
        entries: vec![crate::skills::SkillEntry {
            name: "PRIVATE_SKILL_NAME".into(),
            path: "PRIVATE_SKILL_PATH".into(),
            scope: "repo".into(),
            enabled: true,
        }],
    };
    core.messages.push(crate::state::ConversationItem {
        id: "message".into(),
        thread_id: "root-thread".into(),
        turn_id: "turn".into(),
        role: "Agent".into(),
        text: "PRIVATE_PROMPT_AND_OUTPUT".into(),
        complete: true,
        truncated: false,
    });

    let captured = StoredSnapshot::capture(&core);
    assert_eq!(
        captured.usage,
        Some(StoredUsageSummary {
            input_tokens: Some(120),
            cached_input_tokens: Some(30),
            output_tokens: Some(45),
            reasoning_tokens: Some(12),
            total_tokens: Some(165),
            context_window: Some(32_000),
            source: StoredUsageSource::ServerConfirmed,
        })
    );
    assert_eq!(
        captured.token_budget,
        Some(StoredTokenBudgetSnapshot {
            confirmed_total_tokens: Some(165),
            confirmed_complete: true,
            limit: Some(200),
            stop_triggered: false,
        })
    );
    let serialized = serde_json::to_string(&captured).unwrap();
    assert!(!serialized.contains("PRIVATE_"));
    assert!(captured.skills.is_some());
    assert!(!serialized.contains("skills/list"));

    let _store = store(&fixture, captured);
    let replay = replay(&fixture, "usage-budget", 0).unwrap();
    assert_eq!(replay.info.committed_seq, 0);
    assert_eq!(replay.latest_state().root_start_requests, 0);
    assert_eq!(
        replay.latest_state().usage,
        Some(StoredUsageSummary {
            input_tokens: Some(120),
            cached_input_tokens: Some(30),
            output_tokens: Some(45),
            reasoning_tokens: Some(12),
            total_tokens: Some(165),
            context_window: Some(32_000),
            source: StoredUsageSource::ServerConfirmed,
        })
    );
    let mut bytes = Vec::new();
    replay.write_jsonl(&mut bytes).unwrap();
    let output = String::from_utf8(bytes).unwrap();
    assert!(output.contains("\"totalTokens\":165"));
    assert!(output.contains("\"confirmedTotalTokens\":165"));
    assert!(!output.contains("PRIVATE_"));
}

#[test]
fn unconfirmed_usage_is_not_persisted() {
    let mut core = core_snapshot("estimated-usage");
    core.usage = crate::state::UsageSummary {
        total_tokens: Some(100),
        source: crate::state::FactSource::LocalEstimate,
        ..Default::default()
    };
    assert!(StoredSnapshot::capture(&core).usage.is_none());
}
