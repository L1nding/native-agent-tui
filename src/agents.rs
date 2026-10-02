use std::collections::{BTreeMap, BTreeSet, VecDeque};

use thiserror::Error;

use crate::gate::{ChildOutcome, GateEvent, WaitTarget};

const AGENT_LIMIT: usize = 64;
const ID_BYTES: usize = 1024;

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
    retired: VecDeque<String>,
}

#[derive(Debug, Default)]
pub struct AgentRegistry {
    agents: BTreeMap<String, Agent>,
}

impl AgentRegistry {
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
        if let Some(agent) = self.agents.get_mut(&info.id) {
            if agent.info.parent_id != info.parent_id && agent.info.confirmed {
                return Err(AgentError::ParentChanged);
            }
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
        self.agents.insert(
            info.id.clone(),
            Agent {
                info,
                generation: 0,
                turn_id: None,
                started_seq: 0,
                awaiting_after: None,
                outcome: None,
                retired: VecDeque::new(),
            },
        );
        Ok(())
    }

    pub fn known(&self, id: &str) -> bool {
        self.agents.contains_key(id)
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

    pub fn started(&mut self, id: &str, turn: &str, seq: u64) -> Option<GateEvent> {
        let agent = self.agents.get_mut(id)?;
        if turn.is_empty()
            || turn.len() > ID_BYTES
            || agent.retired.iter().any(|old| old == turn)
            || agent.turn_id.as_deref() == Some(turn)
        {
            return None;
        }
        if agent.awaiting_after.is_some_and(|after| seq <= after) {
            return None;
        }
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
        Some(GateEvent {
            target: id.into(),
            generation: agent.generation,
            turn_id: turn.into(),
            outcome: None,
        })
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
        assert!(agents.started("a", "old", 3).is_none());
        let event = agents.started("a", "new", 4).unwrap();
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
}
