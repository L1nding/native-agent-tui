use std::path::PathBuf;

use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    pub cwd: Option<PathBuf>,
    pub check_shell: bool,
    pub goal: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CliCommand {
    Help,
    Version,
    CheckShell,
    Run { goal: String },
    Tui { goal: Option<String> },
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum CliError {
    #[error("unknown option: {0}")]
    UnknownOption(String),
    #[error("--run requires a non-empty goal")]
    MissingGoal,
}

pub fn parse_args<I, S>(args: I) -> Result<CliCommand, CliError>
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
{
    let args: Vec<String> = args.into_iter().map(Into::into).collect();
    if args.is_empty() {
        return Ok(CliCommand::Tui { goal: None });
    }
    if args.iter().any(|arg| arg == "--help" || arg == "-h") {
        return Ok(CliCommand::Help);
    }
    if args.iter().any(|arg| arg == "--version" || arg == "-V") {
        return Ok(CliCommand::Version);
    }
    if args.iter().any(|arg| arg == "--check-shell") {
        return Ok(CliCommand::CheckShell);
    }
    if args.iter().any(|arg| arg == "--tui") {
        let goal = args
            .iter()
            .position(|arg| arg == "--tui")
            .and_then(|index| args.get(index + 1))
            .filter(|value| !value.starts_with('-'))
            .cloned();
        return Ok(CliCommand::Tui { goal });
    }
    if let Some(index) = args.iter().position(|arg| arg == "--run") {
        let goal = args.get(index + 1).cloned().unwrap_or_default();
        if goal.trim().is_empty() {
            return Err(CliError::MissingGoal);
        }
        return Ok(CliCommand::Run { goal });
    }
    Err(CliError::UnknownOption(args.join(" ")))
}

#[cfg(test)]
mod tests {
    use super::{parse_args, CliCommand, CliError};

    #[test]
    fn no_arguments_open_the_tui() {
        assert_eq!(
            parse_args(std::iter::empty::<String>()).unwrap(),
            CliCommand::Tui { goal: None }
        );
    }

    #[test]
    fn parses_run_goal() {
        assert_eq!(
            parse_args(["--run", "inspect repository"]).unwrap(),
            CliCommand::Run {
                goal: "inspect repository".into()
            }
        );
    }

    #[test]
    fn parses_optional_tui_goal() {
        assert_eq!(
            parse_args(["--tui", "inspect repository"]).unwrap(),
            CliCommand::Tui {
                goal: Some("inspect repository".into())
            }
        );
    }

    #[test]
    fn rejects_missing_goal() {
        assert_eq!(parse_args(["--run"]), Err(CliError::MissingGoal));
    }
}
