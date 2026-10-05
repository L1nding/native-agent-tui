use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use ratatui::text::Line;
use ratatui::widgets::{Block, Borders, Paragraph};

use super::reminders::Reminders;
use super::wrap;
use crate::observation::{
    ActivityScope, ActivitySnapshot, AttentionLevel, CompactionFact, CompactionFactStatus,
    ExecutionState,
};
use crate::state::{display_text, CoreSnapshot, FactSource};
pub(super) fn age(ms: Option<u64>) -> String {
    ms.map_or_else(|| "unknown".into(), |ms| format!("{}s", ms / 1000))
}

pub(super) fn focus_activity<'a>(
    snapshot: &'a CoreSnapshot,
    agent_id: &str,
) -> Option<&'a ActivitySnapshot> {
    snapshot
        .observation
        .activities
        .iter()
        .filter(|activity| activity.identity.agent_id == agent_id)
        .max_by_key(|activity| {
            let rank = if activity.attention.requires_action {
                5
            } else if activity.attention.level == AttentionLevel::AttentionNeeded {
                4
            } else if matches!(
                activity.execution_state,
                ExecutionState::Starting | ExecutionState::Running | ExecutionState::Waiting
            ) {
                3
            } else if activity.scope == ActivityScope::Turn {
                2
            } else {
                1
            };
            (
                rank,
                activity.scope == ActivityScope::Tool,
                activity.silence_ms.unwrap_or(0),
            )
        })
}

pub(super) fn activity_brief(activity: &ActivitySnapshot) -> String {
    format!(
        "{:?} {} | quiet {} | {:?}",
        activity.execution_state,
        activity.tool_category.map_or_else(
            || format!("{:?}", activity.kind),
            |category| format!("{category:?}")
        ),
        age(activity.silence_ms),
        activity.attention.level
    )
}

pub(super) fn reminder_brief(activity: &ActivitySnapshot, reminders: &Reminders) -> String {
    let mut text = activity_brief(activity);
    if reminders.is_acknowledged(activity) {
        text.push_str(" | reminder off locally");
    }
    text
}

pub(super) fn next_action(activity: &ActivitySnapshot, reminders: &Reminders) -> &'static str {
    if activity.attention.requires_action {
        "F2 answer request"
    } else if activity.execution_state == ExecutionState::Unknown {
        "inspect unknown outcome"
    } else if reminders.is_acknowledged(activity) {
        "continuing to wait; Ctrl+W restore"
    } else if matches!(
        activity.attention.level,
        AttentionLevel::Quiet | AttentionLevel::AttentionNeeded
    ) {
        "F11 inspect; Ctrl+W wait or Ctrl+C"
    } else if activity.attention.level == AttentionLevel::Ended {
        "review result"
    } else {
        "wait; F11 evidence"
    }
}

pub(super) fn evidence_brief(activity: &ActivitySnapshot, reminders: &Reminders) -> String {
    format!(
        "Next: {} | Last: {}",
        next_action(activity, reminders),
        activity.last_evidence.as_ref().map_or_else(
            || "unknown".into(),
            |evidence| format!("{:?} {:?} #{}", evidence.kind, evidence.source, evidence.id)
        )
    )
}

pub(super) fn draw_evidence(
    frame: &mut ratatui::Frame<'_>,
    area: ratatui::layout::Rect,
    snapshot: &CoreSnapshot,
    reminders: &Reminders,
    evidence_scroll: usize,
    agent_id: &str,
    selected_agent: Option<&crate::agents::AgentSnapshot>,
) {
    let mut rows = Vec::new();
    let diagnostics = &snapshot.diagnostics;
    rows.push(format!(
        "Transport bytes in/out {} / {} | control events {} | telemetry events {}",
        diagnostics.transport_bytes_in,
        diagnostics.transport_bytes_out,
        diagnostics.control_events,
        diagnostics.telemetry_events
    ));
    rows.push(format_usage_evidence(snapshot, selected_agent));
    rows.push(format_token_budget_evidence(snapshot));
    if let Some(journal) = &snapshot.journal {
        rows.push(format!(
            "Session {} / committed {} / submitted {} / persistence {:?}",
            journal.session_id, journal.committed_seq, journal.submitted_seq, snapshot.persistence
        ));
    }
    let compactions = snapshot
        .observation
        .activities
        .iter()
        .filter(|activity| {
            activity.identity.agent_id == agent_id
                && activity.tool_category == Some(crate::protocol::ToolCategory::Compaction)
        })
        .count();
    rows.push(format!(
        "Compactions retained: {compactions} | lifetime total unavailable"
    ));
    for activity in snapshot
        .observation
        .activities
        .iter()
        .filter(|activity| activity.identity.agent_id == agent_id)
    {
        rows.push(format!(
            "{:?} {} | {:?}",
            activity.scope,
            activity.item_id.as_deref().unwrap_or("turn"),
            activity.attention.level
        ));
        if activity.tool_category == Some(crate::protocol::ToolCategory::Compaction) {
            rows.push(format!(
                "Compaction source {:?}",
                activity
                    .last_evidence
                    .as_ref()
                    .map(|evidence| evidence.source)
            ));
            rows.extend(compaction_fact_rows(compaction_fact_for_activity(
                snapshot, activity,
            )));
        }
        rows.push(activity_brief(activity));
        if reminders.is_acknowledged(activity) {
            rows.push("Silence reminder: off locally until new evidence; Ctrl+W restore".into());
        }
        rows.push(format!(
            "Last: {}",
            activity.last_evidence.as_ref().map_or_else(
                || "unknown".into(),
                |evidence| format!(
                    "{:?} / {:?} #{}",
                    evidence.kind, evidence.source, evidence.id
                )
            )
        ));
        rows.push(format!("Next: {}", next_action(activity, reminders)));
        rows.push(format!(
            "Elapsed {} / progress {} / bytes {}",
            age(activity.elapsed_ms),
            activity.progress_seq,
            activity.output_bytes
        ));
        rows.push(format!(
            "Thread {:?} turn {:?} gen {:?} attempt {:?}",
            activity.identity.thread_id,
            activity.identity.turn_id,
            activity.identity.generation,
            activity.identity.attempt_id
        ));
        rows.push(format!(
            "Quiet {:?}ms / attention {:?}ms / source {:?}",
            activity.attention.quiet_after_ms,
            activity.attention.attention_after_ms,
            activity.attention.config_source
        ));
        rows.push("Provider execution: unavailable".into());
        if let Some(reason) = activity.wait_reason {
            rows.push(format!(
                "Wait: {reason:?} / resume: {:?}",
                activity.resume_condition
            ));
        }
        for target in &activity.wait_targets {
            rows.push(format!(
                "Target {} / turn {:?} gen {} / {:?} / quiet {} / {:?}",
                target.thread_id,
                target.turn_id,
                target.generation,
                target.outcome,
                age(target.silence_ms),
                target.attention.as_ref().map(|attention| attention.level)
            ));
        }
        rows.push(String::new());
    }
    if rows.is_empty() {
        rows.push("Activity evidence unavailable.".into());
    }
    let lines: Vec<_> = rows
        .iter()
        .flat_map(|row| wrap(&display_text(row), area.width.saturating_sub(2) as usize))
        .map(Line::from)
        .collect();
    let height = area.height.saturating_sub(2) as usize;
    let start = evidence_scroll.min(lines.len().saturating_sub(height));
    frame.render_widget(
        Paragraph::new(
            lines
                .into_iter()
                .skip(start)
                .take(height)
                .collect::<Vec<_>>(),
        )
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Evidence · F11 · PgUp/PgDn "),
        ),
        area,
    );
}

fn compaction_fact_for_activity<'a>(
    snapshot: &'a CoreSnapshot,
    activity: &ActivitySnapshot,
) -> Option<&'a CompactionFact> {
    let thread_id = activity.identity.thread_id.as_ref()?;
    let turn_id = activity.identity.turn_id.as_ref()?;
    let item_id = activity.item_id.as_ref()?;
    snapshot.observation.compactions.iter().find(|fact| {
        fact.thread_id == *thread_id && fact.turn_id == *turn_id && fact.item_id == *item_id
    })
}

pub(super) fn compaction_fact_rows(fact: Option<&CompactionFact>) -> Vec<String> {
    let status = fact.map_or("unavailable", |fact| match fact.status {
        CompactionFactStatus::Started => "Started",
        CompactionFactStatus::Completed => "Completed",
        CompactionFactStatus::Unknown => "Unknown",
    });
    vec![
        format!("Compaction status: {status}"),
        format!(
            "Compaction usage: input {} | cached input {} | output {} | total {} | context window {}",
            usage_value(fact.and_then(|fact| fact.input_tokens)),
            usage_value(fact.and_then(|fact| fact.cached_input_tokens)),
            usage_value(fact.and_then(|fact| fact.output_tokens)),
            usage_value(fact.and_then(|fact| fact.total_tokens)),
            usage_value(fact.and_then(|fact| fact.context_window)),
        ),
    ]
}

pub(super) fn usage_status(snapshot: &CoreSnapshot) -> String {
    let usage = snapshot.usage;
    let base = token_budget_brief(snapshot);
    if usage.input_tokens.is_none()
        && usage.cached_input_tokens.is_none()
        && usage.output_tokens.is_none()
        && usage.reasoning_tokens.is_none()
    {
        return base;
    }
    format!(
        "{base} in:{} cached:{} out:{} reasoning:{}",
        usage_value(usage.input_tokens),
        usage_value(usage.cached_input_tokens),
        usage_value(usage.output_tokens),
        usage_value(usage.reasoning_tokens)
    )
}

pub(super) fn token_budget_brief(snapshot: &CoreSnapshot) -> String {
    let total = snapshot.token_budget.confirmed_total_tokens.map_or_else(
        || "unavailable".to_owned(),
        |total| {
            if snapshot.token_budget.confirmed_complete {
                total.to_string()
            } else {
                format!("partial {total}")
            }
        },
    );
    format!(
        "tokens:{}/{}",
        total,
        usage_value(snapshot.token_budget.limit)
    )
}

pub(super) fn format_token_budget_evidence(snapshot: &CoreSnapshot) -> String {
    let budget = snapshot.token_budget;
    format!(
        "Session token budget: {} | stop triggered: {} | Per-agent token budget: {}",
        token_budget_brief(snapshot),
        if budget.stop_triggered { "yes" } else { "no" },
        per_agent_budget_brief(budget)
    )
}

pub(super) fn per_agent_budget_brief(budget: crate::state::TokenBudgetSnapshot) -> String {
    let Some(limit) = budget.per_agent_limit else {
        return "not set".to_owned();
    };
    format!(
        "limit {limit} | stop triggered: {}",
        if budget.per_agent_stop_triggered {
            "yes"
        } else {
            "no"
        }
    )
}

pub(super) fn agent_usage_brief(agent: &crate::agents::AgentSnapshot) -> String {
    match (agent.usage.source, agent.usage.total_tokens) {
        (FactSource::ServerConfirmed, Some(tokens)) => format!("tokens:{tokens}"),
        _ => "tokens: unavailable".to_owned(),
    }
}

pub(super) fn truncate_display_label(text: &str, max_width: usize) -> String {
    let text = display_text(text);
    if UnicodeWidthStr::width(text.as_str()) <= max_width {
        return text;
    }
    if max_width == 0 {
        return String::new();
    }
    let mut output = String::new();
    let mut width = 0;
    for grapheme in text.graphemes(true) {
        let grapheme_width = UnicodeWidthStr::width(grapheme);
        if width + grapheme_width + 1 > max_width {
            break;
        }
        output.push_str(grapheme);
        width += grapheme_width;
    }
    output.push('…');
    output
}

pub(super) fn format_usage_evidence(
    snapshot: &CoreSnapshot,
    selected_agent: Option<&crate::agents::AgentSnapshot>,
) -> String {
    let (owner, usage) = selected_agent.map_or_else(
        || ("root".to_owned(), snapshot.usage),
        |agent| (format!("child {}", agent.info.id), agent.usage),
    );
    let total = usage.total_tokens;
    format!(
        "Usage ({owner}) source: {} | total {} | input {} | cached {} | output {} | reasoning {} | context window {}",
        source_label(usage.source),
        usage_value(total),
        usage_value(usage.input_tokens),
        usage_value(usage.cached_input_tokens),
        usage_value(usage.output_tokens),
        usage_value(usage.reasoning_tokens),
        usage_value(usage.context_window),
    )
}

pub(super) fn usage_value(value: Option<u64>) -> String {
    value.map_or_else(|| "unavailable".to_owned(), |value| value.to_string())
}

pub(super) fn source_label(source: FactSource) -> &'static str {
    match source {
        FactSource::ServerConfirmed => "server confirmed",
        FactSource::LocalEstimate => "local estimate",
        FactSource::Unknown => "unknown",
    }
}
