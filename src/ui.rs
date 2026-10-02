use std::io::{self, stdout, Stdout};
use std::time::Duration;

use crossterm::event::{self, Event, KeyCode, KeyEvent};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Gauge, List, ListItem, Paragraph, Wrap};
use ratatui::Terminal;
use thiserror::Error;

use crate::client::{ClientHandle, Command};
use crate::gate::{ChildOutcome, GateEvent, WaitTarget};
use crate::state::{CoreSnapshot, SessionPhase};

#[derive(Debug, Error)]
pub enum UiError {
    #[error("terminal I/O failed: {0}")]
    Io(#[from] io::Error),
    #[error("client command queue is full or closed")]
    CommandUnavailable,
}

pub async fn run(mut client: ClientHandle, goal: Option<String>) -> Result<(), UiError> {
    let mut terminal = TerminalGuard::enter()?;
    client
        .commands
        .try_send(Command::Start)
        .map_err(|_| UiError::CommandUnavailable)?;
    if let Some(goal) = goal {
        client
            .commands
            .try_send(Command::SubmitRootInput { text: goal })
            .map_err(|_| UiError::CommandUnavailable)?;
    }

    event_loop(&mut terminal.terminal, &mut client)?;
    let _ = client.commands.send(Command::Quit).await;
    let _ = client.join.await;
    Ok(())
}

fn event_loop(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    client: &mut ClientHandle,
) -> Result<(), UiError> {
    loop {
        let snapshot = client.snapshots.borrow().clone();
        terminal.draw(|frame| draw(frame, &snapshot))?;

        if event::poll(Duration::from_millis(120))? {
            if let Event::Key(key) = event::read()? {
                match key {
                    KeyEvent {
                        code: KeyCode::Char('q'),
                        ..
                    }
                    | KeyEvent {
                        code: KeyCode::Esc, ..
                    } => return Ok(()),
                    KeyEvent {
                        code: KeyCode::Char('s'),
                        ..
                    } => send(client, Command::Start)?,
                    KeyEvent {
                        code: KeyCode::Char('r'),
                        ..
                    } => send(
                        client,
                        Command::SubmitRootInput {
                            text: "interactive goal".into(),
                        },
                    )?,
                    KeyEvent {
                        code: KeyCode::Char('c'),
                        ..
                    } => send(
                        client,
                        Command::Apply(crate::state::CoreEvent::RootTurnCompleted),
                    )?,
                    KeyEvent {
                        code: KeyCode::Char('g'),
                        ..
                    } => send(
                        client,
                        Command::OpenGate {
                            targets: vec![WaitTarget {
                                id: "demo-child".into(),
                                generation: 1,
                            }],
                        },
                    )?,
                    KeyEvent {
                        code: KeyCode::Char('a'),
                        ..
                    } => send(
                        client,
                        Command::ApplyGate(GateEvent {
                            target: "demo-child".into(),
                            generation: 1,
                            outcome: ChildOutcome::Completed,
                        }),
                    )?,
                    KeyEvent {
                        code: KeyCode::Char('x'),
                        ..
                    } => {
                        send(
                            client,
                            Command::Stop {
                                mode: crate::client::StopMode::Graceful,
                            },
                        )?;
                        return Ok(());
                    }
                    _ => {}
                }
            }
        }
    }
}

fn send(client: &ClientHandle, command: Command) -> Result<(), UiError> {
    client
        .commands
        .try_send(command)
        .map_err(|_| UiError::CommandUnavailable)
}

fn draw(frame: &mut ratatui::Frame<'_>, snapshot: &CoreSnapshot) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(7),
            Constraint::Length(5),
        ])
        .split(frame.area());

    let header = Paragraph::new(Line::from(vec![
        Span::styled(
            " native-agent-tui 0.1.0 ",
            Style::default().add_modifier(Modifier::BOLD),
        ),
        Span::raw(format!(
            "  phase={}  snapshot={} ",
            phase_label(snapshot.phase),
            snapshot.version
        )),
    ]))
    .block(Block::default().borders(Borders::ALL).title("Session"));
    frame.render_widget(header, chunks[0]);

    let body = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(55), Constraint::Percentage(45)])
        .split(chunks[1]);

    let progress = match snapshot.phase {
        SessionPhase::Created => 0,
        SessionPhase::Launching => 15,
        SessionPhase::Ready => 30,
        SessionPhase::Running => 65,
        SessionPhase::GatePending => 80,
        SessionPhase::Completed | SessionPhase::Stopped => 100,
        SessionPhase::Failed | SessionPhase::Disconnected => 100,
        _ => 45,
    };
    let gauge = Gauge::default()
        .block(Block::default().borders(Borders::ALL).title("Execution"))
        .gauge_style(
            Style::default().fg(if snapshot.phase == SessionPhase::Failed {
                Color::Red
            } else {
                Color::Cyan
            }),
        )
        .label(format!(
            "{}%  root turns: {}",
            progress, snapshot.root_turn_count
        ))
        .percent(progress);
    frame.render_widget(gauge, body[0]);

    let agents = snapshot.agents.iter().map(|agent| {
        ListItem::new(format!(
            "{}  generation={}  {}",
            agent.id,
            agent.generation,
            if agent.completed {
                "completed"
            } else {
                "active"
            }
        ))
    });
    let agent_list = List::new(agents)
        .block(Block::default().borders(Borders::ALL).title("Agents"))
        .highlight_style(Style::default().fg(Color::Yellow));
    frame.render_widget(agent_list, body[1]);

    let help = Paragraph::new(vec![
        Line::from("s start  r run goal  c complete root  g wait child  a child done"),
        Line::from("x stop  q/esc quit"),
    ])
    .wrap(Wrap { trim: true })
    .block(Block::default().borders(Borders::ALL).title("Controls"));
    frame.render_widget(help, chunks[2]);
}

fn phase_label(phase: SessionPhase) -> &'static str {
    match phase {
        SessionPhase::Created => "created",
        SessionPhase::Launching => "launching",
        SessionPhase::Initializing => "initializing",
        SessionPhase::Ready => "ready",
        SessionPhase::Running => "running",
        SessionPhase::GatePending => "gate-pending",
        SessionPhase::Completed => "completed",
        SessionPhase::Stopping => "stopping",
        SessionPhase::ClosingTransport => "closing",
        SessionPhase::Stopped => "stopped",
        SessionPhase::Disconnected => "disconnected",
        SessionPhase::Failed => "failed",
    }
}

struct TerminalGuard {
    terminal: Terminal<CrosstermBackend<Stdout>>,
}

impl TerminalGuard {
    fn enter() -> Result<Self, UiError> {
        enable_raw_mode()?;
        let mut output = stdout();
        execute!(output, EnterAlternateScreen)?;
        let backend = CrosstermBackend::new(output);
        Ok(Self {
            terminal: Terminal::new(backend)?,
        })
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(self.terminal.backend_mut(), LeaveAlternateScreen);
        let _ = self.terminal.show_cursor();
    }
}

#[cfg(test)]
mod tests {
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    use super::draw;
    use crate::state::CoreSnapshot;

    #[test]
    fn renders_snapshot_on_a_small_test_backend() {
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| draw(frame, &CoreSnapshot::default()))
            .unwrap();
        assert!(terminal.backend().buffer().area.width == 80);
    }
}
