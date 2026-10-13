//! View-local activity animation; labels come only from Core lifecycle facts.
use super::lang::tr;
use crate::state::{CoreSnapshot, SessionPhase};

#[derive(Default)]
pub(super) struct ActivityIndicator {
    frame: u8,
}

impl ActivityIndicator {
    pub fn advance(&mut self) {
        self.frame = (self.frame + 1) % 4;
    }
    pub fn marker(&self) -> &'static str {
        ["· ", "• ", "● ", "• "][usize::from(self.frame)]
    }
}

pub(super) fn label(snapshot: &CoreSnapshot) -> Option<&'static str> {
    match snapshot.phase {
        SessionPhase::StartingTurn => Some(tr("Starting…", "正在启动…")),
        SessionPhase::GatePending => Some(tr("Waiting for children…", "正在等待子 Agent…")),
        SessionPhase::Running if !snapshot.requests.is_empty() => {
            Some(tr("Waiting for action…", "正在等待确认…"))
        }
        SessionPhase::Running => Some(tr("Working…", "正在执行…")),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{backend::TestBackend, Terminal};

    #[test]
    fn activity_marker_has_words_in_mono_and_idle_keeps_the_task_prompt() {
        let original = super::super::theme::name();
        super::super::theme::set("mono");
        let mut snapshot = CoreSnapshot {
            phase: SessionPhase::Running,
            ..Default::default()
        };
        let mut local = super::super::LocalState::default();
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal
            .draw(|frame| super::super::render::draw(frame, &snapshot, &local))
            .unwrap();
        let before = terminal.backend().buffer().clone();
        let shown: String = before.content().iter().map(|cell| cell.symbol()).collect();
        assert!(shown.contains("Working…"));
        local.activity.advance();
        terminal
            .draw(|frame| super::super::render::draw(frame, &snapshot, &local))
            .unwrap();
        assert_ne!(before, *terminal.backend().buffer());
        snapshot.phase = SessionPhase::Completed;
        terminal
            .draw(|frame| super::super::render::draw(frame, &snapshot, &local))
            .unwrap();
        let shown: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(shown.contains("Write a task"));
        assert!(!shown.contains("Working…"));
        super::super::theme::set(original);
    }
}
