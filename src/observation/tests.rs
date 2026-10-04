use super::*;
use crate::agents::AgentInfo;
use crate::protocol::{ObservedCompaction, ObservedTool, ObservedToolOutcome};
use crate::scheduler::{ExternalTurn, RootTaskSpec, Scheduler};
use serde_json::json;
use std::time::Duration;

fn root_task() -> TaskSnapshot {
    let mut scheduler = Scheduler::default();
    scheduler
        .enqueue(vec![RootTaskSpec::input("PRIVATE_PROMPT".into())])
        .unwrap();
    let dispatch = scheduler.dispatch().unwrap();
    scheduler
        .started_root(
            dispatch.attempt,
            ExternalTurn {
                thread_id: "parent".into(),
                turn_id: "turn-1".into(),
                generation: 1,
            },
        )
        .unwrap();
    scheduler.task(dispatch.attempt.task).unwrap().clone()
}

fn child(id: &str) -> AgentSnapshot {
    AgentSnapshot {
        info: AgentInfo {
            id: id.into(),
            parent_id: "parent".into(),
            path: None,
            nickname: None,
            role: None,
            model: None,
            confirmed: true,
        },
        generation: 1,
        turn_id: Some(format!("{id}-1")),
        outcome: None,
        awaiting_turn: false,
        usage: Default::default(),
    }
}

fn facts<'a>(
    root: &'a TaskSnapshot,
    children: &'a [AgentSnapshot],
    requests: &'a [RequestView],
    gate: Option<&'a GateSnapshot>,
) -> ObservationFacts<'a> {
    ObservationFacts {
        phase: if gate.is_some_and(|gate| gate.pending) {
            SessionPhase::GatePending
        } else {
            SessionPhase::Running
        },
        root_thread: Some("parent"),
        root_generation: 1,
        root_task: Some(root),
        children: children
            .iter()
            .map(|agent| ChildFact { agent, task: None })
            .collect(),
        requests,
        gate,
    }
}

fn observer(now: Instant) -> Observer {
    Observer::new_at(
        "fixture".into(),
        AttentionSettings::default(),
        now,
        Some(1_000_000),
    )
}
fn main<'a>(snapshot: &'a ObservationSnapshot, agent: &str) -> &'a ActivitySnapshot {
    snapshot
        .activities
        .iter()
        .find(|activity| {
            activity.identity.agent_id == agent && activity.scope == ActivityScope::Turn
        })
        .unwrap()
}
fn tool(id: &str, outcome: Option<ObservedToolOutcome>) -> ObservedTool {
    ObservedTool {
        thread_id: "parent".into(),
        turn_id: "turn-1".into(),
        item_id: id.into(),
        outcome,
        category: ToolCategory::Shell,
        compaction: None,
    }
}

#[test]
fn compaction_fact_tracks_lifecycle_unknown_and_legacy_decode() {
    let now = Instant::now();
    let root = root_task();
    let mut observer = observer(now);
    observer
        .reconcile(facts(&root, &[], &[], None), now)
        .unwrap();
    let usage = ObservedCompaction {
        input_tokens: Some(12),
        cached_input_tokens: Some(3),
        output_tokens: Some(4),
        total_tokens: Some(16),
        context_window: Some(128),
    };
    observer
        .tool(
            &ObservedTool {
                thread_id: "parent".into(),
                turn_id: "turn-1".into(),
                item_id: "compact".into(),
                outcome: None,
                category: ToolCategory::Compaction,
                compaction: Some(usage),
            },
            now,
        )
        .unwrap();
    let started = observer.snapshot_at(1, now);
    assert_eq!(started.compactions.len(), 1);
    assert_eq!(started.compactions[0].status, CompactionFactStatus::Started);
    assert_eq!(started.compactions[0].total_tokens, Some(16));
    let started_timeline = observer.timeline_snapshot();
    let started_entry = started_timeline
        .entries
        .iter()
        .find(|entry| entry.item_id.as_deref() == Some("compact"))
        .unwrap();
    assert_eq!(
        started_entry.compaction.as_deref().unwrap().status,
        CompactionFactStatus::Started
    );

    observer
        .tool(
            &ObservedTool {
                thread_id: "parent".into(),
                turn_id: "turn-1".into(),
                item_id: "compact".into(),
                outcome: Some(ObservedToolOutcome::Completed),
                category: ToolCategory::Compaction,
                compaction: Some(ObservedCompaction {
                    context_window: Some(256),
                    ..Default::default()
                }),
            },
            now + Duration::from_millis(2),
        )
        .unwrap();
    let completed = observer.snapshot_at(2, now + Duration::from_millis(2));
    assert_eq!(
        completed.compactions[0].status,
        CompactionFactStatus::Completed
    );
    assert_eq!(completed.compactions[0].context_window, Some(256));

    observer
        .tool(
            &ObservedTool {
                thread_id: "parent".into(),
                turn_id: "turn-1".into(),
                item_id: "compact".into(),
                outcome: None,
                category: ToolCategory::Compaction,
                compaction: Some(ObservedCompaction {
                    total_tokens: Some(777),
                    ..Default::default()
                }),
            },
            now + Duration::from_millis(3),
        )
        .unwrap();
    let late_start = observer.snapshot_at(2, now + Duration::from_millis(3));
    assert_eq!(
        late_start.compactions[0].status,
        CompactionFactStatus::Completed
    );
    assert_eq!(late_start.compactions[0].total_tokens, Some(16));

    observer
        .tool(
            &ObservedTool {
                thread_id: "parent".into(),
                turn_id: "turn-1".into(),
                item_id: "compact".into(),
                outcome: Some(ObservedToolOutcome::Completed),
                category: ToolCategory::Compaction,
                compaction: Some(ObservedCompaction {
                    total_tokens: Some(42),
                    ..Default::default()
                }),
            },
            now + Duration::from_millis(4),
        )
        .unwrap();
    let repeated = observer.snapshot_at(2, now + Duration::from_millis(4));
    assert_eq!(
        repeated.compactions[0].status,
        CompactionFactStatus::Completed
    );
    assert_eq!(repeated.compactions[0].total_tokens, Some(42));

    let mut open = root.clone();
    open.state = TaskState::Running;
    observer
        .tool(
            &ObservedTool {
                thread_id: "parent".into(),
                turn_id: "turn-1".into(),
                item_id: "open".into(),
                outcome: None,
                category: ToolCategory::Compaction,
                compaction: Some(Default::default()),
            },
            now,
        )
        .unwrap();
    let mut ended = open.clone();
    ended.state = TaskState::Succeeded;
    observer
        .reconcile(
            ObservationFacts {
                phase: SessionPhase::Completed,
                root_thread: Some("parent"),
                root_generation: 1,
                root_task: Some(&ended),
                children: Vec::new(),
                requests: &[],
                gate: None,
            },
            now + Duration::from_millis(3),
        )
        .unwrap();
    let unknown = observer.snapshot_at(3, now + Duration::from_millis(3));
    assert_eq!(
        unknown
            .compactions
            .iter()
            .find(|fact| fact.item_id == "open")
            .unwrap()
            .status,
        CompactionFactStatus::Unknown
    );

    let encoded = serde_json::to_value(&completed).unwrap();
    let mut legacy = encoded;
    legacy.as_object_mut().unwrap().remove("compactions");
    let decoded: ObservationSnapshot = serde_json::from_value(legacy).unwrap();
    assert!(decoded.compactions.is_empty());
}

#[test]
fn compaction_fact_is_not_retained_when_activity_limit_rejects_creation() {
    let now = Instant::now();
    let root = root_task();
    let mut observer = observer(now);
    observer
        .reconcile(facts(&root, &[], &[], None), now)
        .unwrap();
    for index in 0..1023 {
        observer
            .tool(&tool(&format!("tool-{index}"), None), now)
            .unwrap();
    }
    let result = observer.tool(
        &ObservedTool {
            thread_id: "parent".into(),
            turn_id: "turn-1".into(),
            item_id: "rejected-compaction".into(),
            outcome: None,
            category: ToolCategory::Compaction,
            compaction: Some(ObservedCompaction {
                total_tokens: Some(99),
                ..Default::default()
            }),
        },
        now,
    );
    assert_eq!(result, Err(ObservationError::Limit));
    assert!(observer.snapshot_at(1, now).compactions.is_empty());
}

#[test]
fn silence_boundaries_change_attention_without_creating_evidence() {
    let now = Instant::now();
    let root = root_task();
    let mut observer = observer(now);
    observer
        .reconcile(facts(&root, &[], &[], None), now)
        .unwrap();
    let initial = observer.snapshot_at(1, now);
    for (ms, level) in [
        (14999, AttentionLevel::Active),
        (15000, AttentionLevel::Quiet),
        (29999, AttentionLevel::Quiet),
        (30000, AttentionLevel::AttentionNeeded),
    ] {
        let time = now + Duration::from_millis(ms);
        observer
            .reconcile(facts(&root, &[], &[], None), time)
            .unwrap();
        let snapshot = observer.snapshot_at(ms, time);
        assert_eq!(main(&snapshot, "root").attention.level, level);
        assert_eq!(
            snapshot.accepted_evidence_count,
            initial.accepted_evidence_count
        );
        assert_eq!(
            main(&snapshot, "root").progress_seq,
            main(&initial, "root").progress_seq
        );
        assert_eq!(main(&snapshot, "root").provider_state, None);
    }
    let time = now + Duration::from_secs(31);
    observer
        .output("parent", "turn-1", "message", 4, false, time)
        .unwrap();
    let snapshot = observer.snapshot_at(5, time);
    assert_eq!(
        main(&snapshot, "root").attention.level,
        AttentionLevel::Active
    );
    assert_eq!(main(&snapshot, "root").kind, ActivityKind::ModelStreaming);
    assert_eq!(
        main(&snapshot, "root").progress_seq,
        main(&initial, "root").progress_seq + 1
    );
    // Repeated text chunks have no protocol offset; both can be real output.
    observer
        .output("parent", "turn-1", "message", 4, false, time)
        .unwrap();
    assert_eq!(main(&observer.snapshot_at(6, time), "root").output_bytes, 8);
}

#[test]
fn child_output_refreshes_only_its_own_activity_and_wait_target() {
    let now = Instant::now();
    let mut root = root_task();
    root.state = TaskState::WaitingChildren;
    let mut children = [child("a"), child("b")];
    let mut gate = GateSnapshot {
        pending: true,
        targets: children
            .iter()
            .map(|agent| WaitTarget {
                id: agent.info.id.clone(),
                generation: 1,
                turn_id: agent.turn_id.clone(),
                outcome: None,
            })
            .collect(),
        root_starts_at_enter: 1,
        root_starts_at_release: None,
    };
    let mut observer = observer(now);
    observer
        .reconcile(facts(&root, &children, &[], Some(&gate)), now)
        .unwrap();
    let initial = observer.snapshot_at(1, now);
    let time = now + Duration::from_secs(130);
    observer
        .output("a", "a-1", "message", 3, false, time)
        .unwrap();
    observer
        .reconcile(facts(&root, &children, &[], Some(&gate)), time)
        .unwrap();
    let snapshot = observer.snapshot_at(2, time);
    assert_eq!(main(&snapshot, "a").attention.level, AttentionLevel::Active);
    assert_eq!(
        main(&snapshot, "b").attention.level,
        AttentionLevel::AttentionNeeded
    );
    let parent = main(&snapshot, "root");
    assert_eq!(parent.progress_seq, main(&initial, "root").progress_seq);
    assert_eq!(parent.attention.level, AttentionLevel::AttentionNeeded);
    assert_eq!(parent.wait_targets[0].silence_ms, Some(0));
    assert_eq!(parent.wait_targets[1].silence_ms, Some(130000));
    assert_eq!(
        parent.resume_condition,
        Some(ResumeCondition::AllChildrenCompleteOrAnyUnsuccessful)
    );
    children[0].outcome = Some(ChildOutcome::Completed);
    gate.targets[0].outcome = Some(ChildOutcome::Completed);
    observer
        .reconcile(facts(&root, &children, &[], Some(&gate)), time)
        .unwrap();
    let ended = observer.snapshot_at(3, time);
    observer
        .reconcile(
            facts(&root, &children, &[], Some(&gate)),
            time + Duration::from_secs(2),
        )
        .unwrap();
    let repeated = observer.snapshot_at(4, time + Duration::from_secs(2));
    assert_eq!(
        repeated.accepted_evidence_count,
        ended.accepted_evidence_count
    );
    assert_eq!(main(&repeated, "root").child_terminal_count, 1);
    assert_eq!(main(&repeated, "a").elapsed_ms, Some(130000));
}

#[test]
fn simultaneous_tools_retain_independent_evidence_and_unknown_starts() {
    let now = Instant::now();
    let root = root_task();
    let mut observer = observer(now);
    observer
        .reconcile(facts(&root, &[], &[], None), now)
        .unwrap();
    observer.tool(&tool("one", None), now).unwrap();
    observer.tool(&tool("two", None), now).unwrap();
    let initial = observer.snapshot_at(1, now);
    observer
        .tool(&tool("one", None), now + Duration::from_secs(5))
        .unwrap();
    assert_eq!(
        observer.snapshot_at(2, now).accepted_evidence_count,
        initial.accepted_evidence_count
    );
    let time = now + Duration::from_secs(61);
    observer
        .tool_output("parent", "turn-1", "one", 8, ToolCategory::Shell, time)
        .unwrap();
    let snapshot = observer.snapshot_at(3, time);
    let find = |id: &str| {
        snapshot
            .activities
            .iter()
            .find(|activity| activity.item_id.as_deref() == Some(id))
            .unwrap()
    };
    assert_eq!(find("one").attention.level, AttentionLevel::Active);
    assert_eq!(find("two").attention.level, AttentionLevel::AttentionNeeded);
    observer
        .tool(
            &tool("unseen", Some(ObservedToolOutcome::Interrupted)),
            time,
        )
        .unwrap();
    let snapshot = observer.snapshot_at(4, time);
    let unseen = snapshot
        .activities
        .iter()
        .find(|activity| activity.item_id.as_deref() == Some("unseen"))
        .unwrap();
    assert_eq!(unseen.started_at_ms, None);
    assert_eq!(unseen.elapsed_ms, None);
    assert_eq!(unseen.execution_state, ExecutionState::Interrupted);
    assert_eq!(unseen.progress_seq, 1);
    assert_eq!(
        unseen.last_evidence.as_ref().unwrap().kind,
        EvidenceKind::ToolCompleted
    );
    observer
        .tool(
            &tool("unseen", Some(ObservedToolOutcome::Interrupted)),
            time,
        )
        .unwrap();
    assert_eq!(
        observer.snapshot_at(5, time).accepted_evidence_count,
        snapshot.accepted_evidence_count
    );
}

#[test]
fn approval_remains_actionable_despite_output_and_resolves_exactly_once() {
    let now = Instant::now();
    let root = root_task();
    let mut requests = [RequestView::decode(RpcId::String("approval".into()), "item/commandExecution/requestApproval", &json!({"threadId":"parent","turnId":"turn-1","command":"PRIVATE_COMMAND"})).unwrap(), RequestView::decode(RpcId::Number(7), "item/tool/requestUserInput", &json!({"threadId":"parent","turnId":"turn-1","questions":[{"id":"secret","header":"PRIVATE_HEADER","question":"PRIVATE_QUESTION","isSecret":true}]})).unwrap()];
    let mut observer = observer(now);
    observer
        .reconcile(facts(&root, &[], &requests, None), now)
        .unwrap();
    let time = now + Duration::from_secs(600);
    observer
        .output("parent", "turn-1", "message", 4, false, time)
        .unwrap();
    let snapshot = observer.snapshot_at(1, time);
    assert_eq!(
        snapshot
            .activities
            .iter()
            .filter(|activity| activity.attention.requires_action)
            .count(),
        2
    );
    requests[0].responding = true;
    observer
        .reconcile(facts(&root, &[], &requests, None), time)
        .unwrap();
    let answered = observer.snapshot_at(2, time);
    let request = answered
        .activities
        .iter()
        .find(|activity| activity.request_id == Some(RpcId::String("approval".into())))
        .unwrap();
    assert_eq!(request.kind, ActivityKind::WaitingTransport);
    assert_eq!(
        request.interaction_state,
        Some(InteractionState::Responding)
    );
    observer
        .reconcile(facts(&root, &[], &requests, None), time)
        .unwrap();
    assert_eq!(
        observer.snapshot_at(3, time).accepted_evidence_count,
        answered.accepted_evidence_count
    );
    observer.request_resolved(&requests[0].reference(), time);
    observer.request_resolved(&requests[0].reference(), time);
    observer
        .reconcile(facts(&root, &[], &requests[1..], None), time)
        .unwrap();
    let snapshot = observer.snapshot_at(4, time);
    assert_eq!(
        snapshot.accepted_evidence_count,
        answered.accepted_evidence_count + 2
    );
    let encoded = serde_json::to_string(&snapshot).unwrap();
    for private in [
        "PRIVATE_PROMPT",
        "PRIVATE_COMMAND",
        "PRIVATE_HEADER",
        "PRIVATE_QUESTION",
    ] {
        assert!(!encoded.contains(private));
    }
    assert_eq!(
        serde_json::from_str::<ObservationSnapshot>(&encoded).unwrap(),
        snapshot
    );
}

#[test]
fn request_delivery_identity_separates_replacements_and_stale_resolution() {
    let now = Instant::now();
    let root = root_task();
    let mut observer = observer(now);
    let mut request = RequestView::decode(
        RpcId::Number(7),
        "item/commandExecution/requestApproval",
        &json!({"threadId":"parent","turnId":"turn-1"}),
    )
    .unwrap();
    request.received_seq = 1;
    observer
        .reconcile(facts(&root, &[], std::slice::from_ref(&request), None), now)
        .unwrap();
    let stale = request.reference();
    // Replacement without a resolution notification expires the old delivery.
    request.received_seq = 2;
    let time = now + Duration::from_secs(1);
    observer
        .reconcile(
            facts(&root, &[], std::slice::from_ref(&request), None),
            time,
        )
        .unwrap();
    let replaced = observer.snapshot_at(1, time);
    assert_eq!(
        replaced
            .activities
            .iter()
            .filter(|a| a.attention.requires_action)
            .count(),
        1
    );
    assert_eq!(
        replaced
            .activities
            .iter()
            .filter(|a| a.interaction_state == Some(InteractionState::Expired))
            .count(),
        1
    );
    observer.request_resolved(&stale, time);
    observer.request_resolved(
        &RequestRef {
            thread_id: "other".into(),
            ..request.reference()
        },
        time,
    );
    observer.request_resolved(
        &RequestRef {
            turn_id: "old-turn".into(),
            ..request.reference()
        },
        time,
    );
    assert_eq!(
        observer.snapshot_at(2, time).accepted_evidence_count,
        replaced.accepted_evidence_count
    );
    assert!(observer
        .snapshot_at(2, time)
        .activities
        .iter()
        .any(|a| a.attention.requires_action));
    observer.request_resolved(&request.reference(), time);
    let resolved = observer.snapshot_at(3, time);
    assert!(!resolved
        .activities
        .iter()
        .any(|a| a.attention.requires_action));
    assert_eq!(
        resolved
            .activities
            .iter()
            .filter(|a| a.interaction_state == Some(InteractionState::Resolved))
            .count(),
        1
    );
    observer.request_resolved(&request.reference(), time);
    assert_eq!(
        observer.snapshot_at(4, time).accepted_evidence_count,
        resolved.accepted_evidence_count
    );
}

#[test]
fn timeline_freezes_gate_target_updates_and_retains_previous_attempt_metadata() {
    let now = Instant::now();
    let mut root = root_task();
    root.state = TaskState::WaitingChildren;
    let mut children = [child("a")];
    children[0].turn_id = None;
    children[0].awaiting_turn = true;
    let mut gate = GateSnapshot {
        pending: true,
        targets: vec![WaitTarget {
            id: "a".into(),
            generation: 1,
            turn_id: None,
            outcome: None,
        }],
        root_starts_at_enter: 1,
        root_starts_at_release: None,
    };
    let mut observer = observer(now);
    observer
        .reconcile(facts(&root, &children, &[], Some(&gate)), now)
        .unwrap();
    let entered = observer.timeline_snapshot();
    let waiting = entered
        .entries
        .iter()
        .find(|entry| entry.evidence.kind == EvidenceKind::GateEntered)
        .unwrap();
    assert!(waiting.wait_targets[0].turn_id.is_none());
    children[0].turn_id = Some("a-1".into());
    children[0].awaiting_turn = false;
    gate.targets[0].turn_id = Some("a-1".into());
    observer
        .reconcile(facts(&root, &children, &[], Some(&gate)), now)
        .unwrap();
    let bound = observer.timeline_snapshot();
    let binding = bound
        .entries
        .iter()
        .find(|entry| entry.evidence.kind == EvidenceKind::ChildTurnBound)
        .unwrap();
    assert_eq!(binding.wait_targets[0].turn_id.as_deref(), Some("a-1"));
    children[0].outcome = Some(ChildOutcome::Completed);
    gate.targets[0].outcome = Some(ChildOutcome::Completed);
    observer
        .reconcile(facts(&root, &children, &[], Some(&gate)), now)
        .unwrap();
    let finished = observer.timeline_snapshot();
    let terminal = finished
        .entries
        .iter()
        .find(|entry| entry.evidence.kind == EvidenceKind::ChildTerminal)
        .unwrap();
    assert_eq!(
        terminal.wait_targets[0].outcome,
        Some(ChildOutcome::Completed)
    );
    assert_eq!(
        binding.wait_targets[0].outcome, None,
        "earlier evidence must stay frozen"
    );
    root.attempt = 2;
    root.external.as_mut().unwrap().turn_id = "new-turn".into();
    root.state = TaskState::Running;
    observer
        .reconcile(facts(&root, &children, &[], None), now)
        .unwrap();
    let retried = observer.timeline_snapshot();
    assert!(retried
        .entries
        .iter()
        .any(|entry| entry.identity.attempt_id == Some(1)
            && entry.identity.turn_id.as_deref() == Some("turn-1")));
    assert!(retried
        .entries
        .iter()
        .any(|entry| entry.identity.attempt_id == Some(2)
            && entry.identity.turn_id.as_deref() == Some("new-turn")));
}

#[test]
fn terminal_results_stop_aging_and_unfinished_tools_never_become_successful() {
    let now = Instant::now();
    let mut root = root_task();
    let mut observer = observer(now);
    observer
        .reconcile(facts(&root, &[], &[], None), now)
        .unwrap();
    observer.tool(&tool("pending", None), now).unwrap();
    root.state = TaskState::Cancelled;
    let ended = now + Duration::from_secs(10);
    observer
        .reconcile(facts(&root, &[], &[], None), ended)
        .unwrap();
    let snapshot = observer.snapshot_at(1, ended);
    assert_eq!(
        main(&snapshot, "root").execution_state,
        ExecutionState::Interrupted
    );
    assert_eq!(
        main(&snapshot, "root").attention.level,
        AttentionLevel::Ended
    );
    assert_eq!(
        snapshot
            .activities
            .iter()
            .find(|activity| activity.scope == ActivityScope::Tool)
            .unwrap()
            .execution_state,
        ExecutionState::Unknown
    );
    observer
        .output("parent", "turn-1", "late", 4, false, ended)
        .unwrap();
    observer
        .reconcile(facts(&root, &[], &[], None), now + Duration::from_secs(900))
        .unwrap();
    let later = observer.snapshot_at(2, now + Duration::from_secs(900));
    assert_eq!(
        later.accepted_evidence_count,
        snapshot.accepted_evidence_count
    );
    assert_eq!(main(&later, "root").elapsed_ms, Some(10000));
    assert_eq!(main(&later, "root").silence_ms, None);
    assert!(!observer.has_timed_activity());
}

#[test]
fn configuration_ticks_empty_and_stale_output_do_not_advance_progress() {
    let now = Instant::now();
    let root = root_task();
    let mut observer = observer(now);
    observer
        .reconcile(facts(&root, &[], &[], None), now)
        .unwrap();
    let initial = observer.snapshot_at(1, now);
    observer
        .output("parent", "turn-1", "empty", 0, false, now)
        .unwrap();
    observer
        .output("parent", "old-turn", "old", 4, false, now)
        .unwrap();
    observer
        .output("unrelated", "turn-1", "old", 4, false, now)
        .unwrap();
    observer.configure(AttentionClass::Model, 10, 20).unwrap();
    observer.raw_message();
    let snapshot = observer.snapshot_at(2, now + Duration::from_millis(20));
    assert_eq!(
        snapshot.accepted_evidence_count,
        initial.accepted_evidence_count
    );
    assert_eq!(snapshot.raw_message_count, 1);
    assert_eq!(
        main(&snapshot, "root").attention.config_source,
        Some(ConfigSource::Tui)
    );
    assert_eq!(
        main(&snapshot, "root").attention.level,
        AttentionLevel::AttentionNeeded
    );
    let rollback = observer.snapshot_at(3, now - Duration::from_millis(1));
    assert_eq!(main(&rollback, "root").freshness, Freshness::Unknown);
    assert_eq!(main(&rollback, "root").silence_ms, None);
    assert_eq!(main(&rollback, "root").elapsed_ms, None);
}

#[test]
fn retry_replaces_activity_identity_and_rejects_prior_turn_evidence() {
    let now = Instant::now();
    let mut root = root_task();
    let mut observer = observer(now);
    observer
        .reconcile(facts(&root, &[], &[], None), now)
        .unwrap();
    observer.tool(&tool("old-tool", None), now).unwrap();
    root.attempt += 1;
    root.external.as_mut().unwrap().turn_id = "turn-2".into();
    observer
        .reconcile(facts(&root, &[], &[], None), now)
        .unwrap();
    let initial = observer.snapshot_at(1, now);
    assert_eq!(initial.activities.len(), 1);
    assert_eq!(main(&initial, "root").identity.attempt_id, Some(2));
    observer
        .output("parent", "turn-1", "old", 4, false, now)
        .unwrap();
    observer.tool(&tool("old-tool", None), now).unwrap();
    assert_eq!(
        observer.snapshot_at(2, now).accepted_evidence_count,
        initial.accepted_evidence_count
    );
}

#[test]
fn startup_has_no_guessed_threshold_and_disconnect_uses_core_provenance() {
    let now = Instant::now();
    let mut observer = observer(now);
    observer
        .reconcile(
            ObservationFacts {
                phase: SessionPhase::Initializing,
                root_thread: None,
                root_generation: 0,
                root_task: None,
                children: vec![],
                requests: &[],
                gate: None,
            },
            now,
        )
        .unwrap();
    let snapshot = observer.snapshot_at(1, now + Duration::from_secs(900));
    assert_eq!(
        snapshot.activities[0].attention.level,
        AttentionLevel::Unknown
    );
    assert_eq!(snapshot.activities[0].attention.attention_after_ms, None);
    assert!(!observer.has_timed_activity());
    observer
        .reconcile(
            ObservationFacts {
                phase: SessionPhase::Disconnected,
                root_thread: None,
                root_generation: 0,
                root_task: None,
                children: vec![],
                requests: &[],
                gate: None,
            },
            now,
        )
        .unwrap();
    let snapshot = observer.snapshot_at(2, now);
    assert_eq!(
        snapshot.activities[0].execution_state,
        ExecutionState::Unknown
    );
    assert_eq!(
        snapshot.activities[0]
            .last_evidence
            .as_ref()
            .unwrap()
            .source,
        EvidenceSource::Core
    );
}

#[test]
fn activity_and_recent_evidence_memory_is_bounded_with_an_explicit_limit_error() {
    let now = Instant::now();
    let root = root_task();
    let mut observer = observer(now);
    observer
        .reconcile(facts(&root, &[], &[], None), now)
        .unwrap();
    for index in 0..ACTIVITY_LIMIT - 1 {
        observer
            .tool(&tool(&format!("tool-{index}"), None), now)
            .unwrap();
    }
    assert_eq!(
        observer.tool(&tool("overflow", None), now),
        Err(ObservationError::Limit)
    );
    let snapshot = observer.snapshot_at(1, now);
    assert_eq!(snapshot.activities.len(), ACTIVITY_LIMIT);
    assert_eq!(
        main(&snapshot, "root").recent_evidence.len(),
        RECENT_EVIDENCE
    );
    assert!(snapshot
        .activities
        .iter()
        .all(|activity| activity.recent_evidence.len() <= RECENT_EVIDENCE));
}

#[test]
fn settings_defaults_atomic_overrides_and_validation_are_explicit() {
    let mut settings = AttentionSettings::default();
    assert_eq!(
        (settings.model.quiet_ms, settings.model.attention_ms),
        (15000, 30000)
    );
    assert_eq!(
        (settings.tool.quiet_ms, settings.tool.attention_ms),
        (30000, 60000)
    );
    assert_eq!(
        (settings.children.quiet_ms, settings.children.attention_ms),
        (60000, 120000)
    );
    assert_eq!(
        (settings.transport.quiet_ms, settings.transport.attention_ms),
        (15000, 30000)
    );
    let initial = settings.clone();
    for bytes in [
        br#"{"model":{"quiet_ms":5,"attention_ms":10},"tool":{"quiet_ms":0,"attention_ms":10}}"#
            .as_slice(),
        br#"{"unknown":{}}"#,
        br#"{"model":{"quiet_ms":5,"attention_ms":604800001}}"#,
        br#"{"model":{"quiet_ms":10,"attention_ms":10}}"#,
    ] {
        assert!(settings.apply_json(bytes, ConfigSource::Global).is_err());
        assert_eq!(settings, initial);
    }
    assert!(settings
        .apply_json(&vec![b' '; 8193], ConfigSource::Global)
        .is_err());
    settings
        .apply_json(
            br#"{"model":{"quiet_ms":5,"attention_ms":10}}"#,
            ConfigSource::Global,
        )
        .unwrap();
    assert_eq!(settings.model.source, ConfigSource::Global);
    assert_eq!(settings.tool.source, ConfigSource::Default);
}

#[test]
fn tool_output_without_a_start_has_unknown_elapsed_and_final_items_keep_stable_identity() {
    let now = Instant::now();
    let root = root_task();
    let mut observer = observer(now);
    observer
        .reconcile(facts(&root, &[], &[], None), now)
        .unwrap();
    observer
        .tool_output("parent", "turn-1", "early", 8, ToolCategory::Shell, now)
        .unwrap();
    let snapshot = observer.snapshot_at(1, now);
    let early = snapshot
        .activities
        .iter()
        .find(|activity| activity.item_id.as_deref() == Some("early"))
        .unwrap();
    assert_eq!(early.started_at_ms, None);
    assert_eq!(early.elapsed_ms, None);
    assert_eq!(early.progress_seq, 1);
    assert_eq!(
        early.last_evidence.as_ref().unwrap().kind,
        EvidenceKind::Output
    );
    observer.tool(&tool("early", None), now).unwrap();
    let snapshot = observer.snapshot_at(2, now);
    let early = snapshot
        .activities
        .iter()
        .find(|activity| activity.item_id.as_deref() == Some("early"))
        .unwrap();
    assert_eq!(early.progress_seq, 2);
    assert_eq!(early.elapsed_ms, Some(0));
    for index in 0..130 {
        observer
            .output(
                "parent",
                "turn-1",
                &format!("message-{index}"),
                8,
                true,
                now,
            )
            .unwrap();
    }
    let finalized = observer.snapshot_at(3, now);
    observer
        .output("parent", "turn-1", "message-0", 8, true, now)
        .unwrap();
    observer
        .output("parent", "turn-1", "message-0", 8, false, now)
        .unwrap();
    assert_eq!(
        observer.snapshot_at(4, now).accepted_evidence_count,
        finalized.accepted_evidence_count
    );
    assert_eq!(observer.finalized_items.len(), 130);
}
