use std::env;
use std::io::{self, IsTerminal, Write};
use std::process::ExitCode;

use native_agent_tui::client::Command;
use native_agent_tui::config::{self, CliCommand, Config};
use native_agent_tui::state::{display_text_for_cli, SessionPhase};
use native_agent_tui::{ui, ClientHandle};

fn print_help() {
    let windows = if cfg!(windows) {
        "\nWindows option: --windows-sandbox elevated|unelevated (inherits Codex config)\n"
    } else {
        ""
    };
    println!("Native Agent TUI {}\n\nUsage:\n  native-agent-tui [--tui [TASK]] [OPTIONS]\n  native-agent-tui --run TASK [OPTIONS]\n  native-agent-tui --check-shell [OPTIONS]\n\nOptions:\n  --cwd PATH       Working directory (default: current directory)\n  --codex PATH     Codex executable (or CODEX_BIN)\n  --model NAME     Requested model; uses Codex configuration when omitted\n  --sandbox MODE   read-only | workspace-write | danger-full-access\n  --approval MODE  untrusted | on-request | never (default: on-request)\n  --help, -h       Show this help\n  --version, -V    Show version\n\nTUI: Enter send, Shift+Enter newline, Ctrl+C interrupt, Ctrl+Q quit,\n     PgUp/PgDn scroll, Ctrl+Y approve, Ctrl+N decline, F1 help, F2 next request.\n\nHeadless runs decline interactive approval requests; use the TUI to review them.",
        env!("CARGO_PKG_VERSION"));
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
        CliCommand::Run { goal, config } => return run_headless(config, Some(goal), false).await,
        CliCommand::CheckShell(config) => return run_headless(config, None, true).await,
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
                .map_err(|error| (1, error.to_string()))?;
            ui::run(client, goal)
                .await
                .map_err(|error| (1, error.to_string()))?;
        }
    }
    Ok(())
}

async fn run_headless(
    config: Config,
    goal: Option<String>,
    check_only: bool,
) -> Result<(), (u8, String)> {
    let mut client = if check_only {
        ClientHandle::check_shell(config).await
    } else {
        ClientHandle::spawn(config).await
    }
    .map_err(|error| (1, error.to_string()))?;
    if let Some(text) = goal {
        client
            .commands
            .send(Command::SubmitRootInput { text })
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
                break Ok(());
            }
            SessionPhase::Interrupted => break Err((130, "Turn interrupted.".into())),
            SessionPhase::Failed | SessionPhase::Unknown | SessionPhase::Disconnected => {
                break Err((
                    1,
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
                    break Err((1, "Client exited before confirming a terminal result.".into()));
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
    let report = client.join.await.map_err(|error| (1, error.to_string()))?;
    if let Some(error) = report.cleanup_error {
        return Err((1, format!("Process cleanup failed: {error}")));
    }
    outcome
}
