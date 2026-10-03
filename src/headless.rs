//! Noninteractive lifecycle policy shared by plain text and JSONL consumers.
use std::io::{self, Write};

use crate::client::{ClientHandle, Command};
use crate::json_events::{LiveOutput, OutputError};
use crate::scheduler::{RootTaskSpec, TaskKind};
use crate::state::{display_text_for_cli, SessionPhase};

pub async fn run(
    mut client: ClientHandle,
    tasks: Vec<RootTaskSpec>,
    check_only: bool,
    mut output: Option<LiveOutput>,
) -> Result<(), (u8, String)> {
    let json_events = output.is_some();
    if let Some(journal) = &client.snapshots.borrow().journal {
        eprintln!("session: {}", journal.session_id);
    }
    let mut output_failure = if let Some(output) = &mut output {
        output.ready().await.err()
    } else {
        None
    };
    let mut submission_closed = false;
    if output_failure.is_none() && !tasks.is_empty() {
        submission_closed = client
            .commands
            .send(Command::QueueRootTasks { tasks })
            .await
            .is_err();
    }
    let mut displayed = std::collections::HashMap::<(String, String), String>::new();
    let mut declined = std::collections::HashSet::new();
    let mut interaction_deadline = None;
    let mut output_done = output
        .as_ref()
        .is_some_and(|output| output.status.borrow().done);
    let mut monitor = tokio::time::interval(std::time::Duration::from_millis(100));
    monitor.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let outcome = loop {
        output_failure = output_failure.or_else(|| output.as_ref().and_then(LiveOutput::failure));
        if let Some(error) = &output_failure {
            if let Some(output) = &output {
                output.stop();
            }
            let _ = client.commands.send(Command::OutputUnavailable).await;
            break Err((4, error.to_string()));
        }
        let snapshot = client.snapshots.borrow().clone();
        displayed.retain(|(turn, id), _| {
            snapshot
                .messages
                .iter()
                .any(|message| &message.turn_id == turn && &message.id == id)
        });
        declined.retain(|reference| {
            snapshot
                .requests
                .iter()
                .any(|request| request.matches(reference))
        });
        if !json_events {
            for message in snapshot.messages.iter().filter(|message| {
                message.role == "Agent"
                    && Some(message.thread_id.as_str()) == snapshot.thread_id.as_deref()
            }) {
                let previous = displayed
                    .entry((message.turn_id.clone(), message.id.clone()))
                    .or_default();
                if message.text != *previous {
                    if let Some(suffix) = message.text.strip_prefix(previous.as_str()) {
                        print!("{}", display_text_for_cli(suffix));
                    } else {
                        print!("\n{}", display_text_for_cli(&message.text));
                    }
                    *previous = message.text.clone();
                    let _ = io::stdout().flush();
                }
            }
        }
        for request in &snapshot.requests {
            if request.responding || !declined.insert(request.reference()) {
                continue;
            }
            match &request.kind {
                crate::interactions::RequestKind::UserInput { .. } => {
                    eprintln!("\nUser input required. Use --tui to answer questions.");
                    interaction_deadline.get_or_insert_with(|| {
                        tokio::time::Instant::now() + std::time::Duration::from_secs(5)
                    });
                }
                _ if !request.allow_decline => {
                    eprintln!("\nApproval has no decline decision; requesting interruption. Use the TUI to review.");
                    interaction_deadline.get_or_insert_with(|| {
                        tokio::time::Instant::now() + std::time::Duration::from_secs(5)
                    });
                }
                _ => {
                    eprintln!(
                        "\nApproval required; declining in headless mode. Use --tui to review."
                    );
                }
            }
            let _ = client
                .commands
                .send(Command::HeadlessRequest {
                    request: request.reference(),
                })
                .await;
        }
        let outcome = crate::state::workflow_outcome(
            snapshot.phase,
            snapshot
                .scheduler
                .tasks
                .iter()
                .filter(|task| task.kind == TaskKind::RootTurn)
                .map(|task| task.state),
        );
        match outcome {
            SessionPhase::Ready if check_only => {
                if !json_events {
                    println!("shell preflight: passed (zero model turns)");
                }
                break Ok(());
            }
            SessionPhase::Completed => {
                if !json_events {
                    println!();
                }
                break Ok(());
            }
            SessionPhase::Interrupted => break Err((130, "Turn interrupted.".into())),
            SessionPhase::Unknown | SessionPhase::Disconnected => {
                break Err((
                    4,
                    if json_events {
                        "Execution outcome is unknown; inspect committed session state.".into()
                    } else {
                        snapshot
                            .last_error
                            .clone()
                            .unwrap_or_else(|| format!("{:?}", snapshot.phase))
                    },
                ))
            }
            SessionPhase::Failed => {
                break Err((
                    if snapshot.root_start_requests == 0 {
                        3
                    } else {
                        1
                    },
                    if json_events {
                        "Execution failed; inspect the redacted session journal.".into()
                    } else {
                        snapshot
                            .last_error
                            .clone()
                            .unwrap_or_else(|| format!("{:?}", snapshot.phase))
                    },
                ))
            }
            _ => {}
        }
        if submission_closed {
            break Err((4, "Client exited before accepting workflow tasks.".into()));
        }
        if interaction_deadline.is_some_and(|deadline| tokio::time::Instant::now() >= deadline) {
            let _ = client
                .commands
                .send(Command::UnconfirmedHeadlessInteraction)
                .await;
            break Err((
                4,
                "Interaction requires the TUI; interruption was not confirmed within five seconds."
                    .into(),
            ));
        }
        tokio::select! {
            changed = async {
                match &mut output { Some(output) => Some(output.status.changed().await), None => std::future::pending().await }
            }, if !output_done => {
                if matches!(changed, Some(Err(_))) && output.as_ref().is_some_and(|output| !output.status.borrow().done) { output_failure = Some(OutputError::Closed); }
                // A finished output channel is no longer a wake source.
                output_done = output.as_ref().is_some_and(|output| output.status.borrow().done);
            }
            _ = monitor.tick(), if output.is_some() || interaction_deadline.is_some() => {}
            result = client.snapshots.changed() => {
                if result.is_err() {
                    break Err((4, "Client exited before confirming a terminal result.".into()));
                }
            }
            _ = tokio::signal::ctrl_c() => {
                if snapshot.turn_id.is_some() {
                    let _ = client.commands.send(Command::Interrupt).await;
                } else {
                    break Err((130, "Cancelled before the first turn.".into()));
                }
            }
        }
    };
    let _ = client.commands.send(Command::Quit).await;
    let report = client.join.await;
    let output_result = if let Some(output) = &mut output {
        output.finish().await
    } else {
        Ok(())
    };
    let report =
        report.map_err(|_| (4, "Execution owner exited without a cleanup report.".into()))?;
    if let Some(error) = report.cleanup_error {
        return Err((
            4,
            if json_events {
                "Process cleanup was not confirmed.".into()
            } else {
                format!("Process cleanup failed: {error}")
            },
        ));
    }
    if let Some(error) = report.journal_error {
        return Err((4, error.to_string()));
    }
    if let Some(error) = output_failure {
        return Err((4, error.to_string()));
    }
    if let Err(error) = output_result {
        return Err((4, error.to_string()));
    }
    outcome
}
