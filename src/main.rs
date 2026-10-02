use std::env;
use std::io::IsTerminal;

use native_agent_tui::{config, ui, ClientHandle, Command};

fn print_help() {
    println!(
        "native-agent-tui 0.1.0\n\nUsage:\n  native-agent-tui\n  native-agent-tui --tui [goal]\n  native-agent-tui --run <goal>\n  native-agent-tui --check-shell\n  native-agent-tui --help\n  native-agent-tui --version\n\nTUI controls:\n  s start  r run demo goal  c complete root  g wait child  a child done\n  x stop   q or Esc quit"
    );
}

#[tokio::main]
async fn main() {
    match config::parse_args(env::args().skip(1)) {
        Ok(config::CliCommand::Help) => print_help(),
        Ok(config::CliCommand::Version) => println!("native-agent-tui 0.1.0"),
        Ok(config::CliCommand::CheckShell) => println!("shell preflight: scaffold ready"),
        Ok(config::CliCommand::Run { goal }) => run_once(goal).await,
        Ok(config::CliCommand::Tui { goal }) => {
            if goal.is_none() && !std::io::stdout().is_terminal() {
                print_help();
                return;
            }
            if let Err(error) = ui::run(ClientHandle::spawn().await, goal).await {
                eprintln!("TUI error: {error}");
                std::process::exit(1);
            }
        }
        Err(error) => {
            eprintln!("error: {error}");
            print_help();
            std::process::exit(2);
        }
    }
}

async fn run_once(goal: String) {
    let mut client = ClientHandle::spawn().await;
    let _ = client.commands.send(Command::Start).await;
    let _ = client.snapshots.changed().await;
    let _ = client
        .commands
        .send(Command::SubmitRootInput { text: goal.clone() })
        .await;
    let _ = client.snapshots.changed().await;
    let snapshot = client.snapshots.borrow().clone();
    println!("accepted goal: {goal}");
    println!(
        "phase: {:?}, root turns: {}",
        snapshot.phase, snapshot.root_turn_count
    );
    let _ = client.commands.send(Command::Quit).await;
    let _ = client.join.await;
}
