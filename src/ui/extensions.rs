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
            .filter(|_| {
                snapshot.phase.can_submit()
                    && !snapshot
                        .scheduler
                        .tasks
                        .iter()
                        .any(|task| task.state.active())
            })
            .filter(|confirmation| confirmation.id() == id);
        true
    }

    pub fn draw_conversation_card(
        &self,
        frame: &mut ratatui::Frame<'_>,
        area: Rect,
        snapshot: &CoreSnapshot,
        panel: &ExtensionPanel,
    ) -> Rect {
        let Some((view, projection)) = self.projection(snapshot, ExtensionId::Modeling) else {
            return area;
        };
        let domain = projection.modeling().expect("modeling projection");
        let content = super::domain::approval_card_lines(domain, panel.selected_image);
        if content.is_empty() {
            return area;
        }
        let lines: Vec<_> = content
            .iter()
            .flat_map(|line| {
                super::layout::wrap_text(line, usize::from(area.width.saturating_sub(2).max(1)))
            })
            .collect();
        let needed = lines.len().saturating_add(2);
        let complete = area.width >= 24 && needed <= usize::from(area.height.saturating_sub(1));
        let height = if complete {
            needed as u16
        } else {
            area.height.saturating_sub(1).min(4)
        };
        let card = Rect {
            y: area.y + area.height.saturating_sub(height),
            height,
            ..area
        };
        let text = if complete {
            lines.join("\n")
        } else {
            "建模审批信息未完整显示，暂不能在会话页批准。\nF6 查看详情；放大窗口后可直接 Ctrl+Y。"
                .into()
        };
        frame.render_widget(
            ratatui::widgets::Paragraph::new(text).block(
                ratatui::widgets::Block::default()
                    .borders(ratatui::widgets::Borders::ALL)
                    .title("本版建模审批"),
            ),
            card,
        );
        if complete
            && snapshot.phase.can_submit()
            && !snapshot
                .scheduler
                .tasks
                .iter()
                .any(|task| task.state.active())
            && snapshot.requests.is_empty()
        {
            *panel.displayed.borrow_mut() = view.confirmation(projection);
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
        let image_or_approval_key = matches!((key.code, key.modifiers),
            (KeyCode::Char('y' | 'o'), modifiers) if modifiers.contains(KeyModifiers::CONTROL))
            || (matches!(key.code, KeyCode::Left | KeyCode::Right)
                && key.modifiers.contains(KeyModifiers::ALT));
        if panel.visible.is_none() && !image_or_approval_key {
            return None;
        }
        if panel.visible.is_none() && projection.modeling()?.proposal.is_none() {
            return None;
        }
        let context = ViewContext::new(snapshot, panel, draft, notice);
        match key.code {
            KeyCode::Left | KeyCode::Right if key.modifiers.contains(KeyModifiers::ALT) => {
                let count = projection.modeling()?.review_images.len();
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
                let images = &projection.modeling()?.review_images;
                return Some(
                    images
                        .get(panel.selected_image.min(images.len().saturating_sub(1)))
                        .filter(|image| image.verified_path().is_some())
                        .map_or(ExtensionAction::ImageUnavailable, |image| {
                            ExtensionAction::OpenImage(image.binding.clone())
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
                return Some(
                    panel
                        .displayed
                        .borrow()
                        .clone()
                        .filter(|confirmation| confirmation.id() == id)
                        .filter(|confirmation| {
                            view.confirmation(projection).as_ref() == Some(confirmation)
                                && snapshot.phase.can_submit()
                                && !snapshot
                                    .scheduler
                                    .tasks
                                    .iter()
                                    .any(|task| task.state.active())
                                && area.width >= 24
                                && area.height >= 8
                        })
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
    OpenImage(crate::domain::ReviewImageBinding),
    ImageUnavailable,
}

#[cfg(test)]
mod tests {
    use super::super::LocalState;
    use super::*;
    use crate::domain::{
        ArtifactBinding, ArtifactFact, ArtifactState, DomainSnapshot, ProposalBinding, ProposalFact,
    };
    use ratatui::{backend::TestBackend, Terminal};

    fn snapshot() -> CoreSnapshot {
        let mut snapshot = CoreSnapshot {
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
    fn running_model_turn_cannot_present_an_actionable_modeling_approval() {
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
        assert!(
            local.extension.displayed.borrow().is_none(),
            "running turn offered approval that Core will reject"
        );
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
        for change in ["invalid", "different-build", "binding", "running"] {
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
                "running" => snapshot.phase = SessionPhase::Running,
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
            "修订 7",
            "1274",
            "295",
            "2559",
            "示意参数",
            "墙体：依据CAD重建",
            "门窗洞口：依据CAD重建",
            "楼板：依据CAD重建",
            "其他结构实体：依据CAD重建",
            "约束：依据CAD重建",
            "GLB",
            "未纳入项",
            "Ctrl+Y",
        ] {
            let expected: String = expected.chars().filter(|c| !c.is_whitespace()).collect();
            assert!(compact.contains(&expected), "{expected} missing");
        }
        assert!(!text.contains("信息未完整"));
        assert!(
            !text.contains("category-9"),
            "progress table took priority over the approval"
        );
        assert_eq!(
            super::super::domain::approval_card_lines(
                snapshot.extensions[0].modeling().unwrap(),
                0
            )
            .iter()
            .filter(|line| line.starts_with("示意假设"))
            .count(),
            5
        );
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
    fn approval_card_preserves_different_assumption_parameters_and_unknown_text() {
        let mut snapshot = snapshot();
        let ExtensionSnapshot::Modeling(domain) = &mut snapshot.extensions[0];
        let original = vec![
            serde_json::json!({"code":"assumed_value","message":"height","entity_id":"a","height":0.95}).to_string(),
            serde_json::json!({"code":"assumed_value","message":"height","entity_id":"b","height":1.05}).to_string(),
            serde_json::json!({"code":"assumed_value","message":"unknown height 0.95","entity_id":"c"}).to_string(),
            serde_json::json!({"code":"assumed_value","message":"unknown height 1.05","entity_id":"d"}).to_string(),
            "unstructured assumption with complete dimensions 0.5 x 0.7 x 1.05".into(),
        ];
        domain.proposal.as_mut().unwrap().assumptions = original.clone();
        let lines = super::super::domain::approval_card_lines(domain, 0);
        for text in [&original[0], &original[1], &original[4]] {
            assert!(
                lines.iter().any(|line| line.contains(text)),
                "assumption omitted: {text}"
            );
        }
        for message in ["unknown height 0.95", "unknown height 1.05"] {
            assert!(lines.iter().any(|line| line.contains(message)));
        }
        assert_eq!(domain.proposal.as_ref().unwrap().assumptions, original);
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
            "扶手椅 100",
            "示意假设",
            "已构建",
            "验证：通过",
            "版本：当前",
            "GLB：未知",
            "对照 0 · 总览 1 · 局部 0",
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
        let mut snapshot = snapshot();
        let mut local = LocalState {
            viewport: Rect::new(0, 0, 120, 44),
            ..Default::default()
        };
        let mut terminal = Terminal::new(TestBackend::new(120, 44)).unwrap();
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

    #[test]
    fn conversation_card_never_confirms_clipped_assumptions_small_windows_or_running_tasks() {
        for condition in ["small", "assumptions", "running", "task", "review"] {
            let mut snapshot = snapshot();
            let mut local = LocalState {
                viewport: Rect::new(0, 0, 120, 44),
                ..Default::default()
            };
            match condition {
                "small" => local.viewport = Rect::new(0, 0, 40, 16),
                "assumptions" => {
                    let ExtensionSnapshot::Modeling(domain) = &mut snapshot.extensions[0];
                    domain.proposal.as_mut().unwrap().assumptions =
                        (0..100).map(|n| format!("height {n} assumed")).collect();
                }
                "running" => snapshot.phase = SessionPhase::Running,
                "review" => {
                    let ExtensionSnapshot::Modeling(domain) = &mut snapshot.extensions[0];
                    domain.artifacts[0].reviewed = false;
                }
                "task" => {
                    let mut scheduler = crate::scheduler::Scheduler::default();
                    scheduler
                        .enqueue(vec![crate::scheduler::RootTaskSpec::input("work".into())])
                        .unwrap();
                    scheduler.dispatch().unwrap();
                    snapshot.scheduler = scheduler.snapshot();
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
