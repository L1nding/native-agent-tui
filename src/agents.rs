use std::collections::{BTreeMap, BTreeSet, VecDeque};

use thiserror::Error;

use crate::gate::{ChildOutcome, GateEvent, WaitTarget};
use crate::state::UsageSummary;

const AGENT_LIMIT: usize = 64;
const ID_BYTES: usize = 1024;
pub const DEFAULT_MAX_NATIVE_CHILDREN: usize = 8;
pub const DEFAULT_MAX_NATIVE_DEPTH: usize = 2;
pub const DEFAULT_MAX_NATIVE_TURNS: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AgentLimits {
    pub max_children: usize,
    pub max_depth: usize,
    pub max_active_turns: usize,
}

impl Default for AgentLimits {
    fn default() -> Self {
        Self {
            max_children: DEFAULT_MAX_NATIVE_CHILDREN,
            max_depth: DEFAULT_MAX_NATIVE_DEPTH,
            max_active_turns: DEFAULT_MAX_NATIVE_TURNS,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentInfo {
    pub id: String,
    pub parent_id: String,
    pub path: Option<String>,
    pub nickname: Option<String>,
    pub role: Option<String>,
    pub model: Option<String>,
    pub confirmed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentSnapshot {
    pub info: AgentInfo,
    pub generation: u64,
    pub turn_id: Option<String>,
    pub outcome: Option<ChildOutcome>,
    pub awaiting_turn: bool,
}

#[derive(Debug, Error)]
pub enum AgentError {
    #[error("agent identity exceeds its size or count limit")]
    Limit,
    #[error("native child capacity reached ({0} direct children)")]
    Capacity(usize),
    #[error("native agent depth exceeds the configured limit ({0})")]
    DepthLimit(usize),
    #[error("native active turn capacity reached ({0} active turns)")]
    ActiveTurnLimit(usize),
    #[error("agent parent identity changed")]
    ParentChanged,
    #[error("unknown child target: {0}")]
    Unknown(String),
    #[error("ambiguous child target: {0}")]
    Ambiguous(String),
    #[error("target is not a direct child: {0}")]
    NotDirect(String),
}

#[derive(Debug)]
struct Agent {
    info: AgentInfo,
    generation: u64,
    turn_id: Option<String>,
    started_seq: u64,
    awaiting_after: Option<u64>,
    outcome: Option<ChildOutcome>,
    usage: UsageSummary,
    retired: VecDeque<String>,
}

#[derive(Debug, Default)]
pub struct AgentRegistry {
    agents: BTreeMap<String, Agent>,
    limits: AgentLimits,
}

impl AgentRegistry {
    pub fn with_limits(limits: AgentLimits) -> Self {
        Self {
            agents: BTreeMap::new(),
            limits: AgentLimits {
                max_children: limits.max_children.max(1),
                max_depth: limits.max_depth.max(1),
                max_active_turns: limits.max_active_turns.max(1),
            },
        }
    }

    pub fn limits(&self) -> AgentLimits {
        self.limits
    }

    pub fn register(&mut self, info: AgentInfo) -> Result<(), AgentError> {
        if [&info.id, &info.parent_id]
            .iter()
            .any(|id| id.is_empty() || id.len() > ID_BYTES)
            || [&info.path, &info.nickname, &info.role, &info.model]
                .iter()
                .any(|value| value.as_ref().is_some_and(|value| value.len() > ID_BYTES))
        {
            return Err(AgentError::Limit);
        }
        if let Some(agent) = self.agents.get(&info.id) {
            if agent.info.parent_id != info.parent_id && agent.info.confirmed {
                return Err(AgentError::ParentChanged);
            }
            if agent.info.parent_id != info.parent_id {
                let direct_children = self
                    .agents
                    .values()
                    .filter(|candidate| {
                        candidate.info.id != info.id && candidate.info.parent_id == info.parent_id
                    })
                    .count();
                if direct_children >= self.limits.max_children {
                    return Err(AgentError::Capacity(self.limits.max_children));
                }
                if self.depth_for_parent(&info.parent_id) > self.limits.max_depth {
                    return Err(AgentError::DepthLimit(self.limits.max_depth));
                }
            }
            let agent = self.agents.get_mut(&info.id).unwrap();
            if info.confirmed {
                agent.info.parent_id = info.parent_id;
            }
            agent.info.confirmed |= info.confirmed;
            agent.info.path = info.path.or(agent.info.path.take());
            agent.info.nickname = info.nickname.or(agent.info.nickname.take());
            agent.info.role = info.role.or(agent.info.role.take());
            agent.info.model = info.model.or(agent.info.model.take());
            return Ok(());
        }
        if self.agents.len() >= AGENT_LIMIT {
            return Err(AgentError::Limit);
        }
        let direct_children = self
            .agents
            .values()
            .filter(|agent| agent.info.parent_id == info.parent_id)
            .count();
        if direct_children >= self.limits.max_children {
            return Err(AgentError::Capacity(self.limits.max_children));
        }
        let depth = self.depth_for_parent(&info.parent_id);
        if depth > self.limits.max_depth {
            return Err(AgentError::DepthLimit(self.limits.max_depth));
        }
        self.agents.insert(
            info.id.clone(),
            Agent {
                info,
                generation: 0,
                turn_id: None,
                started_seq: 0,
                awaiting_after: None,
                outcome: None,
                usage: UsageSummary::default(),
                retired: VecDeque::new(),
            },
        );
        Ok(())
    }

    pub fn known(&self, id: &str) -> bool {
        self.agents.contains_key(id)
    }

    pub fn update_usage(&mut self, id: &str, usage: UsageSummary) -> bool {
        let Some(agent) = self.agents.get_mut(id) else {
            return false;
        };
        if usage.source != crate::state::FactSource::ServerConfirmed || !usage.has_value() {
            return false;
        }
        agent.usage = usage;
        true
    }

    pub fn confirmed_total_tokens(&self) -> u64 {
        self.agents
            .values()
            .filter(|agent| agent.usage.source == crate::state::FactSource::ServerConfirmed)
            .filter_map(|agent| agent.usage.total_tokens)
            .fold(0, u64::saturating_add)
    }
    pub fn confirmed(&self, id: &str) -> bool {
        self.agents
            .get(id)
            .is_some_and(|agent| agent.info.confirmed)
    }

    /// 调用 started 的序号用于区分“新轮已先到达”和“还在等新轮”。
    pub fn rearm(&mut self, id: &str, evidence_seq: u64) {
        if let Some(agent) = self.agents.get_mut(id) {
            if agent.started_seq <= evidence_seq {
                agent.awaiting_after = Some(evidence_seq);
            }
        }
    }

    pub fn cancel_rearm(&mut self, id: &str, evidence_seq: u64) {
        if let Some(agent) = self.agents.get_mut(id) {
            if agent.awaiting_after == Some(evidence_seq) {
                agent.awaiting_after = None;
            }
        }
    }

    pub fn started(
        &mut self,
        id: &str,
        turn: &str,
        seq: u64,
    ) -> Result<Option<GateEvent>, AgentError> {
        let agent = match self.agents.get(id) {
            Some(agent) => agent,
            None => return Ok(None),
        };
        if turn.is_empty()
            || turn.len() > ID_BYTES
            || agent.retired.iter().any(|old| old == turn)
            || agent.turn_id.as_deref() == Some(turn)
        {
            return Ok(None);
        }
        if agent.awaiting_after.is_some_and(|after| seq <= after) {
            return Ok(None);
        }
        let has_active_slot =
            agent.turn_id.is_some() && agent.outcome.is_none() && agent.awaiting_after.is_none();
        if !has_active_slot && self.active_turn_count() >= self.limits.max_active_turns {
            return Err(AgentError::ActiveTurnLimit(self.limits.max_active_turns));
        }
        let agent = self.agents.get_mut(id).expect("agent was checked above");
        if let Some(old) = agent.turn_id.replace(turn.into()) {
            agent.retired.push_back(old);
            if agent.retired.len() > 128 {
                agent.retired.pop_front();
            }
        }
        agent.generation += 1;
        agent.started_seq = seq;
        agent.awaiting_after = None;
        agent.outcome = None;
        Ok(Some(GateEvent {
            target: id.into(),
            generation: agent.generation,
            turn_id: turn.into(),
            outcome: None,
        }))
    }

    pub fn completed(&mut self, id: &str, turn: &str, outcome: ChildOutcome) -> Option<GateEvent> {
        let agent = self.agents.get_mut(id)?;
        if agent.turn_id.as_deref() != Some(turn) || agent.outcome.is_some() {
            return None;
        }
        agent.outcome = Some(outcome.clone());
        Some(GateEvent {
            target: id.into(),
            generation: agent.generation,
            turn_id: turn.into(),
            outcome: Some(outcome),
        })
    }

    pub fn active_turn(&self, id: &str, turn: &str) -> bool {
        self.agents.get(id).is_some_and(|agent| {
            agent.turn_id.as_deref() == Some(turn)
                && agent.outcome.is_none()
                && agent.awaiting_after.is_none()
        })
    }

    pub fn current_turn(&self, id: &str, turn: &str) -> bool {
        self.agents.get(id).is_some_and(|agent| {
            agent.turn_id.as_deref() == Some(turn) && agent.awaiting_after.is_none()
        })
    }

    pub fn capture(&self, parent: &str, targets: &[String]) -> Result<Vec<WaitTarget>, AgentError> {
        let mut ids = BTreeSet::new();
        if targets.is_empty() {
            ids.extend(
                self.agents
                    .values()
                    .filter(|agent| agent.info.parent_id == parent)
                    .map(|agent| agent.info.id.clone()),
            );
        } else {
            for target in targets {
                let agent = if let Some(agent) = self.agents.get(target) {
                    agent
                } else {
                    let mut matches = self.agents.values().filter(|agent| {
                        agent.info.path.as_ref() == Some(target)
                            || agent.info.nickname.as_ref() == Some(target)
                    });
                    let agent = matches
                        .next()
                        .ok_or_else(|| AgentError::Unknown(target.clone()))?;
                    if matches.next().is_some() {
                        return Err(AgentError::Ambiguous(target.clone()));
                    }
                    agent
                };
                if agent.info.parent_id != parent {
                    return Err(AgentError::NotDirect(target.clone()));
                }
                ids.insert(agent.info.id.clone());
            }
        }
        Ok(ids
            .into_iter()
            .map(|id| {
                let agent = &self.agents[&id];
                let awaiting = agent.turn_id.is_none() || agent.awaiting_after.is_some();
                WaitTarget {
                    id,
                    generation: agent.generation + u64::from(awaiting),
                    turn_id: if awaiting {
                        None
                    } else {
                        agent.turn_id.clone()
                    },
                    outcome: if awaiting {
                        None
                    } else {
                        agent.outcome.clone()
                    },
                }
            })
            .collect())
    }

    pub fn snapshots(&self) -> Vec<AgentSnapshot> {
        self.agents
            .values()
            .map(|agent| AgentSnapshot {
                info: agent.info.clone(),
                generation: agent.generation,
                turn_id: agent.turn_id.clone(),
                outcome: agent.outcome.clone(),
                awaiting_turn: agent.turn_id.is_none() || agent.awaiting_after.is_some(),
            })
            .collect()
    }

    fn depth_for_parent(&self, parent: &str) -> usize {
        let mut depth: usize = 1;
        let mut current = parent;
        let mut seen = BTreeSet::new();
        while current != "root" && seen.insert(current.to_owned()) {
            let Some(agent) = self.agents.get(current) else {
                break;
            };
            depth = depth.saturating_add(1);
            current = &agent.info.parent_id;
        }
        depth
    }

    fn active_turn_count(&self) -> usize {
        self.agents
            .values()
            .filter(|agent| {
                agent.turn_id.is_some() && agent.outcome.is_none() && agent.awaiting_after.is_none()
            })
            .count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn child(id: &str, parent: &str) -> AgentInfo {
        AgentInfo {
            id: id.into(),
            parent_id: parent.into(),
            path: Some(format!("/root/{id}")),
            nickname: Some("same".into()),
            role: None,
            model: None,
            confirmed: true,
        }
    }
    #[test]
    fn resolves_direct_identity_and_rejects_unknown_ambiguous_and_non_direct_targets() {
        let mut agents = AgentRegistry::default();
        agents.register(child("a", "root")).unwrap();
        agents.register(child("b", "root")).unwrap();
        agents.register(child("grandchild", "a")).unwrap();
        assert_eq!(agents.capture("root", &[]).unwrap().len(), 2);
        assert_eq!(
            agents.capture("root", &["/root/a".into()]).unwrap()[0].id,
            "a"
        );
        assert!(matches!(
            agents.capture("root", &["same".into()]),
            Err(AgentError::Ambiguous(_))
        ));
        assert!(matches!(
            agents.capture("root", &["missing".into()]),
            Err(AgentError::Unknown(_))
        ));
        assert!(matches!(
            agents.capture("root", &["grandchild".into()]),
            Err(AgentError::NotDirect(_))
        ));
    }
    #[test]
    fn followup_captures_the_new_generation_and_old_events_cannot_bind_it() {
        let mut agents = AgentRegistry::default();
        agents.register(child("a", "root")).unwrap();
        agents.started("a", "old", 1).unwrap();
        agents
            .completed("a", "old", ChildOutcome::Completed)
            .unwrap();
        agents.rearm("a", 2);
        let target = agents.capture("root", &[]).unwrap().remove(0);
        assert_eq!(target.generation, 2);
        assert_eq!(target.turn_id, None);
        assert!(agents.started("a", "old", 3).unwrap().is_none());
        let event = agents.started("a", "new", 4).unwrap().unwrap();
        assert_eq!(event.generation, 2);
        assert!(agents
            .completed("a", "old", ChildOutcome::Completed)
            .is_none());
        agents.rearm("a", 2); // completed collab item arrives after the new started event.
        assert_eq!(
            agents.capture("root", &[]).unwrap()[0].turn_id.as_deref(),
            Some("new")
        );
    }

    #[test]
    fn limits_direct_children_and_nested_depth_without_affecting_existing_agents() {
        let mut agents = AgentRegistry::with_limits(AgentLimits {
            max_children: 2,
            max_depth: 2,
            max_active_turns: 2,
        });
        agents.register(child("a", "root")).unwrap();
        agents.register(child("b", "root")).unwrap();
        assert!(matches!(
            agents.register(child("c", "root")),
            Err(AgentError::Capacity(2))
        ));
        agents.register(child("grandchild", "a")).unwrap();
        assert!(matches!(
            agents.register(child("great-grandchild", "grandchild")),
            Err(AgentError::DepthLimit(2))
        ));
        agents
            .register(AgentInfo {
                id: "a".into(),
                parent_id: "root".into(),
                path: Some("/root/a-renamed".into()),
                nickname: None,
                role: None,
                model: None,
                confirmed: true,
            })
            .unwrap();
        assert!(agents.known("a"));
    }

    #[test]
    fn active_turn_limit_releases_only_after_a_confirmed_terminal_event() {
        let mut agents = AgentRegistry::with_limits(AgentLimits {
            max_children: 2,
            max_depth: 1,
            max_active_turns: 1,
        });
        agents.register(child("a", "root")).unwrap();
        agents.register(child("b", "root")).unwrap();
        agents.started("a", "a-1", 1).unwrap().unwrap();
        assert!(matches!(
            agents.started("b", "b-1", 2),
            Err(AgentError::ActiveTurnLimit(1))
        ));
        agents
            .completed("a", "a-1", ChildOutcome::Completed)
            .unwrap();
        agents.started("b", "b-1", 3).unwrap().unwrap();
    }

    #[test]
    fn confirmed_usage_keeps_latest_known_children_and_ignores_missing_values() {
        let mut agents = AgentRegistry::default();
        agents.register(child("a", "root")).unwrap();
        agents.register(child("nested", "a")).unwrap();
        agents.register(child("unknown", "root")).unwrap();
        let usage = |total_tokens| UsageSummary {
            total_tokens: Some(total_tokens),
            source: crate::state::FactSource::ServerConfirmed,
            ..UsageSummary::default()
        };
        assert!(agents.update_usage("a", usage(12)));
        assert!(agents.update_usage("nested", usage(7)));
        assert!(!agents.update_usage("missing", usage(100)));
        assert!(!agents.update_usage("unknown", UsageSummary::default()));
        assert_eq!(agents.confirmed_total_tokens(), 19);
        assert!(agents.update_usage("a", usage(20)));
        assert_eq!(agents.confirmed_total_tokens(), 27);
    }
}
