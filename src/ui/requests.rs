//! Request detail rendering from Core's bounded live projection.
use ratatui::layout::Rect;
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders, Clear, Paragraph};

use super::layout::wrap;
use super::{request_locked, selected_request, LocalState};
use crate::interactions::{RequestKind, RequestView};
use crate::state::{display_text, CoreSnapshot};

pub(super) fn actions(
    snapshot: &CoreSnapshot,
    local: &LocalState,
    request: &RequestView,
) -> String {
    if request_locked(snapshot, local, request) {
        return "Submitted or unavailable; answers disabled".into();
    }
    if matches!(request.kind, RequestKind::UserInput { .. }) {
        return "Enter answer | F2 next request".into();
    }
    let mut actions = Vec::new();
    if request.allow_accept {
        actions.push("Ctrl+Y accept");
    }
    if request.allow_decline {
        actions.push("Ctrl+N decline");
    }
    if request.allow_cancel {
        actions.push("Ctrl+B stop turn");
    }
    if actions.is_empty() {
        actions.push("No supported decision; Ctrl+C interrupt root");
    }
    actions.join(" | ")
}

fn details(request: &RequestView, snapshot: &CoreSnapshot, local: &LocalState) -> Vec<String> {
    let mut lines = vec![
        format!(
            "Request: {:?} | received sequence {}",
            request.id, request.received_seq
        ),
        format!("Agent/thread: {}", request.thread_id),
        format!("Turn: {}", request.turn_id),
        format!(
            "Item: {}",
            request.details.item_id.as_deref().unwrap_or("unavailable")
        ),
        format!(
            "Server start ms: {}",
            request
                .details
                .started_at_ms
                .map_or_else(|| "unavailable".into(), |ms| ms.to_string())
        ),
        format!(
            "Kind: {}",
            match request.kind {
                RequestKind::CommandApproval => "command approval",
                RequestKind::FileApproval => "file approval",
                RequestKind::UserInput { .. } => "user input",
            }
        ),
        format!(
            "State: {}",
            if request_locked(snapshot, local, request) {
                "submitted or unavailable"
            } else {
                "awaiting your answer"
            }
        ),
    ];
    let root = snapshot.thread_id.as_deref() == Some(&request.thread_id);
    if root {
        lines.push(format!("Effective sandbox:\n{}", snapshot.sandbox));
        lines.push(format!(
            "Effective approval policy:\n{}",
            snapshot.approval_policy
        ));
    } else {
        lines.push("Child effective sandbox / approval policy: unavailable".into());
        lines.push(format!(
            "Root sandbox: {} | approval policy: {}",
            snapshot.sandbox, snapshot.approval_policy
        ));
    }
    lines.push(format!("Session cwd: {}", snapshot.cwd));
    lines.push("Context source: server request".into());
    lines.push("Proposals: not applied".into());
    match &request.kind {
        RequestKind::UserInput { questions } => {
            lines.push(format!(
                "Server blocking hint: {}",
                request
                    .details
                    .is_blocking
                    .map_or_else(|| "unavailable".into(), |blocking| blocking.to_string())
            ));
            lines.push(format!(
                "Legacy auto-resolution hint: {}",
                request.details.auto_resolution_ms.map_or_else(
                    || "unavailable".into(),
                    |ms| format!("{ms} ms (informational)")
                )
            ));
            for (index, question) in questions.iter().enumerate() {
                lines.push(format!(
                    "\nQuestion {}/{} · {} · id={}{}{}",
                    index + 1,
                    questions.len(),
                    question.header,
                    question.id,
                    if index == local.question_index {
                        " [current]"
                    } else {
                        ""
                    },
                    if question.is_secret {
                        " [secret answer masked]"
                    } else {
                        ""
                    }
                ));
                lines.push(question.question.clone());
                if let Some(options) = &question.options {
                    for option in options {
                        lines.push(format!("  {}: {}", option.label, option.description));
                    }
                } else {
                    lines.push("Options: free text".into());
                }
            }
        }
        _ => {
            lines.push(format!(
                "Server decisions:\n{}",
                request.details.available_decisions.as_ref().map_or_else(
                    || "unavailable; client choices: accept / decline / cancel".into(),
                    |decisions| if decisions.is_empty() {
                        "none".into()
                    } else {
                        decisions.join("\n")
                    }
                )
            ));
            lines.push("Cancel rejects this approval and interrupts its owning turn. Decline rejects it and lets the agent continue.".into());
            lines.push("UI supports accept / decline / cancel. Session grants and policy amendments require a supported decision form.".into());
            lines.push("Risk assessment: unavailable (review the request context)".into());
            if matches!(request.kind, RequestKind::CommandApproval) {
                for label in ["Command", "Command cwd"] {
                    if !request
                        .details
                        .fields
                        .iter()
                        .any(|field| field.label == label)
                    {
                        lines.push(format!("{label}: unavailable"));
                    }
                }
            }
            for field in &request.details.fields {
                lines.push(format!("\n{}:\n{}", field.label, field.text));
            }
            if matches!(request.kind, RequestKind::FileApproval) {
                match &request.details.file_preview {
                    Some(preview) if !preview.unavailable => {
                        lines.push(format!(
                            "\nFile preview: server fileChange item, sequence {}{}",
                            preview.source_seq,
                            if preview.truncated {
                                " [truncated at 32 KiB]"
                            } else {
                                ""
                            }
                        ));
                        lines.push(preview.text.clone());
                    }
                    Some(_) => lines.push(
                        "File preview: unavailable (missing or invalid server changes)".into(),
                    ),
                    None => lines.push(
                        "File preview: unavailable (no matching retained server item)".into(),
                    ),
                }
            }
        }
    }
    lines
}

pub(super) fn draw(frame: &mut ratatui::Frame<'_>, snapshot: &CoreSnapshot, local: &LocalState) {
    let area = frame.area();
    // Preserve the masked live editor and reserve fixed action/navigation rows.
    let panel = Rect {
        height: area.height.saturating_sub(4),
        ..area
    };
    frame.render_widget(Clear, panel);
    let request = selected_request(snapshot, local);
    let text = if let Some(request) = request {
        details(request, snapshot, local).join("\n")
    } else if let Some(reference) = &local.request_selection {
        format!("Selected request {:?} expired or resolved.\nThread: {}\nTurn: {}\nReceived sequence: {}\nAnswers disabled. F2 explicitly selects a current request.", reference.id, reference.thread_id, reference.turn_id, reference.received_seq)
    } else {
        "No pending requests.".into()
    };
    let lines = wrap(&display_text(&text), area.width.saturating_sub(2) as usize);
    let actions = request.map_or_else(
        || "Answers disabled".into(),
        |request| actions(snapshot, local, request),
    );
    let action_text = display_text(local.notice.as_deref().unwrap_or(&actions));
    let action_lines = if action_text.len() <= area.width as usize {
        vec![action_text]
    } else if local.notice.is_none() {
        action_text
            .split(" | ")
            .flat_map(|action| wrap(action, area.width as usize))
            .collect()
    } else {
        wrap(&action_text, area.width as usize)
    };
    let action_height = action_lines.len().min(3) as u16;
    let body = Rect {
        height: panel.height.saturating_sub(action_height),
        ..panel
    };
    let height = body.height.saturating_sub(2) as usize;
    let start = local.request_scroll.min(lines.len().saturating_sub(height));
    let visible: Vec<_> = lines
        .iter()
        .skip(start)
        .take(height)
        .cloned()
        .map(Line::from)
        .collect();
    frame.render_widget(
        Paragraph::new(visible).block(Block::default().borders(Borders::ALL).title(format!(
            " Requests: {} | {:?} ",
            snapshot.requests.len(),
            snapshot.phase
        ))),
        body,
    );
    frame.render_widget(
        Paragraph::new(action_lines.into_iter().map(Line::from).collect::<Vec<_>>()),
        Rect {
            y: panel.y + panel.height.saturating_sub(action_height),
            height: action_height,
            ..area
        },
    );
    frame.render_widget(
        Paragraph::new("F2 next  PgUp/Dn  Esc close"),
        Rect {
            y: area.y + area.height.saturating_sub(1),
            height: 1,
            ..area
        },
    );
}
