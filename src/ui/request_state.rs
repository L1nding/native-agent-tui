//! Request selection, submission locks, and per-request answer drafts.

use crate::interactions::{RequestKind, RequestView};
use crate::state::{CoreSnapshot, SessionPhase};

use super::{InputDraft, LocalState};

pub(super) fn selected_request<'a>(
    snapshot: &'a CoreSnapshot,
    local: &LocalState,
) -> Option<&'a RequestView> {
    match &local.request_selection {
        Some(reference) => snapshot
            .requests
            .iter()
            .find(|request| request.matches(reference)),
        None => snapshot.requests.iter().find(|request| !request.responding),
    }
}

pub(super) fn sync_local_requests(local: &mut LocalState, snapshot: &CoreSnapshot) {
    if local.request_selection.is_none() {
        local.request_selection = selected_request(snapshot, local).map(RequestView::reference);
    } else if snapshot.requests.is_empty() && !local.request_panel {
        local.request_selection = None;
    }
    sync_questions(local, selected_request(snapshot, local).cloned());
    local.input_drafts.retain(|reference, _| {
        snapshot
            .requests
            .iter()
            .any(|request| request.matches(reference) && !request.responding)
    });
    local.submitted.retain(|reference| {
        snapshot
            .requests
            .iter()
            .any(|request| request.matches(reference))
    });
}

pub(super) fn request_locked(
    snapshot: &CoreSnapshot,
    local: &LocalState,
    request: &RequestView,
) -> bool {
    request.responding
        || local.submitted.contains(&request.reference())
        || matches!(
            snapshot.phase,
            SessionPhase::Unknown
                | SessionPhase::Disconnected
                | SessionPhase::Stopping
                | SessionPhase::ClosingTransport
                | SessionPhase::Stopped
        )
}

pub(super) fn sync_questions(local: &mut LocalState, request: Option<RequestView>) {
    let id = request
        .filter(|r| matches!(r.kind, RequestKind::UserInput { .. }))
        .map(|r| r.reference());
    if local.answering != id {
        if let Some(reference) = local.answering.take() {
            local.input_drafts.insert(
                reference,
                InputDraft {
                    editor: std::mem::take(&mut local.editor),
                    question_index: local.question_index,
                    answers: std::mem::take(&mut local.answers),
                },
            );
        }
        match (local.task_draft.is_some(), id.is_some()) {
            (false, true) => local.task_draft = Some(std::mem::take(&mut local.editor)),
            (true, false) => local.editor = local.task_draft.take().unwrap_or_default(),
            (true, true) => local.editor.clear(),
            (false, false) => {}
        }
        local.answers.clear();
        local.question_index = 0;
        if let Some(draft) = id
            .as_ref()
            .and_then(|reference| local.input_drafts.remove(reference))
        {
            local.editor = draft.editor;
            local.question_index = draft.question_index;
            local.answers = draft.answers;
        }
        local.answering = id;
    }
}
