use std::env;
use std::io::{self, IsTerminal, Read, Write};
use std::process::ExitCode;

use native_agent_tui::client::Command;
use native_agent_tui::config::{self, CliCommand, Config};
use native_agent_tui::journal::{self, JournalError, Replay};
use native_agent_tui::scheduler::{
    RootTaskSpec, TaskKind, TaskState, WorkflowPlan, WORKFLOW_BYTES,
};
use native_agent_tui::state::{display_text_for_cli, SessionPhase};
use native_agent_tui::{ui, ClientHandle};

fn print_help() {
    let windows = if cfg!(windows) {
        "\nWindows option: --windows-sandbox elevated|unelevated (inherits Codex config)\n"
    } else {
        ""
    };
    println!("Native Agent TUI {}", env!("CARGO_PKG_VERSION"));
    println!(
        r#"
Usage:
  native-agent-tui [--tui [TASK]] [OPTIONS]
  native-agent-tui --run TASK [OPTIONS]
  native-agent-tui --workflow FILE [--headless] [OPTIONS]
  native-agent-tui --check-shell [OPTIONS]
  native-agent-tui --sessions [--cwd PATH] [--journal-dir PATH]
  native-agent-tui --replay SESSION_ID [--since SEQ] [--json-events] [OPTIONS]

Options:
  --cwd PATH       Working directory (default: current directory)
  --codex PATH     Codex executable (or CODEX_BIN)
  --model NAME     Requested model; uses Codex configuration when omitted
  --sandbox MODE   read-only | workspace-write | danger-full-access
  --approval MODE  untrusted | on-request | never (default: on-request)
  --journal-dir PATH  Override the managed journal data directory
  --attention-config FILE  JSON threshold pairs (milliseconds)
  --attention-model QUIET_MS,ATTENTION_MS  Override model silence thresholds
  --attention-tool QUIET_MS,ATTENTION_MS   Override tool silence thresholds
  --attention-children QUIET_MS,ATTENTION_MS  Override child wait thresholds
  --attention-transport QUIET_MS,ATTENTION_MS  Override response wait thresholds
  --help, -h       Show this help
  --version, -V    Show version

Workflow files contain a JSON task DAG, validated before launching.
Sessions persist redacted snapshots by default. Replay is read-only and
never launches app-server. --json-events currently supports replay only.
Default is the TUI; --headless returns 0 only when all root tasks succeed.
See docs/scheduler-usage.md and docs/workflow-example.json.

TUI: Enter send, Ctrl+Enter queue, Shift+Enter newline, Ctrl+C interrupt,
     Ctrl+Q quit, PgUp/PgDn scroll, Ctrl+Y approve, Ctrl+N decline,
     F1 help, F2 request, F3 agent, F4 tasks, F5 pause dispatch.
     F10 attention thresholds, F11 activity evidence.
Tasks: Up/Down select, F6 pause task, F7 cancel, F8 retry (may repeat
       side effects), +/- priority, F9 twice stop workflow.

Headless runs decline interactive approvals; use the TUI to review them."#
    );
    print!("{windows}");
}

#[tokio::main]
async fn main() -> ExitCode {
    match execute().await {
        Ok(()) => ExitCode::SUCCESS,
        Err((code, error)) => {
            eprintln!("error: {}", display_text_for_cli(&error));
            ExitCode::from(code)
        }
    }
}

async fn execute() -> Result<(), (u8, String)> {
    match config::parse_args(env::args().skip(1)).map_err(|error| (2, error.to_string()))? {
        CliCommand::Help => print_help(),
        CliCommand::Version => println!("native-agent-tui {}", env!("CARGO_PKG_VERSION")),
        CliCommand::Sessions(config) => {
            let sessions = journal::sessions(&config.journal, &config.cwd).map_err(replay_error)?;
            if sessions.is_empty() {
                println!("No stored sessions in this workspace.");
            }
            for session in sessions {
                println!(
                    "{} | seq {} | {:?} | {}",
                    session.session_id,
                    session.committed_seq,
                    session.execution_result.unwrap_or(SessionPhase::Unknown),
                    if session.needs_recovery {
                        "needs observation review"
                    } else {
                        "closed"
                    }
                );
            }
        }
        CliCommand::Replay {
            session,
            since,
            json_events,
            config,
        } => {
            let replay = Replay::open(&config.journal, &config.cwd, &session, since)
                .map_err(replay_error)?;
            if json_events {
                replay
                    .write_jsonl(&mut io::stdout().lock())
                    .map_err(replay_error)?;
            } else {
                let state = replay.latest_state();
                println!(
                    "Historical session {} through event {} (live_attached=false)",
                    replay.info.session_id, replay.info.committed_seq
                );
                println!(
                    "Execution result: {:?} | recorded phase: {:?} | cleanup confirmed: {:?}",
                    state.execution_result.unwrap_or(SessionPhase::Unknown),
                    state.phase,
                    state.cleanup_confirmed
                );
                if replay.info.needs_recovery {
                    println!("Observation review required. Replay cannot answer old requests or resume execution.");
                }
                for activity in &state.observation.activities {
                    println!(
                        "{} {:?} {:?} | progress {} | historical quiet {:?}ms",
                        display_text_for_cli(&activity.identity.agent_id),
                        activity.scope,
                        activity.execution_state,
                        activity.progress_seq,
                        activity.silence_ms
                    );
                }
            }
        }
        CliCommand::Run { goal, config } => {
            return run_headless(config, vec![RootTaskSpec::input(goal)], false).await
        }
        CliCommand::CheckShell(config) => return run_headless(config, Vec::new(), true).await,
        CliCommand::Workflow {
            path,
            headless,
            config,
        } => {
            // Validate the whole plan before starting an execution owner or model turn.
            let mut bytes = Vec::new();
            std::fs::File::open(&path)
                .and_then(|file| file.take(WORKFLOW_BYTES as u64 + 1).read_to_end(&mut bytes))
                .map_err(|error| (2, format!("Cannot read workflow: {error}")))?;
            let tasks = WorkflowPlan::parse(&bytes)
                .map_err(|error| (2, error))?
                .tasks;
            if headless {
                return run_headless(config, tasks, false).await;
            }
            if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
                return Err((2, "The workflow TUI needs an interactive terminal. Add --headless for scripted execution.".into()));
            }
            let client = ClientHandle::spawn(config)
                .await
                .map_err(|error| (3, error.to_string()))?;
            ui::run_tasks(client, tasks)
                .await
                .map_err(|error| (3, error.to_string()))?;
        }
        CliCommand::Tui { goal, config } => {
            if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
                if goal.is_none() {
                    print_help();
                    return Ok(());
                }
                return Err((
                    2,
                    "The TUI needs an interactive terminal. Use --run TASK for headless execution."
                        .into(),
                ));
            }
            let client = ClientHandle::spawn(config)
                .await
                .map_err(|error| (3, error.to_string()))?;
            ui::run(client, goal)
                .await
                .map_err(|error| (3, error.to_string()))?;
        }
    }
    Ok(())
}

async fn run_headless(
    config: Config,
    tasks: Vec<RootTaskSpec>,
    check_only: bool,
) -> Result<(), (u8, String)> {
    let mut client = if check_only {
        ClientHandle::check_shell(config).await
    } else {
        ClientHandle::spawn(config).await
    }
    .map_err(|error| (3, error.to_string()))?;
    if let Some(journal) = &client.snapshots.borrow().journal {
        eprintln!("session: {}", journal.session_id);
    }
    if !tasks.is_empty() {
        client
            .commands
            .send(Command::QueueRootTasks { tasks })
            .await
            .map_err(|error| (1, error.to_string()))?;
    }
    let mut displayed = std::collections::HashMap::<(String, String), String>::new();
    let mut declined = std::collections::HashSet::new();
    let outcome = loop {
        let snapshot = client.snapshots.borrow().clone();
        displayed.retain(|(turn, id), _| {
            snapshot
                .messages
                .iter()
                .any(|message| &message.turn_id == turn && &message.id == id)
        });
        declined.retain(|id| snapshot.requests.iter().any(|request| &request.id == id));
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
        for request in &snapshot.requests {
            if request.responding || !declined.insert(request.id.clone()) {
                continue;
            }
            match &request.kind {
                native_agent_tui::interactions::RequestKind::UserInput { .. } => {
                    eprintln!("\nUser input required. Use --tui to answer questions.");
                    let _ = client.commands.send(Command::Interrupt).await;
                }
                _ => {
                    eprintln!(
                        "\nApproval required; declining in headless mode. Use --tui to review."
                    );
                    let _ = client
                        .commands
                        .send(Command::AnswerApproval {
                            request_id: request.id.clone(),
                            decision: native_agent_tui::interactions::ApprovalDecision::Decline,
                        })
                        .await;
                }
            }
        }
        match snapshot.phase {
            SessionPhase::Ready if check_only => {
                println!("shell preflight: passed (zero model turns)");
                break Ok(());
            }
            SessionPhase::Completed => {
                println!();
                let roots: Vec<_> = snapshot
                    .scheduler
                    .tasks
                    .iter()
                    .filter(|task| task.kind == TaskKind::RootTurn)
                    .collect();
                if roots.iter().all(|task| task.state == TaskState::Succeeded) {
                    break Ok(());
                }
                break Err((1, "Workflow ended with failed, cancelled, or blocked tasks. Use the TUI task panel to inspect or explicitly retry them.".into()));
            }
            SessionPhase::Interrupted => break Err((130, "Turn interrupted.".into())),
            SessionPhase::Unknown | SessionPhase::Disconnected => {
                break Err((
                    4,
                    snapshot
                        .last_error
                        .clone()
                        .unwrap_or_else(|| format!("{:?}", snapshot.phase)),
                ))
            }
            SessionPhase::Failed => {
                break Err((
                    if snapshot.root_start_requests == 0 {
                        3
                    } else {
                        1
                    },
                    snapshot
                        .last_error
                        .clone()
                        .unwrap_or_else(|| format!("{:?}", snapshot.phase)),
                ))
            }
            _ => {}
        }
        tokio::select! {
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
    let report = client.join.await.map_err(|error| (4, error.to_string()))?;
    if let Some(error) = report.cleanup_error {
        return Err((4, format!("Process cleanup failed: {error}")));
    }
    if let Some(error) = report.journal_error {
        return Err((4, error.to_string()));
    }
    outcome
}

fn replay_error(error: JournalError) -> (u8, String) {
    let code = if matches!(
        error,
        JournalError::Identity
            | JournalError::NotFound
            | JournalError::Removed
            | JournalError::Cursor { .. }
    ) {
        2
    } else {
        3
    };
    (code, error.to_string())
}
