use std::sync::Arc;

use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;

use crate::gate::{CompletionGate, GateChange, GateEvent, PendingGate, WaitRequest, WaitTarget};
use crate::state::{CoreEvent, CoreSnapshot, SessionPhase, SessionState};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StopMode {
    Graceful,
    Immediate,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Start,
    SubmitRootInput { text: String },
    Apply(CoreEvent),
    OpenGate { targets: Vec<WaitTarget> },
    ApplyGate(GateEvent),
    Stop { mode: StopMode },
    Quit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExitReport {
    pub final_phase: SessionPhase,
}

pub struct ClientHandle {
    pub commands: mpsc::Sender<Command>,
    pub snapshots: watch::Receiver<Arc<CoreSnapshot>>,
    pub join: JoinHandle<ExitReport>,
}

impl ClientHandle {
    pub async fn spawn() -> Self {
        let (commands, mut command_rx) = mpsc::channel(32);
        let (snapshot_tx, snapshots) = watch::channel(Arc::new(CoreSnapshot::default()));

        let join = tokio::spawn(async move {
            let mut state = SessionState::default();
            let mut gate = PendingGate::default();
            while let Some(command) = command_rx.recv().await {
                match command {
                    Command::Start => {
                        publish(&mut state, &snapshot_tx, CoreEvent::LaunchRequested);
                        publish(&mut state, &snapshot_tx, CoreEvent::Initialized);
                    }
                    Command::SubmitRootInput { text } if !text.trim().is_empty() => {
                        let _ = text;
                        publish(&mut state, &snapshot_tx, CoreEvent::RootTurnStarted);
                    }
                    Command::Apply(event) => {
                        publish(&mut state, &snapshot_tx, event);
                    }
                    Command::OpenGate { targets } => {
                        if gate.accept_wait(WaitRequest { targets }).is_ok() {
                            publish(&mut state, &snapshot_tx, CoreEvent::GateEntered);
                        } else {
                            publish(&mut state, &snapshot_tx, CoreEvent::Failed);
                        }
                    }
                    Command::ApplyGate(event) => {
                        if gate.apply(&event) == GateChange::Released {
                            let _ = gate.take_result();
                            publish(&mut state, &snapshot_tx, CoreEvent::GateReleased);
                        }
                    }
                    Command::Stop { mode: _ } | Command::Quit => {
                        publish(&mut state, &snapshot_tx, CoreEvent::StopRequested);
                        publish(&mut state, &snapshot_tx, CoreEvent::TransportClosed);
                        break;
                    }
                    Command::SubmitRootInput { .. } => {}
                }
            }

            ExitReport {
                final_phase: state.snapshot().phase,
            }
        });

        Self {
            commands,
            snapshots,
            join,
        }
    }
}

fn publish(
    state: &mut SessionState,
    snapshot_tx: &watch::Sender<Arc<CoreSnapshot>>,
    event: CoreEvent,
) {
    if let Ok(snapshot) = state.apply(event) {
        let _ = snapshot_tx.send(snapshot);
    } else {
        let _ = snapshot_tx.send(state.snapshot());
    }
}

#[allow(dead_code)]
fn _wait_target(id: impl Into<String>, generation: u64) -> WaitTarget {
    WaitTarget {
        id: id.into(),
        generation,
    }
}

#[cfg(test)]
mod tests {
    use super::{ClientHandle, Command};
    use crate::gate::{ChildOutcome, GateEvent, WaitTarget};
    use crate::state::SessionPhase;

    #[tokio::test]
    async fn client_publishes_state_changes_and_stops_cleanly() {
        let mut client = ClientHandle::spawn().await;
        client.commands.send(Command::Start).await.unwrap();
        client.snapshots.changed().await.unwrap();
        client
            .commands
            .send(Command::SubmitRootInput {
                text: "hello".into(),
            })
            .await
            .unwrap();
        client.snapshots.changed().await.unwrap();
        client.commands.send(Command::Quit).await.unwrap();
        let report = client.join.await.unwrap();
        assert_eq!(report.final_phase, SessionPhase::Stopped);
    }

    #[tokio::test]
    async fn client_releases_gate_only_after_current_child_generation_finishes() {
        let mut client = ClientHandle::spawn().await;
        client.commands.send(Command::Start).await.unwrap();
        client.snapshots.changed().await.unwrap();
        client
            .commands
            .send(Command::SubmitRootInput {
                text: "hello".into(),
            })
            .await
            .unwrap();
        client.snapshots.changed().await.unwrap();
        client
            .commands
            .send(Command::OpenGate {
                targets: vec![WaitTarget {
                    id: "child".into(),
                    generation: 2,
                }],
            })
            .await
            .unwrap();
        client.snapshots.changed().await.unwrap();
        client
            .commands
            .send(Command::ApplyGate(GateEvent {
                target: "child".into(),
                generation: 1,
                outcome: ChildOutcome::Completed,
            }))
            .await
            .unwrap();
        assert_eq!(client.snapshots.borrow().phase, SessionPhase::GatePending);
        client
            .commands
            .send(Command::ApplyGate(GateEvent {
                target: "child".into(),
                generation: 2,
                outcome: ChildOutcome::Completed,
            }))
            .await
            .unwrap();
        client.snapshots.changed().await.unwrap();
        assert_eq!(client.snapshots.borrow().phase, SessionPhase::Running);
        client.commands.send(Command::Quit).await.unwrap();
        let _ = client.join.await;
    }
}
