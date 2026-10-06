use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::Rect;
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use ratatui::Frame;

use crate::agents::AgentSnapshot;
use crate::observation::CompactionFact;
use crate::state::{CoreSnapshot, FactSource};

#[derive(Debug, Default)]
pub(super) struct ContextPanel {
    pub visible: bool,
    pub scroll: usize,
    from_end: bool,
}

impl ContextPanel {
    pub fn toggle(&mut self) {
        self.visible = !self.visible;
        if self.visible {
            self.scroll = 0;
            self.from_end = false;
        }
    }

    /// 处理面板内导航，不产生 Core 命令。
    pub fn handle_key(&mut self, key: KeyEvent, page_size: usize) -> bool {
        if !self.visible {
            return false;
        }
        match key.code {
            KeyCode::Esc | KeyCode::Char('g')
                if key.code == KeyCode::Esc || key.modifiers.contains(KeyModifiers::CONTROL) =>
            {
                self.visible = false;
                self.scroll = 0;
                self.from_end = false;
            }
            KeyCode::PageUp if self.from_end => {
                self.scroll = self.scroll.saturating_add(page_size.max(1))
            }
            KeyCode::PageUp => self.scroll = self.scroll.saturating_sub(page_size.max(1)),
            KeyCode::PageDown if self.from_end => {
                self.scroll = self.scroll.saturating_sub(page_size.max(1))
            }
            KeyCode::PageDown => self.scroll = self.scroll.saturating_add(page_size.max(1)),
            KeyCode::Home if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.scroll = 0;
                self.from_end = false;
            }
            KeyCode::End if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.scroll = 0;
                self.from_end = true;
            }
            KeyCode::Home => {
                self.scroll = 0;
                self.from_end = false;
            }
            KeyCode::End => {
                self.scroll = 0;
                self.from_end = true;
            }
            _ => return false,
        }
        true
    }

    pub fn draw(
        &self,
        frame: &mut Frame<'_>,
        area: Rect,
        snapshot: &CoreSnapshot,
        selected_agent: Option<&AgentSnapshot>,
    ) {
        let mut rows = vec![crate::ui::activity::format_usage_evidence(
            snapshot,
            selected_agent,
        )];
        let usage_fact = crate::ui::activity::usage_fact_for(snapshot, selected_agent);
        let owner_thread = selected_agent
            .map(|agent| agent.info.id.as_str())
            .or(snapshot.thread_id.as_deref());
        let identity = usage_fact.map(|fact| &fact.identity);
        let selected_thread = identity.and_then(|identity| identity.thread_id.as_deref());
        let selected_turn = identity.and_then(|identity| identity.turn_id.as_deref());
        let generation = identity
            .and_then(|identity| identity.generation)
            .map_or_else(
                || "unavailable".to_owned(),
                |generation| generation.to_string(),
            );
        let usage_source = usage_fact.map_or(FactSource::Unknown, |fact| fact.summary.source);
        let identity_label = selected_agent.map_or("root", |_| "child");
        rows.push(format!(
            "Usage identity {identity_label} · thread {} · turn {} · generation {generation}",
            selected_thread.unwrap_or("unavailable"),
            selected_turn.unwrap_or("unavailable"),
        ));
        rows.push(
            if usage_fact.is_some() && usage_source == FactSource::ServerConfirmed {
                "Usage recency: most recent server-confirmed value for this thread/turn/generation"
                    .to_owned()
            } else {
                "Usage recency: current thread/turn/generation usage unavailable".to_owned()
            },
        );

        let budget = snapshot.token_budget;
        let completeness = if budget.confirmed_total_tokens.is_none() {
            "unavailable"
        } else if budget.confirmed_complete {
            "complete"
        } else {
            "partial"
        };
        rows.push(format!(
            "Session-wide token budget: total {} | limit {} | completeness {} | stop triggered {} | per-agent limit {}",
            crate::ui::activity::usage_value(budget.confirmed_total_tokens),
            crate::ui::activity::usage_value(budget.limit),
            completeness,
            yes_no(budget.stop_triggered),
            budget.per_agent_limit.map_or_else(
                || "unavailable".to_owned(),
                |limit| limit.to_string()
            ),
        ));
        rows.push(format!(
            "Per-agent stop triggered: {}",
            yes_no(budget.per_agent_stop_triggered)
        ));

        let compactions: Vec<_> = owner_thread.map_or_else(Vec::new, |thread_id| {
            snapshot
                .observation
                .compactions
                .iter()
                .filter(|fact| fact.thread_id == thread_id)
                .collect()
        });
        rows.push(format!(
            "Compactions retained for selected agent: {} | lifetime total unavailable",
            compactions.len()
        ));
        for (index, fact) in compactions.iter().enumerate() {
            rows.push(format!(
                "Compaction #{} · turn {} · item {}",
                index + 1,
                fact.turn_id,
                fact.item_id
            ));
            rows.push(format!(
                "  status {} | started at {} | completed at {}",
                compaction_status(fact),
                crate::ui::activity::usage_value(fact.started_at_ms),
                crate::ui::activity::usage_value(fact.completed_at_ms),
            ));
            rows.push(format!(
                "  usage: input {} | cached input {} | output {} | total {} | context window {}",
                crate::ui::activity::usage_value(fact.input_tokens),
                crate::ui::activity::usage_value(fact.cached_input_tokens),
                crate::ui::activity::usage_value(fact.output_tokens),
                crate::ui::activity::usage_value(fact.total_tokens),
                crate::ui::activity::usage_value(fact.context_window),
            ));
        }
        if compactions.is_empty() {
            rows.push("Compaction facts: unavailable".to_owned());
        }

        let inner_width = area.width.saturating_sub(2) as usize;
        let inner_height = area.height.saturating_sub(2) as usize;
        let wrapped_rows: Vec<_> = rows
            .iter()
            .flat_map(|row| super::wrap(row, inner_width))
            .collect();
        let max_scroll = wrapped_rows.len().saturating_sub(inner_height);
        let scroll = self.scroll.min(max_scroll);
        let scroll = if self.from_end {
            max_scroll.saturating_sub(scroll)
        } else {
            scroll
        };
        frame.render_widget(
            Paragraph::new(
                wrapped_rows
                    .into_iter()
                    .map(ratatui::text::Line::from)
                    .collect::<Vec<_>>(),
            )
            .wrap(Wrap { trim: true })
            .scroll((scroll.min(u16::MAX as usize) as u16, 0))
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(" Context · Ctrl+G/Esc close · PgUp/PgDn/Home/End scroll "),
            ),
            area,
        );
    }
}

fn yes_no(value: bool) -> &'static str {
    if value {
        "yes"
    } else {
        "no"
    }
}

fn compaction_status(fact: &CompactionFact) -> &'static str {
    match fact.status {
        crate::observation::CompactionFactStatus::Started => "Started",
        crate::observation::CompactionFactStatus::Completed => "Completed",
        crate::observation::CompactionFactStatus::Unknown => "Unknown",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{backend::TestBackend, Terminal};

    #[test]
    fn narrow_context_shows_unavailable_usage_and_keeps_compaction_by_owner_thread() {
        let snapshot = CoreSnapshot {
            thread_id: Some("root-thread".into()),
            turn_id: Some("new-turn".into()),
            usage: crate::state::UsageSummary {
                total_tokens: Some(9),
                source: FactSource::ServerConfirmed,
                ..Default::default()
            },
            observation: crate::observation::ObservationSnapshot {
                compactions: vec![crate::observation::CompactionFact {
                    thread_id: "root-thread".into(),
                    turn_id: "old-turn".into(),
                    item_id: "old-compaction".into(),
                    status: crate::observation::CompactionFactStatus::Completed,
                    started_at_ms: None,
                    completed_at_ms: None,
                    input_tokens: Some(8),
                    cached_input_tokens: None,
                    output_tokens: None,
                    total_tokens: Some(9),
                    context_window: None,
                }],
                ..Default::default()
            },
            ..Default::default()
        };
        let panel = ContextPanel {
            visible: true,
            ..Default::default()
        };
        let mut terminal = Terminal::new(TestBackend::new(72, 18)).unwrap();
        terminal
            .draw(|frame| panel.draw(frame, frame.area(), &snapshot, None))
            .unwrap();
        let screen: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(screen.contains("total unavailable"), "{screen}");
        assert!(screen.contains("thread unavailable"), "{screen}");
        assert!(screen.contains("generation unavailable"), "{screen}");
        assert!(screen.contains("old-compaction"), "{screen}");
    }
}
