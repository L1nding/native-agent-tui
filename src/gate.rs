use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WaitTarget {
    pub id: String,
    pub generation: u64,
    pub turn_id: Option<String>,
    pub outcome: Option<ChildOutcome>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WaitRequest {
    pub targets: Vec<WaitTarget>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct WaitToken(pub u64);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ChildOutcome {
    Completed,
    Failed,
    Interrupted,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GateChange {
    Pending,
    Released,
    Cancelled,
    Disconnected,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GateError {
    AlreadyPending,
    InvalidTargets,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GateEvent {
    pub target: String,
    pub generation: u64,
    pub turn_id: String,
    pub outcome: Option<ChildOutcome>,
}

pub trait CompletionGate {
    fn accept_wait(&mut self, request: WaitRequest) -> Result<WaitToken, GateError>;
    fn apply(&mut self, event: &GateEvent) -> GateChange;
    fn cancel(&mut self) -> GateChange;
    fn disconnect(&mut self) -> GateChange;
}

#[derive(Debug, Default)]
pub struct PendingGate {
    next_token: u64,
    pending: Option<PendingWait>,
}

#[derive(Debug)]
struct PendingWait {
    token: WaitToken,
    targets: BTreeMap<String, WaitTarget>,
}

impl PendingWait {
    fn released(&self) -> bool {
        self.targets
            .values()
            .all(|target| target.outcome == Some(ChildOutcome::Completed))
            || self.targets.values().any(|target| {
                matches!(
                    target.outcome,
                    Some(ChildOutcome::Failed | ChildOutcome::Interrupted)
                )
            })
    }
}

impl PendingGate {
    pub fn targets(&self) -> Vec<WaitTarget> {
        self.pending
            .as_ref()
            .map(|pending| pending.targets.values().cloned().collect())
            .unwrap_or_default()
    }
    pub fn result(&self) -> Option<(WaitToken, Vec<WaitTarget>)> {
        let pending = self.pending.as_ref()?;
        pending
            .released()
            .then(|| (pending.token, pending.targets.values().cloned().collect()))
    }

    pub fn take_result(&mut self) -> Option<(WaitToken, Vec<WaitTarget>)> {
        let result = self.result()?;
        self.pending.take();
        Some(result)
    }
}

impl CompletionGate for PendingGate {
    fn accept_wait(&mut self, request: WaitRequest) -> Result<WaitToken, GateError> {
        if self.pending.is_some() {
            return Err(GateError::AlreadyPending);
        }
        let count = request.targets.len();
        if request.targets.iter().any(|target| {
            target.id.is_empty()
                || target.generation == 0
                || target.turn_id.as_deref() == Some("")
                || target.outcome.is_some() && target.turn_id.is_none()
        }) {
            return Err(GateError::InvalidTargets);
        }
        let targets: BTreeMap<_, _> = request
            .targets
            .into_iter()
            .map(|target| (target.id.clone(), target))
            .collect();
        if targets.len() != count {
            return Err(GateError::InvalidTargets);
        }
        self.next_token += 1;
        let token = WaitToken(self.next_token);
        self.pending = Some(PendingWait { token, targets });
        Ok(token)
    }

    fn apply(&mut self, event: &GateEvent) -> GateChange {
        let Some(pending) = self.pending.as_mut() else {
            return GateChange::Pending;
        };
        let Some(target) = pending.targets.get_mut(&event.target) else {
            return GateChange::Pending;
        };
        if target.generation != event.generation
            || target.outcome.is_some()
            || target
                .turn_id
                .as_ref()
                .is_some_and(|id| id != &event.turn_id)
        {
            return GateChange::Pending;
        }
        // 新一轮必须先由 started 绑定；旧完成或状态摘要不能补绑定。
        if target.turn_id.is_none() {
            if event.outcome.is_some() {
                return GateChange::Pending;
            }
            target.turn_id = Some(event.turn_id.clone());
        }
        target.outcome = event.outcome.clone();
        if pending.released() {
            GateChange::Released
        } else {
            GateChange::Pending
        }
    }

    fn cancel(&mut self) -> GateChange {
        self.pending.take();
        GateChange::Cancelled
    }

    fn disconnect(&mut self) -> GateChange {
        self.pending.take();
        GateChange::Disconnected
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ChildOutcome, CompletionGate, GateChange, GateEvent, PendingGate, WaitRequest, WaitTarget,
    };

    fn target(id: &str, generation: u64) -> WaitTarget {
        WaitTarget {
            id: id.into(),
            generation,
            turn_id: Some(format!("{id}-{generation}")),
            outcome: None,
        }
    }

    #[test]
    fn releases_once_all_current_generations_are_terminal() {
        let mut gate = PendingGate::default();
        gate.accept_wait(WaitRequest {
            targets: vec![target("a", 1), target("b", 2)],
        })
        .unwrap();

        assert_eq!(
            gate.apply(&GateEvent {
                target: "a".into(),
                generation: 1,
                turn_id: "a-1".into(),
                outcome: Some(ChildOutcome::Completed),
            }),
            GateChange::Pending
        );
        assert_eq!(
            gate.apply(&GateEvent {
                target: "b".into(),
                generation: 1,
                turn_id: "b-1".into(),
                outcome: Some(ChildOutcome::Completed),
            }),
            GateChange::Pending
        );
        assert_eq!(
            gate.apply(&GateEvent {
                target: "b".into(),
                generation: 2,
                turn_id: "b-2".into(),
                outcome: Some(ChildOutcome::Failed),
            }),
            GateChange::Released
        );
        assert!(gate.take_result().is_some());
        assert!(gate.result().is_none());
    }

    #[test]
    fn pending_result_cannot_be_taken_and_failure_releases_without_other_children() {
        let mut gate = PendingGate::default();
        gate.accept_wait(WaitRequest {
            targets: vec![target("a", 1), target("b", 1)],
        })
        .unwrap();
        assert!(gate.take_result().is_none());
        assert_eq!(
            gate.apply(&GateEvent {
                target: "a".into(),
                generation: 1,
                turn_id: "a-1".into(),
                outcome: Some(ChildOutcome::Failed)
            }),
            GateChange::Released
        );
        let (_, targets) = gate.take_result().unwrap();
        assert_eq!(targets[1].outcome, None);
        assert!(gate.take_result().is_none());
    }

    #[test]
    fn awaiting_generation_requires_a_started_event_before_completion() {
        let mut gate = PendingGate::default();
        gate.accept_wait(WaitRequest {
            targets: vec![WaitTarget {
                id: "a".into(),
                generation: 2,
                turn_id: None,
                outcome: None,
            }],
        })
        .unwrap();
        assert_eq!(
            gate.apply(&GateEvent {
                target: "a".into(),
                generation: 1,
                turn_id: "old".into(),
                outcome: Some(ChildOutcome::Completed)
            }),
            GateChange::Pending
        );
        assert_eq!(
            gate.apply(&GateEvent {
                target: "a".into(),
                generation: 2,
                turn_id: "new".into(),
                outcome: Some(ChildOutcome::Completed)
            }),
            GateChange::Pending
        );
        gate.apply(&GateEvent {
            target: "a".into(),
            generation: 2,
            turn_id: "new".into(),
            outcome: None,
        });
        assert_eq!(
            gate.apply(&GateEvent {
                target: "a".into(),
                generation: 2,
                turn_id: "new".into(),
                outcome: Some(ChildOutcome::Completed)
            }),
            GateChange::Released
        );
    }
}
