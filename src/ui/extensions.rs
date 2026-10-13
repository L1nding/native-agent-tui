//! Registered domain views. Routing, painting and displayed confirmations share one registry.
use std::cell::RefCell;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::Rect;

use crate::extensions::{ExtensionConfirmation, ExtensionId, ExtensionSnapshot};
use crate::state::{CoreSnapshot, SessionPhase};

#[derive(Default)]
pub(super) struct ExtensionPanel {
    pub visible: Option<ExtensionId>,
    pub scroll: u16,
    pub reply_scroll_from_bottom: u16,
    displayed: RefCell<Option<ExtensionConfirmation>>,
}

impl ExtensionPanel {
    pub fn new(visible: Option<ExtensionId>) -> Self {
        Self {
            visible,
            ..Default::default()
        }
    }
    pub fn clear_displayed(&self) {
        *self.displayed.borrow_mut() = None;
    }

    pub fn toggle(&mut self, snapshot: &CoreSnapshot) {
        self.visible = REGISTRY.next(snapshot, self.visible);
        self.scroll = 0;
        self.reply_scroll_from_bottom = 0;
        self.clear_displayed();
    }

    #[cfg(test)]
    pub fn displayed(&self, confirmation: ExtensionConfirmation) {
        *self.displayed.borrow_mut() = Some(confirmation);
    }
}

/// Only read-only facts and view-local presentation data cross the view seam.
pub(super) struct ViewContext<'a> {
    pub phase: SessionPhase,
    pub model: Option<&'a str>,
    pub reply: Option<&'a str>,
    pub draft: &'a str,
    pub notice: Option<&'a str>,
    pub activity: Option<&'static str>,
    pub activity_marker: &'static str,
    pub panel: &'a ExtensionPanel,
}

impl<'a> ViewContext<'a> {
    pub fn new(
        snapshot: &'a CoreSnapshot,
        panel: &'a ExtensionPanel,
        draft: &'a str,
        notice: Option<&'a str>,
    ) -> Self {
        Self {
            phase: snapshot.phase,
            model: snapshot.model.as_deref(),
            reply: current_reply(snapshot),
            panel,
            draft,
            notice,
            activity: super::busy::label(snapshot),
            activity_marker: "● ",
        }
    }
}

pub(super) fn current_reply(snapshot: &CoreSnapshot) -> Option<&str> {
    let thread = snapshot.thread_id.as_deref()?;
    let turn = snapshot.turn_id.as_deref()?;
    snapshot
        .messages
        .iter()
        .rev()
        .find(|item| {
            item.thread_id == thread
                && item.turn_id == turn
                && item.role == "Agent"
                && !item.text.trim().is_empty()
        })
        .map(|item| item.text.as_str())
}

pub(super) trait ExtensionView: Sync {
    fn id(&self) -> ExtensionId;
    fn available(&self, snapshot: &ExtensionSnapshot) -> bool;
    fn max_scroll(
        &self,
        snapshot: &ExtensionSnapshot,
        context: &ViewContext<'_>,
        area: Rect,
    ) -> u16;
    fn draw(
        &self,
        frame: &mut ratatui::Frame<'_>,
        area: Rect,
        snapshot: &ExtensionSnapshot,
        context: &ViewContext<'_>,
    );
    fn confirmation(&self, snapshot: &ExtensionSnapshot) -> Option<ExtensionConfirmation>;
}

pub(super) struct Registry<'a> {
    views: &'a [&'a dyn ExtensionView],
}

pub(super) const REGISTRY: Registry<'static> = Registry {
    views: &[&super::domain::ModelingView],
};

impl Registry<'_> {
    fn projection<'a>(
        &self,
        snapshot: &'a CoreSnapshot,
        id: ExtensionId,
    ) -> Option<(&dyn ExtensionView, &'a ExtensionSnapshot)> {
        let mut views = self.views.iter().filter(|view| view.id() == id);
        let view = views.next()?;
        let mut projections = snapshot
            .extensions
            .iter()
            .filter(|projection| projection.id() == id);
        let projection = projections.next()?;
        // 同一身份的两份投影或视图是不确定状态，不能任选一份授予确认。
        if views.next().is_some() || projections.next().is_some() {
            return None;
        }
        view.available(projection).then_some((*view, projection))
    }

    pub fn first(&self, snapshot: &CoreSnapshot) -> Option<ExtensionId> {
        self.views
            .iter()
            .map(|view| view.id())
            .find(|id| self.projection(snapshot, *id).is_some())
    }

    fn next(&self, snapshot: &CoreSnapshot, current: Option<ExtensionId>) -> Option<ExtensionId> {
        let mut available = self
            .views
            .iter()
            .map(|view| view.id())
            .filter(|id| self.projection(snapshot, *id).is_some());
        match current {
            None => available.next(),
            Some(current) => {
                available.find(|id| *id == current)?;
                available.next()
            }
        }
    }

    pub fn draw(
        &self,
        frame: &mut ratatui::Frame<'_>,
        snapshot: &CoreSnapshot,
        context: &ViewContext<'_>,
    ) -> bool {
        context.panel.clear_displayed();
        let Some(id) = context.panel.visible else {
            return false;
        };
        let area = frame.area();
        if !snapshot.requests.is_empty() || area.width < 24 || area.height < 8 {
            return false;
        }
        let Some((view, projection)) = self.projection(snapshot, id) else {
            return false;
        };
        view.draw(frame, area, projection, context);
        *context.panel.displayed.borrow_mut() = view
            .confirmation(projection)
            .filter(|confirmation| confirmation.id() == id);
        true
    }

    pub fn handle_key(
        &self,
        key: KeyEvent,
        snapshot: &CoreSnapshot,
        panel: &mut ExtensionPanel,
        area: Rect,
        draft: &str,
        notice: Option<&str>,
    ) -> Option<ExtensionAction> {
        let id = panel.visible?;
        if !snapshot.requests.is_empty() {
            return None;
        }
        let (view, projection) = self.projection(snapshot, id)?;
        let context = ViewContext::new(snapshot, panel, draft, notice);
        match key.code {
            KeyCode::Esc => {
                panel.visible = None;
                panel.clear_displayed();
            }
            KeyCode::PageUp if key.modifiers.contains(KeyModifiers::SHIFT) => {
                panel.reply_scroll_from_bottom = panel.reply_scroll_from_bottom.saturating_add(8);
            }
            KeyCode::PageDown if key.modifiers.contains(KeyModifiers::SHIFT) => {
                panel.reply_scroll_from_bottom = panel.reply_scroll_from_bottom.saturating_sub(8);
            }
            KeyCode::Home if key.modifiers.contains(KeyModifiers::CONTROL) => {
                panel.scroll = 0;
            }
            KeyCode::End if key.modifiers.contains(KeyModifiers::CONTROL) => {
                panel.scroll = view.max_scroll(projection, &context, area);
            }
            KeyCode::PageUp | KeyCode::PageDown => {
                let max_scroll = view.max_scroll(projection, &context, area);
                let current = panel.scroll.min(max_scroll);
                panel.scroll = if key.code == KeyCode::PageUp {
                    current.saturating_sub(8)
                } else {
                    current.saturating_add(8).min(max_scroll)
                };
            }
            KeyCode::Char('y') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                return Some(
                    panel
                        .displayed
                        .borrow()
                        .clone()
                        .filter(|confirmation| confirmation.id() == id)
                        .map_or(
                            ExtensionAction::NoDisplayedConfirmation,
                            ExtensionAction::Confirm,
                        ),
                );
            }
            _ => return None,
        }
        Some(ExtensionAction::Handled)
    }
}

pub(super) enum ExtensionAction {
    Handled,
    Confirm(ExtensionConfirmation),
    NoDisplayedConfirmation,
}

#[cfg(test)]
mod tests {
    use super::super::LocalState;
    use super::*;
    use crate::domain::{DomainSnapshot, ProposalBinding, ProposalFact};
    use ratatui::{backend::TestBackend, Terminal};

    fn snapshot() -> CoreSnapshot {
        CoreSnapshot {
            phase: SessionPhase::Completed,
            extensions: vec![ExtensionSnapshot::Modeling(DomainSnapshot {
                enabled: true,
                proposal: Some(ProposalFact {
                    binding: ProposalBinding {
                        run_id: "run".into(),
                        context_sha256: "context".into(),
                        proposal_id: "proposal".into(),
                        revision: 7,
                        prepare_token: "PRIVATE_TOKEN".into(),
                        base_sha256: "base".into(),
                        candidate_sha256: "candidate".into(),
                    },
                    state: "prepared".into(),
                    summary: "typed extension proposal".into(),
                    objects: Vec::new(),
                    assumptions: Vec::new(),
                    diagnostics: Vec::new(),
                    ready: true,
                }),
                ..Default::default()
            })],
            ..Default::default()
        }
    }

    #[test]
    fn registered_projection_routes_rendering_navigation_and_exact_confirmation() {
        let snapshot = snapshot();
        let mut local = LocalState::default();
        local.extension.toggle(&snapshot);
        assert_eq!(local.extension.visible, Some(ExtensionId::Modeling));
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal
            .draw(|frame| super::super::render::draw(frame, &snapshot, &local))
            .unwrap();
        let confirmation = local.extension.displayed.borrow().clone().unwrap();
        assert!(!format!("{confirmation:?}").contains("PRIVATE_TOKEN"));
        let (tx, mut rx) = tokio::sync::mpsc::channel(4);
        super::super::handle_key(
            KeyEvent::new(KeyCode::Char('y'), KeyModifiers::CONTROL),
            &snapshot,
            &mut local,
            &tx,
        );
        assert_eq!(
            rx.try_recv().unwrap(),
            crate::client::Command::ConfirmExtension { confirmation }
        );
        local.extension.toggle(&snapshot);
        assert_eq!(local.extension.visible, None);
        assert!(local.extension.displayed.borrow().is_none());
    }

    #[test]
    fn overlay_small_window_and_disappeared_facts_revoke_displayed_confirmation() {
        let mut snapshot = snapshot();
        let mut local = LocalState {
            extension: ExtensionPanel::new(Some(ExtensionId::Modeling)),
            ..Default::default()
        };
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal
            .draw(|frame| super::super::render::draw(frame, &snapshot, &local))
            .unwrap();
        assert!(local.extension.displayed.borrow().is_some());
        local.palette.visible = true;
        terminal
            .draw(|frame| super::super::render::draw(frame, &snapshot, &local))
            .unwrap();
        assert!(local.extension.displayed.borrow().is_none());
        local.palette.visible = false;
        let mut tiny = Terminal::new(TestBackend::new(20, 7)).unwrap();
        tiny.draw(|frame| super::super::render::draw(frame, &snapshot, &local))
            .unwrap();
        assert!(local.extension.displayed.borrow().is_none());
        snapshot.extensions.clear();
        terminal
            .draw(|frame| super::super::render::draw(frame, &snapshot, &local))
            .unwrap();
        assert!(local.extension.displayed.borrow().is_none());
        assert!(REGISTRY.first(&snapshot).is_none());
    }

    #[test]
    fn ambiguous_projection_identity_cannot_authorize_a_confirmation() {
        let mut snapshot = snapshot();
        snapshot.extensions.push(snapshot.extensions[0].clone());
        assert!(REGISTRY.first(&snapshot).is_none());
        let local = LocalState {
            extension: ExtensionPanel::new(Some(ExtensionId::Modeling)),
            ..Default::default()
        };
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal
            .draw(|frame| super::super::render::draw(frame, &snapshot, &local))
            .unwrap();
        assert!(local.extension.displayed.borrow().is_none());
    }
}
