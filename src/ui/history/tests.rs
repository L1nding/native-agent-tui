use super::*;
use crate::journal::{Payload, RecordKind, StoredSnapshot};
use ratatui::backend::TestBackend;
use ratatui::Terminal;
use std::path::PathBuf;

fn panel() -> HistoryPanel {
    let state = StoredSnapshot::capture(&CoreSnapshot::default());
    let selected = crate::journal::Record {
        schema_version: 2,
        kind: RecordKind::State,
        session_id: "history-session".into(),
        attempt_id: None,
        event_seq: 1,
        snapshot_version: 0,
        recorded_at: None,
        payload: Payload::Snapshot(Box::new(state)),
        historical: Some(true),
    };
    HistoryPanel {
        visible: true,
        view: Some(Box::new(HistoricalView {
            info: SessionInfo {
                schema_version: 2,
                workspace_id: "workspace".into(),
                session_id: "history-session".into(),
                committed_seq: 1,
                committed_bytes: 1,
                snapshot_version: 0,
                recorded_at: None,
                session_closed: false,
                needs_recovery: true,
                execution_result: None,
            },
            selected,
            uncommitted_tail: true,
        })),
        ..Default::default()
    }
}

#[tokio::test]
async fn historical_controls_do_not_create_execution_effects_or_reuse_the_task_editor() {
    let mut service = HistoryHandle::start(Default::default(), PathBuf::from(".")).unwrap();
    let mut panel = panel();
    for key in [
        KeyEvent::new(KeyCode::Char('y'), KeyModifiers::CONTROL),
        KeyEvent::new(KeyCode::F(8), KeyModifiers::NONE),
        KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
    ] {
        assert!(!panel.key(key, &mut service, false));
        assert!(panel.pending.is_none());
    }
    panel.key(
        KeyEvent::new(KeyCode::Char('e'), KeyModifiers::NONE),
        &mut service,
        false,
    );
    panel.paste("123PRIVATE_SECRET");
    assert!(matches!(&panel.form, Some(Form::Range(editor)) if editor.text == "0123"));
    assert!(panel.preview.is_none());
    panel.key(
        KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
        &mut service,
        false,
    );
    assert!(panel.form.is_none() && panel.view.is_some());
    service.shutdown().await.unwrap();
}

#[test]
fn history_and_export_preview_render_unknown_results_in_compact_and_wide_terminals() {
    for (width, height) in [
        (30, 10),
        (60, 20),
        (80, 24),
        (100, 30),
        (120, 40),
        (160, 50),
    ] {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        let mut panel = panel();
        terminal.draw(|frame| panel.draw(frame, None)).unwrap();
        let screen = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(screen.contains("read-only"));
        assert!(!screen.contains("Completed"));
        panel.preview = Some(ExportPreview {
            manifest: serde_json::json!({"since":0,"high_watermark":1,"execution_result":"unknown"}),
            excerpt: "中文 🦀 stable aliases".into(),
        });
        let mut editor = Editor::default();
        editor.insert("target/中文导出.jsonl");
        panel.form = Some(Form::Destination(editor));
        terminal
            .draw(|frame| panel.draw(frame, Some(&CoreSnapshot::default())))
            .unwrap();
    }
}

#[test]
fn historical_details_render_persisted_usage_and_budget_without_private_content() {
    let mut panel = panel();
    let mut state = StoredSnapshot::capture(&CoreSnapshot::default());
    state.usage = Some(crate::journal::StoredUsageSummary {
        input_tokens: Some(12),
        cached_input_tokens: Some(3),
        output_tokens: Some(8),
        reasoning_tokens: Some(2),
        total_tokens: Some(20),
        context_window: Some(1024),
        source: crate::journal::StoredUsageSource::ServerConfirmed,
    });
    state.token_budget = Some(crate::journal::StoredTokenBudgetSnapshot {
        confirmed_total_tokens: Some(20),
        confirmed_complete: true,
        limit: Some(30),
        stop_triggered: false,
        per_agent_limit: Some(15),
        per_agent_stop_triggered: true,
    });
    state.observation.session_id = "history-session".into();
    let evidence = crate::observation::Evidence {
        id: 1,
        kind: crate::observation::EvidenceKind::ToolCompleted,
        source: crate::observation::EvidenceSource::AppServer,
        recorded_at_ms: Some(1000),
        item_id: Some("compact-item".into()),
        request_id: None,
        output_bytes: 0,
    };
    state.observation.accepted_evidence_count = 1;
    state.observation.activities = vec![crate::observation::ActivitySnapshot {
        session_id: "history-session".into(),
        clock_epoch: "history-session:clock-1".into(),
        activity_id: "compaction".into(),
        identity: crate::observation::ActivityIdentity {
            agent_id: "root".into(),
            task_id: None,
            attempt_id: Some(1),
            thread_id: Some("root-thread".into()),
            turn_id: Some("root-turn".into()),
            generation: Some(1),
        },
        scope: crate::observation::ActivityScope::Tool,
        kind: crate::observation::ActivityKind::Completed,
        execution_state: crate::observation::ExecutionState::Completed,
        item_id: Some("compact-item".into()),
        request_id: None,
        interaction_state: None,
        tool_category: Some(crate::protocol::ToolCategory::Compaction),
        started_at_ms: Some(900),
        last_evidence_at_ms: Some(1000),
        elapsed_ms: Some(100),
        silence_ms: None,
        freshness: crate::observation::Freshness::Final,
        last_evidence: Some(evidence.clone()),
        recent_evidence: vec![evidence],
        progress_seq: 1,
        output_bytes: 0,
        transition_count: 1,
        child_terminal_count: 0,
        wait_reason: None,
        resume_condition: None,
        wait_targets: vec![],
        attention: crate::observation::Attention {
            level: crate::observation::AttentionLevel::Ended,
            reason: crate::observation::AttentionReason::ExecutionEnded,
            requires_action: false,
            quiet_after_ms: None,
            attention_after_ms: None,
            config_source: None,
        },
        provider_state: None,
    }];
    panel.view.as_mut().unwrap().selected.payload = Payload::Snapshot(Box::new(state));

    let mut terminal = Terminal::new(TestBackend::new(160, 50)).unwrap();
    terminal.draw(|frame| panel.draw(frame, None)).unwrap();
    let screen = terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();
    assert!(screen.contains("Recorded usage"), "{screen}");
    assert!(screen.contains("total 20"), "{screen}");
    assert!(screen.contains("Recorded token budget"), "{screen}");
    assert!(screen.contains("20 / 30"), "{screen}");
    assert!(
        screen.contains("per-agent: limit 15, stop triggered: true"),
        "{screen}"
    );
    assert!(screen.contains("Compactions retained: 1"), "{screen}");
    assert!(screen.contains("source Some(AppServer)"), "{screen}");
    assert!(
        screen.contains("before/after usage, reason and summary unavailable"),
        "{screen}"
    );
    assert!(!screen.contains("PRIVATE"), "{screen}");
}

#[tokio::test]
async fn history_search_editor_accepts_paste_and_renders_its_fixed_scope() {
    let mut service = HistoryHandle::start(Default::default(), PathBuf::from(".")).unwrap();
    let mut panel = panel();
    panel.search_editing = true;
    panel.paste("thread-中文\n");
    assert_eq!(panel.search_query, "thread-中文");
    let mut terminal = Terminal::new(TestBackend::new(140, 60)).unwrap();
    terminal.draw(|frame| panel.draw(frame, None)).unwrap();
    let screen = terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();
    assert!(screen.contains("Search metadata"));
    assert!(screen.contains("fixed committed journal prefix"));
    service.shutdown().await.unwrap();
}

#[tokio::test]
async fn history_search_editor_moves_and_deletes_unicode_graphemes() {
    let mut service = HistoryHandle::start(Default::default(), PathBuf::from(".")).unwrap();
    let mut panel = panel();
    panel.search_editing = true;
    panel.paste("甲🙂乙");
    assert_eq!(panel.search_query, "甲🙂乙");
    panel.key(
        KeyEvent::new(KeyCode::Left, KeyModifiers::NONE),
        &mut service,
        false,
    );
    panel.key(
        KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE),
        &mut service,
        false,
    );
    assert_eq!(panel.search_query, "甲乙");
    panel.key(
        KeyEvent::new(KeyCode::Char('界'), KeyModifiers::NONE),
        &mut service,
        false,
    );
    assert_eq!(panel.search_query, "甲界乙");
    service.shutdown().await.unwrap();
}

#[tokio::test]
async fn history_search_can_start_from_the_session_list_without_quitting() {
    let mut service = HistoryHandle::start(Default::default(), PathBuf::from(".")).unwrap();
    let mut panel = HistoryPanel {
        visible: true,
        sessions: vec![SessionInfo {
            schema_version: 2,
            workspace_id: "workspace".into(),
            session_id: "retained-session".into(),
            committed_seq: 1,
            committed_bytes: 1,
            snapshot_version: 0,
            recorded_at: None,
            session_closed: true,
            needs_recovery: false,
            execution_result: Some(SessionPhase::Unknown),
        }],
        ..Default::default()
    };
    assert!(!panel.key(
        KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE),
        &mut service,
        true,
    ));
    assert!(panel.search_editing);
    assert!(!panel.key(
        KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
        &mut service,
        true,
    ));
    assert!(!panel.search_editing);
    service.shutdown().await.unwrap();
}

#[tokio::test]
async fn export_preview_can_be_scrolled_while_the_destination_remains_editable() {
    let mut service = HistoryHandle::start(Default::default(), PathBuf::from(".")).unwrap();
    let mut panel = panel();
    panel.preview = Some(ExportPreview {
        manifest: serde_json::json!({"since":0,"high_watermark":1,"execution_result":"unknown"}),
        excerpt: (0..60).map(|row| format!("Evidence row {row}\n")).collect(),
    });
    let mut editor = Editor::default();
    editor.insert("中文导出.jsonl");
    panel.form = Some(Form::Destination(editor));
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
    let screen = |terminal: &Terminal<TestBackend>| {
        terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>()
    };
    terminal.draw(|frame| panel.draw(frame, None)).unwrap();
    assert!(!screen(&terminal).contains("Evidence row 20"));
    for _ in 0..3 {
        assert!(!panel.key(
            KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE),
            &mut service,
            false
        ));
    }
    terminal.draw(|frame| panel.draw(frame, None)).unwrap();
    assert!(screen(&terminal).contains("Evidence row 20"));
    assert!(
        matches!(&panel.form, Some(Form::Destination(editor)) if editor.text == "中文导出.jsonl")
    );
    assert!(panel.pending.is_none());
    service.shutdown().await.unwrap();
}
