use super::*;
use crate::history::search::{Category, Query};
use crate::journal::{tests::Fixture, Journal, StoredSnapshot};
use crate::observation::{
    Evidence, EvidenceKind, EvidenceSource, Freshness, ObservationFacts, Observer,
};
use crate::protocol::ToolCategory;
use crate::scheduler::{ExternalTurn, RootTaskSpec, Scheduler};
use crate::state::{CoreSnapshot, SessionPhase};

fn state(version: u64) -> StoredSnapshot {
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
                thread_id: "Bearer PRIVATE_KEY".into(),
                turn_id: "C:\\Users\\PRIVATE_PATH".into(),
                generation: 1,
            },
        )
        .unwrap();
    let mut observer = Observer::new_at(
        "PRIVATE-SESSION".into(),
        Default::default(),
        now,
        Some(1000),
    );
    observer
        .reconcile(
            ObservationFacts {
                phase: SessionPhase::Running,
                root_thread: Some("Bearer PRIVATE_KEY"),
                root_generation: 1,
                root_task: scheduler.task(attempt.task),
                children: vec![],
                requests: &[],
                gate: None,
            },
            now,
        )
        .unwrap();
    observer
        .output(
            "Bearer PRIVATE_KEY",
            "C:\\Users\\PRIVATE_PATH",
            "sk-PRIVATE_ITEM",
            16,
            false,
            now,
        )
        .unwrap();
    let mut core = CoreSnapshot {
        phase: SessionPhase::Running,
        scheduler: scheduler.snapshot(),
        observation: observer.snapshot_at(version, now),
        ..Default::default()
    };
    core.observation.activities[0].provider_state = Some("PRIVATE_PROVIDER".into());
    core.last_headless_action = Some(crate::interactions::HeadlessAction::DeclineApproval {
        request_id: crate::protocol::RpcId::String("Cookie=PRIVATE_COOKIE".into()),
        thread_id: "Bearer PRIVATE_KEY".into(),
        turn_id: "C:\\Users\\PRIVATE_PATH".into(),
    });
    StoredSnapshot::capture(&core)
}

fn with_compaction(mut stored: StoredSnapshot) -> StoredSnapshot {
    let mut activity = stored.observation.activities[0].clone();
    let id = stored.observation.accepted_evidence_count.saturating_add(1);
    let evidence = Evidence {
        id,
        kind: EvidenceKind::ToolCompleted,
        source: EvidenceSource::AppServer,
        recorded_at_ms: Some(2000),
        item_id: Some("compact-item".into()),
        request_id: None,
        output_bytes: 0,
    };
    activity.activity_id = "compaction-fixture".into();
    activity.scope = crate::observation::ActivityScope::Tool;
    activity.kind = crate::observation::ActivityKind::Completed;
    activity.execution_state = crate::observation::ExecutionState::Completed;
    activity.item_id = Some("compact-item".into());
    activity.tool_category = Some(ToolCategory::Compaction);
    activity.last_evidence = Some(evidence.clone());
    activity.recent_evidence = vec![evidence];
    activity.progress_seq = 1;
    activity.transition_count = 1;
    stored.observation.accepted_evidence_count = id;
    stored.observation.activities.push(activity);
    stored
}

fn with_unknown_compaction(mut stored: StoredSnapshot) -> StoredSnapshot {
    stored = with_compaction(stored);
    let activity = stored.observation.activities.last_mut().unwrap();
    let started_id = stored.observation.accepted_evidence_count.saturating_add(1);
    let terminal_id = started_id.saturating_add(1);
    let started = Evidence {
        id: started_id,
        kind: EvidenceKind::ToolStarted,
        source: EvidenceSource::AppServer,
        recorded_at_ms: Some(1500),
        item_id: Some("compact-item".into()),
        request_id: None,
        output_bytes: 0,
    };
    let unknown = Evidence {
        id: terminal_id,
        kind: EvidenceKind::ExecutionUnknown,
        source: EvidenceSource::Core,
        recorded_at_ms: Some(2000),
        item_id: Some("compact-item".into()),
        request_id: None,
        output_bytes: 0,
    };
    activity.kind = crate::observation::ActivityKind::Unknown;
    activity.execution_state = crate::observation::ExecutionState::Unknown;
    activity.last_evidence = Some(unknown.clone());
    activity.recent_evidence = vec![started, unknown];
    activity.progress_seq = 2;
    activity.transition_count = 2;
    stored.observation.accepted_evidence_count = terminal_id;
    stored
}

#[tokio::test]
async fn history_preview_pins_the_prefix_and_export_aliases_every_free_identity() {
    let fixture = Fixture::new();
    let settings = fixture.settings();
    let cwd = Path::new(env!("CARGO_MANIFEST_DIR"));
    let journal = Journal::open(&settings, cwd, state(1)).unwrap();
    let cancel = AtomicBool::new(false);
    let export = PreparedExport::open(&settings, cwd, "PRIVATE-SESSION", 0, &cancel).unwrap();
    let preview = export.preview.clone();
    let mut final_state = state(2);
    final_state.close(SessionPhase::Unknown, true);
    journal.finish(final_state).await.unwrap();
    let destination = fixture.root.with_extension("export.jsonl");
    let bytes = export.write_file(&destination, &cancel).unwrap();
    let text = fs::read_to_string(&destination).unwrap();
    fs::remove_file(&destination).unwrap();
    assert_eq!(bytes, text.len() as u64);
    assert!(!text.contains("PRIVATE_") && !text.contains("Bearer") && !text.contains("Cookie="));
    assert!(!preview.excerpt.contains("PRIVATE_"));
    let values: Vec<Value> = text
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(values[0], preview.manifest);
    assert_eq!(values[0]["high_watermark"], 0);
    assert_eq!(values.last().unwrap()["payload"]["session_closed"], false);
    assert_eq!(
        values.last().unwrap()["payload"]["execution_result"],
        Value::Null
    );
    let record = &values[1];
    let activity = &record["payload"]["observation"]["activities"][0];
    assert_eq!(record["session_id"], activity["session_id"]);
    assert_eq!(
        activity["identity"]["thread_id"],
        record["payload"]["tasks"][0]["external"]["thread_id"]
    );
    assert_eq!(
        activity["identity"]["thread_id"],
        record["payload"]["last_headless_action"]["thread_id"]
    );
    assert_eq!(activity["provider_state"], Value::Null);
    assert_eq!(
        activity["progress_seq"],
        state(1).observation.activities[0].progress_seq
    );
    assert_ne!(activity["freshness"], "current");
    assert!(preview
        .excerpt
        .starts_with(&serde_json::to_string(record).unwrap()));
}

#[tokio::test]
async fn export_preview_is_bounded_while_the_saved_artifact_retains_the_entire_selected_range() {
    let fixture = Fixture::new();
    let settings = fixture.settings();
    let cwd = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut journal = Journal::open(&settings, cwd, state(1)).unwrap();
    for version in 2..=5 {
        journal.append(state(version)).unwrap();
    }
    let mut terminal = state(6);
    terminal.close(SessionPhase::Unknown, true);
    journal.finish(terminal).await.unwrap();
    let cancel = AtomicBool::new(false);
    let export = PreparedExport::open(&settings, cwd, "PRIVATE-SESSION", 0, &cancel).unwrap();
    assert_eq!(export.preview.excerpt.len(), PREVIEW_BYTES);
    assert_eq!(export.preview.manifest["high_watermark"], 5);
    let destination = fixture.root.with_extension("full-export.jsonl");
    export.write_file(&destination, &cancel).unwrap();
    let text = fs::read_to_string(&destination).unwrap();
    fs::remove_file(&destination).unwrap();
    assert!(text.len() > PREVIEW_BYTES);
    let records: Vec<Value> = text
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(records.len(), 9);
    assert_eq!(records[7]["event_seq"], 5);
    assert_eq!(records[8]["kind"], "replay_end");
    assert_eq!(records[8]["payload"]["execution_result"], "unknown");
}

#[tokio::test]
async fn export_cannot_replace_files_or_write_inside_the_managed_store() {
    let fixture = Fixture::new();
    let mut settings = fixture.settings();
    settings.root = Some(fixture.root.join("journal"));
    let cwd = Path::new(env!("CARGO_MANIFEST_DIR"));
    let journal = Journal::open(&settings, cwd, state(1)).unwrap();
    let mut terminal = state(2);
    terminal.close(SessionPhase::Unknown, true);
    journal.finish(terminal).await.unwrap();
    let cancel = AtomicBool::new(false);
    let source = fs::read_dir(settings.directory().unwrap())
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| {
            path.extension()
                .is_some_and(|extension| extension == "jsonl")
        })
        .unwrap();
    let before = fs::read(&source).unwrap();
    let export = PreparedExport::open(&settings, cwd, "PRIVATE-SESSION", 0, &cancel).unwrap();
    assert_eq!(
        export.write_file(&source, &cancel),
        Err(HistoryError::Exists)
    );
    assert_eq!(fs::read(&source).unwrap(), before);
    let export = PreparedExport::open(&settings, cwd, "PRIVATE-SESSION", 1, &cancel).unwrap();
    assert_eq!(
        export.write_file(
            &settings.directory().unwrap().join("new-export.jsonl"),
            &cancel
        ),
        Err(HistoryError::ManagedDirectory)
    );
    let export = PreparedExport::open(&settings, cwd, "PRIVATE-SESSION", 1, &cancel).unwrap();
    cancel.store(true, Ordering::Release);
    let destination = fixture.root.join("cancelled.jsonl");
    assert_eq!(
        export.write_file(&destination, &cancel),
        Err(HistoryError::Cancelled)
    );
    assert!(!destination.exists());
    assert!(!fs::read_dir(destination.parent().unwrap())
        .unwrap()
        .any(|entry| entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(&format!(".native-agent-export-{}-", std::process::id()))));
}

#[tokio::test]
async fn history_service_reads_frozen_evidence_and_requires_an_explicit_export_preview() {
    let fixture = Fixture::new();
    let settings = fixture.settings();
    let cwd = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let journal = Journal::open(&settings, &cwd, state(1)).unwrap();
    let mut terminal = state(2);
    terminal.close(SessionPhase::Unknown, true);
    journal.finish(terminal).await.unwrap();
    let mut service = HistoryHandle::start(settings, cwd).unwrap();
    let id = service
        .request(HistoryRequest::Open {
            session: "PRIVATE-SESSION".into(),
            sequence: Some(0),
        })
        .unwrap();
    let HistoryResult::Loaded(view) = service.response(id).await.unwrap() else {
        panic!()
    };
    assert_eq!(view.selected.event_seq, 0);
    assert!(view.selected.historical.unwrap());
    assert!(view
        .selected
        .state()
        .unwrap()
        .observation
        .activities
        .iter()
        .all(|activity| activity.freshness != Freshness::Current));
    let original = view.selected.clone();
    tokio::time::sleep(Duration::from_millis(25)).await;
    assert_eq!(
        view.selected, original,
        "Historical ages cannot be recomputed in this process"
    );
    let id = service
        .request(HistoryRequest::Export {
            destination: fixture.root.with_extension("no-preview.jsonl"),
        })
        .unwrap();
    assert!(matches!(
        service.response(id).await,
        Err(HistoryError::PreviewRequired)
    ));
    service.shutdown().await.unwrap();
}

#[tokio::test]
async fn history_search_returns_locatable_redacted_metadata() {
    let fixture = Fixture::new();
    let settings = fixture.settings();
    let cwd = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let journal = Journal::open(&settings, &cwd, state(1)).unwrap();
    let mut terminal = state(2);
    terminal.close(SessionPhase::Unknown, true);
    journal.finish(terminal).await.unwrap();

    let mut service = HistoryHandle::start(settings, cwd).unwrap();
    let search_id = service
        .search
        .submit(
            "PRIVATE-SESSION".into(),
            Query {
                text: "Bearer".into(),
                category: Category::All,
                ..Default::default()
            },
        )
        .unwrap();
    let results = service.search.response(search_id).await.unwrap();
    assert!(results.total > 0);
    assert!(results.hits.iter().all(|hit| {
        let metadata = hit.metadata();
        !metadata.contains("Bearer")
            && !metadata.contains("PRIVATE")
            && !metadata.contains("C:\\Users")
            && !metadata.contains("Cookie=")
    }));

    let hit = results.hits.first().unwrap();
    let request_id = service
        .request(HistoryRequest::Open {
            session: hit.session_id.clone(),
            sequence: Some(hit.event_seq),
        })
        .unwrap();
    let HistoryResult::Loaded(view) = service.response(request_id).await.unwrap() else {
        panic!()
    };
    assert_eq!(view.selected.event_seq, hit.event_seq);
    service.shutdown().await.unwrap();
}

#[tokio::test]
async fn history_compaction_category_finds_only_locatable_confirmed_event_metadata() {
    let fixture = Fixture::new();
    let settings = fixture.settings();
    let cwd = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let journal = Journal::open(&settings, &cwd, with_compaction(state(1))).unwrap();
    let mut terminal = with_compaction(state(2));
    terminal.close(SessionPhase::Unknown, true);
    journal.finish(terminal).await.unwrap();

    let mut service = HistoryHandle::start(settings, cwd).unwrap();
    let search_id = service
        .search
        .submit(
            "PRIVATE-SESSION".into(),
            Query {
                text: "Compaction".into(),
                category: Category::Compaction,
                ..Default::default()
            },
        )
        .unwrap();
    let results = service.search.response(search_id).await.unwrap();
    assert_eq!(results.total, 1);
    let hit = &results.hits[0];
    assert_eq!(hit.tool_category, Some(ToolCategory::Compaction));
    assert!(hit.metadata().contains("Compaction"));
    assert!(!hit.metadata().contains("PRIVATE") && !hit.metadata().contains("Bearer"));

    let request_id = service
        .request(HistoryRequest::Open {
            session: hit.session_id.clone(),
            sequence: Some(hit.event_seq),
        })
        .unwrap();
    let HistoryResult::Loaded(view) = service.response(request_id).await.unwrap() else {
        panic!()
    };
    assert!(hit.matches(&view));
    service.shutdown().await.unwrap();
}

#[tokio::test]
async fn history_compaction_category_finds_locatable_unknown_core_evidence() {
    let fixture = Fixture::new();
    let settings = fixture.settings();
    let cwd = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let journal = Journal::open(&settings, &cwd, with_unknown_compaction(state(1))).unwrap();
    let mut terminal = with_unknown_compaction(state(2));
    terminal.close(SessionPhase::Unknown, true);
    journal.finish(terminal).await.unwrap();

    let mut service = HistoryHandle::start(settings, cwd).unwrap();
    let search_id = service
        .search
        .submit(
            "PRIVATE-SESSION".into(),
            Query {
                text: "ExecutionUnknown".into(),
                category: Category::Compaction,
                ..Default::default()
            },
        )
        .unwrap();
    let results = service.search.response(search_id).await.unwrap();
    assert_eq!(results.total, 1);
    let hit = &results.hits[0];
    assert_eq!(hit.tool_category, Some(ToolCategory::Compaction));
    assert_eq!(hit.evidence.kind, EvidenceKind::ExecutionUnknown);
    assert_eq!(hit.evidence.source, EvidenceSource::Core);
    let metadata = hit.metadata();
    assert!(metadata.contains("ExecutionUnknown") && metadata.contains("Core"));
    assert!(!metadata.contains("PRIVATE") && !metadata.contains("Bearer"));

    let request_id = service
        .request(HistoryRequest::Open {
            session: hit.session_id.clone(),
            sequence: Some(hit.event_seq),
        })
        .unwrap();
    let HistoryResult::Loaded(view) = service.response(request_id).await.unwrap() else {
        panic!()
    };
    assert!(hit.matches(&view));
    service.shutdown().await.unwrap();
}

#[tokio::test]
async fn cancelled_history_search_does_not_publish_the_old_result() {
    let fixture = Fixture::new();
    let settings = fixture.settings();
    let cwd = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let journal = Journal::open(&settings, &cwd, state(1)).unwrap();
    let mut terminal = state(2);
    terminal.close(SessionPhase::Unknown, true);
    journal.finish(terminal).await.unwrap();

    let mut service = HistoryHandle::start(settings, cwd).unwrap();
    let search_id = service
        .search
        .submit("PRIVATE-SESSION".into(), Query::default())
        .unwrap();
    service.search.cancel();
    assert!(matches!(
        service.search.response(search_id).await,
        Err(HistoryError::Cancelled)
    ));
    assert!(service.search.status.borrow().is_none());
    service.shutdown().await.unwrap();
}

#[tokio::test]
async fn history_search_can_scan_multiple_retained_sessions_and_preserve_the_hit_session() {
    let fixture = Fixture::new();
    let settings = fixture.settings();
    let cwd = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let first = Journal::open(&settings, &cwd, state(1)).unwrap();
    let mut second_state = state(1);
    second_state.observation.session_id = "SECOND-SESSION".into();
    let second = Journal::open(&settings, &cwd, second_state).unwrap();
    let mut first_terminal = state(2);
    first_terminal.close(SessionPhase::Unknown, true);
    first.finish(first_terminal).await.unwrap();
    let mut second_terminal = state(2);
    second_terminal.observation.session_id = "SECOND-SESSION".into();
    second_terminal.close(SessionPhase::Unknown, true);
    second.finish(second_terminal).await.unwrap();

    let mut service = HistoryHandle::start(settings, cwd).unwrap();
    let search_id = service
        .search
        .submit_sessions(
            vec!["PRIVATE-SESSION".into(), "SECOND-SESSION".into()],
            Query {
                text: "Bearer".into(),
                category: Category::All,
                ..Default::default()
            },
        )
        .unwrap();
    let results = service.search.response(search_id).await.unwrap();
    assert_eq!(results.sessions.len(), 2);
    assert!(results
        .hits
        .iter()
        .any(|hit| hit.session_id == "PRIVATE-SESSION"));
    assert!(results
        .hits
        .iter()
        .any(|hit| hit.session_id == "SECOND-SESSION"));
    service.shutdown().await.unwrap();
}

#[test]
fn export_cannot_publish_cached_completion_after_the_source_prefix_is_truncated() {
    let fixture = Fixture::new();
    let mut settings = fixture.settings();
    settings.root = Some(fixture.root.join("journal"));
    let cwd = Path::new(env!("CARGO_MANIFEST_DIR"));
    let cancel = AtomicBool::new(false);
    let runtime = tokio::runtime::Runtime::new().unwrap();
    runtime.block_on(async {
        let journal = Journal::open(&settings, cwd, state(1)).unwrap();
        let mut terminal = state(2);
        terminal.close(SessionPhase::Unknown, true);
        journal.finish(terminal).await.unwrap();
    });
    let export = PreparedExport::open(&settings, cwd, "PRIVATE-SESSION", 0, &cancel).unwrap();
    let log = fs::read_dir(settings.directory().unwrap())
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| {
            path.extension()
                .is_some_and(|extension| extension == "jsonl")
        })
        .unwrap();
    OpenOptions::new()
        .write(true)
        .open(log)
        .unwrap()
        .set_len(0)
        .unwrap();
    let destination = fixture.root.join("truncated.jsonl");
    assert_eq!(
        export.write_file(&destination, &cancel),
        Err(HistoryError::Journal(JournalError::Corrupt))
    );
    assert!(!destination.exists());
    assert_eq!(fs::read_dir(&fixture.root).unwrap().count(), 1);
}

#[test]
fn partial_export_write_failure_cleans_the_temporary_file_without_publishing_a_destination() {
    struct FailsAfter<'a> {
        file: &'a mut File,
        remaining: usize,
    }
    impl Write for FailsAfter<'_> {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            if self.remaining == 0 {
                return Err(io::ErrorKind::StorageFull.into());
            }
            let count = self.file.write(&bytes[..bytes.len().min(self.remaining)])?;
            self.remaining -= count;
            Ok(count)
        }
        fn flush(&mut self) -> io::Result<()> {
            self.file.flush()
        }
    }
    let fixture = Fixture::new();
    let mut settings = fixture.settings();
    settings.root = Some(fixture.root.join("journal"));
    let cwd = Path::new(env!("CARGO_MANIFEST_DIR"));
    let cancel = AtomicBool::new(false);
    let runtime = tokio::runtime::Runtime::new().unwrap();
    runtime.block_on(async {
        let journal = Journal::open(&settings, cwd, state(1)).unwrap();
        let mut terminal = state(2);
        terminal.close(SessionPhase::Unknown, true);
        journal.finish(terminal).await.unwrap();
    });
    let export = PreparedExport::open(&settings, cwd, "PRIVATE-SESSION", 0, &cancel).unwrap();
    let destination = fixture.root.join("failure.jsonl");
    let result = export.save(&destination, &cancel, |export, file| {
        export.write(
            &mut FailsAfter {
                file,
                remaining: 700,
            },
            &cancel,
        )
    });
    assert_eq!(result, Err(HistoryError::ExportIo));
    assert!(!destination.exists());
    assert_eq!(
        fs::read_dir(&fixture.root).unwrap().count(),
        1,
        "Only the original journal directory may remain"
    );
}

#[test]
fn export_identity_budget_fails_explicitly_and_keeps_numeric_request_ids_typed() {
    let mut redactor = Redactor::default();
    let mut value =
        serde_json::json!({"request_id":7,"thread_id":"same","turn_id":"same","phase":"completed"});
    redactor.value("", &mut value).unwrap();
    assert_eq!(value["request_id"], 7);
    assert_eq!(value["thread_id"], value["turn_id"]);
    assert_eq!(value["phase"], "completed");
    redactor.bytes = ID_BYTES;
    let mut value = Value::String("new credential".into());
    assert_eq!(
        redactor.value("item_id", &mut value),
        Err(HistoryError::IdentityLimit)
    );
}
