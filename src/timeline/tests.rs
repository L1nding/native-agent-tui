use super::*;
use crate::observation::{EvidenceKind, EvidenceSource};

fn entry(id: u64) -> TimelineEntry {
    TimelineEntry {
        activity_id: "root:turn:1:1".into(),
        identity: ActivityIdentity {
            agent_id: "root".into(),
            task_id: None,
            attempt_id: Some(1),
            thread_id: Some("parent".into()),
            turn_id: Some("turn".into()),
            generation: Some(1),
        },
        scope: ActivityScope::Turn,
        activity_kind: ActivityKind::ModelStreaming,
        execution_state: ExecutionState::Running,
        interaction_state: None,
        tool_category: None,
        item_id: Some("message".into()),
        evidence: Evidence {
            id,
            kind: EvidenceKind::Output,
            source: EvidenceSource::AppServer,
            recorded_at_ms: Some(1000),
            item_id: Some("message".into()),
            request_id: None,
            output_bytes: 1,
        },
        request: None,
        wait_targets: Vec::new(),
        compaction: None,
    }
}

#[test]
fn snapshots_share_entries_and_keep_their_original_range_after_eviction() {
    let mut timeline = Timeline::new("session".into());
    timeline.record(entry(1));
    let first = timeline.snapshot();
    let tick = timeline.snapshot();
    assert!(Arc::ptr_eq(&first.entries, &tick.entries));
    for id in 2..=ENTRY_LIMIT as u64 + 2 {
        timeline.record(entry(id));
    }
    let latest = timeline.snapshot();
    assert_eq!(first.entries.len(), 1);
    assert_eq!(first.entries[0].evidence.id, 1);
    assert_eq!(latest.entries.len(), ENTRY_LIMIT);
    assert_eq!(latest.entries[0].evidence.id, 3);
    assert_eq!(latest.dropped_entries, 2);
    assert_eq!(latest.high_water, ENTRY_LIMIT as u64 + 2);
    assert!(latest.retained_bytes <= BYTE_LIMIT);
}

#[test]
fn byte_budget_evicts_whole_records_and_reports_oversized_gaps() {
    let mut timeline = Timeline::new("session".into());
    for id in 1..=40 {
        let mut value = entry(id);
        value.identity.thread_id = Some("中".repeat(4000));
        timeline.record(value);
    }
    let kept = timeline.snapshot();
    assert!(kept.entries.len() < 40);
    assert!(kept.retained_bytes <= BYTE_LIMIT);
    assert!(kept.entries.iter().all(|entry| entry
        .identity
        .thread_id
        .as_ref()
        .unwrap()
        .chars()
        .count()
        == 4000));
    let mut oversized = entry(41);
    oversized.identity.thread_id = Some("x".repeat(BYTE_LIMIT));
    timeline.record(oversized);
    let gap = timeline.snapshot();
    assert_eq!(gap.entries, kept.entries);
    assert_eq!(gap.high_water, 41);
    assert_eq!(gap.dropped_entries, kept.dropped_entries + 1);
    timeline.record(entry(42));
    assert_eq!(timeline.snapshot().entries.back().unwrap().evidence.id, 42);
}

#[test]
fn compaction_fact_memory_is_counted_only_when_present() {
    let base = entry(1);
    let mut with_fact = base.clone();
    with_fact.compaction = Some(Box::new(crate::observation::CompactionFact {
        thread_id: "thread-id".into(),
        turn_id: "turn-id".into(),
        item_id: "item-id".into(),
        status: crate::observation::CompactionFactStatus::Started,
        started_at_ms: Some(1),
        completed_at_ms: None,
        input_tokens: Some(10),
        cached_input_tokens: None,
        output_tokens: None,
        total_tokens: None,
        context_window: None,
    }));
    let fact = with_fact.compaction.as_deref().unwrap();
    assert_eq!(
        with_fact.metadata_bytes() - base.metadata_bytes(),
        std::mem::size_of_val(fact)
            + fact.thread_id.capacity()
            + fact.turn_id.capacity()
            + fact.item_id.capacity()
    );
}
