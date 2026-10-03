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
