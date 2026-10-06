use std::collections::{BTreeMap, HashMap, HashSet};

use ratatui::text::Line;
use ratatui::widgets::{Block, Borders, Paragraph};
use unicode_width::UnicodeWidthStr;

use super::activity::{
    age, agent_usage_brief, focus_activity, truncate_display_label, usage_fact_for,
};
use super::layout::wrap;
use super::workflow::{
    self, project_workflow, selected_task, task_reference, workflow_conversation,
    ConversationTarget,
};
use crate::agents::AgentSnapshot;
use crate::scheduler::TaskSnapshot;
use crate::state::{display_text, CoreSnapshot};

pub(super) fn draw_agents(
    frame: &mut ratatui::Frame<'_>,
    area: ratatui::layout::Rect,
    snapshot: &CoreSnapshot,
    selected_id: Option<&str>,
    compact: bool,
) {
    if compact {
        let (name, status, generation, usage) = snapshot
            .agents
            .iter()
            .find(|agent| Some(agent.info.id.as_str()) == selected_id)
            .map_or_else(
                || {
                    let usage = usage_fact_for(snapshot, None)
                        .and_then(|fact| fact.summary.total_tokens)
                        .map_or_else(
                            || "tokens: unavailable".into(),
                            |tokens| format!("tokens:{tokens}"),
                        );
                    (
                        "root",
                        format!("{:?}", snapshot.phase),
                        "unavailable".to_owned(),
                        usage,
                    )
                },
                |agent| {
                    let status = if agent.awaiting_turn {
                        "starting".to_owned()
                    } else {
                        agent
                            .outcome
                            .as_ref()
                            .map_or_else(|| "running".to_owned(), |outcome| format!("{outcome:?}"))
                    };
                    let name = agent
                        .info
                        .path
                        .as_deref()
                        .or(agent.info.nickname.as_deref())
                        .unwrap_or(&agent.info.id);
                    (
                        name,
                        status,
                        agent.generation.to_string(),
                        agent_usage_brief(snapshot, agent),
                    )
                },
            );
        frame.render_widget(
            Paragraph::new(vec![
                Line::from(format!("Agent: {}", truncate_display_label(name, 25))),
                Line::from(format!("State: {status} / gen {generation}")),
                Line::from(usage),
            ]),
            area,
        );
        return;
    }
    let mut agents = vec![Line::from(if selected_id.is_none() {
        "> root"
    } else {
        "  root"
    })];
    let mut selected_line = None;
    for row in project_agent_tree(&snapshot.agents, snapshot.thread_id.as_deref()) {
        let agent = &snapshot.agents[row.index];
        let status = if agent.awaiting_turn {
            "starting".into()
        } else {
            agent
                .outcome
                .as_ref()
                .map_or_else(|| "running".into(), |outcome| format!("{outcome:?}"))
        };
        let name = agent
            .info
            .path
            .as_deref()
            .or(agent.info.nickname.as_deref())
            .unwrap_or(&agent.info.id);
        let tree_prefix = if row.depth == 1 {
            String::new()
        } else {
            format!("{}+- ", "  ".repeat(row.depth - 2))
        };
        let parent_label = if row.parent_missing {
            "[parent unavailable] "
        } else if row.relationship_unknown {
            "[parent unresolved] "
        } else {
            ""
        };
        let selected = Some(agent.info.id.as_str()) == selected_id;
        if selected {
            selected_line = Some(agents.len());
        }
        agents.push(Line::from(display_text(&format!(
            "{} {}{}{}",
            if selected { ">" } else { " " },
            tree_prefix,
            parent_label,
            name
        ))));
        if row.depth_truncated {
            agents.push(Line::from("[depth truncated]"));
        }
        agents.push(Line::from(format!(
            "    {}{status} / gen {}",
            "  ".repeat(row.depth.saturating_sub(1)),
            agent.generation
        )));
        agents.push(Line::from(format!(
            "    {}{}",
            "  ".repeat(row.depth.saturating_sub(1)),
            agent_usage_brief(snapshot, agent)
        )));
        if let Some(activity) = focus_activity(snapshot, &agent.info.id) {
            agents.push(Line::from(format!(
                "    {}{:?} / quiet {}",
                "  ".repeat(row.depth.saturating_sub(1)),
                activity.attention.level,
                age(activity.silence_ms)
            )));
        }
    }
    let visible_height = area.height.saturating_sub(2) as usize;
    let scroll = selected_line
        .unwrap_or(0)
        .saturating_sub(visible_height / 3)
        .min(agents.len().saturating_sub(visible_height))
        .min(u16::MAX as usize) as u16;
    frame.render_widget(
        Paragraph::new(agents)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(" Agents · F3 "),
            )
            .scroll((scroll, 0)),
        area,
    );
}

pub(super) fn next_agent_id(snapshot: &CoreSnapshot, current: Option<&str>) -> Option<String> {
    let rows = project_agent_tree(&snapshot.agents, snapshot.thread_id.as_deref());
    let next = current
        .and_then(|id| {
            rows.iter()
                .position(|row| snapshot.agents[row.index].info.id == id)
        })
        .map_or(0, |index| index + 1);
    rows.get(next)
        .map(|row| snapshot.agents[row.index].info.id.clone())
}

pub(super) fn draw_workflow(
    frame: &mut ratatui::Frame<'_>,
    area: ratatui::layout::Rect,
    snapshot: &CoreSnapshot,
    selected_id: Option<crate::scheduler::TaskId>,
    link_cursor: Option<workflow::WorkflowLinkCursor>,
    scroll: usize,
    manual_scroll: bool,
) {
    let scheduler = &snapshot.scheduler;
    let selected = selected_task(snapshot, selected_id);
    let workflow_order = project_workflow(snapshot);
    let agent_rows = project_agent_tree(&snapshot.agents, snapshot.thread_id.as_deref())
        .into_iter()
        .filter_map(|row| {
            snapshot
                .agents
                .get(row.index)
                .map(|agent| (agent.info.id.clone(), row))
        })
        .collect();
    let width = area.width.saturating_sub(2).max(1) as usize;
    let viewport = area.height.saturating_sub(2) as usize;
    let by_id = scheduler
        .tasks
        .iter()
        .map(|task| (task.id, task))
        .collect::<BTreeMap<_, _>>();
    let ready = if scheduler.ready_roots.is_empty() {
        "none".into()
    } else {
        scheduler
            .ready_roots
            .iter()
            .map(|id| format!("#{}", id.0))
            .collect::<Vec<_>>()
            .join(",")
    };
    let mut raw = vec![format!(
        "Workflow: {}{}{} | ready roots: {ready}",
        if scheduler.paused {
            "paused"
        } else {
            "dispatch active"
        },
        if scheduler.stopping {
            " · stopping"
        } else {
            ""
        },
        if scheduler.disconnected {
            " · disconnected"
        } else {
            ""
        },
    )];
    let native_reserved = scheduler
        .native_slot_capacity
        .map(|capacity| format!("{}/{}", scheduler.native_slots_reserved, capacity))
        .unwrap_or_else(|| format!("{}/unavailable", scheduler.native_slots_reserved));
    let summaries = [
        vec![format!(
            "Root slots: {} | native reserved: {} | native turns observed: {}",
            scheduler.root_slots_reserved, native_reserved, scheduler.native_turns_observed
        )],
        vec![
            format!(
                "Root slots: {} | native reserved: {}",
                scheduler.root_slots_reserved, native_reserved
            ),
            format!("Native turns observed: {}", scheduler.native_turns_observed),
        ],
        vec![
            format!("native reserved: {native_reserved}"),
            format!("native observed: {}", scheduler.native_turns_observed),
        ],
        vec![
            format!("N reserved: {native_reserved}"),
            format!("N observed: {}", scheduler.native_turns_observed),
        ],
    ];
    raw.extend(
        summaries
            .into_iter()
            .find(|candidate| candidate.iter().all(|line| line.width() <= width))
            .unwrap_or_else(|| {
                vec![
                    format!("N:{native_reserved}"),
                    format!("O:{}", scheduler.native_turns_observed),
                ]
            }),
    );
    let mut selected_visual = None;
    for id in &workflow_order {
        let Some(task) = by_id.get(id).copied() else {
            continue;
        };
        let selected_row = selected.is_some_and(|selected| selected.id == task.id);
        let marker = if selected_row { ">" } else { " " };
        let tree = if task.parent.is_some() {
            "  └─ "
        } else {
            ""
        };
        let slot = match task.kind {
            crate::scheduler::TaskKind::NativeChild if task.native_slot_reserved => {
                " · slot reserved"
            }
            crate::scheduler::TaskKind::NativeChild => " · slot free",
            crate::scheduler::TaskKind::RootTurn => "",
        };
        let flags = format!(
            "{}{}",
            if task.pause_requested {
                " · pause requested"
            } else {
                ""
            },
            if task.cancel_requested {
                " · cancel requested"
            } else {
                ""
            }
        );
        let text = format!(
            "{marker}{tree}#{} {:?} · attempt {} · priority {}{slot}{flags} · {}",
            task.id.0, task.state, task.attempt, task.priority, task.title
        );
        if selected_row {
            selected_visual = Some(
                raw.iter()
                    .map(|line| wrap(line, width).len())
                    .sum::<usize>(),
            );
        }
        raw.push(text);
    }
    if let Some(task) = selected {
        raw.push(format!(
            "Task #{}: {} · {:?} · attempt {} · blocked: {:?} · requests: {}",
            task.id.0,
            task.title,
            task.state,
            task.attempt,
            task.blocked_reason,
            task.pending_requests
        ));
        let parent = task.parent.map_or_else(
            || match task.kind {
                crate::scheduler::TaskKind::RootTurn => "none".into(),
                crate::scheduler::TaskKind::NativeChild => "unconfirmed / unavailable".into(),
            },
            |id| task_reference(id, &by_id),
        );
        raw.push(format!(
            "Parent: {parent} | dependency policy: {:?} | failure policy: {:?}",
            task.policy, task.failure
        ));
        if task.dependencies.is_empty() {
            raw.push("Dependencies: none".into());
        } else {
            raw.push(format!("Dependencies ({:?}):", task.policy));
            for id in &task.dependencies {
                raw.push(format!("  {}", task_reference(*id, &by_id)));
            }
        }
        if task.wait_targets.is_empty() {
            raw.push("Gate wait targets: none".into());
        } else {
            raw.push("Gate wait targets (captured attempts):".into());
            for target in &task.wait_targets {
                let current = by_id.get(&target.task).map_or_else(
                    || format!("#{} unavailable", target.task.0),
                    |linked| {
                        format!(
                            "{} · current attempt {} · {:?}",
                            task_reference(target.task, &by_id),
                            linked.attempt,
                            linked.state
                        )
                    },
                );
                raw.push(format!(
                    "  captured #{} attempt {} → {current}",
                    target.task.0, target.attempt
                ));
            }
        }
        raw.push(format!(
            "Conversation: {}",
            workflow_agent_label(snapshot, task, &agent_rows)
        ));
        if let Some(cursor) = link_cursor.filter(|cursor| cursor.target == task.id) {
            if let Some(captured) = cursor.captured_attempt {
                raw.push(format!(
                    "Selected Gate link captured attempt {captured}; current task attempt is {}.",
                    task.attempt
                ));
            }
        }
    } else {
        raw.push("No workflow tasks.".into());
    }
    let lines = raw
        .iter()
        .flat_map(|text| wrap(text, width).into_iter().map(Line::from))
        .collect::<Vec<_>>();
    let max_scroll = lines.len().saturating_sub(viewport);
    let mut scroll = scroll.min(max_scroll);
    if !manual_scroll {
        if let Some(selected_row) = selected_visual {
            if selected_row < scroll {
                scroll = selected_row;
            } else if selected_row >= scroll.saturating_add(viewport) && viewport > 0 {
                scroll = selected_row + 1 - viewport;
            }
        }
    }
    let title =
        " Workflow · ↑/↓ select · d dependencies · g Gate · Enter conversation · PgUp/PgDn scroll ";
    frame.render_widget(
        Paragraph::new(lines)
            .scroll((scroll.min(u16::MAX as usize) as u16, 0))
            .block(Block::default().borders(Borders::ALL).title(title)),
        area,
    );
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct AgentTreeRow {
    pub(super) index: usize,
    pub(super) depth: usize,
    pub(super) parent_missing: bool,
    pub(super) relationship_unknown: bool,
    pub(super) depth_truncated: bool,
}

/// 把快照投影为稳定的父子 DFS 顺序；渲染层不重新推断父链。
pub(super) fn project_agent_tree(
    agents: &[AgentSnapshot],
    root_id: Option<&str>,
) -> Vec<AgentTreeRow> {
    let root_id = root_id.unwrap_or("root");
    let known_ids = agents
        .iter()
        .map(|agent| agent.info.id.as_str())
        .collect::<HashSet<_>>();
    let mut children = BTreeMap::<&str, Vec<usize>>::new();
    for (index, agent) in agents.iter().enumerate() {
        children
            .entry(agent.info.parent_id.as_str())
            .or_default()
            .push(index);
    }
    let mut seeds = agents
        .iter()
        .enumerate()
        .filter(|(_, agent)| {
            agent.info.parent_id == root_id || !known_ids.contains(agent.info.parent_id.as_str())
        })
        .map(|(index, agent)| {
            let parent_missing = agent.info.parent_id != root_id;
            (index, parent_missing, false)
        })
        .collect::<Vec<_>>();
    // 非 root 连通分量可能是循环；按快照顺序作兜底根，并标记关系未确认。
    let mut is_seed = vec![false; agents.len()];
    for (index, _, _) in &seeds {
        is_seed[*index] = true;
    }
    for (index, _) in agents.iter().enumerate() {
        if !is_seed[index] {
            seeds.push((index, false, true));
        }
    }
    let mut stack = seeds
        .into_iter()
        .rev()
        .map(|(index, parent_missing, relationship_unknown)| {
            (index, 1, parent_missing, relationship_unknown, false)
        })
        .collect::<Vec<_>>();
    let mut rows = Vec::with_capacity(agents.len());
    let mut visited = vec![false; agents.len()];
    while let Some((index, actual_depth, parent_missing, relationship_unknown, depth_truncated)) =
        stack.pop()
    {
        if visited[index] {
            continue;
        }
        visited[index] = true;
        rows.push(AgentTreeRow {
            index,
            depth: actual_depth.min(8),
            parent_missing,
            relationship_unknown: relationship_unknown || !agents[index].info.confirmed,
            depth_truncated,
        });
        if let Some(siblings) = children.get(agents[index].info.id.as_str()) {
            for child in siblings.iter().rev() {
                let child_depth = actual_depth.saturating_add(1);
                stack.push((
                    *child,
                    child_depth,
                    false,
                    relationship_unknown,
                    depth_truncated || child_depth > 8,
                ));
            }
        }
    }
    rows
}

fn workflow_agent_label(
    snapshot: &CoreSnapshot,
    task: &TaskSnapshot,
    agent_rows: &HashMap<String, AgentTreeRow>,
) -> String {
    match workflow_conversation(snapshot, task) {
        ConversationTarget::Root => format!(
            "root thread {} · current task attempt {}",
            snapshot.thread_id.as_deref().unwrap_or("unknown"),
            task.attempt
        ),
        ConversationTarget::Child(agent) => {
            let depth = agent_rows
                .get(&agent.info.id)
                .map_or("unknown".to_string(), |row| row.depth.to_string());
            format!(
                "agent {} · Agent tree depth {} · generation {} · {:?}",
                agent.info.id, depth, agent.generation, agent.outcome
            )
        }
        ConversationTarget::Unavailable(message) => message.into(),
    }
}

pub(super) fn gate_status(snapshot: &CoreSnapshot, gate: &crate::state::GateSnapshot) -> String {
    let done = gate
        .targets
        .iter()
        .filter(|target| target.outcome.is_some())
        .count();
    let mut lines = vec![format!(
        "Waiting children: {done}/{} | queued: {} | root starts during wait: {}",
        gate.targets.len(),
        snapshot.queued_inputs,
        snapshot
            .root_start_requests
            .saturating_sub(gate.root_starts_at_enter)
    )];
    for target in gate.targets.iter().take(3) {
        let agent = snapshot
            .agents
            .iter()
            .find(|agent| agent.info.id == target.id);
        let name = agent
            .and_then(|agent| {
                agent
                    .info
                    .path
                    .as_deref()
                    .or(agent.info.nickname.as_deref())
            })
            .unwrap_or(&target.id);
        let status = target.outcome.as_ref().map_or_else(
            || {
                if target.turn_id.is_some() {
                    "running".into()
                } else {
                    "awaiting turn".into()
                }
            },
            |outcome| format!("{outcome:?}"),
        );
        lines.push(format!("{name}: {status} (gen {})", target.generation));
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::AgentInfo;
    use crate::gate::WaitTarget;
    use crate::state::{FactSource, GateSnapshot};
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    fn agent(id: &str, parent_id: &str) -> AgentSnapshot {
        AgentSnapshot {
            info: AgentInfo {
                id: id.into(),
                parent_id: parent_id.into(),
                path: Some(format!("/root/{id}")),
                nickname: None,
                role: None,
                model: None,
                confirmed: true,
            },
            generation: 1,
            turn_id: Some(format!("{id}-turn")),
            outcome: None,
            awaiting_turn: false,
            usage: Default::default(),
        }
    }

    fn rendered(terminal: &Terminal<TestBackend>) -> String {
        terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect()
    }

    #[test]
    fn agent_tree_keeps_snapshot_order_and_exposes_unknown_relationships_and_depth() {
        let agents = vec![
            agent("leaf", "branch"),
            agent("orphan", "missing"),
            agent("branch", "root"),
            agent("sibling", "root"),
        ];
        let rows = project_agent_tree(&agents, Some("root"));
        assert_eq!(
            rows.iter()
                .map(|row| agents[row.index].info.id.as_str())
                .collect::<Vec<_>>(),
            ["orphan", "branch", "leaf", "sibling"]
        );
        assert!(rows[0].parent_missing);
        assert!(!rows[1].parent_missing);

        let cycle = vec![agent("a", "b"), agent("b", "a")];
        let rows = project_agent_tree(&cycle, Some("root"));
        assert_eq!(
            rows.iter()
                .map(|row| cycle[row.index].info.id.as_str())
                .collect::<Vec<_>>(),
            ["a", "b"]
        );
        assert!(rows.iter().all(|row| row.relationship_unknown));

        let mut deep = Vec::new();
        for index in 0..10 {
            deep.push(agent(
                &format!("deep-{index}"),
                if index == 0 { "root" } else { "deep-x" },
            ));
            if index > 0 {
                deep[index].info.parent_id = format!("deep-{}", index - 1);
            }
        }
        let rows = project_agent_tree(&deep, Some("root"));
        assert_eq!(
            rows.iter().map(|row| row.depth).collect::<Vec<_>>(),
            [1, 2, 3, 4, 5, 6, 7, 8, 8, 8]
        );
        assert!(rows[..8].iter().all(|row| !row.depth_truncated));
        assert!(rows[8..].iter().all(|row| row.depth_truncated));
    }

    #[test]
    fn agent_tree_and_workflow_views_render_at_wide_and_narrow_sizes() {
        let snapshot = CoreSnapshot {
            thread_id: Some("root".into()),
            agents: vec![agent("child", "root")],
            usage: crate::state::UsageSummary {
                source: FactSource::ServerConfirmed,
                ..Default::default()
            },
            ..Default::default()
        };
        for (width, height) in [(120, 30), (100, 12), (60, 20), (30, 10)] {
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            terminal
                .draw(|frame| {
                    draw_agents(frame, frame.area(), &snapshot, Some("child"), height <= 16)
                })
                .unwrap();
            if (width, height) == (120, 30) {
                terminal
                    .draw(|frame| {
                        draw_agents(
                            frame,
                            ratatui::layout::Rect::new(0, 0, 32, 12),
                            &snapshot,
                            Some("child"),
                            false,
                        )
                    })
                    .unwrap();
                let panel = rendered(&terminal);
                assert!(panel.contains("Agents · F3"), "{panel}");
                assert!(panel.contains("/root/child"), "{panel}");
                assert!(!panel.contains("Agent: child"), "{panel}");
            }
            let text = rendered(&terminal);
            assert!(text.contains("child"), "{width}x{height}: {text}");
            assert!(text.contains("running"), "{width}x{height}: {text}");

            terminal
                .draw(|frame| draw_workflow(frame, frame.area(), &snapshot, None, None, 0, false))
                .unwrap();
            let text = rendered(&terminal);
            assert!(text.contains("Workflow"), "{width}x{height}: {text}");
            assert!(
                text.contains("No workflow tasks"),
                "{width}x{height}: {text}"
            );
        }
    }

    #[test]
    fn gate_status_reports_confirmed_outcomes_and_captured_generations() {
        let mut snapshot = CoreSnapshot {
            queued_inputs: 3,
            root_start_requests: 5,
            agents: vec![agent("child", "root")],
            ..Default::default()
        };
        let gate = GateSnapshot {
            targets: vec![
                WaitTarget {
                    id: "child".into(),
                    generation: 4,
                    turn_id: Some("turn-4".into()),
                    outcome: None,
                },
                WaitTarget {
                    id: "gone".into(),
                    generation: 2,
                    turn_id: None,
                    outcome: Some(crate::gate::ChildOutcome::Completed),
                },
            ],
            pending: true,
            root_starts_at_enter: 4,
            root_starts_at_release: None,
        };
        let text = gate_status(&snapshot, &gate);
        assert!(text.contains("Waiting children: 1/2"));
        assert!(text.contains("queued: 3"));
        assert!(text.contains("root starts during wait: 1"));
        assert!(text.contains("/root/child: running (gen 4)"));
        assert!(text.contains("gone: Completed (gen 2)"));
        snapshot.root_start_requests = 4;
        assert!(gate_status(&snapshot, &gate).contains("root starts during wait: 0"));
    }
}
