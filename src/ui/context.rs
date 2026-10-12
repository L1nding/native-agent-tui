use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::widgets::{Borders, Clear, Paragraph, Wrap};
use ratatui::Frame;

use crate::agents::AgentSnapshot;
use crate::observation::CompactionFact;
use crate::state::{CoreSnapshot, FactSource};

use super::activity::{footer, label_mark, shown_value, unavailable, wrap_marked, yes_no};
use super::lang::{self, tr};

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
        // 整屏浮层；先清空，头部和对话区的字符才不会从短行后面透出来。
        frame.render_widget(Clear, area);
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
        let generation = shown_value(identity.and_then(|identity| identity.generation));
        let usage_source = usage_fact.map_or(FactSource::Unknown, |fact| fact.summary.source);
        let zh = lang::zh();
        let missing = unavailable();
        let thread = selected_thread.unwrap_or(missing);
        let turn = selected_turn.unwrap_or(missing);
        rows.push(if zh {
            let owner = selected_agent.map_or("root", |_| "子 Agent");
            format!("用量归属 {owner} · 线程 {thread} · 轮次 {turn} · 代次 {generation}")
        } else {
            let identity_label = selected_agent.map_or("root", |_| "child");
            format!(
                "Usage identity {identity_label} · thread {thread} · turn {turn} · generation {generation}"
            )
        });
        rows.push(
            if usage_fact.is_some() && usage_source == FactSource::ServerConfirmed {
                tr(
                    "Usage recency: most recent server-confirmed value for this thread/turn/generation",
                    "用量时效: 本线程/轮次/代次最近一次由服务端确认的值",
                )
            } else {
                tr(
                    "Usage recency: current thread/turn/generation usage unavailable",
                    "用量时效: 当前线程/轮次/代次的用量不可用",
                )
            }
            .to_owned(),
        );

        let budget = snapshot.token_budget;
        let completeness = if budget.confirmed_total_tokens.is_none() {
            missing
        } else if budget.confirmed_complete {
            tr("complete", "完整")
        } else {
            tr("partial", "部分")
        };
        let total = shown_value(budget.confirmed_total_tokens);
        let limit = shown_value(budget.limit);
        let stopped = yes_no(budget.stop_triggered);
        let per_agent = shown_value(budget.per_agent_limit);
        rows.push(if zh {
            format!(
                "会话 token 预算: 总计 {total} | 上限 {limit} | 完整性 {completeness} | 已触发停止 {stopped} | 单 Agent 上限 {per_agent}"
            )
        } else {
            format!(
                "Session-wide token budget: total {total} | limit {limit} | completeness {completeness} | stop triggered {stopped} | per-agent limit {per_agent}"
            )
        });
        rows.push(format!(
            "{}: {}",
            tr("Per-agent stop triggered", "单 Agent 已触发停止"),
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
        rows.push(if zh {
            format!(
                "所选 Agent 保留的压缩记录: {} | 累计总数不可用",
                compactions.len()
            )
        } else {
            format!(
                "Compactions retained for selected agent: {} | lifetime total unavailable",
                compactions.len()
            )
        });
        for (index, fact) in compactions.iter().enumerate() {
            rows.push(format!(
                "{} #{} · {} {} · {} {}",
                tr("Compaction", "压缩"),
                index + 1,
                tr("turn", "轮次"),
                fact.turn_id,
                tr("item", "条目"),
                fact.item_id
            ));
            rows.push(format!(
                "  {} {} | {} {} | {} {}",
                tr("status", "状态"),
                compaction_status(fact),
                tr("started at", "开始于"),
                shown_value(fact.started_at_ms),
                tr("completed at", "完成于"),
                shown_value(fact.completed_at_ms),
            ));
            rows.push(format!(
                "  {}: {} {} | {} {} | {} {} | {} {} | {} {}",
                tr("usage", "用量"),
                tr("input", "输入"),
                shown_value(fact.input_tokens),
                tr("cached input", "缓存输入"),
                shown_value(fact.cached_input_tokens),
                tr("output", "输出"),
                shown_value(fact.output_tokens),
                tr("total", "总计"),
                shown_value(fact.total_tokens),
                tr("context window", "上下文窗口"),
                shown_value(fact.context_window),
            ));
        }
        if compactions.is_empty() {
            rows.push(tr("Compaction facts: unavailable", "压缩记录: 不可用").to_owned());
        }

        let inner_width = area.width.saturating_sub(2) as usize;
        let inner_height = area.height.saturating_sub(2) as usize;
        // 换行方式与 `wrap` 相同；缩进行是上一行的元数据，用次要样式。
        let theme = super::theme::current();
        let wrapped_rows: Vec<_> = rows
            .iter()
            .flat_map(|row| {
                if row.starts_with(' ') {
                    wrap_marked(row, inner_width, theme.secondary(), |_| Vec::new())
                } else {
                    wrap_marked(row, inner_width, Style::default(), |clean| {
                        label_mark(clean).into_iter().collect()
                    })
                }
            })
            .collect();
        let max_scroll = wrapped_rows.len().saturating_sub(inner_height);
        let scroll = self.scroll.min(max_scroll);
        let scroll = if self.from_end {
            max_scroll.saturating_sub(scroll)
        } else {
            scroll
        };
        frame.render_widget(
            Paragraph::new(wrapped_rows)
                .wrap(Wrap { trim: true })
                .scroll((scroll.min(u16::MAX as usize) as u16, 0))
                .block(
                    super::theme::focus_panel()
                        .borders(Borders::ALL)
                        .title(tr(" Context · Ctrl+G ", " 上下文 · Ctrl+G "))
                        .title_bottom(footer(
                            tr(
                                "Ctrl+G/Esc close | PgUp/PgDn scroll | Home/End jump",
                                "Ctrl+G/Esc 关闭 | PgUp/PgDn 滚动 | Home/End 跳转",
                            ),
                            area.width,
                        )),
                ),
            area,
        );
    }
}

fn compaction_status(fact: &CompactionFact) -> &'static str {
    match fact.status {
        crate::observation::CompactionFactStatus::Started => tr("Started", "已开始"),
        crate::observation::CompactionFactStatus::Completed => tr("Completed", "已完成"),
        crate::observation::CompactionFactStatus::Unknown => tr("Unknown", "未知"),
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

    #[test]
    fn chinese_context_panel_wraps_inside_its_borders() {
        use unicode_width::UnicodeWidthStr;

        let snapshot = CoreSnapshot {
            thread_id: Some("root-thread".into()),
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
        for width in [40u16, 72] {
            let mut terminal = Terminal::new(TestBackend::new(width, 30)).unwrap();
            lang::with_zh(|| {
                terminal
                    .draw(|frame| panel.draw(frame, frame.area(), &snapshot, None))
                    .unwrap();
            });
            let rows = crate::ui::activity::buffer_rows(terminal.backend().buffer());
            let screen = rows.join("\n");
            for text in ["上下文 · Ctrl+G", "Esc 关闭", "PgUp/PgDn 滚动"] {
                assert!(screen.contains(text), "{width}: {text}\n{screen}");
            }
            // 换行位置随宽度变化，去掉边框和空格后正文必须完整。
            let joined: String = rows[1..rows.len() - 1]
                .iter()
                .flat_map(|row| row.chars())
                .filter(|ch| *ch != '│' && *ch != ' ')
                .collect();
            for text in [
                "用量归属root·线程不可用·轮次不可用·代次不可用",
                "会话token预算:总计不可用|上限不可用",
                "压缩#1·轮次old-turn·条目old-compaction",
                "状态已完成|开始于不可用",
                "上下文窗口不可用",
            ] {
                assert!(joined.contains(text), "{width}: {text}\n{screen}");
            }
            for row in &rows[1..rows.len() - 1] {
                assert_eq!(row.width(), width as usize, "{row}");
                assert!(row.ends_with('│'), "{row}");
            }
            assert!(!screen.contains("unavailable"), "{screen}");
        }
    }
}
