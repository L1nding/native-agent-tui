use std::sync::Arc;

use crate::agents::{AgentRegistry, AgentSnapshot};
use crate::gate::WaitTarget;
use crate::interactions::RequestView;

pub const MESSAGE_BYTES: usize = 32 * 1024;
pub const HISTORY_BYTES: usize = 256 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionPhase {
    Created,
    Launching,
    Initializing,
    CheckingShell,
    Ready,
    StartingTurn,
    Running,
    GatePending,
    Completed,
    Interrupted,
    Stopping,
    ClosingTransport,
    Stopped,
    Disconnected,
    Failed,
    Unknown,
}

impl SessionPhase {
    pub fn can_submit(self) -> bool {
        matches!(
            self,
            Self::Ready | Self::Completed | Self::Interrupted | Self::Failed
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConversationItem {
    pub id: String,
    pub thread_id: String,
    pub turn_id: String,
    pub role: String,
    pub text: String,
    pub complete: bool,
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoreSnapshot {
    pub version: u64,
    pub phase: SessionPhase,
    pub root_turn_count: u64,
    pub gate: Option<GateSnapshot>,
    pub root_start_requests: u64,
    pub queued_inputs: usize,
    pub agents: Vec<AgentSnapshot>,
    pub thread_id: Option<String>,
    pub turn_id: Option<String>,
    pub model: Option<String>,
    pub cwd: String,
    pub sandbox: String,
    pub approval_policy: String,
    pub messages: Vec<ConversationItem>,
    pub requests: Vec<RequestView>,
    pub notice: Option<String>,
    pub last_error: Option<String>,
    pub tool_activity: Option<String>,
    pub total_tokens: Option<u64>,
    pub history_truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GateSnapshot {
    pub targets: Vec<WaitTarget>,
    pub pending: bool,
    pub root_starts_at_enter: u64,
    pub root_starts_at_release: Option<u64>,
}

impl Default for CoreSnapshot {
    fn default() -> Self {
        Self {
            version: 0,
            phase: SessionPhase::Created,
            root_turn_count: 0,
            gate: None,
            root_start_requests: 0,
            queued_inputs: 0,
            agents: Vec::new(),
            thread_id: None,
            turn_id: None,
            model: None,
            cwd: String::new(),
            sandbox: String::new(),
            approval_policy: String::new(),
            messages: Vec::new(),
            requests: Vec::new(),
            notice: None,
            last_error: None,
            tool_activity: None,
            total_tokens: None,
            history_truncated: false,
        }
    }
}

/// Modified only by the Core command/event owner.
#[derive(Debug, Default)]
pub(crate) struct SessionState {
    pub view: CoreSnapshot,
    pub agents: AgentRegistry,
}

impl SessionState {
    pub fn snapshot(&mut self) -> Arc<CoreSnapshot> {
        self.view.agents = self.agents.snapshots();
        self.view.version += 1;
        Arc::new(self.view.clone())
    }

    pub fn submission(&mut self, text: &str) {
        self.view.phase = SessionPhase::StartingTurn;
        self.view.last_error = None;
        self.view.notice = None;
        self.view.messages.push(ConversationItem {
            id: format!("user-{}", self.view.root_turn_count + 1),
            thread_id: self.view.thread_id.clone().unwrap_or_default(),
            turn_id: String::new(),
            role: "You".into(),
            text: text.to_owned(),
            complete: true,
            truncated: false,
        });
        self.bound_history();
    }

    pub fn turn_started(&mut self, id: String) {
        if self.view.turn_id.as_ref() != Some(&id) {
            self.view.root_turn_count += 1;
        }
        self.view.turn_id = Some(id);
        self.view.phase = SessionPhase::Running;
        self.view.tool_activity = None;
    }

    pub fn message(&mut self, turn: &str, id: &str, text: &str, complete: bool) -> bool {
        if self.view.turn_id.as_deref() != Some(turn)
            || (!complete
                && !matches!(
                    self.view.phase,
                    SessionPhase::Running | SessionPhase::GatePending
                ))
        {
            return false;
        }
        let thread = self.view.thread_id.clone().unwrap_or_default();
        self.message_for(&thread, turn, id, text, complete)
    }

    pub fn child_message(
        &mut self,
        thread: &str,
        turn: &str,
        id: &str,
        text: &str,
        complete: bool,
    ) -> bool {
        if !self.agents.current_turn(thread, turn)
            || (!complete && !self.agents.active_turn(thread, turn))
        {
            return false;
        }
        self.message_for(thread, turn, id, text, complete)
    }

    fn message_for(
        &mut self,
        thread: &str,
        turn: &str,
        id: &str,
        text: &str,
        complete: bool,
    ) -> bool {
        if id.is_empty() || id.len() > 1024 {
            return false;
        }
        let item =
            match self.view.messages.iter_mut().find(|m| {
                m.thread_id == thread && m.turn_id == turn && m.id == id && m.role == "Agent"
            }) {
                Some(item) => item,
                None => {
                    self.view.messages.push(ConversationItem {
                        id: id.into(),
                        thread_id: thread.into(),
                        turn_id: turn.into(),
                        role: "Agent".into(),
                        text: String::new(),
                        complete: false,
                        truncated: false,
                    });
                    self.view.messages.last_mut().unwrap()
                }
            };
        if item.complete && !complete {
            return false;
        }
        if complete {
            item.text = text.to_owned();
            item.complete = true;
        } else {
            item.text.push_str(text);
        }
        if item.text.len() > MESSAGE_BYTES {
            trim_front(&mut item.text, MESSAGE_BYTES);
            item.truncated = true;
        }
        self.bound_history();
        true
    }

    fn bound_history(&mut self) {
        for item in &mut self.view.messages {
            if item.text.len() > MESSAGE_BYTES {
                trim_front(&mut item.text, MESSAGE_BYTES);
                item.truncated = true;
            }
        }
        while self.view.messages.len() > 128
            || self
                .view
                .messages
                .iter()
                .map(|m| m.text.len())
                .sum::<usize>()
                > HISTORY_BYTES
        {
            self.view.messages.remove(0);
            self.view.history_truncated = true;
        }
    }

    pub fn error(&mut self, phase: SessionPhase, error: impl Into<String>) {
        self.view.phase = phase;
        self.view.last_error = Some(error.into());
        self.view.tool_activity = None;
    }
}

pub(crate) fn trim_front(text: &mut String, max_bytes: usize) {
    let mut cut = text.len().saturating_sub(max_bytes);
    while !text.is_char_boundary(cut) {
        cut += 1;
    }
    text.drain(..cut);
}

pub(crate) fn display_text(text: &str) -> String {
    text.chars()
        .filter(|c| !c.is_control() || *c == '\n' || *c == '\t')
        .collect()
}

pub fn display_text_for_cli(text: &str) -> String {
    display_text(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn final_item_corrects_deltas_without_duplicates_and_old_turn_is_ignored() {
        let mut state = SessionState::default();
        state.turn_started("turn-2".into());
        assert!(!state.message("turn-1", "item", "late", false));
        state.message("turn-2", "item", "hel", false);
        state.message("turn-2", "item", "lo", false);
        state.message("turn-2", "item", "hello!", true);
        state.message("turn-2", "item", "late", false);
        assert_eq!(state.view.messages.len(), 1);
        assert_eq!(state.view.messages[0].text, "hello!");
    }

    #[test]
    fn truncates_chinese_on_character_boundaries_and_bounds_total_history() {
        let mut state = SessionState::default();
        state.turn_started("turn".into());
        for i in 0..30 {
            state.message("turn", &i.to_string(), &"中文".repeat(12000), true);
        }
        assert!(state.view.history_truncated);
        assert!(
            state
                .view
                .messages
                .iter()
                .map(|m| m.text.len())
                .sum::<usize>()
                <= HISTORY_BYTES
        );
        assert!(state
            .view
            .messages
            .iter()
            .all(|m| m.text.len() <= MESSAGE_BYTES && m.truncated));
    }

    #[test]
    fn terminal_turn_ignores_late_deltas_but_accepts_authoritative_final_text() {
        let mut state = SessionState::default();
        state.turn_started("one".into());
        state.message("one", "a", "partial", false);
        state.view.phase = SessionPhase::Completed;
        assert!(!state.message("one", "a", "late", false));
        assert!(!state.message("one", "new", "late", false));
        assert!(state.message("one", "a", "final", true));
        assert_eq!(state.view.messages.len(), 1);
        assert_eq!(state.view.messages[0].text, "final");
    }
}
