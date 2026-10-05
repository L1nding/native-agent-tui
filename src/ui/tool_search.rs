//! 对当前保留的工具详情做只读、本地搜索。
use std::ops::Range;

use crate::protocol::ToolCategory;
use crate::tool_details::{ToolDetail, ToolDetailLocator, ToolDetailsSnapshot, ToolLifecycle};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Field {
    Command,
    Cwd,
    Parameters,
    Result,
    Output,
}

impl Field {
    pub fn text(self, detail: &ToolDetail) -> Option<&str> {
        match self {
            Self::Command => detail.command.as_deref(),
            Self::Cwd => detail.cwd.as_deref(),
            Self::Parameters => detail.parameters.as_deref(),
            Self::Result => detail.result.as_deref(),
            Self::Output => Some(&detail.output),
        }
    }
}

#[derive(Clone, PartialEq, Eq)]
pub(super) struct Hit {
    pub locator: ToolDetailLocator,
    pub field: Field,
    pub range: Range<usize>,
    pub revision: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum CategoryFilter {
    #[default]
    All,
    Shell,
    Other,
}

impl CategoryFilter {
    fn accepts(self, category: ToolCategory) -> bool {
        match self {
            Self::All => true,
            Self::Shell => category == ToolCategory::Shell,
            Self::Other => category != ToolCategory::Shell,
        }
    }

    pub fn next(self) -> Self {
        match self {
            Self::All => Self::Shell,
            Self::Shell => Self::Other,
            Self::Other => Self::All,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum LifecycleFilter {
    #[default]
    All,
    Running,
    Ended,
}

impl LifecycleFilter {
    fn accepts(self, lifecycle: ToolLifecycle) -> bool {
        match self {
            Self::All => true,
            Self::Running => lifecycle == ToolLifecycle::Running,
            Self::Ended => lifecycle != ToolLifecycle::Running,
        }
    }

    pub fn next(self) -> Self {
        match self {
            Self::All => Self::Running,
            Self::Running => Self::Ended,
            Self::Ended => Self::All,
        }
    }
}

pub(super) fn scan(
    snapshot: &ToolDetailsSnapshot,
    query: &str,
    allowed_threads: Option<&[String]>,
    turn: &str,
    category: CategoryFilter,
    lifecycle: LifecycleFilter,
    limit: usize,
) -> (Vec<Hit>, usize) {
    let mut hits = Vec::new();
    let mut total = 0;
    if query.is_empty() {
        return (hits, total);
    }
    for detail in snapshot.entries.iter() {
        if !category.accepts(detail.category) || !lifecycle.accepts(detail.lifecycle) {
            continue;
        }
        let detail_thread = detail
            .locator
            .identity
            .thread_id
            .as_deref()
            .unwrap_or_default();
        let detail_turn = detail
            .locator
            .identity
            .turn_id
            .as_deref()
            .unwrap_or_default();
        if allowed_threads
            .is_some_and(|threads| !threads.iter().any(|thread| thread == detail_thread))
            || (!turn.is_empty() && turn != detail_turn)
        {
            continue;
        }
        for field in [
            Field::Command,
            Field::Cwd,
            Field::Parameters,
            Field::Result,
            Field::Output,
        ] {
            let Some(text) = field.text(detail) else {
                continue;
            };
            for (start, _) in text.match_indices(query) {
                total += 1;
                if hits.len() < limit {
                    hits.push(Hit {
                        locator: detail.locator.clone(),
                        field,
                        range: start..start + query.len(),
                        revision: detail.revision,
                    });
                }
            }
        }
    }
    (hits, total)
}

pub(super) fn valid(hit: &Hit, current: &ToolDetailsSnapshot) -> bool {
    current.get(&hit.locator).is_some_and(|detail| {
        detail.revision == hit.revision
            && hit.field.text(&detail).is_some_and(|text| {
                hit.range.start <= hit.range.end
                    && hit.range.end <= text.len()
                    && text.is_char_boundary(hit.range.start)
                    && text.is_char_boundary(hit.range.end)
            })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::observation::ActivityIdentity;
    use crate::state::CoreSnapshot;
    use crate::tool_details::ToolTextSource;
    use std::collections::VecDeque;
    use std::sync::Arc;

    fn snapshot() -> CoreSnapshot {
        let detail = ToolDetail {
            locator: ToolDetailLocator {
                session_id: "session".into(),
                identity: ActivityIdentity {
                    agent_id: "root".into(),
                    task_id: None,
                    attempt_id: None,
                    thread_id: Some("thread".into()),
                    turn_id: Some("turn".into()),
                    generation: Some(1),
                },
                item_id: "item".into(),
            },
            revision: 4,
            category: ToolCategory::Shell,
            lifecycle: ToolLifecycle::Running,
            command: Some("echo 中文👋".into()),
            cwd: None,
            parameters: None,
            result: None,
            output: "中文👋".into(),
            output_source: ToolTextSource::OutputDelta,
            exit_code: None,
            duration_ms: None,
            bytes_observed: 0,
            bytes_retained: 0,
            clipped: false,
            authoritative: false,
        };
        let details = ToolDetailsSnapshot {
            entries: Arc::new(VecDeque::from([Arc::new(detail)])),
            ..Default::default()
        };
        CoreSnapshot {
            thread_id: Some("thread".into()),
            tool_details: details,
            ..Default::default()
        }
    }

    #[test]
    fn search_reports_field_utf8_ranges_and_applies_filters() {
        let snapshot = snapshot();
        let (hits, total) = scan(
            &snapshot.tool_details,
            "中文",
            Some(&["thread".into()]),
            "",
            CategoryFilter::All,
            LifecycleFilter::All,
            8,
        );
        assert_eq!(total, 2);
        assert_eq!(hits[0].field, Field::Command);
        assert_eq!(hits[0].range, 5..11);
        assert_eq!(hits[1].field, Field::Output);
        assert_eq!(hits[1].range, 0..6);
        assert!(scan(
            &snapshot.tool_details,
            "中文",
            Some(&["thread".into()]),
            "",
            CategoryFilter::Other,
            LifecycleFilter::All,
            8
        )
        .0
        .is_empty());
        assert!(scan(
            &snapshot.tool_details,
            "中文",
            Some(&["thread".into()]),
            "",
            CategoryFilter::All,
            LifecycleFilter::Ended,
            8
        )
        .0
        .is_empty());
    }

    #[test]
    fn hit_requires_the_same_retained_locator_revision_and_valid_range() {
        let snapshot = snapshot();
        let (hits, _) = scan(
            &snapshot.tool_details,
            "中文",
            Some(&["thread".into()]),
            "",
            CategoryFilter::All,
            LifecycleFilter::All,
            8,
        );
        assert!(valid(&hits[0], &snapshot.tool_details));
        let mut changed = snapshot.clone();
        let mut entries = (*changed.tool_details.entries).clone();
        Arc::make_mut(&mut entries[0]).revision += 1;
        changed.tool_details.entries = Arc::new(entries);
        assert!(!valid(&hits[0], &changed.tool_details));
        assert!(!valid(&hits[0], &ToolDetailsSnapshot::default()));
    }
}
