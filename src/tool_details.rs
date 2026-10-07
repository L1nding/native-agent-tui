//! 有界的实时工具详情缓存。只由 Core 持有，不进入 journal、history 或诊断。
use std::collections::{BTreeSet, VecDeque};
use std::fmt;
use std::sync::Arc;

use crate::observation::ActivityIdentity;
use crate::protocol::{ObservedToolDetails, ToolCategory};

pub const ENTRY_LIMIT: usize = 128;
pub const TOTAL_BYTES: usize = 512 * 1024;
pub const ITEM_BYTES: usize = 64 * 1024;
const RETIRED_TURN_LIMIT: usize = 128;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolDetailLocator {
    pub session_id: String,
    pub identity: ActivityIdentity,
    pub item_id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolLifecycle {
    Running,
    Completed,
    Failed,
    Interrupted,
    EndedUnknown,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolTextSource {
    CommandAggregate,
    OutputDelta,
    Unavailable,
}

#[derive(Clone, PartialEq, Eq)]
pub struct ToolDetail {
    pub locator: ToolDetailLocator,
    /// 内容或生命周期变化时递增，用于拒绝过期搜索命中。
    pub revision: u64,
    pub category: ToolCategory,
    pub lifecycle: ToolLifecycle,
    pub command: Option<String>,
    pub cwd: Option<String>,
    pub parameters: Option<String>,
    pub result: Option<String>,
    pub output: String,
    pub output_source: ToolTextSource,
    pub exit_code: Option<i64>,
    pub duration_ms: Option<u64>,
    pub bytes_observed: usize,
    pub bytes_retained: usize,
    pub clipped: bool,
    pub authoritative: bool,
}

impl ToolDetail {
    fn bytes(&self) -> usize {
        std::mem::size_of::<Self>()
            + self.locator.session_id.capacity()
            + self.locator.identity.agent_id.capacity()
            + self
                .locator
                .identity
                .thread_id
                .as_ref()
                .map_or(0, String::capacity)
            + self
                .locator
                .identity
                .turn_id
                .as_ref()
                .map_or(0, String::capacity)
            + self.locator.item_id.capacity()
            + self.command.as_ref().map_or(0, String::capacity)
            + self.cwd.as_ref().map_or(0, String::capacity)
            + self.parameters.as_ref().map_or(0, String::capacity)
            + self.result.as_ref().map_or(0, String::capacity)
            + self.output.capacity()
    }
}

/// 对话中显示的一行工具摘要：状态、退出码、耗时和命令首行（最多 96 字符）。
pub fn summary_line(detail: &ToolDetail) -> String {
    let what = detail
        .command
        .as_deref()
        .map(crate::protocol::shell_script)
        .and_then(|command| command.lines().find(|line| !line.trim().is_empty()))
        .map(|line| {
            let line = line.trim();
            match line.char_indices().nth(96) {
                Some((end, _)) => format!("{}…", &line[..end]),
                None => line.to_owned(),
            }
        })
        .unwrap_or_else(|| format!("{:?}", detail.category));
    let state = match detail.lifecycle {
        ToolLifecycle::Running => "running",
        ToolLifecycle::Completed => "done",
        ToolLifecycle::Failed => "failed",
        ToolLifecycle::Interrupted => "interrupted",
        ToolLifecycle::EndedUnknown => "ended, outcome unknown",
        ToolLifecycle::Unknown => "unknown",
    };
    // 状态在前：命令很长而换行时，状态和退出码仍在第一行可见。
    let mut line = format!("▸ {state}");
    if let Some(code) = detail.exit_code {
        line.push_str(&format!(" · exit {code}"));
    }
    if let Some(ms) = detail.duration_ms {
        line.push_str(&format!(" · {:.1}s", ms as f64 / 1000.0));
    }
    line.push_str(&format!(" · {what}"));
    crate::state::display_text(&line)
}

#[derive(Clone, Default, PartialEq, Eq)]
pub struct ToolDetailsSnapshot {
    pub entries: Arc<VecDeque<Arc<ToolDetail>>>,
    pub retained_bytes: usize,
    pub dropped_entries: u64,
}

impl fmt::Debug for ToolDetailsSnapshot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // CoreSnapshot 可安全用于调试；不要把命令、参数或工具结果写入日志。
        f.debug_struct("ToolDetailsSnapshot")
            .field("entry_count", &self.entries.len())
            .field("retained_bytes", &self.retained_bytes)
            .field("dropped_entries", &self.dropped_entries)
            .finish()
    }
}

impl ToolDetailsSnapshot {
    pub fn get(&self, locator: &ToolDetailLocator) -> Option<Arc<ToolDetail>> {
        self.entries
            .iter()
            .find(|entry| entry.locator == *locator)
            .cloned()
    }
}

#[derive(Default)]
pub(crate) struct ToolDetails {
    entries: VecDeque<Arc<ToolDetail>>,
    retained_bytes: usize,
    dropped_entries: u64,
    retired: BTreeSet<(String, String, String)>,
    retired_order: VecDeque<(String, String, String)>,
}

impl ToolDetails {
    pub fn get(&self, locator: &ToolDetailLocator) -> Option<Arc<ToolDetail>> {
        self.entries
            .iter()
            .find(|entry| entry.locator == *locator)
            .cloned()
    }

    pub fn snapshot(&self) -> ToolDetailsSnapshot {
        ToolDetailsSnapshot {
            entries: Arc::new(self.entries.clone()),
            retained_bytes: self.retained_bytes,
            dropped_entries: self.dropped_entries,
        }
    }

    pub fn observe_started(
        &mut self,
        locator: ToolDetailLocator,
        category: ToolCategory,
        fields: ObservedToolDetails,
    ) {
        if self.is_retired(&locator) {
            return;
        }
        if let Some(existing) = self.find_mut(&locator) {
            // Duplicate started events never reset accumulated output.
            if matches!(
                existing.lifecycle,
                ToolLifecycle::Unknown
                    | ToolLifecycle::Completed
                    | ToolLifecycle::Failed
                    | ToolLifecycle::Interrupted
                    | ToolLifecycle::EndedUnknown
            ) {
                return;
            }
            let changed = (existing.command.is_none() && fields.command.is_some())
                || (existing.cwd.is_none() && fields.cwd.is_some())
                || (existing.parameters.is_none() && fields.parameters.is_some());
            if existing.command.is_none() {
                existing.command = fields.command;
            }
            if existing.cwd.is_none() {
                existing.cwd = fields.cwd;
            }
            if existing.parameters.is_none() {
                existing.parameters = fields.parameters;
            }
            normalize_detail(existing);
            if changed {
                existing.revision = existing.revision.saturating_add(1);
            }
            self.rebalance();
            return;
        }
        let detail = ToolDetail {
            locator,
            revision: 1,
            category,
            lifecycle: ToolLifecycle::Running,
            command: fields.command,
            cwd: fields.cwd,
            parameters: fields.parameters,
            result: None,
            output: String::new(),
            output_source: ToolTextSource::Unavailable,
            exit_code: None,
            duration_ms: None,
            bytes_observed: 0,
            bytes_retained: 0,
            clipped: false,
            authoritative: false,
        };
        self.insert(detail);
    }

    pub fn observe_completed(
        &mut self,
        locator: &ToolDetailLocator,
        category: ToolCategory,
        lifecycle: ToolLifecycle,
        fields: ObservedToolDetails,
        authoritative: bool,
    ) {
        if self.is_retired(locator) {
            return;
        }
        let Some(detail) = self.find_mut(locator) else {
            let mut detail = ToolDetail {
                locator: locator.clone(),
                revision: 1,
                category,
                lifecycle,
                command: fields.command,
                cwd: fields.cwd,
                parameters: fields.parameters,
                result: fields.result,
                output: String::new(),
                output_source: ToolTextSource::Unavailable,
                exit_code: fields.exit_code,
                duration_ms: fields.duration_ms,
                bytes_observed: 0,
                bytes_retained: 0,
                clipped: false,
                authoritative,
            };
            if category == ToolCategory::Shell {
                detail.result = None;
            }
            if let Some(output) = fields.output {
                detail.bytes_observed = output.len();
                detail.output = output;
                detail.output_source = ToolTextSource::CommandAggregate;
                detail.authoritative = true;
            }
            normalize_detail(&mut detail);
            self.insert(detail);
            return;
        };
        if matches!(
            detail.lifecycle,
            ToolLifecycle::Completed
                | ToolLifecycle::Failed
                | ToolLifecycle::Interrupted
                | ToolLifecycle::EndedUnknown
                | ToolLifecycle::Unknown
        ) {
            return;
        }
        let next_result = if category == ToolCategory::Shell {
            None
        } else {
            fields.result.as_deref()
        };
        let changed = detail.lifecycle != lifecycle
            || detail.result.as_deref() != next_result
            || (detail.command.is_none() && fields.command.is_some())
            || (detail.cwd.is_none() && fields.cwd.is_some())
            || (detail.parameters.is_none() && fields.parameters.is_some())
            || fields.exit_code != detail.exit_code
            || fields.duration_ms != detail.duration_ms
            || authoritative != detail.authoritative
            || (fields.output.as_ref().is_some_and(|output| {
                detail.output != *output || detail.output_source != ToolTextSource::CommandAggregate
            }));
        detail.lifecycle = lifecycle;
        detail.result = if category == ToolCategory::Shell {
            None
        } else {
            fields.result
        };
        if detail.command.is_none() {
            detail.command = fields.command;
        }
        if detail.cwd.is_none() {
            detail.cwd = fields.cwd;
        }
        if detail.parameters.is_none() {
            detail.parameters = fields.parameters;
        }
        detail.exit_code = fields.exit_code;
        detail.duration_ms = fields.duration_ms;
        detail.authoritative = authoritative;
        if let Some(output) = fields.output {
            detail.bytes_observed = detail.bytes_observed.saturating_add(output.len());
            detail.output = output;
            detail.output_source = ToolTextSource::CommandAggregate;
            detail.authoritative = true;
        }
        normalize_detail(detail);
        if changed {
            detail.revision = detail.revision.saturating_add(1);
        }
        self.rebalance();
    }

    pub fn observe_output(&mut self, locator: &ToolDetailLocator, text: &str) {
        if self.is_retired(locator) {
            return;
        }
        let Some(detail) = self.find_mut(locator) else {
            return;
        };
        if matches!(
            detail.lifecycle,
            ToolLifecycle::Completed
                | ToolLifecycle::Failed
                | ToolLifecycle::Interrupted
                | ToolLifecycle::EndedUnknown
                | ToolLifecycle::Unknown
        ) {
            return;
        }
        let old_len = detail.output.len();
        append_text(detail, text);
        normalize_detail(detail);
        if detail.output.len() != old_len {
            detail.revision = detail.revision.saturating_add(1);
        }
        self.rebalance();
    }

    pub fn execution_unavailable(&mut self) {
        for entry in &mut self.entries {
            let detail = Arc::make_mut(entry);
            if detail.lifecycle == ToolLifecycle::Running {
                detail.lifecycle = ToolLifecycle::Unknown;
                detail.revision = detail.revision.saturating_add(1);
                normalize_detail(detail);
            }
        }
        self.rebalance();
    }

    pub fn retire(&mut self, session: &str, thread: &str, turn: &str) {
        let key = (session.to_owned(), thread.to_owned(), turn.to_owned());
        if self.retired.insert(key.clone()) {
            self.retired_order.push_back(key);
            while self.retired_order.len() > RETIRED_TURN_LIMIT {
                if let Some(expired) = self.retired_order.pop_front() {
                    self.retired.remove(&expired);
                }
            }
        }
        self.entries.retain(|entry| {
            entry.locator.session_id != session
                || entry.locator.identity.thread_id.as_deref() != Some(thread)
                || entry.locator.identity.turn_id.as_deref() != Some(turn)
        });
        self.rebalance();
    }

    fn is_retired(&self, locator: &ToolDetailLocator) -> bool {
        self.retired.contains(&(
            locator.session_id.clone(),
            locator.identity.thread_id.clone().unwrap_or_default(),
            locator.identity.turn_id.clone().unwrap_or_default(),
        ))
    }

    fn find_mut(&mut self, locator: &ToolDetailLocator) -> Option<&mut ToolDetail> {
        self.entries
            .iter_mut()
            .find_map(|entry| (entry.locator == *locator).then(|| Arc::make_mut(entry)))
    }

    fn insert(&mut self, detail: ToolDetail) {
        let mut detail = detail;
        normalize_detail(&mut detail);
        let bytes = detail.bytes();
        if bytes > ITEM_BYTES {
            self.dropped_entries = self.dropped_entries.saturating_add(1);
            return;
        }
        self.entries.push_back(Arc::new(detail));
        self.rebalance();
    }

    fn rebalance(&mut self) {
        let mut oversized = 0;
        self.entries.retain(|entry| {
            let keep = entry.bytes() <= ITEM_BYTES;
            if !keep {
                oversized += 1;
            }
            keep
        });
        self.dropped_entries = self.dropped_entries.saturating_add(oversized);
        self.retained_bytes = self.entries.iter().map(|entry| entry.bytes()).sum();
        while self.entries.len() > ENTRY_LIMIT || self.retained_bytes > TOTAL_BYTES {
            let Some(old) = self.entries.pop_front() else {
                break;
            };
            self.retained_bytes = self.retained_bytes.saturating_sub(old.bytes());
            self.dropped_entries = self.dropped_entries.saturating_add(1);
        }
    }
}

fn append_text(detail: &mut ToolDetail, text: &str) {
    detail.bytes_observed = detail.bytes_observed.saturating_add(text.len());
    let room = ITEM_BYTES.saturating_sub(detail.bytes());
    let mut end = text.len().min(room);
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    detail.output.push_str(&text[..end]);
    detail.clipped |= end < text.len();
    detail.output_source = ToolTextSource::OutputDelta;
}

fn normalize_detail(detail: &mut ToolDetail) {
    for value in [
        &mut detail.command,
        &mut detail.cwd,
        &mut detail.parameters,
        &mut detail.result,
    ]
    .into_iter()
    .flatten()
    {
        let end = clipped_end(value, 8 * 1024);
        if end < value.len() {
            detail.clipped = true;
        }
        *value = value[..end].to_owned();
    }
    let fixed = detail.bytes().saturating_sub(detail.output.capacity());
    let max_output = ITEM_BYTES.saturating_sub(fixed);
    let end = clipped_end(&detail.output, max_output);
    if end < detail.output.len() {
        detail.clipped = true;
    }
    detail.output = detail.output[..end].to_owned();
    if detail.bytes() > ITEM_BYTES {
        // 重新分配后仍超限时直接丢弃正文，避免逐字符缩容造成无界工作。
        detail.output = String::new();
        detail.clipped = true;
    }
    detail.bytes_retained = detail.output.len();
}

fn clipped_end(text: &str, budget: usize) -> usize {
    let mut end = text.len().min(budget);
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    end
}

#[cfg(test)]
mod tests {
    use super::*;

    fn locator(item: &str) -> ToolDetailLocator {
        ToolDetailLocator {
            session_id: "session".into(),
            identity: ActivityIdentity {
                agent_id: "root".into(),
                task_id: None,
                attempt_id: Some(1),
                thread_id: Some("thread".into()),
                turn_id: Some("turn".into()),
                generation: Some(1),
            },
            item_id: item.into(),
        }
    }

    #[test]
    fn duplicate_started_and_identical_deltas_are_retained() {
        let mut store = ToolDetails::default();
        let key = locator("one");
        store.observe_started(
            key.clone(),
            ToolCategory::Shell,
            ObservedToolDetails {
                command: Some("echo".into()),
                ..Default::default()
            },
        );
        let initial_revision = store.entries.back().unwrap().revision;
        store.observe_started(
            key.clone(),
            ToolCategory::Shell,
            ObservedToolDetails {
                command: Some("reset".into()),
                ..Default::default()
            },
        );
        assert_eq!(store.entries.back().unwrap().revision, initial_revision);
        store.observe_output(&key, "同一块");
        let first_output_revision = store.entries.back().unwrap().revision;
        store.observe_output(&key, "同一块");
        assert_eq!(store.entries.back().unwrap().output, "同一块同一块");
        assert_eq!(
            store.entries.back().unwrap().revision,
            first_output_revision + 1
        );
    }

    #[test]
    fn late_output_cannot_overwrite_terminal_and_budget_clips_utf8() {
        let mut store = ToolDetails::default();
        let key = locator("one");
        store.observe_started(
            key.clone(),
            ToolCategory::Shell,
            ObservedToolDetails::default(),
        );
        store.observe_output(&key, &"中".repeat(40_000));
        store.observe_completed(
            &key,
            ToolCategory::Shell,
            ToolLifecycle::Failed,
            ObservedToolDetails {
                result: Some("exit".into()),
                output: Some("final".into()),
                exit_code: Some(1),
                ..Default::default()
            },
            true,
        );
        store.observe_output(&key, "late");
        let detail = store.entries.back().unwrap();
        assert!(detail.clipped);
        assert!(!detail.output.contains("late"));
        assert_eq!(detail.lifecycle, ToolLifecycle::Failed);
        assert_eq!(detail.output, "final");
    }

    #[test]
    fn aggregate_utf8_output_budget_evicts_with_fewer_than_entry_limit() {
        let mut store = ToolDetails::default();
        let text = "界".repeat(21_000);
        let input_count = 9;

        for index in 0..input_count {
            let key = locator(&format!("large-{index}"));
            store.observe_started(
                key.clone(),
                ToolCategory::Shell,
                ObservedToolDetails::default(),
            );
            store.observe_output(&key, &text);

            let snapshot = store.snapshot();
            assert!(snapshot.entries.len() < ENTRY_LIMIT);
            assert!(snapshot.retained_bytes <= TOTAL_BYTES);
            assert!(snapshot
                .entries
                .iter()
                .all(|entry| entry.bytes() <= ITEM_BYTES));
        }

        let snapshot = store.snapshot();
        assert!(snapshot.entries.len() < input_count);
        assert_eq!(
            snapshot.dropped_entries,
            (input_count - snapshot.entries.len()) as u64
        );
        assert!(snapshot.retained_bytes <= TOTAL_BYTES);
        assert!(snapshot.entries.iter().all(|entry| {
            entry.bytes() <= ITEM_BYTES
                && entry.bytes_retained == entry.output.len()
                && entry.output.is_char_boundary(entry.output.len())
        }));
    }

    #[test]
    fn retired_turn_tombstone_blocks_late_first_item_and_unknown_is_terminal() {
        let mut store = ToolDetails::default();
        let key = locator("late");
        store.retire("session", "thread", "turn");
        store.observe_started(
            key.clone(),
            ToolCategory::Shell,
            ObservedToolDetails::default(),
        );
        assert!(store.entries.is_empty());

        let mut key = locator("unknown");
        key.identity.turn_id = Some("other-turn".into());
        store.observe_started(
            key.clone(),
            ToolCategory::Shell,
            ObservedToolDetails::default(),
        );
        store.observe_completed(
            &key,
            ToolCategory::Shell,
            ToolLifecycle::Unknown,
            ObservedToolDetails::default(),
            false,
        );
        store.observe_started(
            key.clone(),
            ToolCategory::Shell,
            ObservedToolDetails::default(),
        );
        store.observe_output(&key, "late delta");
        assert_eq!(
            store.entries.back().unwrap().lifecycle,
            ToolLifecycle::Unknown
        );
        assert!(store.entries.back().unwrap().output.is_empty());
    }

    #[test]
    fn retired_turn_tombstone_window_evicts_only_the_oldest_turn() {
        let mut store = ToolDetails::default();
        let mut old_turn = locator("late-old-turn");
        old_turn.identity.turn_id = Some("turn-1".into());
        store.retire("session", "thread", "turn-1");
        assert!(store.is_retired(&old_turn));

        for generation in 2..=129 {
            store.retire("session", "thread", &format!("turn-{generation}"));
        }

        let mut second_turn = old_turn.clone();
        second_turn.identity.turn_id = Some("turn-2".into());
        let mut newest_turn = old_turn.clone();
        newest_turn.identity.turn_id = Some("turn-129".into());
        assert_eq!(store.retired_order.len(), RETIRED_TURN_LIMIT);
        assert!(!store.is_retired(&old_turn));
        assert!(store.is_retired(&second_turn));
        assert!(store.is_retired(&newest_turn));
        assert_eq!(store.retired.len(), RETIRED_TURN_LIMIT);
    }

    #[test]
    fn retirement_removes_cached_details_without_mutating_an_older_snapshot() {
        let mut store = ToolDetails::default();
        let key = locator("running");
        store.observe_started(
            key.clone(),
            ToolCategory::Shell,
            ObservedToolDetails::default(),
        );
        store.observe_output(&key, "private output");
        let before_retirement = store.snapshot();

        store.retire("session", "thread", "turn");

        let after_retirement = store.snapshot();
        let old_detail = before_retirement.get(&key).unwrap();
        assert_eq!(old_detail.lifecycle, ToolLifecycle::Running);
        assert_eq!(old_detail.output, "private output");
        assert!(after_retirement.get(&key).is_none());
        assert!(after_retirement.retained_bytes <= TOTAL_BYTES);
    }

    #[test]
    fn completed_fields_fill_missing_started_fields_and_keep_budget_bounded() {
        let mut store = ToolDetails::default();
        let key = locator("without-start");
        store.observe_completed(
            &key,
            ToolCategory::Shell,
            ToolLifecycle::Completed,
            ObservedToolDetails {
                command: Some("echo 中文".into()),
                cwd: Some("C:/workspace".into()),
                output: Some("done".into()),
                ..Default::default()
            },
            true,
        );

        let detail = store.snapshot().get(&key).unwrap();
        assert_eq!(detail.command.as_deref(), Some("echo 中文"));
        assert_eq!(detail.cwd.as_deref(), Some("C:/workspace"));
        assert_eq!(detail.output, "done");
        assert!(detail.bytes() <= ITEM_BYTES);
        assert!(store.retained_bytes <= TOTAL_BYTES);
    }
}
