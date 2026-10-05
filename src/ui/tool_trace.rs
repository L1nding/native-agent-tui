//! 从 Core 保留的时间线投影实时工具生命周期元数据。
use crate::observation::{ActivityScope, EvidenceKind};
use crate::state::CoreSnapshot;
use crate::tool_details::ToolDetailLocator;
use std::sync::Arc;

pub(super) fn lines(locator: &ToolDetailLocator, current: &CoreSnapshot) -> Vec<String> {
    let mut entries = matching_entries(locator, current);
    if entries.is_empty() {
        return unavailable(current);
    }
    entries.sort_by_key(|entry| entry.evidence.id);

    entries
        .into_iter()
        .map(|entry| {
            let kind = match entry.evidence.kind {
                EvidenceKind::ToolStarted => "ToolStarted",
                EvidenceKind::Output => "Output",
                EvidenceKind::ToolCompleted => "ToolCompleted",
                EvidenceKind::ExecutionUnknown => "ExecutionUnknown",
                _ => unreachable!("filtered to tool trace evidence"),
            };
            format!(
                "Evidence #{} · {} · state={:?} · output_bytes={} · recorded_at_ms={}",
                entry.evidence.id,
                kind,
                entry.execution_state,
                entry.evidence.output_bytes,
                entry
                    .evidence
                    .recorded_at_ms
                    .map_or_else(|| "unavailable".into(), |time| time.to_string())
            )
        })
        .collect()
}

pub(super) fn has_matching_metadata(locator: &ToolDetailLocator, current: &CoreSnapshot) -> bool {
    !matching_entries(locator, current).is_empty()
}

fn matching_entries<'a>(
    locator: &ToolDetailLocator,
    current: &'a CoreSnapshot,
) -> Vec<&'a crate::timeline::TimelineEntry> {
    if current.timeline.session_id != locator.session_id {
        return Vec::new();
    }

    current
        .timeline
        .entries
        .iter()
        .map(Arc::as_ref)
        .filter(|entry| {
            entry.scope == ActivityScope::Tool
                && entry.identity == locator.identity
                && entry.item_id.as_deref() == Some(locator.item_id.as_str())
                && matches!(
                    entry.evidence.kind,
                    EvidenceKind::ToolStarted
                        | EvidenceKind::Output
                        | EvidenceKind::ToolCompleted
                        | EvidenceKind::ExecutionUnknown
                )
        })
        .collect()
}

fn unavailable(current: &CoreSnapshot) -> Vec<String> {
    let reason = if current.timeline.dropped_entries > 0 {
        "unavailable: matching evidence is absent or evicted from the retained timeline"
    } else {
        "unavailable: no matching evidence in the retained timeline"
    };
    vec![format!("Tool trace: {reason}.")]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::observation::{
        ActivityIdentity, ActivityKind, Evidence, EvidenceSource, ExecutionState,
    };
    use crate::timeline::{TimelineEntry, TimelineSnapshot};
    use crate::tool_details::ToolDetailLocator;
    use std::sync::Arc;

    fn locator() -> ToolDetailLocator {
        ToolDetailLocator {
            session_id: "session-a".into(),
            identity: ActivityIdentity {
                agent_id: "agent-a".into(),
                task_id: None,
                attempt_id: Some(3),
                thread_id: Some("thread-a".into()),
                turn_id: Some("turn-a".into()),
                generation: Some(7),
            },
            item_id: "item-a".into(),
        }
    }

    fn entry(id: u64, kind: EvidenceKind) -> Arc<TimelineEntry> {
        let locator = locator();
        Arc::new(TimelineEntry {
            activity_id: "tool-a".into(),
            identity: locator.identity,
            scope: ActivityScope::Tool,
            activity_kind: ActivityKind::ToolRunning,
            execution_state: if kind == EvidenceKind::ToolCompleted {
                ExecutionState::Completed
            } else {
                ExecutionState::Running
            },
            interaction_state: None,
            tool_category: Some(crate::protocol::ToolCategory::Shell),
            item_id: Some(locator.item_id.clone()),
            evidence: Evidence {
                id,
                kind,
                source: EvidenceSource::AppServer,
                recorded_at_ms: Some(id * 10),
                // Core 终态证据本身没有 item_id；TimelineEntry 从活动保留该关联。
                item_id: (kind != EvidenceKind::ToolCompleted).then_some(locator.item_id),
                request_id: None,
                output_bytes: if kind == EvidenceKind::Output { 12 } else { 0 },
            },
            request: None,
            wait_targets: Vec::new(),
            compaction: None,
        })
    }

    fn snapshot(entries: Vec<Arc<TimelineEntry>>) -> CoreSnapshot {
        CoreSnapshot {
            timeline: TimelineSnapshot {
                session_id: "session-a".into(),
                entries: Arc::new(entries.into()),
                ..Default::default()
            },
            ..Default::default()
        }
    }

    #[test]
    fn tool_trace_preserves_started_output_and_completed_order() {
        let current = snapshot(vec![
            entry(1, EvidenceKind::ToolStarted),
            entry(2, EvidenceKind::Output),
            entry(3, EvidenceKind::Output),
            entry(4, EvidenceKind::ToolCompleted),
        ]);

        let rendered = lines(&locator(), &current);
        assert_eq!(rendered.len(), 4);
        assert!(rendered[0].contains("Evidence #1 · ToolStarted"));
        assert!(rendered[1].contains("Evidence #2 · Output"));
        assert!(rendered[2].contains("Evidence #3 · Output"));
        assert!(rendered[3].contains("Evidence #4 · ToolCompleted"));
        assert!(rendered[1].contains("state=Running · output_bytes=12 · recorded_at_ms=20"));
    }

    #[test]
    fn tool_trace_shows_unknown_tool_completion_without_evidence_item_id() {
        let mut unknown = (*entry(4, EvidenceKind::ToolCompleted)).clone();
        unknown.execution_state = ExecutionState::Unknown;
        unknown.activity_kind = ActivityKind::Unknown;
        unknown.evidence.item_id = None;

        let rendered = lines(&locator(), &snapshot(vec![Arc::new(unknown)]));
        assert_eq!(rendered.len(), 1);
        assert!(rendered[0].contains("ToolCompleted"));
        assert!(rendered[0].contains("state=Unknown"));
    }

    #[test]
    fn tool_trace_can_match_metadata_when_evidence_item_id_is_absent() {
        let mut completed = (*entry(1, EvidenceKind::ToolCompleted)).clone();
        completed.evidence.item_id = None;
        let current = snapshot(vec![Arc::new(completed)]);

        assert!(has_matching_metadata(&locator(), &current));
        assert!(lines(&locator(), &current)[0].contains("ToolCompleted"));
    }

    #[test]
    fn tool_trace_does_not_mix_other_items_or_turns() {
        let mut other_item = (*entry(1, EvidenceKind::ToolStarted)).clone();
        other_item.item_id = Some("item-b".into());
        other_item.evidence.item_id = Some("item-b".into());
        let mut other_turn = (*entry(2, EvidenceKind::Output)).clone();
        other_turn.identity.turn_id = Some("turn-b".into());
        let current = snapshot(vec![Arc::new(other_item), Arc::new(other_turn)]);

        assert!(lines(&locator(), &current)[0].contains("no matching evidence"));
    }

    #[test]
    fn tool_trace_marks_missing_or_evicted_evidence_unavailable() {
        let mut current = snapshot(Vec::new());
        assert!(lines(&locator(), &current)[0].contains("no matching evidence"));
        current.timeline.dropped_entries = 1;
        assert!(lines(&locator(), &current)[0].contains("absent or evicted"));
        current.timeline.session_id = "another-session".into();
        assert!(lines(&locator(), &current)[0].contains("absent or evicted"));
    }

    #[test]
    fn narrow_tool_detail_renders_trace_without_changing_core_facts() {
        use ratatui::backend::TestBackend;
        use ratatui::layout::Rect;
        use ratatui::Terminal;

        let current = snapshot(vec![
            entry(1, EvidenceKind::ToolStarted),
            entry(2, EvidenceKind::Output),
            entry(3, EvidenceKind::ToolCompleted),
        ]);
        let before = current.clone();
        let mut terminal = Terminal::new(TestBackend::new(28, 8)).unwrap();
        let mut max_scroll = 0;
        terminal
            .draw(|frame| {
                max_scroll = super::super::tool_detail::draw(
                    frame,
                    Rect::new(0, 0, 28, 8),
                    &current,
                    &locator(),
                    0,
                    None,
                );
            })
            .unwrap();
        terminal
            .draw(|frame| {
                super::super::tool_detail::draw(
                    frame,
                    Rect::new(0, 0, 28, 8),
                    &current,
                    &locator(),
                    max_scroll,
                    None,
                );
            })
            .unwrap();

        let buffer = terminal.backend().buffer();
        let text: String = (0..buffer.area.height)
            .flat_map(|y| (0..buffer.area.width).map(move |x| (x, y)))
            .map(|(x, y)| buffer[(x, y)].symbol())
            .collect();
        assert!(text.contains("Live tool trace") || text.contains("Evidence #"));
        assert!(text.contains("Evidence"));
        assert_eq!(current, before);
    }
}
