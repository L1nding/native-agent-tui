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
    pub selected_image: usize,
    selected_approval: Option<crate::domain::ProposalBinding>,
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

    fn selected_approval_index(&self, cards: &[&crate::domain::DomainSnapshot]) -> Option<usize> {
        if cards.is_empty() {
            return None;
        }
        Some(
            self.selected_approval
                .as_ref()
                .and_then(|selected| {
                    cards.iter().position(|card| {
                        card.proposal
                            .as_ref()
                            .is_some_and(|proposal| proposal.binding == *selected)
                    })
                })
                .unwrap_or(0),
        )
    }

    pub fn toggle(&mut self, snapshot: &CoreSnapshot) {
        self.visible = REGISTRY.next(snapshot, self.visible);
        self.scroll = 0;
        self.reply_scroll_from_bottom = 0;
        self.selected_image = 0;
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
    pub subagents: &'a [crate::state::DshSubagent],
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
            subagents: &snapshot.dsh_subagents,
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
        let selected_projection = projection.modeling().and_then(|domain| {
            let cards = domain.approval_card_snapshots();
            let selected = context.panel.selected_approval_index(&cards)?;
            let mut card = cards[selected].clone();
            card.authorizing = domain.authorizing;
            card.fact_sync_notice = domain.fact_sync_notice.clone();
            Some(ExtensionSnapshot::Modeling(card))
        });
        let display_projection = selected_projection.as_ref().unwrap_or(projection);
        view.draw(frame, area, display_projection, context);
        let confirmation = projection.modeling().and_then(|domain| {
            let cards = domain.approval_card_snapshots();
            context
                .panel
                .selected_approval_index(&cards)
                .and_then(|index| cards.get(index).copied())
                .filter(|card| domain.approval_card_waiting_for(card))
                .and_then(|_| view.confirmation(display_projection))
        });
        *context.panel.displayed.borrow_mut() =
            confirmation.filter(|confirmation| confirmation.id() == id);
        true
    }

    pub fn draw_conversation_card(
        &self,
        frame: &mut ratatui::Frame<'_>,
        area: Rect,
        snapshot: &CoreSnapshot,
        panel: &ExtensionPanel,
    ) -> Rect {
        panel.clear_displayed();
        let Some((_view, projection)) = self.projection(snapshot, ExtensionId::Modeling) else {
            return area;
        };
        let domain = projection.modeling().expect("modeling projection");
        let cards = domain.approval_card_snapshots();
        let Some(selected_index) = panel.selected_approval_index(&cards) else {
            return area;
        };
        let selected_card = cards[selected_index];
        let approvable_count = cards
            .iter()
            .filter(|card| domain.approval_card_waiting_for(card))
            .count();
        let selected_can_approve = domain.approval_card_waiting_for(selected_card);
        let mut display_card = selected_card.clone();
        display_card.authorizing = domain.authorizing;
        display_card.fact_sync_notice = domain.fact_sync_notice.clone();
        let mut content = super::domain::approval_card_lines(&display_card, panel.selected_image);
        if content.is_empty() {
            return area;
        }
        if approvable_count > 0 && !selected_can_approve {
            *content.last_mut().expect("approval action") =
                "本卡未就绪 · 有可批准卡 · Alt+[/]切换后 Ctrl+Y 确认".into();
        }
        // Human approval binds the exact displayed version; running turns and tasks
        // (the root "继续" task, subagents) do not block it. Only a pending question does.
        let pending = if !snapshot.requests.is_empty() {
            Some("还不能确认：请先回答当前问题")
        } else {
            None
        };
        if let Some(pending) = pending {
            *content.last_mut().expect("approval action") = pending.into();
        }
        let lines: Vec<_> = content
            .iter()
            .flat_map(|line| {
                super::layout::wrap_text(line, usize::from(area.width.saturating_sub(2).max(1)))
            })
            .collect();
        let needed = lines.len().saturating_add(2);
        let full = area.width >= 40
            && lines.len() == content.len()
            && needed <= usize::from(area.height.saturating_sub(1));
        let compact = !full && area.width >= 40 && area.height >= 8;
        let lines = if compact {
            super::domain::compact_approval_card_lines(
                &display_card,
                &content,
                usize::from(area.width.saturating_sub(2)),
            )
        } else {
            lines
        };
        // Compact cards omit the bottom border so seven information rows fit in eight rows.
        let complete = full || compact && lines.len() < usize::from(area.height);
        let height = if complete {
            (lines.len() + if full { 2 } else { 1 }) as u16
        } else {
            area.height.min(4)
        };
        let card = Rect {
            y: area.y + area.height.saturating_sub(height),
            height,
            ..area
        };
        let text = if complete {
            lines.join("\n")
        } else {
            super::layout::wrap_text(
                "窗口过小，放大或 F6 查看详情",
                usize::from(area.width.saturating_sub(2).max(1)),
            )
            .join("\n")
        };
        frame.render_widget(
            ratatui::widgets::Paragraph::new(text).block(
                ratatui::widgets::Block::default()
                    .borders(if full || !complete {
                        ratatui::widgets::Borders::ALL
                    } else {
                        ratatui::widgets::Borders::TOP
                            | ratatui::widgets::Borders::LEFT
                            | ratatui::widgets::Borders::RIGHT
                    })
                    .title(if area.width >= 64 {
                        format!(
                            "建模确认 · 待批 {} 张（可确认 {}） · F6 详情",
                            cards.len(),
                            approvable_count
                        )
                    } else if area.width >= 32 {
                        format!(
                            "建模确认 · 待批 {} 张/可确认 {}",
                            cards.len(),
                            approvable_count
                        )
                    } else {
                        format!("待批 {} 张/可确认 {}", cards.len(), approvable_count)
                    }),
            ),
            card,
        );
        if complete && snapshot.requests.is_empty() && selected_can_approve {
            *panel.displayed.borrow_mut() = Some(ExtensionConfirmation::Modeling(
                selected_card.confirmation(),
            ));
        }
        Rect {
            height: area.height.saturating_sub(height),
            ..area
        }
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
        let id = panel.visible.or_else(|| self.first(snapshot))?;
        if !snapshot.requests.is_empty() {
            return None;
        }
        let (view, projection) = self.projection(snapshot, id)?;
        let domain = projection.modeling()?;
        let cards = domain.approval_card_snapshots();
        if key.modifiers.contains(KeyModifiers::ALT)
            && matches!(key.code, KeyCode::Char('[' | ']'))
            && !cards.is_empty()
        {
            let current = panel.selected_approval_index(&cards).unwrap_or(0);
            let next = if key.code == KeyCode::Char('[') {
                current.checked_sub(1).unwrap_or(cards.len() - 1)
            } else {
                (current + 1) % cards.len()
            };
            panel.selected_approval = Some(
                cards[next]
                    .proposal
                    .as_ref()
                    .expect("approval card proposal")
                    .binding
                    .clone(),
            );
            panel.selected_image = 0;
            panel.clear_displayed();
            return Some(ExtensionAction::Handled);
        }
        let selected_index = panel.selected_approval_index(&cards);
        let selected_card = selected_index.and_then(|index| cards.get(index).copied());
        let image_or_approval_key = matches!((key.code, key.modifiers),
            (KeyCode::Char('y' | 'o'), modifiers) if modifiers.contains(KeyModifiers::CONTROL))
            || (matches!(key.code, KeyCode::Left | KeyCode::Right)
                && key.modifiers.contains(KeyModifiers::ALT));
        if panel.visible.is_none() && !image_or_approval_key {
            return None;
        }
        if panel.visible.is_none() && selected_card.is_none() && domain.proposal.is_none() {
            return None;
        }
        let context = ViewContext::new(snapshot, panel, draft, notice);
        match key.code {
            KeyCode::Left | KeyCode::Right if key.modifiers.contains(KeyModifiers::ALT) => {
                let count = selected_card?.review_images.len();
                panel.selected_image = if count == 0 {
                    0
                } else if key.code == KeyCode::Left {
                    panel.selected_image.min(count - 1).saturating_sub(1)
                } else {
                    panel.selected_image.saturating_add(1).min(count - 1)
                };
                panel.scroll = 0;
            }
            KeyCode::Char('o') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                let images = &selected_card?.review_images;
                return Some(
                    images
                        .get(panel.selected_image.min(images.len().saturating_sub(1)))
                        .filter(|image| image.verified_path().is_some())
                        .map_or(ExtensionAction::ImageUnavailable, |image| {
                            ExtensionAction::OpenImage(Box::new(image.binding.clone()))
                        }),
                );
            }
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
                let current_confirmation = selected_card
                    .filter(|card| domain.approval_card_waiting_for(card))
                    .map(|card| ExtensionConfirmation::Modeling(card.confirmation()));
                return Some(
                    panel
                        .displayed
                        .borrow()
                        .clone()
                        .filter(|confirmation| confirmation.id() == id)
                        .filter(|confirmation| current_confirmation.as_ref() == Some(confirmation))
                        .filter(|_| area.width >= 24 && area.height >= 8)
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
    OpenImage(Box<crate::domain::ReviewImageBinding>),
    ImageUnavailable,
}

#[cfg(test)]
mod tests {
    use super::super::LocalState;
    use super::*;
    use crate::domain::{
        ApprovalCard, ArtifactBinding, ArtifactFact, ArtifactState, DomainSnapshot,
        ProposalBinding, ProposalFact,
    };
    use ratatui::{backend::TestBackend, Terminal};

    fn snapshot() -> CoreSnapshot {
        let mut snapshot = CoreSnapshot {
            phase: SessionPhase::Completed,
            extensions: vec![ExtensionSnapshot::Modeling(DomainSnapshot {
                enabled: true,
                source_current: Some(true),
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
                    parameter_only: false,
                }),
                artifacts: vec![ArtifactFact {
                    binding: ArtifactBinding {
                        build_id: "preview".into(),
                        proposal_id: "proposal".into(),
                        revision: 7,
                        prepare_token: "PRIVATE_TOKEN".into(),
                        candidate: true,
                        authority_sha256: "base".into(),
                        input_sha256: "candidate".into(),
                        output: "preview.blend".into(),
                    },
                    state: ArtifactState::Built,
                    current: Some(true),
                    valid: Some(true),
                    reviewed: true,
                }],
                ..Default::default()
            })],
            ..Default::default()
        };
        let crate::extensions::ExtensionSnapshot::Modeling(domain) = &mut snapshot.extensions[0];
        domain.review_images.push(crate::domain::review::test_image(
            &domain.proposal.as_ref().unwrap().binding,
            "preview",
        ));
        snapshot
    }

    fn approval_domain(
        seed: &DomainSnapshot,
        proposal_id: &str,
        revision: u64,
        category: &str,
        ready: bool,
    ) -> DomainSnapshot {
        let mut card = seed.clone();
        card.approval_cards.clear();
        card.source_current = Some(true);
        card.proposal_not_approvable = false;
        let proposal = card.proposal.as_mut().unwrap();
        proposal.binding.proposal_id = proposal_id.into();
        proposal.binding.revision = revision;
        proposal.binding.prepare_token = format!("{proposal_id}-prepare");
        proposal.summary = proposal_id.into();
        proposal.ready = ready;
        for artifact in &mut card.artifacts {
            artifact.binding.proposal_id = proposal_id.into();
            artifact.binding.revision = revision;
            artifact.binding.prepare_token = proposal.binding.prepare_token.clone();
        }
        for image in &mut card.review_images {
            image.binding.proposal = proposal.binding.clone();
        }
        card.proposal_categories.clear();
        card.proposal_categories.insert(category.into(), 1);
        if !ready {
            card.artifacts.clear();
            card.review_images.clear();
        }
        card
    }

    fn snapshot_with_approval_cards() -> CoreSnapshot {
        let mut snapshot = snapshot();
        let ExtensionSnapshot::Modeling(root) = &mut snapshot.extensions[0];
        let seed = root.clone();
        let earlier_ready = approval_domain(&seed, "ready-earlier", 8, "wall", true);
        let latest_ready = approval_domain(&seed, "ready-latest", 9, "armchair", true);
        let latest_unready = approval_domain(&seed, "unready-latest", 10, "examination_bed", false);
        let mut current = latest_unready;
        current.approval_cards = vec![
            ApprovalCard {
                fact_sequence: 8,
                snapshot: earlier_ready.approval_card_snapshot(),
            },
            ApprovalCard {
                fact_sequence: 9,
                snapshot: latest_ready.approval_card_snapshot(),
            },
        ];
        *root = current;
        snapshot
    }

    fn terminal_text(terminal: &Terminal<TestBackend>) -> String {
        let buffer = terminal.backend().buffer();
        let mut text = String::new();
        for row in buffer.content().chunks(usize::from(buffer.area.width)) {
            let mut continuation = 0;
            for cell in row {
                if continuation > 0 {
                    continuation -= 1;
                    continue;
                }
                let symbol = cell.symbol();
                continuation = unicode_width::UnicodeWidthStr::width(symbol).saturating_sub(1);
                if !symbol.chars().all(char::is_whitespace) {
                    text.push_str(symbol);
                }
            }
        }
        text
    }

    #[test]
    fn approval_card_collection_counts_prioritizes_switches_and_confirms_exact_card() {
        let snapshot = snapshot_with_approval_cards();
        let ExtensionSnapshot::Modeling(domain) = &snapshot.extensions[0];
        let cards = domain.approval_card_snapshots();
        assert_eq!(cards.len(), 3);
        assert!(domain.approval_card_waiting_for(cards[0]));
        assert_eq!(
            cards[0].proposal.as_ref().unwrap().binding.proposal_id,
            "ready-latest"
        );

        let panel = ExtensionPanel::default();
        let mut terminal = Terminal::new(TestBackend::new(120, 44)).unwrap();
        terminal
            .draw(|frame| {
                REGISTRY.draw_conversation_card(frame, frame.area(), &snapshot, &panel);
            })
            .unwrap();
        let rendered = terminal_text(&terminal);
        assert!(rendered.contains("待批3张（可确认2）"), "{rendered}");
        assert!(rendered.contains("待你确认：扶手椅1把"), "{rendered}");
        assert_eq!(
            panel
                .displayed
                .borrow()
                .as_ref()
                .map(|confirmation| match confirmation {
                    ExtensionConfirmation::Modeling(binding) =>
                        binding.proposal.proposal_id.as_str(),
                }),
            Some("ready-latest")
        );

        let mut panel = panel;
        let switch = |panel: &mut ExtensionPanel, code| {
            REGISTRY.handle_key(
                KeyEvent::new(code, KeyModifiers::ALT),
                &snapshot,
                panel,
                Rect::new(0, 0, 120, 44),
                "",
                None,
            )
        };
        assert!(matches!(
            switch(&mut panel, KeyCode::Char(']')),
            Some(ExtensionAction::Handled)
        ));
        terminal
            .draw(|frame| {
                REGISTRY.draw_conversation_card(frame, frame.area(), &snapshot, &panel);
            })
            .unwrap();
        assert!(terminal_text(&terminal).contains("待你确认：墙体1处"));
        let action = REGISTRY.handle_key(
            KeyEvent::new(KeyCode::Char('y'), KeyModifiers::CONTROL),
            &snapshot,
            &mut panel,
            Rect::new(0, 0, 120, 44),
            "",
            None,
        );
        assert!(matches!(
            action,
            Some(ExtensionAction::Confirm(ExtensionConfirmation::Modeling(ref confirmation)))
                if confirmation.proposal.proposal_id == "ready-earlier"
        ));

        panel.visible = Some(ExtensionId::Modeling);
        terminal
            .draw(|frame| {
                let context = ViewContext::new(&snapshot, &panel, "", None);
                REGISTRY.draw(frame, &snapshot, &context);
            })
            .unwrap();
        assert!(terminal_text(&terminal).contains("ready-earlier"));
        assert!(!terminal_text(&terminal).contains("unready-latest"));
        panel.visible = None;

        assert!(matches!(
            switch(&mut panel, KeyCode::Char(']')),
            Some(ExtensionAction::Handled)
        ));
        terminal
            .draw(|frame| {
                REGISTRY.draw_conversation_card(frame, frame.area(), &snapshot, &panel);
            })
            .unwrap();
        let rendered = terminal_text(&terminal);
        assert!(rendered.contains("有可批准卡"), "{rendered}");
        assert!(panel.displayed.borrow().is_none());
        let mut compact = Terminal::new(TestBackend::new(40, 8)).unwrap();
        compact
            .draw(|frame| {
                REGISTRY.draw_conversation_card(frame, frame.area(), &snapshot, &panel);
            })
            .unwrap();
        let compact_text = terminal_text(&compact);
        assert!(
            compact_text.contains("建模确认·待批3张/可确认2"),
            "{compact_text}"
        );
        assert!(compact_text.contains("有可批准卡"), "{compact_text}");
        assert!(compact_text.contains("Ctrl+Y确认"), "{compact_text}");

        for width in [24, 27, 31] {
            let mut narrow = Terminal::new(TestBackend::new(width, 8)).unwrap();
            narrow
                .draw(|frame| {
                    REGISTRY.draw_conversation_card(frame, frame.area(), &snapshot, &panel);
                })
                .unwrap();
            let text = terminal_text(&narrow);
            assert!(text.contains("待批3张/可确认2"), "{width} columns: {text}");
        }
        let action = REGISTRY.handle_key(
            KeyEvent::new(KeyCode::Char('y'), KeyModifiers::CONTROL),
            &snapshot,
            &mut panel,
            Rect::new(0, 0, 120, 44),
            "",
            None,
        );
        assert!(matches!(
            action,
            Some(ExtensionAction::NoDisplayedConfirmation)
        ));
    }

    #[test]
    fn workflow_overlay_owns_escape_without_hiding_the_modeling_page() {
        let snapshot = snapshot();
        let mut local = LocalState {
            extension: ExtensionPanel::new(Some(ExtensionId::Modeling)),
            ..Default::default()
        };
        local.workflow.visible = true;
        let (tx, mut rx) = tokio::sync::mpsc::channel(4);
        super::super::handle_key(
            KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
            &snapshot,
            &mut local,
            &tx,
        );
        assert!(!local.workflow.visible);
        assert_eq!(local.extension.visible, Some(ExtensionId::Modeling));
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn running_model_turn_still_presents_the_modeling_approval() {
        let mut snapshot = snapshot();
        snapshot.phase = SessionPhase::Running;
        let local = LocalState {
            extension: ExtensionPanel::new(Some(ExtensionId::Modeling)),
            ..Default::default()
        };
        let mut terminal = Terminal::new(TestBackend::new(120, 40)).unwrap();
        terminal
            .draw(|frame| super::super::render::draw(frame, &snapshot, &local))
            .unwrap();
        // Human approval binds the displayed version; the agent may keep working.
        assert!(local.extension.displayed.borrow().is_some());
    }

    #[test]
    fn registered_projection_routes_rendering_navigation_and_exact_confirmation() {
        let snapshot = snapshot();
        let mut local = LocalState {
            viewport: Rect::new(0, 0, 80, 24),
            ..Default::default()
        };
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
    fn modeling_confirmation_waits_until_the_candidate_image_is_delivered() {
        let mut snapshot = snapshot();
        let ExtensionSnapshot::Modeling(domain) = &mut snapshot.extensions[0];
        domain.artifacts[0].reviewed = false;
        let local = LocalState {
            extension: ExtensionPanel::new(Some(ExtensionId::Modeling)),
            ..Default::default()
        };
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal
            .draw(|frame| super::super::render::draw(frame, &snapshot, &local))
            .unwrap();
        assert!(
            local.extension.displayed.borrow().is_none(),
            "approval must wait for image delivery of the verified candidate"
        );
    }

    #[test]
    fn modeling_confirmation_requires_a_verified_candidate_preview() {
        let mut snapshot = snapshot();
        let ExtensionSnapshot::Modeling(domain) = &mut snapshot.extensions[0];
        domain.artifacts.clear();
        let local = LocalState {
            extension: ExtensionPanel::new(Some(ExtensionId::Modeling)),
            ..Default::default()
        };
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal
            .draw(|frame| super::super::render::draw(frame, &snapshot, &local))
            .unwrap();
        assert!(
            local.extension.displayed.borrow().is_none(),
            "approval must wait for a verified Blender candidate preview"
        );
    }

    #[test]
    fn ctrl_y_rechecks_invalidated_evidence_and_the_current_proposal_before_confirming() {
        for change in ["invalid", "different-build", "binding"] {
            let mut snapshot = snapshot();
            let mut panel = ExtensionPanel::new(Some(ExtensionId::Modeling));
            panel.displayed(
                REGISTRY
                    .projection(&snapshot, ExtensionId::Modeling)
                    .unwrap()
                    .0
                    .confirmation(&snapshot.extensions[0])
                    .unwrap(),
            );
            let ExtensionSnapshot::Modeling(domain) = &mut snapshot.extensions[0];
            match change {
                "invalid" => domain.artifacts[0].valid = Some(false),
                "different-build" => {
                    let mut other = domain.artifacts[0].clone();
                    domain.artifacts[0].valid = Some(false);
                    other.binding.build_id = "other-build".into();
                    other.reviewed = false;
                    domain.artifacts.push(other);
                }
                "binding" => {
                    domain.proposal.as_mut().unwrap().binding.prepare_token = "new-token".into();
                    domain.artifacts[0].binding.prepare_token = "new-token".into();
                }
                _ => unreachable!(),
            }
            let action = REGISTRY.handle_key(
                KeyEvent::new(KeyCode::Char('y'), KeyModifiers::CONTROL),
                &snapshot,
                &mut panel,
                Rect::new(0, 0, 80, 24),
                "",
                None,
            );
            assert!(
                matches!(action, Some(ExtensionAction::NoDisplayedConfirmation)),
                "{change}"
            );
        }
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

    #[test]
    fn normal_terminal_shows_complete_structural_approval_with_per_entity_assumptions() {
        let mut snapshot = snapshot();
        let ExtensionSnapshot::Modeling(domain) = &mut snapshot.extensions[0];
        let proposal = domain.proposal.as_mut().unwrap();
        proposal.summary = format!("初始化 二层平面 结构；4130 个实体；参数假设保留; 待具体批准的墙体图层选择 [\"WALL\"]; 理由 当前楼层带的逐图层统计中，WALL 含 3789 条墙线，是已证实的主墙层；P-WALL1 不在本楼层带统计内，因此不纳入。; 定位证据 {}", "a".repeat(64));
        proposal.assumptions = [
            ("walls", 1274), ("openings", 295), ("slabs", 1),
            ("elements", 2559), ("constraints", 1),
        ].into_iter().flat_map(|(kind, count)| (0..count).map(move |n| {
            serde_json::json!({"code":"assumed_value", "entity_id":format!("{kind}.f02.1402E2.{n}"),
                "message":format!("{kind}: CAD-derived structure; parameter basis retained as assumptions")}).to_string()
        })).collect();
        domain.proposal_categories.extend([
            ("wall".into(), 1274),
            ("opening".into(), 295),
            ("slab".into(), 1),
            ("cad_reference".into(), 2559),
        ]);
        domain.floor_progress = Some(crate::domain::FloorProgress {
            scan_id: "scan".into(),
            scene_sha256: "base".into(),
            categories: (0..10)
                .map(|index| crate::domain::CategoryProgress {
                    category: format!("category-{index}"),
                    built: 0,
                    pending: 10,
                    blocked: 0,
                    unidentified: 0,
                })
                .collect(),
            covered: 0,
            total: 100,
            coverage_unit: "来源实体".into(),
            unreviewed_regions: 10,
        });
        domain.groups = Some(crate::domain::SourceGroupsFact {
            scan_id: "scan".into(),
            scene_sha256: "base".into(),
            total_objects: 100,
            total_groups: 10,
            assessed_groups: 0,
            category_counts: Default::default(),
            rows: Vec::new(),
            counts: [("pending_addition".into(), 100)].into(),
        });
        domain.linework = Some(crate::domain::LineworkCoverageFact {
            scan_id: "scan".into(),
            scene_sha256: "base".into(),
            member_count: 100,
            selected_member_count: 0,
            unselected_member_count: 100,
            candidate_count: 0,
            unclassified_candidate_count: 0,
            region_count: 10,
            annotation_count: 0,
            uncovered_count: 0,
            diagnostic_member_count: 0,
            stale_selection_count: 0,
        });
        assert!(domain.current_floor_progress().is_some());
        for build_id in ["preview-f02-structure-r1", "preview-f02-structure-r1-retry"] {
            let mut failed = domain.artifacts[0].clone();
            failed.binding.build_id = build_id.into();
            failed.state = ArtifactState::Failed;
            failed.valid = Some(false);
            failed.reviewed = false;
            domain.artifacts.insert(0, failed);
        }
        let mut local = LocalState {
            viewport: Rect::new(0, 0, 132, 43),
            ..Default::default()
        };
        let mut terminal = Terminal::new(TestBackend::new(132, 43)).unwrap();
        terminal
            .draw(|frame| super::super::render::draw(frame, &snapshot, &local))
            .unwrap();
        let text = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect::<String>();
        let compact: String = text.chars().filter(|c| !c.is_whitespace()).collect();
        assert!(
            local.extension.displayed.borrow().is_some(),
            "complete 132x43 approval was not registered"
        );
        for expected in [
            "墙体 1274",
            "洞口 295",
            "楼板 1",
            "图纸参考线 2559",
            "尺寸依据：按图纸重建",
            "模型已建好并通过检查",
            "Ctrl+Y 确认",
        ] {
            let expected: String = expected.chars().filter(|c| !c.is_whitespace()).collect();
            assert!(compact.contains(&expected), "{expected} missing: {text}");
        }
        for forbidden in [
            "proposal",
            "PRIVATE_TOKEN",
            "cad_reference",
            "wall_detail",
            "GLB：未知",
            "SHA",
            "投影",
            "修订",
            "未纳入项",
        ] {
            assert!(!text.contains(forbidden), "{forbidden}: {text}");
        }
        assert!(!text.contains("category-9"));
        let (tx, mut rx) = tokio::sync::mpsc::channel(4);
        super::super::handle_key(
            KeyEvent::new(KeyCode::Char('y'), KeyModifiers::CONTROL),
            &snapshot,
            &mut local,
            &tx,
        );
        assert!(matches!(
            rx.try_recv(),
            Ok(crate::client::Command::ConfirmExtension { .. })
        ));
    }

    #[test]
    fn approval_card_summarizes_dimensions_and_preserves_raw_evidence_in_details() {
        let mut snapshot = snapshot();
        let ExtensionSnapshot::Modeling(domain) = &mut snapshot.extensions[0];
        let original = vec![
            serde_json::json!({"code":"assumed_value","message":"height","entity_id":"a","height":0.95}).to_string(),
            serde_json::json!({"code":"assumed_value","message":"height","entity_id":"b","height":1.05}).to_string(),
            "unstructured assumption with complete dimensions 0.5 x 0.7 x 1.05".into(),
        ];
        domain.proposal.as_mut().unwrap().assumptions = original.clone();
        let lines = super::super::domain::approval_card_lines(domain, 0);
        assert!(lines[2].contains("高度 0.95 米（暂用值）"));
        assert!(lines[2].contains("高度 1.05 米（暂用值）"));
        assert_eq!(domain.proposal.as_ref().unwrap().assumptions, original);
        let panel = ExtensionPanel::default();
        let context = ViewContext::new(&snapshot, &panel, "", None);
        let mut terminal = Terminal::new(TestBackend::new(300, 160)).unwrap();
        terminal
            .draw(|frame| {
                super::super::domain::draw(frame, frame.area(), &snapshot.extensions[0], &context);
            })
            .unwrap();
        let details = terminal
            .backend()
            .buffer()
            .content()
            .chunks(300)
            .map(|row| {
                let mut line = String::new();
                let mut x = 0;
                while x < row.len() {
                    let symbol = row[x].symbol();
                    line.push_str(symbol);
                    x += unicode_width::UnicodeWidthStr::width(symbol).max(1);
                }
                line
            })
            .collect::<Vec<_>>()
            .join("\n");
        for text in &original {
            assert!(details.contains(text), "assumption omitted: {text}");
        }
        for technical in [
            "proposal",
            "PRIVATE_TOKEN",
            "base_sha256",
            "candidate_sha256",
            "review_set_sha256",
            "image_sha256",
            "未纳入项",
            "GLB：未知",
        ] {
            assert!(
                details.contains(technical),
                "{technical} missing from details"
            );
        }
    }

    #[test]
    fn conversation_card_directly_confirms_the_displayed_version_without_opening_details() {
        let mut snapshot = snapshot();
        let ExtensionSnapshot::Modeling(domain) = &mut snapshot.extensions[0];
        domain.proposal_categories.insert("armchair".into(), 100);
        domain.proposal.as_mut().unwrap().summary = "新增一批扶手椅".into();
        domain.proposal.as_mut().unwrap().assumptions =
            vec![r#"{"basis":"assumed","message":"高度0.83米"}"#.into()];
        let mut local = LocalState {
            viewport: Rect::new(0, 0, 120, 44),
            ..Default::default()
        };
        let mut terminal = Terminal::new(TestBackend::new(120, 44)).unwrap();
        terminal
            .draw(|frame| super::super::render::draw(frame, &snapshot, &local))
            .unwrap();
        let card_text = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        let compact_text: String = card_text
            .chars()
            .filter(|character| !character.is_whitespace())
            .collect();
        for expected in [
            "待你确认：扶手椅 100 把",
            "尺寸依据：高度 0.83 米（暂用值）",
            "模型已建好并通过检查",
            "图片 1 张",
            "Ctrl+Y 确认",
        ] {
            let compact_expected: String = expected
                .chars()
                .filter(|character| !character.is_whitespace())
                .collect();
            assert!(
                compact_text.contains(&compact_expected),
                "{expected} absent in {card_text}"
            );
        }
        assert!(!card_text.contains("Some(true)"));
        assert!(!card_text.contains("Built"));
        let confirmation = local
            .extension
            .displayed
            .borrow()
            .clone()
            .expect("complete conversation card");
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
        assert_eq!(local.extension.visible, None);
        super::super::handle_key(
            KeyEvent::new(KeyCode::Char('o'), KeyModifiers::CONTROL),
            &snapshot,
            &mut local,
            &tx,
        );
        assert!(matches!(
            rx.try_recv().unwrap(),
            crate::client::Command::OpenDomainImage { .. }
        ));
        assert!(
            local.editor.text.is_empty(),
            "Ctrl+O was swallowed as a newline"
        );
    }

    #[test]
    fn conversation_card_rejects_a_new_version_without_redraw_and_changed_review_set() {
        for (width, height) in [(120, 44), (40, 16)] {
            let mut snapshot = snapshot();
            let mut local = LocalState {
                viewport: Rect::new(0, 0, width, height),
                ..Default::default()
            };
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            terminal
                .draw(|frame| super::super::render::draw(frame, &snapshot, &local))
                .unwrap();
            let ExtensionSnapshot::Modeling(domain) = &mut snapshot.extensions[0];
            domain.proposal.as_mut().unwrap().binding.revision += 1;
            domain.artifacts[0].binding.revision += 1;
            domain.review_images[0].binding.proposal.revision += 1;
            assert!(super::super::domain::ModelingView
                .confirmation(&snapshot.extensions[0])
                .is_some());
            let (tx, mut rx) = tokio::sync::mpsc::channel(4);
            super::super::handle_key(
                KeyEvent::new(KeyCode::Char('y'), KeyModifiers::CONTROL),
                &snapshot,
                &mut local,
                &tx,
            );
            assert!(rx.try_recv().is_err());
            let ExtensionSnapshot::Modeling(domain) = &snapshot.extensions[0];
            local.extension.displayed(ExtensionConfirmation::Modeling(
                crate::extensions::ModelingConfirmation {
                    proposal: domain.proposal.as_ref().unwrap().binding.clone(),
                    review_set_sha256: Some("old-set".into()),
                },
            ));
            super::super::handle_key(
                KeyEvent::new(KeyCode::Char('y'), KeyModifiers::CONTROL),
                &snapshot,
                &mut local,
                &tx,
            );
            assert!(rx.try_recv().is_err());
        }
    }

    #[test]
    fn responsive_conversation_cards_confirm_the_same_version_at_all_supported_sizes() {
        let mut snapshot = snapshot();
        let ExtensionSnapshot::Modeling(domain) = &mut snapshot.extensions[0];
        domain
            .proposal_categories
            .insert("examination_bed".into(), 1);
        domain.proposal.as_mut().unwrap().assumptions =
            (0..100).map(|n| format!("height {n} assumed")).collect();
        let expected = super::super::domain::ModelingView
            .confirmation(&snapshot.extensions[0])
            .unwrap();
        let mut panel = ExtensionPanel::default();
        for width in [120, 80, 65, 48, 40] {
            for height in [160, 12, 8] {
                let area = Rect::new(0, 0, width, height);
                let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
                terminal
                    .draw(|frame| {
                        REGISTRY.draw_conversation_card(frame, area, &snapshot, &panel);
                    })
                    .unwrap();
                let text = terminal
                    .backend()
                    .buffer()
                    .content()
                    .iter()
                    .map(|cell| cell.symbol())
                    .collect::<String>();
                let text: String = text.chars().filter(|c| !c.is_whitespace()).collect();
                for label in ["待你确认", "检查床", "Ctrl+Y确认", "F6"] {
                    assert!(text.contains(label), "{width}x{height}: {label}: {text}");
                }
                assert_eq!(panel.displayed.borrow().as_ref(), Some(&expected));
                let action = REGISTRY.handle_key(
                    KeyEvent::new(KeyCode::Char('y'), KeyModifiers::CONTROL),
                    &snapshot,
                    &mut panel,
                    area,
                    "",
                    None,
                );
                assert!(matches!(action, Some(ExtensionAction::Confirm(ref c)) if c == &expected));
            }
            for height in [16, 24] {
                let local = LocalState::default();
                let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
                terminal
                    .draw(|frame| super::super::render::draw(frame, &snapshot, &local))
                    .unwrap();
                assert_eq!(local.extension.displayed.borrow().as_ref(), Some(&expected));
            }
        }
        for (width, height) in [(30, 12), (40, 7)] {
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            terminal
                .draw(|frame| {
                    REGISTRY.draw_conversation_card(frame, frame.area(), &snapshot, &panel);
                })
                .unwrap();
            assert!(panel.displayed.borrow().is_none());
            let text = terminal
                .backend()
                .buffer()
                .content()
                .iter()
                .map(|cell| cell.symbol())
                .collect::<String>();
            let text: String = text.chars().filter(|c| !c.is_whitespace()).collect();
            assert!(text.contains("窗口过小"));
            assert!(text.contains("F6"));
        }
    }

    #[test]
    fn compact_conversation_card_keeps_long_fields_on_one_row_and_explains_blockers() {
        use unicode_width::UnicodeWidthStr;
        for condition in [
            "diagnostics",
            "review",
            "artifact",
            "ready",
            "sync",
            "authorizing",
            "approved",
        ] {
            let mut snapshot = snapshot();
            let ExtensionSnapshot::Modeling(domain) = &mut snapshot.extensions[0];
            domain
                .proposal_categories
                .insert(format!("长类别👨‍👩‍👧‍👦{}\n尾部", "很长".repeat(30)), 1);
            domain.proposal.as_mut().unwrap().diagnostics = (0..20)
                .map(|n| format!("诊断{n}\n{}", "很长".repeat(30)))
                .collect();
            let reason = match condition {
                "review" => {
                    domain.review_images.clear();
                    "复核图片尚未准备好"
                }
                "artifact" => {
                    domain.artifacts.clear();
                    "模型尚未建好"
                }
                "ready" => {
                    domain.proposal.as_mut().unwrap().ready = false;
                    "方案尚未准备好"
                }
                "sync" => {
                    domain.fact_sync_notice = Some("proposal #4：字段无效".into());
                    "proposal #4：字段无效"
                }
                "authorizing" => {
                    domain.authorizing = true;
                    "正在保存你的确认"
                }
                "approved" => {
                    domain.operation = Some(crate::domain::OperationFact {
                        operation_id: "op".into(),
                        state: "applied".into(),
                        recovery: "not_required".into(),
                        current_sha256: String::new(),
                    });
                    ""
                }
                _ => "Ctrl+Y 确认",
            };
            if condition == "approved" {
                assert!(domain.approval_card_snapshots().is_empty());
                let panel = ExtensionPanel::default();
                let mut terminal = Terminal::new(TestBackend::new(40, 8)).unwrap();
                terminal
                    .draw(|frame| {
                        REGISTRY.draw_conversation_card(frame, frame.area(), &snapshot, &panel);
                    })
                    .unwrap();
                assert!(panel.displayed.borrow().is_none());
                let screen = terminal
                    .backend()
                    .buffer()
                    .content()
                    .iter()
                    .map(|cell| cell.symbol())
                    .collect::<String>();
                assert!(!screen.contains("待你确认"));
                continue;
            }
            let full = super::super::domain::approval_card_lines(domain, 0);
            let lines = super::super::domain::compact_approval_card_lines(domain, &full, 38);
            assert_eq!(lines.len(), 7);
            assert!(lines
                .iter()
                .all(|line| !line.contains('\n') && line.width() <= 38));
            assert!(lines[1].contains("其他"));
            assert!(lines[6].contains(reason), "{condition}: {:?}", lines);
            assert!(!lines.iter().any(|line| line.contains("诊断")));
            let panel = ExtensionPanel::default();
            let mut terminal = Terminal::new(TestBackend::new(40, 8)).unwrap();
            terminal
                .draw(|frame| {
                    REGISTRY.draw_conversation_card(frame, frame.area(), &snapshot, &panel);
                })
                .unwrap();
            assert_eq!(
                panel.displayed.borrow().is_some(),
                condition == "diagnostics"
            );
            let buffer = terminal.backend().buffer();
            for (row, line) in lines.iter().enumerate() {
                let mut shown = String::new();
                let mut x = 1;
                while x < 39 {
                    let symbol = buffer[(x, row as u16 + 1)].symbol();
                    shown.push_str(symbol);
                    x += (symbol.width() as u16).max(1);
                }
                assert_eq!(shown.trim_end(), line);
            }
        }
    }

    #[test]
    fn conversation_cards_use_plain_chinese_for_structure_furniture_and_blocked_batches() {
        for batch in ["structure", "furniture", "blocked"] {
            for (width, height) in [(160, 40), (40, 8)] {
                let mut snapshot = snapshot();
                let ExtensionSnapshot::Modeling(domain) = &mut snapshot.extensions[0];
                let label = if batch == "furniture" {
                    domain
                        .proposal_categories
                        .insert("examination_bed".into(), 1);
                    domain.proposal.as_mut().unwrap().assumptions = vec![
                        r#"{"code":"assumed_value","height":0.65,"entity_id":"bed-internal-id"}"#
                            .into(),
                    ];
                    "检查床"
                } else {
                    domain.proposal_categories.extend([
                        ("wall".into(), 3),
                        ("column".into(), 2),
                        ("slab".into(), 1),
                        ("cad_reference".into(), 5),
                        ("wall_detail".into(), 4),
                        ("未分类".into(), 6),
                        ("unknown_english_category".into(), 7),
                    ]);
                    domain.proposal.as_mut().unwrap().assumptions = vec![
                        r#"{"code":"assumed_value","entity_id":"wall-internal-id","message":"walls: CAD-derived structure; parameter basis retained as assumptions"}"#.into(),
                    ];
                    "图纸参考线"
                };
                domain.proposal.as_mut().unwrap().summary =
                    format!("proposal PRIVATE_TOKEN {} cad_reference", "a".repeat(64));
                if batch == "blocked" {
                    domain.artifacts.clear();
                }
                let panel = ExtensionPanel::default();
                let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
                terminal
                    .draw(|frame| {
                        REGISTRY.draw_conversation_card(frame, frame.area(), &snapshot, &panel);
                    })
                    .unwrap();
                let text = terminal
                    .backend()
                    .buffer()
                    .content()
                    .chunks(usize::from(width))
                    .map(|row| {
                        let mut line = String::new();
                        let mut x = 0;
                        while x < row.len() {
                            let symbol = row[x].symbol();
                            line.push_str(symbol);
                            x += unicode_width::UnicodeWidthStr::width(symbol).max(1);
                        }
                        line
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                for forbidden in [
                    "proposal",
                    "PRIVATE_TOKEN",
                    "cad_reference",
                    "wall_detail",
                    "unknown_english_category",
                    "GLB：未知",
                    "SHA",
                    "投影",
                    "修订",
                    "preview",
                    "未纳入项",
                    &"a".repeat(64),
                ] {
                    assert!(
                        !text.contains(forbidden),
                        "{batch} {width}x{height}: {forbidden}: {text}"
                    );
                }
                for expected in ["待你确认", "内容", "尺寸依据", "图片", label] {
                    assert!(
                        text.contains(expected),
                        "{batch} {width}x{height}: {expected}: {text}"
                    );
                }
                if batch == "blocked" {
                    assert!(text.contains("还不能确认：模型尚未建好"));
                    assert!(!text.contains("Ctrl+Y 确认"));
                    assert!(panel.displayed.borrow().is_none());
                } else {
                    assert!(text.contains("Ctrl+Y 确认"), "{text}");
                    assert!(panel.displayed.borrow().is_some());
                }
                if batch == "furniture" {
                    assert!(text.contains("高度 0.65 米"));
                }
                if batch == "structure" && width == 160 {
                    for expected in [
                        "墙体 3 处",
                        "柱 2 处",
                        "楼板 1 处",
                        "图纸参考线 5 处",
                        "墙面细部 4 处",
                        "其他结构 6 处",
                        "其他 7 处",
                    ] {
                        assert!(text.contains(expected), "{expected}: {text}");
                    }
                }
            }
        }
    }

    #[test]
    fn conversation_card_never_confirms_small_windows_or_unreviewed_builds() {
        for condition in ["small", "review"] {
            let mut snapshot = snapshot();
            let mut local = LocalState {
                viewport: Rect::new(0, 0, 120, 44),
                ..Default::default()
            };
            match condition {
                "small" => local.viewport = Rect::new(0, 0, 30, 16),
                "review" => {
                    let ExtensionSnapshot::Modeling(domain) = &mut snapshot.extensions[0];
                    domain.artifacts[0].reviewed = false;
                }
                _ => unreachable!(),
            }
            let mut terminal = Terminal::new(TestBackend::new(
                local.viewport.width,
                local.viewport.height,
            ))
            .unwrap();
            terminal
                .draw(|frame| super::super::render::draw(frame, &snapshot, &local))
                .unwrap();
            assert!(local.extension.displayed.borrow().is_none(), "{condition}");
            let text = terminal
                .backend()
                .buffer()
                .content()
                .iter()
                .map(|cell| cell.symbol())
                .collect::<String>();
            let text: String = text.chars().filter(|c| !c.is_whitespace()).collect();
            assert!(!text.contains("Ctrl+Y确认"), "{condition}: {text}");
            if condition != "small" {
                assert!(text.contains("还不能确认"), "{condition}: {text}");
            }
            let (tx, mut rx) = tokio::sync::mpsc::channel(4);
            super::super::handle_key(
                KeyEvent::new(KeyCode::Char('y'), KeyModifiers::CONTROL),
                &snapshot,
                &mut local,
                &tx,
            );
            assert!(rx.try_recv().is_err(), "{condition}");
            assert_eq!(local.extension.visible, None);
        }
    }

    #[test]
    fn conversation_image_selection_is_typed_and_unavailable_images_are_not_opened() {
        let mut snapshot = snapshot();
        let ExtensionSnapshot::Modeling(domain) = &mut snapshot.extensions[0];
        let mut detail = domain.review_images[0].clone();
        detail.label = "候选局部".into();
        detail.view = crate::domain::ReviewImageView::Detail;
        detail.binding.image_sha256 = "b".repeat(64);
        domain.review_images.push(detail);
        let mut local = LocalState {
            viewport: Rect::new(0, 0, 120, 44),
            ..Default::default()
        };
        let (tx, mut rx) = tokio::sync::mpsc::channel(4);
        super::super::handle_key(
            KeyEvent::new(KeyCode::Right, KeyModifiers::ALT),
            &snapshot,
            &mut local,
            &tx,
        );
        assert_eq!(local.extension.selected_image, 1);
        super::super::handle_key(
            KeyEvent::new(KeyCode::Char('o'), KeyModifiers::CONTROL),
            &snapshot,
            &mut local,
            &tx,
        );
        let crate::client::Command::OpenDomainImage { binding } = rx.try_recv().unwrap() else {
            panic!("not an image command")
        };
        assert_eq!(binding.image_sha256, "b".repeat(64));
        let ExtensionSnapshot::Modeling(domain) = &mut snapshot.extensions[0];
        domain.review_images[1].file_state = crate::domain::ImageFileState::Unavailable;
        super::super::handle_key(
            KeyEvent::new(KeyCode::Char('o'), KeyModifiers::CONTROL),
            &snapshot,
            &mut local,
            &tx,
        );
        assert!(rx.try_recv().is_err());
        assert!(local.notice.as_deref().unwrap().contains("图片不可用"));
    }

    #[test]
    fn conversation_card_covered_by_help_cannot_confirm_before_or_after_redraw() {
        let snapshot = snapshot();
        let mut local = LocalState {
            viewport: Rect::new(0, 0, 120, 44),
            ..Default::default()
        };
        let mut terminal = Terminal::new(TestBackend::new(120, 44)).unwrap();
        terminal
            .draw(|frame| super::super::render::draw(frame, &snapshot, &local))
            .unwrap();
        assert!(local.extension.displayed.borrow().is_some());
        let (tx, mut rx) = tokio::sync::mpsc::channel(4);
        super::super::handle_key(
            KeyEvent::new(KeyCode::F(1), KeyModifiers::NONE),
            &snapshot,
            &mut local,
            &tx,
        );
        assert!(local.help);
        super::super::handle_key(
            KeyEvent::new(KeyCode::Char('y'), KeyModifiers::CONTROL),
            &snapshot,
            &mut local,
            &tx,
        );
        assert!(rx.try_recv().is_err());
        assert!(local.extension.displayed.borrow().is_none());
        terminal
            .draw(|frame| super::super::render::draw(frame, &snapshot, &local))
            .unwrap();
        assert!(local.extension.displayed.borrow().is_none());
    }
}
