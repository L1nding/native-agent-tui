//! Bounded live evidence archive owned by Core. Not a journal or an RPC transcript.
use std::collections::VecDeque;
use std::sync::Arc;

use crate::gate::WaitTarget;
use crate::interactions::RequestRef;
use crate::observation::{
    ActivityIdentity, ActivityKind, ActivityScope, Evidence, ExecutionState, InteractionState,
};
use crate::protocol::{RpcId, ToolCategory};

pub const ENTRY_LIMIT: usize = 512;
pub const BYTE_LIMIT: usize = 256 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TimelineEntry {
    pub activity_id: String,
    pub identity: ActivityIdentity,
    pub scope: ActivityScope,
    pub activity_kind: ActivityKind,
    pub execution_state: ExecutionState,
    pub interaction_state: Option<InteractionState>,
    pub tool_category: Option<ToolCategory>,
    pub item_id: Option<String>,
    pub evidence: Evidence,
    pub request: Option<RequestRef>,
    pub wait_targets: Vec<WaitTarget>,
}

impl TimelineEntry {
    /// Owned metadata allocation budget; excludes message bodies and request details.
    pub fn metadata_bytes(&self) -> usize {
        fn rpc_bytes(id: &RpcId) -> usize {
            match id {
                RpcId::String(text) => text.capacity(),
                RpcId::Number(_) => 0,
            }
        }
        fn text_bytes(text: &Option<String>) -> usize {
            text.as_ref().map_or(0, String::capacity)
        }
        std::mem::size_of::<Self>()
            + 2 * std::mem::size_of::<usize>()
            + self.activity_id.capacity()
            + self.identity.agent_id.capacity()
            + text_bytes(&self.identity.thread_id)
            + text_bytes(&self.identity.turn_id)
            + text_bytes(&self.item_id)
            + text_bytes(&self.evidence.item_id)
            + self.evidence.request_id.as_ref().map_or(0, rpc_bytes)
            + self.request.as_ref().map_or(0, |request| {
                rpc_bytes(&request.id) + request.thread_id.capacity() + request.turn_id.capacity()
            })
            + self.wait_targets.capacity() * std::mem::size_of::<WaitTarget>()
            + self
                .wait_targets
                .iter()
                .map(|target| target.id.capacity() + text_bytes(&target.turn_id))
                .sum::<usize>()
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TimelineSnapshot {
    pub session_id: String,
    /// Evidence acceptance sequence; not ingress sequence or a durable cursor.
    pub high_water: u64,
    pub dropped_entries: u64,
    pub retained_bytes: usize,
    pub entries: Arc<VecDeque<Arc<TimelineEntry>>>,
}

#[derive(Default)]
pub(crate) struct Timeline {
    view: TimelineSnapshot,
}

impl Timeline {
    pub fn new(session_id: String) -> Self {
        Self {
            view: TimelineSnapshot {
                session_id,
                ..Default::default()
            },
        }
    }

    pub fn record(&mut self, entry: TimelineEntry) {
        let bytes = entry.metadata_bytes();
        self.view.high_water = entry.evidence.id;
        // Keep complete identities. A single oversized record creates a visible gap.
        if bytes > BYTE_LIMIT {
            self.view.dropped_entries = self.view.dropped_entries.saturating_add(1);
            return;
        }
        // Snapshots share immutable entries; copy at most 512 Arc pointers on change.
        let entries = Arc::make_mut(&mut self.view.entries);
        while entries.len() >= ENTRY_LIMIT || self.view.retained_bytes + bytes > BYTE_LIMIT {
            let old = entries.pop_front().expect("a budget eviction has an entry");
            self.view.retained_bytes -= old.metadata_bytes();
            self.view.dropped_entries = self.view.dropped_entries.saturating_add(1);
        }
        entries.push_back(Arc::new(entry));
        self.view.retained_bytes += bytes;
    }

    pub fn snapshot(&self) -> TimelineSnapshot {
        self.view.clone()
    }
}

#[cfg(test)]
mod tests;
