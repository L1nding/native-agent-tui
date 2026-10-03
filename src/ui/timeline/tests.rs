use super::*;
use crate::agents::{AgentInfo, AgentSnapshot};
use crate::interactions::RequestView;
use crate::observation::{ActivityIdentity, ActivityKind, ActivityScope, Evidence, EvidenceSource};
use crate::protocol::RpcId;
use crate::state::ConversationItem;
use crate::timeline::TimelineSnapshot;
use serde_json::json;

fn event(id: u64, thread: &str, turn: &str, item: &str) -> Arc<TimelineEntry> {
    Arc::new(TimelineEntry {
        activity_id: format!("{thread}:turn"),
        identity: ActivityIdentity {
            agent_id: thread.into(),
            task_id: None,
            attempt_id: Some(1),
            thread_id: Some(thread.into()),
            turn_id: Some(turn.into()),
            generation: Some(1),
        },
        scope: ActivityScope::Turn,
        activity_kind: ActivityKind::ModelStreaming,
        execution_state: ExecutionState::Running,
        interaction_state: None,
        tool_category: None,
        item_id: Some(item.into()),
        evidence: Evidence {
            id,
            kind: EvidenceKind::Output,
            source: EvidenceSource::AppServer,
            recorded_at_ms: Some(id),
            item_id: Some(item.into()),
            request_id: None,
            output_bytes: 3,
        },
        request: None,
        wait_targets: Vec::new(),
    })
}

fn source(entries: Vec<Arc<TimelineEntry>>) -> CoreSnapshot {
    CoreSnapshot {
        thread_id: Some("root".into()),
        timeline: TimelineSnapshot {
            session_id: "live-session".into(),
            high_water: entries.last().map_or(0, |e| e.evidence.id),
            entries: Arc::new(entries.into()),
            ..Default::default()
        },
        ..Default::default()
    }
}

fn key(panel: &mut TimelinePanel, code: KeyCode, source: &CoreSnapshot) -> Option<Locate> {
    panel.key(KeyEvent::new(code, KeyModifiers::NONE), source)
}

fn child(id: &str, parent: &str, confirmed: bool) -> AgentSnapshot {
    AgentSnapshot {
        info: AgentInfo {
            id: id.into(),
            parent_id: parent.into(),
            confirmed,
            path: None,
            nickname: None,
            role: None,
            model: None,
        },
        generation: 1,
        turn_id: None,
        outcome: None,
        awaiting_turn: false,
        usage: Default::default(),
    }
}

#[test]
fn filters_use_confirmed_scopes_and_apply_before_a_same_batch_open() {
    let mut current = source(
        ["root", "child", "grandchild", "sibling", "unconfirmed"]
            .iter()
            .enumerate()
            .map(|(index, thread)| event(index as u64 + 1, thread, "one", "item"))
            .collect(),
    );
    current.agents = vec![
        child("child", "root", true),
        child("grandchild", "child", true),
        child("sibling", "root", true),
        child("unconfirmed", "child", false),
    ];
    let mut panel = TimelinePanel::default();
    panel.open("child".into(), &current);
    for (scope, expected) in [
        (Scope::Thread, vec![2]),
        (Scope::Subtree, vec![2, 3]),
        (Scope::Path, vec![1, 2]),
        (Scope::All, vec![1, 2, 3, 4, 5]),
    ] {
        panel.scope = scope;
        assert_eq!(
            panel
                .rows(&current)
                .iter()
                .map(|entry| entry.evidence.id)
                .collect::<Vec<_>>(),
            expected
        );
    }
    panel.scope = Scope::Thread;
    current.messages.push(ConversationItem {
        id: "item".into(),
        thread_id: "child".into(),
        turn_id: "one".into(),
        role: "Agent".into(),
        text: "PRIVATE_BODY中文".into(),
        complete: true,
        truncated: false,
    });
    panel.select_latest(&current);
    key(&mut panel, KeyCode::F(6), &current); // Lifecycle excludes output.
    assert!(key(&mut panel, KeyCode::Enter, &current).is_none());
    assert!(panel.visible);
    panel.category = Category::All;
    panel.query.text = "PRIVATE_BODY".into();
    assert!(
        panel.rows(&current).is_empty(),
        "metadata queries must not search message bodies"
    );
    panel.query.text = "item".into();
    panel.turn.text = "other".into();
    assert!(panel.rows(&current).is_empty());
    panel.turn.text = "one".into();
    panel.select_latest(&current);
    assert!(matches!(
        key(&mut panel, KeyCode::Enter, &current),
        Some(Locate::Message(_))
    ));
}

#[test]
fn selection_and_bookmarks_never_open_evicted_or_reused_request_deliveries() {
    let mut request = RequestView::decode(
        RpcId::Number(7),
        "item/commandExecution/requestApproval",
        &json!({"threadId":"root","turnId":"one"}),
    )
    .unwrap();
    request.received_seq = 10;
    let mut approval = (*event(1, "root", "one", "approval")).clone();
    approval.evidence.kind = EvidenceKind::RequestCreated;
    approval.request = Some(request.reference());
    let mut current = source(vec![Arc::new(approval)]);
    current.requests = vec![request.clone()];
    let mut panel = TimelinePanel::default();
    panel.open("root".into(), &current);
    key(&mut panel, KeyCode::Char('b'), &current);
    request.received_seq = 11; // Same ID, thread and turn; different delivery.
    current.requests = vec![request];
    assert!(key(&mut panel, KeyCode::Enter, &current).is_none());
    assert!(panel
        .notice
        .as_ref()
        .unwrap()
        .contains("delivery has ended"));
    current.timeline.entries = Arc::new(vec![event(2, "root", "one", "new")].into());
    current.timeline.high_water = 2;
    current.timeline.dropped_entries = 1;
    panel.sync(&current);
    assert_eq!(panel.selected, Some(1));
    assert!(key(&mut panel, KeyCode::Enter, &current).is_none());
    key(&mut panel, KeyCode::Char('B'), &current);
    assert_eq!(panel.selected, Some(1));
    assert!(key(&mut panel, KeyCode::Enter, &current).is_none());
    assert!(panel.notice.as_ref().unwrap().contains("evicted"));
    // An identical event number in another session cannot revive the bookmark.
    current.timeline.session_id = "different-session".into();
    panel.sync(&current);
    assert!(panel.rows(&current).is_empty());
    assert!(key(&mut panel, KeyCode::Enter, &current).is_none());
}

#[test]
fn current_request_links_are_exact_and_pending_filter_tracks_the_live_projection() {
    let mut request = RequestView::decode(
        RpcId::String("7".into()),
        "item/commandExecution/requestApproval",
        &json!({"threadId":"root","turnId":"one"}),
    )
    .unwrap();
    request.received_seq = 10;
    let mut value = (*event(1, "root", "one", "approval")).clone();
    value.evidence.kind = EvidenceKind::RequestCreated;
    value.request = Some(request.reference());
    let mut current = source(vec![Arc::new(value)]);
    current.requests = vec![request.clone()];
    let mut panel = TimelinePanel::default();
    panel.open("root".into(), &current);
    key(&mut panel, KeyCode::F(8), &current);
    assert_eq!(panel.rows(&current).len(), 1);
    let Some(Locate::Request(reference)) = key(&mut panel, KeyCode::Enter, &current) else {
        panic!("expected a current request link");
    };
    assert_eq!(reference, request.reference());
    panel.open("root".into(), &current);
    current.requests[0].responding = true;
    assert!(panel.rows(&current).is_empty());
    assert!(key(&mut panel, KeyCode::Enter, &current).is_none());
}

#[test]
fn bookmark_and_editor_budgets_are_bounded_and_editing_letters_are_text() {
    let current = source(
        (1..=70)
            .map(|id| event(id, "root", "one", "item"))
            .collect(),
    );
    let mut panel = TimelinePanel::default();
    panel.open("root".into(), &current);
    for id in 1..=70 {
        panel.selected = Some(id);
        key(&mut panel, KeyCode::Char('b'), &current);
    }
    assert_eq!(panel.bookmarks.len(), BOOKMARK_LIMIT);
    assert!(panel.bookmarks.iter().map(Bookmark::bytes).sum::<usize>() <= BOOKMARK_BYTES);
    key(&mut panel, KeyCode::Char('/'), &current);
    for ch in "bBnt".chars() {
        key(&mut panel, KeyCode::Char(ch), &current);
    }
    panel.paste("中文\n👋");
    assert_eq!(panel.query.text, "bBnt中文 👋");
    panel.paste(&"x".repeat(FIELD_BYTES));
    assert_eq!(panel.query.text, "bBnt中文 👋");
    assert!(!panel.bookmarks_only);
    assert_eq!(panel.bookmarks.len(), BOOKMARK_LIMIT);
    let mut large = (*event(71, "root", "one", "large")).clone();
    large.identity.agent_id = "x".repeat(BOOKMARK_BYTES);
    let current = source(vec![Arc::new(large)]);
    let mut panel = TimelinePanel::default();
    panel.open("root".into(), &current);
    key(&mut panel, KeyCode::Char('b'), &current);
    assert!(panel.bookmarks.is_empty());
}

#[test]
fn evicted_messages_and_duplicate_item_ids_require_the_full_conversation_key() {
    let mut current = source(vec![event(1, "root", "one", "same")]);
    current.messages = ["child", "root"]
        .map(|thread| ConversationItem {
            id: "same".into(),
            thread_id: thread.into(),
            turn_id: "other".into(),
            role: "Agent".into(),
            text: "unrelated".into(),
            complete: true,
            truncated: false,
        })
        .to_vec();
    let mut panel = TimelinePanel::default();
    panel.open("root".into(), &current);
    assert!(key(&mut panel, KeyCode::Enter, &current).is_none());
    current.messages.push(ConversationItem {
        id: "same".into(),
        thread_id: "root".into(),
        turn_id: "one".into(),
        role: "Agent".into(),
        text: "retained中文👋".into(),
        complete: false,
        truncated: true,
    });
    assert!(matches!(
        key(&mut panel, KeyCode::Enter, &current),
        Some(Locate::Message(_))
    ));
}

#[test]
fn timeline_and_scrollable_help_render_from_narrow_to_wide_without_body_leaks() {
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    let mut current = source(vec![event(1, "root", "one", "item中文👋")]);
    current.messages.push(ConversationItem {
        id: "item中文👋".into(),
        thread_id: "root".into(),
        turn_id: "one".into(),
        role: "Agent".into(),
        text: "PRIVATE_BODY".into(),
        complete: true,
        truncated: false,
    });
    let mut panel = TimelinePanel::default();
    panel.open("root".into(), &current);
    for (width, height) in [(30, 7), (60, 17), (80, 21), (100, 24), (120, 34), (160, 44)] {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|frame| panel.draw(frame, frame.area(), &current))
            .unwrap();
        let text = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(text.contains("Evidence timeline"));
        assert!(!text.contains("PRIVATE_BODY"));
    }
    key(&mut panel, KeyCode::F(1), &current);
    key(&mut panel, KeyCode::End, &current);
    let mut terminal = Terminal::new(TestBackend::new(80, 12)).unwrap();
    terminal
        .draw(|frame| panel.draw(frame, frame.area(), &current))
        .unwrap();
    let text = terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();
    assert!(text.contains("64 KiB"));
}

#[test]
fn empty_timeline_has_no_locatable_selection() {
    let mut panel = TimelinePanel::default();
    let current = CoreSnapshot::default();
    panel.open("root".into(), &current);
    assert!(panel
        .key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), &current)
        .is_none());
    assert!(panel.visible);
}
