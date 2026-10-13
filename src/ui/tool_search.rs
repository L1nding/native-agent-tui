//! 对当前保留的工具详情做只读、本地搜索。
use std::ops::Range;

use super::lang::tr;
use crate::protocol::ToolCategory;
use crate::tool_details::{ToolDetail, ToolDetailLocator, ToolDetailsSnapshot, ToolLifecycle};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Field {
    Command,
    Cwd,
    Target,
    Parameters,
    Result,
    Output,
}

impl Field {
    pub fn text(self, detail: &ToolDetail) -> Option<&str> {
        match self {
            Self::Command => detail.command.as_deref(),
            Self::Cwd => detail.cwd.as_deref(),
            Self::Target => detail.target.as_deref(),
            Self::Parameters => detail.parameters.as_deref(),
            Self::Result => detail.result.as_deref(),
            Self::Output => Some(&detail.output),
        }
    }

    /// Name shown to the user; English is the variant name.
    pub fn label(self) -> &'static str {
        match self {
            Self::Command => tr("Command", "命令"),
            Self::Cwd => tr("Cwd", "工作目录"),
            Self::Target => tr("Target", "目标"),
            Self::Parameters => tr("Parameters", "参数"),
            Self::Result => tr("Result", "结果"),
            Self::Output => tr("Output", "输出"),
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

    /// Name shown to the user; English is the variant name.
    pub fn label(self) -> &'static str {
        match self {
            Self::All => tr("All", "全部"),
            Self::Shell => "Shell",
            Self::Other => tr("Other", "其他"),
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

    /// Name shown to the user; English is the variant name.
    pub fn label(self) -> &'static str {
        match self {
            Self::All => tr("All", "全部"),
            Self::Running => tr("Running", "运行中"),
            Self::Ended => tr("Ended", "已结束"),
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
            Field::Target,
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
            name: None,
            command: Some("echo 中文👋".into()),
            cwd: None,
            target: None,
            parameters: None,
            result: None,
            output: "中文👋".into(),
            output_source: ToolTextSource::OutputDelta,
            exit_code: None,
            duration_ms: None,
            diff: None,
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
    fn labels_are_the_variant_names_in_english_and_translated_in_chinese() {
        for field in [
            Field::Command,
            Field::Cwd,
            Field::Target,
            Field::Parameters,
            Field::Result,
            Field::Output,
        ] {
            assert_eq!(field.label(), format!("{field:?}"));
        }
        for filter in [
            CategoryFilter::All,
            CategoryFilter::Shell,
            CategoryFilter::Other,
        ] {
            assert_eq!(filter.label(), format!("{filter:?}"));
        }
        for filter in [
            LifecycleFilter::All,
            LifecycleFilter::Running,
            LifecycleFilter::Ended,
        ] {
            assert_eq!(filter.label(), format!("{filter:?}"));
        }
        crate::ui::lang::with_zh(|| {
            assert_eq!(Field::Cwd.label(), "工作目录");
            assert_eq!(CategoryFilter::Other.label(), "其他");
            assert_eq!(LifecycleFilter::Ended.label(), "已结束");
        });
    }

    #[test]
    fn generic_tool_targets_are_searchable_without_a_shell_command() {
        let mut snapshot = snapshot();
        let entries = std::sync::Arc::make_mut(&mut snapshot.tool_details.entries);
        let detail = std::sync::Arc::make_mut(entries.front_mut().unwrap());
        detail.category = ToolCategory::Dynamic;
        detail.command = None;
        detail.output.clear();
        detail.target = Some("src/目标👋.rs".into());
        let (hits, total) = scan(
            &snapshot.tool_details,
            "目标👋",
            None,
            "",
            CategoryFilter::Other,
            LifecycleFilter::All,
            8,
        );
        assert_eq!(total, 1);
        assert_eq!(hits[0].field, Field::Target);
        assert_eq!(hits[0].range, 4..14);
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
