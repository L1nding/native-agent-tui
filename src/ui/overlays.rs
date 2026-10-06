//! Palette overlay transitions and mutual overlay cleanup.

use super::commands::PaletteAction;
use super::workflow_view;
use super::{selected_request, sync_local_requests, AttentionEditor, LocalState};
use crate::state::CoreSnapshot;

fn close_mutual_overlays(local: &mut LocalState) {
    local.search.close();
    local.timeline.close();
    local.skills = false;
    local.context.visible = false;
    local.context.scroll = 0;
    local.request_panel = false;
    local.evidence = false;
    local.workflow.visible = false;
    local.workflow.link_cursor = None;
    local.workflow.scroll = 0;
    local.workflow.manual_scroll = false;
    local.attention_editor = None;
    local.help = false;
}

pub(super) fn open_palette_action(
    action: PaletteAction,
    snapshot: &CoreSnapshot,
    local: &mut LocalState,
) {
    let already_visible = match action {
        PaletteAction::Search => local.search.visible,
        PaletteAction::Timeline => local.timeline.visible,
        PaletteAction::Context => local.context.visible,
        PaletteAction::Skills => local.skills,
        PaletteAction::Requests => local.request_panel,
        PaletteAction::Workflow => local.workflow.visible,
        PaletteAction::Evidence => local.evidence,
        PaletteAction::Attention => local.attention_editor.is_some(),
        PaletteAction::Help => local.help,
        PaletteAction::NextAgent => false,
    };
    close_mutual_overlays(local);
    if already_visible {
        return;
    }
    match action {
        PaletteAction::Search => {
            let thread = local
                .agent_id
                .clone()
                .or_else(|| snapshot.thread_id.clone())
                .unwrap_or_default();
            local.search.open(thread);
        }
        PaletteAction::Timeline => {
            let thread = local
                .agent_id
                .clone()
                .or_else(|| snapshot.thread_id.clone())
                .unwrap_or_default();
            local.timeline.open(thread, snapshot);
        }
        PaletteAction::Context => local.context.toggle(),
        PaletteAction::Skills => {
            local.skills = true;
            local.skills_scroll = 0;
        }
        PaletteAction::Requests => {
            let current = selected_request(snapshot, local)
                .filter(|request| !request.responding)
                .or_else(|| snapshot.requests.iter().find(|request| !request.responding));
            if let Some(request) = current {
                local.request_selection = Some(request.reference());
                local.request_panel = true;
                local.request_scroll = 0;
                sync_local_requests(local, snapshot);
                local.notice = None;
            } else {
                local.notice = Some("No pending requests.".into());
            }
        }
        PaletteAction::Workflow => {
            local.workflow.visible = true;
        }
        PaletteAction::Evidence => {
            local.evidence = true;
            local.evidence_scroll = 0;
        }
        PaletteAction::Attention => {
            local.attention_editor = Some(AttentionEditor::new(snapshot, 0));
        }
        PaletteAction::Help => {
            local.help = true;
        }
        PaletteAction::NextAgent => {
            local.conversation_focus = None;
            local.agent_id = workflow_view::next_agent_id(snapshot, local.agent_id.as_deref());
            local.scroll_from_bottom = 0;
        }
    }
}
