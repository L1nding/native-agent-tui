use std::env;
use std::io::{self, IsTerminal, Read};
use std::process::ExitCode;

use native_agent_tui::client::Command;
use native_agent_tui::config::{self, CliCommand, Config};
use native_agent_tui::history::{HistoryHandle, HistoryRequest, HistoryResult};
use native_agent_tui::journal::{self, JournalError, Replay};
use native_agent_tui::scheduler::{RootTaskSpec, WorkflowPlan, WORKFLOW_BYTES};
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
        "Execution requires codex-cli {}.",
        native_agent_tui::compatibility::SUPPORTED_CODEX_VERSION
    );
    println!(
        r#"
Usage:
  native-agent-tui [--tui [TASK]] [OPTIONS]
  native-agent-tui --run TASK [--json-events] [OPTIONS]
  native-agent-tui --workflow FILE [--headless [--json-events]] [OPTIONS]
  native-agent-tui --check-shell [OPTIONS]
  native-agent-tui --sessions [--cwd PATH] [--journal-dir PATH]
  native-agent-tui --replay SESSION_ID [--since SEQ] [--json-events] [OPTIONS]
  native-agent-tui --history [SESSION_ID] [OPTIONS]
  native-agent-tui --export SESSION_ID [--since SEQ] [--output NEW_FILE] [OPTIONS]

Options:
  --cwd PATH       Working directory (default: current directory)
  --codex PATH     Codex executable (or CODEX_BIN)
  --model NAME     Requested model; uses Codex configuration when omitted
  --sandbox MODE   read-only | workspace-write | danger-full-access
  --approval MODE  untrusted | on-request | never (default: on-request)
  --journal-dir PATH  Override the managed journal data directory
  --max-native-children N  Maximum direct native children (1-64; default: 8)
  --max-native-depth N     Maximum observed native depth (1-8; default: 2)
  --max-native-turns N     Maximum active native turns (1-64; default: 8)
  --max-total-tokens N     Session token budget; interrupt at the confirmed total
  --attention-config FILE  JSON threshold pairs (milliseconds)
  --attention-model QUIET_MS,ATTENTION_MS  Override model silence thresholds
  --attention-tool QUIET_MS,ATTENTION_MS   Override tool silence thresholds
  --attention-children QUIET_MS,ATTENTION_MS  Override child wait thresholds
  --attention-transport QUIET_MS,ATTENTION_MS  Override response wait thresholds
  --help, -h       Show this help
  --version, -V    Show version

Workflow files contain a JSON task DAG, validated before launching.
Sessions persist redacted snapshots by default. Replay is read-only and
never launches app-server. --json-events streams committed redacted state.
--history opens offline read-only observation. --export previews the range;
--output writes that captured range with stable identity aliases to a new file.
Default is the TUI; --headless returns 0 only when all root tasks succeed.
See docs/scheduler-usage.md and docs/workflow-example.json.

TUI: Enter send, Ctrl+S/Ctrl+Enter queue, Ctrl+O/Shift+Enter newline, Ctrl+C interrupt,
     Ctrl+Q quit, PgUp/PgDn scroll, Ctrl+Y approve, Ctrl+N decline,
     Ctrl+W acknowledge/restore silence reminders for the selected agent,
     F1 help, F2 request details/next, F3 agent, F4 tasks, F5 pause dispatch.
     In request details: PgUp/PgDn scroll, Ctrl+Home/End jump, Esc close.
     Ctrl+F opens retained conversation search; F1 inside search shows its controls.
     Ctrl+T opens the live evidence timeline with filters and local bookmarks.
     Ctrl+B cancels the selected approval and interrupts its owning turn when allowed.
     F10 attention thresholds, F11 activity evidence, F12 history/export.
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
    let arguments: Vec<_> = env::args().skip(1).collect();
    let redact_errors = arguments.iter().any(|argument| argument == "--json-events");
    match config::parse_args(arguments).map_err(|error| {
        (
            2,
            if redact_errors {
                "Invalid JSONL command or configuration; inspect --help and local settings.".into()
            } else {
                error.to_string()
            },
        )
    })? {
        CliCommand::Help => print_help(),
        CliCommand::Version => println!("native-agent-tui {}", env!("CARGO_PKG_VERSION")),
        CliCommand::History { session, config } => {
            if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
                return Err((2, "History browsing needs an interactive terminal. Use --replay for text or --export to save retained evidence.".into()));
            }
            let history = HistoryHandle::start(config.journal, config.cwd)
                .map_err(|error| (2, error.to_string()))?;
            ui::run_history(history, session)
                .await
                .map_err(|error| (2, error.to_string()))?;
        }
        CliCommand::Export {
            session,
            since,
            output,
            config,
        } => {
            let mut history = HistoryHandle::start(config.journal, config.cwd)
                .map_err(|error| (2, error.to_string()))?;
            let result = async {
                let id = history.request(HistoryRequest::Preview { session, since })?;
                if let HistoryResult::Preview(preview) = history.response(id).await? {
                    println!("{}", serde_json::to_string_pretty(&preview.manifest).expect("export manifest"));
                    if let Some(destination) = output {
                        let id = history.request(HistoryRequest::Export { destination })?;
                        if let HistoryResult::Exported { bytes } = history.response(id).await? { println!("Export saved: {bytes} bytes."); }
                    } else {
                        println!("Redacted preview excerpt (up to 8 KiB; prompts and full text unavailable):\n{}", preview.excerpt);
                    }
                }
                Ok::<_, native_agent_tui::history::HistoryError>(())
            }.await;
            let stopped = history.shutdown().await;
            result
                .and(stopped)
                .map_err(|error| (2, error.to_string()))?;
        }
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
                let compactions = state
                    .observation
                    .activities
                    .iter()
                    .filter(|activity| {
                        activity.tool_category
                            == Some(native_agent_tui::protocol::ToolCategory::Compaction)
                    })
                    .count();
                println!(
                    "Compactions retained: {} | lifetime total unavailable | before/after usage, reason and summary unavailable",
                    compactions
                );
                for activity in &state.observation.activities {
                    println!(
                        "{} {:?} {:?} {:?} | progress {} | historical quiet {:?}ms | last source {:?}",
                        display_text_for_cli(&activity.identity.agent_id),
                        activity.scope,
                        activity.execution_state,
                        activity.tool_category,
                        activity.progress_seq,
                        activity.silence_ms,
                        activity.last_evidence.as_ref().map(|evidence| evidence.source)
                    );
                }
            }
        }
        CliCommand::Run {
            goal,
            config,
            json_events,
        } => {
            return run_headless(config, vec![RootTaskSpec::input(goal)], false, json_events).await
        }
        CliCommand::CheckShell(config) => {
            return run_headless(config, Vec::new(), true, false).await
        }
        CliCommand::Workflow {
            path,
            headless,
            json_events,
            config,
        } => {
            // Validate the whole plan before starting an execution owner or model turn.
            let mut bytes = Vec::new();
            std::fs::File::open(&path)
                .and_then(|file| file.take(WORKFLOW_BYTES as u64 + 1).read_to_end(&mut bytes))
                .map_err(|error| {
                    (
                        2,
                        if json_events {
                            "Cannot read workflow file.".into()
                        } else {
                            format!("Cannot read workflow: {error}")
                        },
                    )
                })?;
            let tasks = WorkflowPlan::parse(&bytes)
                .map_err(|error| {
                    (
                        2,
                        if json_events {
                            "Workflow file is invalid; inspect its task definitions.".into()
                        } else {
                            error
                        },
                    )
                })?
                .tasks;
            if headless {
                return run_headless(config, tasks, false, json_events).await;
            }
            if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
                return Err((2, "The workflow TUI needs an interactive terminal. Add --headless for scripted execution.".into()));
            }
            let history = HistoryHandle::start(config.journal.clone(), config.cwd.clone())
                .map_err(|error| (3, error.to_string()))?;
            let client = ClientHandle::spawn(config)
                .await
                .map_err(|error| (3, error.to_string()))?;
            ui::run_tasks_with_history(client, tasks, history)
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
            let history = HistoryHandle::start(config.journal.clone(), config.cwd.clone())
                .map_err(|error| (3, error.to_string()))?;
            let client = ClientHandle::spawn(config)
                .await
                .map_err(|error| (3, error.to_string()))?;
            ui::run_tasks_with_history(
                client,
                goal.into_iter().map(RootTaskSpec::input).collect(),
                history,
            )
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
    json_events: bool,
) -> Result<(), (u8, String)> {
    let journal = config.journal.clone();
    let cwd = config.cwd.clone();
    let client = if check_only {
        ClientHandle::check_shell(config).await
    } else {
        ClientHandle::spawn(config).await
    }
    .map_err(|error| {
        (
            3,
            if json_events {
                "Execution startup failed; inspect local Codex configuration and journal storage."
                    .into()
            } else {
                error.to_string()
            },
        )
    })?;
    let output = if json_events {
        let session = client.snapshots.borrow().observation.session_id.clone();
        match native_agent_tui::json_events::LiveOutput::stdout(&journal, &cwd, &session) {
            Ok(output) => Some(output),
            Err(error) => {
                let _ = client.commands.send(Command::OutputUnavailable).await;
                let _ = client.join.await;
                return Err((4, error.to_string()));
            }
        }
    } else {
        None
    };
    native_agent_tui::headless::run(client, tasks, check_only, output).await
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
