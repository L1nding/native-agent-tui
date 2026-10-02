use std::path::PathBuf;

use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    pub cwd: PathBuf,
    pub executable: PathBuf,
    pub model: Option<String>,
    pub sandbox: String,
    pub approval_policy: String,
    pub windows_sandbox: Option<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            cwd: PathBuf::from("."),
            executable: std::env::var_os("CODEX_BIN")
                .map(PathBuf::from)
                .unwrap_or_else(|| {
                    PathBuf::from(if cfg!(windows) { "codex.cmd" } else { "codex" })
                }),
            model: None,
            sandbox: "workspace-write".into(),
            approval_policy: "on-request".into(),
            windows_sandbox: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CliCommand {
    Help,
    Version,
    CheckShell(Config),
    Run {
        goal: String,
        config: Config,
    },
    Tui {
        goal: Option<String>,
        config: Config,
    },
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum CliError {
    #[error("unknown option: {0}")]
    UnknownOption(String),
    #[error("{0} requires a value")]
    MissingValue(String),
    #[error("choose one of --run, --tui, or --check-shell")]
    ConflictingModes,
    #[error("invalid value for {option}: {value}")]
    InvalidValue { option: String, value: String },
}

pub fn parse_args<I, S>(args: I) -> Result<CliCommand, CliError>
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
{
    let args: Vec<String> = args.into_iter().map(Into::into).collect();
    let mut config = Config::default();
    let mut mode = None;
    let mut goal = None;
    let mut index = 0;
    while index < args.len() {
        let option = &args[index];
        index += 1;
        match option.as_str() {
            "--help" | "-h" if args.len() == 1 => return Ok(CliCommand::Help),
            "--version" | "-V" if args.len() == 1 => return Ok(CliCommand::Version),
            "--run" | "--tui" | "--check-shell" => {
                if mode.replace(option.as_str()).is_some() {
                    return Err(CliError::ConflictingModes);
                }
                if option == "--run"
                    || (option == "--tui"
                        && args.get(index).is_some_and(|next| !next.starts_with('-')))
                {
                    goal = Some(value(&args, &mut index, option)?);
                }
            }
            "--cwd" => config.cwd = value(&args, &mut index, option)?.into(),
            "--codex" => config.executable = value(&args, &mut index, option)?.into(),
            "--model" => config.model = Some(value(&args, &mut index, option)?),
            "--windows-sandbox" => {
                let val = value(&args, &mut index, option)?;
                if !cfg!(windows) || !["elevated", "unelevated"].contains(&val.as_str()) {
                    return Err(CliError::InvalidValue {
                        option: option.clone(),
                        value: val,
                    });
                }
                config.windows_sandbox = Some(val);
            }
            "--sandbox" | "--approval" => {
                let val = value(&args, &mut index, option)?;
                let allowed = if option == "--sandbox" {
                    ["read-only", "workspace-write", "danger-full-access"]
                } else {
                    ["untrusted", "on-request", "never"]
                };
                if !allowed.contains(&val.as_str()) {
                    return Err(CliError::InvalidValue {
                        option: option.clone(),
                        value: val,
                    });
                }
                if option == "--sandbox" {
                    config.sandbox = val;
                } else {
                    config.approval_policy = val;
                }
            }
            _ => return Err(CliError::UnknownOption(option.clone())),
        }
    }
    Ok(match mode {
        Some("--run") => CliCommand::Run {
            goal: goal.unwrap(),
            config,
        },
        Some("--check-shell") => CliCommand::CheckShell(config),
        _ => CliCommand::Tui { goal, config },
    })
}

fn value(args: &[String], index: &mut usize, option: &str) -> Result<String, CliError> {
    let val = args
        .get(*index)
        .filter(|arg| !arg.trim().is_empty() && !arg.starts_with("--"))
        .ok_or_else(|| CliError::MissingValue(option.into()))?;
    *index += 1;
    Ok(val.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_settings_and_multilingual_goal() {
        let CliCommand::Run { goal, config } = parse_args([
            "--cwd",
            "a b",
            "--run",
            "检查项目",
            "--sandbox",
            "read-only",
            "--model",
            "test-model",
        ])
        .unwrap() else {
            panic!()
        };
        assert_eq!(goal, "检查项目");
        assert_eq!(config.cwd, PathBuf::from("a b"));
        assert_eq!(config.sandbox, "read-only");
        assert_eq!(config.model.as_deref(), Some("test-model"));
    }

    #[test]
    fn rejects_extra_options_missing_values_and_conflicting_modes() {
        for args in [
            vec!["--run"],
            vec!["--run", "--help"],
            vec!["--run", "hi", "--bogus"],
            vec!["--tui", "--check-shell"],
            vec!["--sandbox", "invalid"],
            vec!["--windows-sandbox", "disabled"],
        ] {
            assert!(parse_args(args).is_err());
        }
    }

    #[test]
    fn default_is_interactive() {
        assert!(matches!(
            parse_args(Vec::<String>::new()).unwrap(),
            CliCommand::Tui { goal: None, .. }
        ));
    }

    #[cfg(windows)]
    #[test]
    fn windows_sandbox_override_is_explicit_and_validated() {
        let CliCommand::CheckShell(config) =
            parse_args(["--check-shell", "--windows-sandbox", "unelevated"]).unwrap()
        else {
            panic!()
        };
        assert_eq!(config.windows_sandbox.as_deref(), Some("unelevated"));
        assert_eq!(Config::default().windows_sandbox, None);
    }
}
