use super::*;
use crate::agents::{AgentInfo, AgentSnapshot};
use crate::interactions::RequestView;
use crate::observation::{
    ActivityIdentity, ActivityKind, ActivityScope, CompactionFact, CompactionFactStatus, Evidence,
    EvidenceSource,
};
use crate::protocol::RpcId;
use crate::state::ConversationItem;
use crate::timeline::TimelineSnapshot;
use crate::tool_details::{
    ToolDetail, ToolDetailLocator, ToolDetailsSnapshot, ToolLifecycle, ToolTextSource,
};
use serde_json::json;
use std::collections::VecDeque;

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
        compaction: None,
    })
}

fn tool_event(id: u64) -> Arc<TimelineEntry> {
    let mut entry = (*event(id, "root", "turn-1", "tool-1")).clone();
    entry.scope = ActivityScope::Tool;
    entry.activity_kind = ActivityKind::ToolRunning;
    entry.tool_category = Some(crate::protocol::ToolCategory::Shell);
    entry.execution_state = ExecutionState::Completed;
    entry.evidence.kind = EvidenceKind::ToolCompleted;
    Arc::new(entry)
}

fn tool_details(locator: ToolDetailLocator) -> ToolDetailsSnapshot {
    let detail = ToolDetail {
        locator,
        revision: 1,
        category: crate::protocol::ToolCategory::Shell,
        lifecycle: ToolLifecycle::Completed,
        name: None,
        command: Some("echo result".into()),
        cwd: None,
        parameters: None,
        result: None,
        output: "result".into(),
        output_source: ToolTextSource::CommandAggregate,
        exit_code: Some(0),
        duration_ms: Some(1),
        bytes_observed: 6,
        bytes_retained: 6,
        clipped: false,
        authoritative: true,
    };
    ToolDetailsSnapshot {
        entries: Arc::new(VecDeque::from([Arc::new(detail)])),
        retained_bytes: 1,
        dropped_entries: 0,
    }
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
fn enter_opens_only_the_exact_tool_locator_and_escape_returns_without_commands() {
    let entry = tool_event(1);
    let mut current = source(vec![entry.clone()]);
    let locator = ToolDetailLocator {
        session_id: current.timeline.session_id.clone(),
        identity: entry.identity.clone(),
        item_id: entry.item_id.clone().unwrap(),
    };
    current.tool_details = tool_details(locator.clone());
    let mut panel = TimelinePanel::default();
    panel.open("root".into(), &current);

    assert!(key(&mut panel, KeyCode::Enter, &current).is_none());
    assert_eq!(panel.tool_detail, Some(locator));
    assert!(matches!(
        key(&mut panel, KeyCode::Char('s'), &current),
        Some(Locate::ToolSearch(thread)) if thread == "root"
    ));
    assert!(key(&mut panel, KeyCode::Esc, &current).is_none());
    assert!(panel.visible);
    assert!(panel.tool_detail.is_none());

    assert!(key(&mut panel, KeyCode::End, &current).is_none());
    panel.open("child".into(), &current);
    assert!(panel.tool_detail.is_none());
    assert_eq!(panel.scroll.get(), 0);

    panel.open("root".into(), &current);
    assert!(key(&mut panel, KeyCode::Enter, &current).is_none());
    let mut new_session = current.clone();
    new_session.timeline.session_id = "another-session".into();
    panel.sync(&new_session);
    assert!(panel.tool_detail.is_none());
    assert_eq!(panel.scroll.get(), 0);
}

#[test]
fn enter_opens_exact_tool_details_from_the_latest_turn_mirror() {
    let tool = tool_event(1);
    let locator = ToolDetailLocator {
        session_id: "live-session".into(),
        identity: tool.identity.clone(),
        item_id: tool.item_id.clone().unwrap(),
    };

    for kind in [EvidenceKind::Output, EvidenceKind::ToolCompleted] {
        let mut mirror = (*tool).clone();
        mirror.scope = ActivityScope::Turn;
        mirror.activity_kind = ActivityKind::ModelStreaming;
        mirror.execution_state = ExecutionState::Running;
        mirror.tool_category = None;
        mirror.evidence.id = 2;
        mirror.evidence.kind = kind;
        let mut current = source(vec![tool.clone(), Arc::new(mirror)]);
        current.tool_details = tool_details(locator.clone());

        let mut panel = TimelinePanel::default();
        panel.open("root".into(), &current);
        assert_eq!(panel.selected, Some(2), "latest mirror: {kind:?}");
        assert!(key(&mut panel, KeyCode::Enter, &current).is_none());
        assert_eq!(panel.tool_detail, Some(locator.clone()), "{kind:?}");
    }

    let mut mirror = (*tool).clone();
    mirror.scope = ActivityScope::Turn;
    mirror.activity_kind = ActivityKind::ModelStreaming;
    mirror.execution_state = ExecutionState::Running;
    mirror.tool_category = None;
    mirror.evidence.id = 2;
    mirror.evidence.kind = EvidenceKind::ToolCompleted;
    let mut stale = locator.clone();
    stale.identity.generation = Some(2);
    let mut current = source(vec![tool, Arc::new(mirror)]);
    current.tool_details = tool_details(stale);

    let mut panel = TimelinePanel::default();
    panel.open("root".into(), &current);
    key(&mut panel, KeyCode::Enter, &current);
    assert!(
        panel.tool_detail.is_none(),
        "stale generation must not open"
    );
}

#[test]
fn stale_exact_tool_details_fall_back_to_retained_metadata_trace() {
    let entry = tool_event(1);
    let mut current = source(vec![entry.clone()]);
    let locator = ToolDetailLocator {
        session_id: current.timeline.session_id.clone(),
        identity: entry.identity.clone(),
        item_id: entry.item_id.clone().unwrap(),
    };

    for mismatch in ["attempt", "generation", "evicted"] {
        let mut stale = locator.clone();
        match mismatch {
            "attempt" => stale.identity.attempt_id = Some(2),
            "generation" => stale.identity.generation = Some(2),
            _ => {}
        }
        current.tool_details = if mismatch == "evicted" {
            ToolDetailsSnapshot::default()
        } else {
            tool_details(stale)
        };
        let mut panel = TimelinePanel::default();
        panel.open("root".into(), &current);
        assert!(key(&mut panel, KeyCode::Enter, &current).is_none());
        assert_eq!(panel.tool_detail, Some(locator.clone()), "{mismatch}");
        assert!(panel.notice.is_none(), "{mismatch}");
    }
}

#[test]
fn enter_opens_metadata_only_tool_trace_when_exact_detail_is_missing() {
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    let mut completed = (*tool_event(1)).clone();
    completed.evidence.item_id = None;
    completed.execution_state = ExecutionState::Unknown;
    completed.activity_kind = ActivityKind::Unknown;
    let current = source(vec![Arc::new(completed.clone())]);
    let locator = ToolDetailLocator {
        session_id: current.timeline.session_id.clone(),
        identity: completed.identity,
        item_id: completed.item_id.unwrap(),
    };
    let mut panel = TimelinePanel::default();
    panel.open("root".into(), &current);

    assert!(key(&mut panel, KeyCode::Enter, &current).is_none());
    assert_eq!(panel.tool_detail, Some(locator));
    let mut terminal = Terminal::new(TestBackend::new(100, 16)).unwrap();
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
    assert!(text.contains("Tool detail unavailable"), "{text}");
    assert!(text.contains("ToolCompleted"), "{text}");
    assert!(text.contains("state=Unknown"), "{text}");
}

#[test]
fn tool_output_timeline_event_can_open_metadata_only_trace() {
    let mut output = (*tool_event(1)).clone();
    output.evidence.kind = EvidenceKind::Output;
    output.evidence.item_id = None;
    output.execution_state = ExecutionState::Running;
    output.activity_kind = ActivityKind::ToolRunning;
    let current = source(vec![Arc::new(output)]);
    let mut panel = TimelinePanel::default();
    panel.open("root".into(), &current);

    assert!(key(&mut panel, KeyCode::Enter, &current).is_none());
    assert_eq!(
        panel.tool_detail.as_ref().map(|item| item.item_id.as_str()),
        Some("tool-1")
    );
}

#[test]
fn tool_detail_end_and_page_navigation_use_the_resized_content_range() {
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    let entry = tool_event(1);
    let mut current = source(vec![entry.clone()]);
    let locator = ToolDetailLocator {
        session_id: current.timeline.session_id.clone(),
        identity: entry.identity.clone(),
        item_id: entry.item_id.clone().unwrap(),
    };
    let mut details = tool_details(locator);
    let mut long = details.entries[0].as_ref().clone();
    long.output = "界".repeat(300);
    long.bytes_observed = long.output.len();
    long.bytes_retained = long.output.len();
    details.entries = Arc::new(VecDeque::from([Arc::new(long)]));
    current.tool_details = details;

    let mut panel = TimelinePanel::default();
    panel.open("root".into(), &current);
    assert!(key(&mut panel, KeyCode::Enter, &current).is_none());
    let mut terminal = Terminal::new(TestBackend::new(60, 8)).unwrap();
    terminal
        .draw(|frame| panel.draw(frame, frame.area(), &current))
        .unwrap();
    let wide_max = panel.tool_detail_max_scroll.get();
    assert!(wide_max > 0);
    key(&mut panel, KeyCode::End, &current);
    let at_end = panel.scroll.get();
    assert_eq!(at_end, wide_max);
    key(&mut panel, KeyCode::PageUp, &current);
    assert_eq!(panel.scroll.get(), at_end - 1);

    drop(terminal);
    let mut terminal = Terminal::new(TestBackend::new(24, 5)).unwrap();
    terminal
        .draw(|frame| panel.draw(frame, frame.area(), &current))
        .unwrap();
    let narrow_max = panel.tool_detail_max_scroll.get();
    assert!(narrow_max > wide_max);
    key(&mut panel, KeyCode::End, &current);
    assert_eq!(panel.scroll.get(), narrow_max);
    key(&mut panel, KeyCode::PageUp, &current);
    assert_eq!(panel.scroll.get(), narrow_max - 1);
    key(&mut panel, KeyCode::PageDown, &current);
    assert_eq!(panel.scroll.get(), narrow_max);
}

#[test]
fn compaction_timeline_metadata_preserves_evidence_source_and_schema_limits() {
    use crate::protocol::ToolCategory;

    let mut entry = (*event(1, "root", "turn", "compact-item")).clone();
    entry.scope = ActivityScope::Tool;
    entry.activity_kind = ActivityKind::Completed;
    entry.execution_state = ExecutionState::Unknown;
    entry.tool_category = Some(ToolCategory::Compaction);
    entry.evidence.kind = EvidenceKind::ExecutionUnknown;
    entry.evidence.source = EvidenceSource::Core;
    entry.compaction = Some(Box::new(CompactionFact {
        thread_id: "root".into(),
        turn_id: "turn".into(),
        item_id: "compact-item".into(),
        status: CompactionFactStatus::Unknown,
        started_at_ms: Some(10),
        completed_at_ms: None,
        input_tokens: Some(120),
        cached_input_tokens: None,
        output_tokens: Some(20),
        total_tokens: Some(140),
        context_window: Some(200),
    }));
    let rows = metadata(&entry).join("\n");
    assert!(rows.contains("tool: Some(Compaction)"), "{rows}");
    assert!(rows.contains("source: Core"), "{rows}");
    assert!(rows.contains("Compaction status: Unknown"), "{rows}");
    assert!(rows.contains("input 120"), "{rows}");
    assert!(rows.contains("cached input unavailable"), "{rows}");
    assert!(rows.contains("output 20"), "{rows}");
    assert!(rows.contains("total 140"), "{rows}");
    assert!(rows.contains("context window 200"), "{rows}");
    assert!(!rows.contains("server observed"));

    let mut fact_without_category = entry.clone();
    fact_without_category.tool_category = None;
    let rows = metadata(&fact_without_category).join("\n");
    assert!(rows.contains("Compaction status: Unknown"), "{rows}");
}

#[test]
fn compaction_timeline_metadata_without_fact_marks_all_values_unavailable() {
    use crate::protocol::ToolCategory;

    let mut entry = (*event(1, "root", "turn", "compact-item")).clone();
    entry.tool_category = Some(ToolCategory::Compaction);
    let rows = metadata(&entry).join("\n");
    assert!(rows.contains("Compaction status: unavailable"), "{rows}");
    assert!(rows.contains("input unavailable"), "{rows}");
    assert!(rows.contains("cached input unavailable"), "{rows}");
    assert!(rows.contains("output unavailable"), "{rows}");
    assert!(rows.contains("total unavailable"), "{rows}");
    assert!(rows.contains("context window unavailable"), "{rows}");
}

#[test]
fn compaction_timeline_details_render_at_narrow_and_wide_sizes() {
    use crate::protocol::ToolCategory;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    let mut value = (*event(1, "root", "turn", "compact-item")).clone();
    value.tool_category = Some(ToolCategory::Compaction);
    value.compaction = Some(Box::new(CompactionFact {
        thread_id: "root".into(),
        turn_id: "turn".into(),
        item_id: "compact-item".into(),
        status: CompactionFactStatus::Completed,
        started_at_ms: None,
        completed_at_ms: Some(20),
        input_tokens: Some(120),
        cached_input_tokens: Some(30),
        output_tokens: Some(20),
        total_tokens: Some(140),
        context_window: Some(200),
    }));
    let current = source(vec![Arc::new(value)]);
    let mut panel = TimelinePanel::default();
    panel.open("root".into(), &current);
    for (width, height) in [(40, 16), (80, 24), (160, 40)] {
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
        assert!(
            text.contains("Evidence timeline"),
            "{width}x{height}: {text}"
        );
        if width >= 80 {
            assert!(
                text.contains("Compaction status: Completed"),
                "{width}x{height}: {text}"
            );
            assert!(text.contains("input 120"), "{width}x{height}: {text}");
            assert!(
                text.contains("context window 200"),
                "{width}x{height}: {text}"
            );
            assert!(
                !text.contains("Raw tool output, reasoning/usage/compaction: unavailable here"),
                "{width}x{height}: {text}"
            );
        }
    }
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
