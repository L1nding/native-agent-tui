use std::collections::BTreeMap;
use std::sync::Arc;

use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionPhase {
    Created,
    Launching,
    Initializing,
    Ready,
    Running,
    GatePending,
    Completed,
    Stopping,
    ClosingTransport,
    Stopped,
    Disconnected,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentSnapshot {
    pub id: String,
    pub generation: u64,
    pub completed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoreSnapshot {
    pub version: u64,
    pub phase: SessionPhase,
    pub root_turn_count: u64,
    pub gate_pending: bool,
    pub agents: Vec<AgentSnapshot>,
}

impl Default for CoreSnapshot {
    fn default() -> Self {
        Self {
            version: 0,
            phase: SessionPhase::Created,
            root_turn_count: 0,
            gate_pending: false,
            agents: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CoreEvent {
    LaunchRequested,
    Initialized,
    RootTurnStarted,
    RootTurnCompleted,
    GateEntered,
    GateReleased,
    StopRequested,
    TransportClosed,
    Failed,
    AgentTurnStarted { id: String, generation: u64 },
    AgentTurnCompleted { id: String, generation: u64 },
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum StateError {
    #[error("invalid transition from {from:?} for event {event:?}")]
    InvalidTransition {
        from: SessionPhase,
        event: CoreEvent,
    },
    #[error("stale generation {generation} for agent {id}; current generation is {current}")]
    StaleGeneration {
        id: String,
        generation: u64,
        current: u64,
    },
}

#[derive(Debug, Default)]
pub struct SessionState {
    snapshot: CoreSnapshot,
    agents: BTreeMap<String, AgentSnapshot>,
}

impl SessionState {
    pub fn snapshot(&self) -> Arc<CoreSnapshot> {
        Arc::new(self.snapshot.clone())
    }

    pub fn apply(&mut self, event: CoreEvent) -> Result<Arc<CoreSnapshot>, StateError> {
        let current = self.snapshot.phase;
        match event.clone() {
            CoreEvent::LaunchRequested if current == SessionPhase::Created => {
                self.snapshot.phase = SessionPhase::Launching;
            }
            CoreEvent::Initialized
                if matches!(
                    current,
                    SessionPhase::Launching | SessionPhase::Initializing
                ) =>
            {
                self.snapshot.phase = SessionPhase::Ready;
            }
            CoreEvent::RootTurnStarted
                if matches!(current, SessionPhase::Ready | SessionPhase::Completed) =>
            {
                self.snapshot.phase = SessionPhase::Running;
                self.snapshot.root_turn_count += 1;
            }
            CoreEvent::RootTurnCompleted if current == SessionPhase::Running => {
                self.snapshot.phase = SessionPhase::Completed;
            }
            CoreEvent::GateEntered if current == SessionPhase::Running => {
                self.snapshot.phase = SessionPhase::GatePending;
                self.snapshot.gate_pending = true;
            }
            CoreEvent::GateReleased if current == SessionPhase::GatePending => {
                self.snapshot.phase = SessionPhase::Running;
                self.snapshot.gate_pending = false;
            }
            CoreEvent::StopRequested
                if !matches!(
                    current,
                    SessionPhase::Stopped | SessionPhase::ClosingTransport
                ) =>
            {
                self.snapshot.phase = SessionPhase::Stopping;
            }
            CoreEvent::TransportClosed
                if matches!(
                    current,
                    SessionPhase::Stopping | SessionPhase::ClosingTransport
                ) =>
            {
                self.snapshot.phase = SessionPhase::Stopped;
                self.snapshot.gate_pending = false;
            }
            CoreEvent::Failed if current != SessionPhase::Stopped => {
                self.snapshot.phase = SessionPhase::Failed;
                self.snapshot.gate_pending = false;
            }
            CoreEvent::AgentTurnStarted { id, generation } => {
                let agent = self
                    .agents
                    .entry(id.clone())
                    .or_insert_with(|| AgentSnapshot {
                        id,
                        generation,
                        completed: false,
                    });
                if generation < agent.generation {
                    return Err(StateError::StaleGeneration {
                        id: agent.id.clone(),
                        generation,
                        current: agent.generation,
                    });
                }
                agent.generation = generation;
                agent.completed = false;
            }
            CoreEvent::AgentTurnCompleted { id, generation } => {
                let agent = self
                    .agents
                    .entry(id.clone())
                    .or_insert_with(|| AgentSnapshot {
                        id,
                        generation,
                        completed: false,
                    });
                if generation != agent.generation {
                    return Err(StateError::StaleGeneration {
                        id: agent.id.clone(),
                        generation,
                        current: agent.generation,
                    });
                }
                agent.completed = true;
            }
            _ => {
                return Err(StateError::InvalidTransition {
                    from: current,
                    event,
                })
            }
        }

        self.snapshot.version += 1;
        self.snapshot.agents = self.agents.values().cloned().collect();
        Ok(self.snapshot())
    }
}

#[cfg(test)]
mod tests {
    use super::{CoreEvent, SessionPhase, SessionState, StateError};

    #[test]
    fn publishes_monotonic_snapshots() {
        let mut state = SessionState::default();
        let first = state.apply(CoreEvent::LaunchRequested).unwrap();
        let second = state.apply(CoreEvent::Initialized).unwrap();
        assert_eq!(first.version, 1);
        assert_eq!(second.version, 2);
        assert_eq!(second.phase, SessionPhase::Ready);
    }

    #[test]
    fn rejects_stale_agent_generation() {
        let mut state = SessionState::default();
        state
            .apply(CoreEvent::AgentTurnStarted {
                id: "child".into(),
                generation: 2,
            })
            .unwrap();
        assert!(matches!(
            state.apply(CoreEvent::AgentTurnCompleted {
                id: "child".into(),
                generation: 1,
            }),
            Err(StateError::StaleGeneration { .. })
        ));
    }
}
