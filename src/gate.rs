use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WaitTarget {
    pub id: String,
    pub generation: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WaitRequest {
    pub targets: Vec<WaitTarget>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct WaitToken(pub u64);

#[derive(Debug, Clone, PartialEq, Eq)]
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
    EmptyTargets,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GateEvent {
    pub target: String,
    pub generation: u64,
    pub outcome: ChildOutcome,
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
    targets: BTreeMap<String, (u64, Option<ChildOutcome>)>,
}

impl PendingGate {
    pub fn result(&self) -> Option<(WaitToken, Vec<(String, ChildOutcome)>)> {
        let pending = self.pending.as_ref()?;
        let outcomes = pending
            .targets
            .iter()
            .filter_map(|(id, (_, outcome))| outcome.clone().map(|outcome| (id.clone(), outcome)))
            .collect();
        Some((pending.token, outcomes))
    }

    pub fn take_result(&mut self) -> Option<(WaitToken, Vec<(String, ChildOutcome)>)> {
        let result = self.result();
        self.pending.take();
        result
    }
}

impl CompletionGate for PendingGate {
    fn accept_wait(&mut self, request: WaitRequest) -> Result<WaitToken, GateError> {
        if self.pending.is_some() {
            return Err(GateError::AlreadyPending);
        }
        if request.targets.is_empty() {
            return Err(GateError::EmptyTargets);
        }

        self.next_token += 1;
        let targets = request
            .targets
            .into_iter()
            .map(|target| (target.id, (target.generation, None)))
            .collect();
        let token = WaitToken(self.next_token);
        self.pending = Some(PendingWait { token, targets });
        Ok(token)
    }

    fn apply(&mut self, event: &GateEvent) -> GateChange {
        let Some(pending) = self.pending.as_mut() else {
            return GateChange::Pending;
        };
        let Some((expected_generation, outcome)) = pending.targets.get_mut(&event.target) else {
            return GateChange::Pending;
        };
        if *expected_generation != event.generation || outcome.is_some() {
            return GateChange::Pending;
        }
        *outcome = Some(event.outcome.clone());
        if pending
            .targets
            .values()
            .all(|(_, outcome)| outcome.is_some())
        {
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
                outcome: ChildOutcome::Completed,
            }),
            GateChange::Pending
        );
        assert_eq!(
            gate.apply(&GateEvent {
                target: "b".into(),
                generation: 1,
                outcome: ChildOutcome::Completed,
            }),
            GateChange::Pending
        );
        assert_eq!(
            gate.apply(&GateEvent {
                target: "b".into(),
                generation: 2,
                outcome: ChildOutcome::Failed,
            }),
            GateChange::Released
        );
        assert!(gate.take_result().is_some());
        assert!(gate.result().is_none());
    }
}
