use std::io::Read;
use std::path::PathBuf;

use crate::journal::JournalSettings;
use crate::observation::{AttentionClass, AttentionSettings, ConfigSource};
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    pub cwd: PathBuf,
    pub executable: PathBuf,
    pub model: Option<String>,
    pub sandbox: String,
    pub approval_policy: String,
    pub windows_sandbox: Option<String>,
    pub attention: AttentionSettings,
    pub journal: JournalSettings,
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
            attention: AttentionSettings::default(),
            journal: JournalSettings::default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CliCommand {
    Help,
    Version,
    CheckShell(Config),
    Sessions(Config),
    Replay {
        session: String,
        since: u64,
        json_events: bool,
        config: Config,
    },
    Workflow {
        path: PathBuf,
        headless: bool,
        config: Config,
    },
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
    #[error("choose one execution, --sessions, or --replay mode; --headless requires --workflow")]
    ConflictingModes,
    #[error("invalid value for {option}: {value}")]
    InvalidValue { option: String, value: String },
    #[error("{0}")]
    Attention(String),
    #[error("--since and --json-events currently require --replay SESSION_ID")]
    ReplayOptions,
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
    let mut headless = false;
    let mut index = 0;
    let mut attention_file = None;
    let mut attention_overrides = Vec::new();
    let mut since = None;
    let mut json_events = false;
    while index < args.len() {
        let option = &args[index];
        index += 1;
        match option.as_str() {
            "--help" | "-h" if args.len() == 1 => return Ok(CliCommand::Help),
            "--version" | "-V" if args.len() == 1 => return Ok(CliCommand::Version),
            "--run" | "--tui" | "--check-shell" | "--workflow" | "--sessions" | "--replay" => {
                if mode.replace(option.as_str()).is_some() {
                    return Err(CliError::ConflictingModes);
                }
                if option == "--run"
                    || option == "--replay"
                    || option == "--workflow"
                    || (option == "--tui"
                        && args.get(index).is_some_and(|next| !next.starts_with('-')))
                {
                    goal = Some(value(&args, &mut index, option)?);
                }
            }
            "--headless" if !headless => headless = true,
            "--cwd" => config.cwd = value(&args, &mut index, option)?.into(),
            "--codex" => config.executable = value(&args, &mut index, option)?.into(),
            "--model" => config.model = Some(value(&args, &mut index, option)?),
            "--journal-dir" => config.journal.root = Some(value(&args, &mut index, option)?.into()),
            "--json-events" if !json_events => json_events = true,
            "--since" if since.is_none() => {
                let val = value(&args, &mut index, option)?;
                since = Some(val.parse::<u64>().map_err(|_| CliError::InvalidValue {
                    option: option.clone(),
                    value: "expected an unsigned event sequence".into(),
                })?);
            }
            "--attention-config" => attention_file = Some(value(&args, &mut index, option)?),
            "--attention-model"
            | "--attention-tool"
            | "--attention-children"
            | "--attention-transport" => {
                let pair = value(&args, &mut index, option)?;
                let class = match option.as_str() {
                    "--attention-model" => AttentionClass::Model,
                    "--attention-tool" => AttentionClass::Tool,
                    "--attention-children" => AttentionClass::Children,
                    _ => AttentionClass::Transport,
                };
                let parsed = pair.split_once(',').and_then(|(quiet, attention)| {
                    Some((quiet.parse::<u64>().ok()?, attention.parse::<u64>().ok()?))
                });
                let (quiet, attention) = parsed.ok_or_else(|| CliError::InvalidValue {
                    option: option.clone(),
                    value: "expected QUIET_MS,ATTENTION_MS".into(),
                })?;
                attention_overrides.push((class, quiet, attention));
            }
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
    if headless && mode != Some("--workflow") {
        return Err(CliError::ConflictingModes);
    }
    if (since.is_some() || json_events) && mode != Some("--replay") {
        return Err(CliError::ReplayOptions);
    }
    if let Some(path) = attention_file {
        let mut bytes = Vec::new();
        std::fs::File::open(path)
            .and_then(|file| file.take(8193).read_to_end(&mut bytes))
            .map_err(|_| CliError::Attention("Cannot read attention configuration.".into()))?;
        config
            .attention
            .apply_json(&bytes, ConfigSource::Global)
            .map_err(CliError::Attention)?;
    }
    for (class, quiet, attention) in attention_overrides {
        config
            .attention
            .set(class, quiet, attention, ConfigSource::Cli)
            .map_err(|error| CliError::Attention(error.to_string()))?;
    }
    Ok(match mode {
        Some("--sessions") => CliCommand::Sessions(config),
        Some("--replay") => CliCommand::Replay {
            session: goal.unwrap(),
            since: since.unwrap_or(0),
            json_events,
            config,
        },
        Some("--workflow") => CliCommand::Workflow {
            path: goal.unwrap().into(),
            headless,
            config,
        },
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
    fn replay_and_sessions_are_explicit_read_only_modes_with_a_journal_override() {
        let CliCommand::Replay {
            session,
            since,
            json_events,
            config,
        } = parse_args([
            "--since",
            "0",
            "--replay",
            "session-42",
            "--json-events",
            "--journal-dir",
            "local history",
            "--codex",
            "never-execute",
        ])
        .unwrap()
        else {
            panic!()
        };
        assert_eq!(session, "session-42");
        assert_eq!(since, 0);
        assert!(json_events);
        assert_eq!(config.journal.root, Some(PathBuf::from("local history")));
        assert_eq!(config.executable, PathBuf::from("never-execute"));
        assert!(matches!(
            parse_args(["--sessions"]).unwrap(),
            CliCommand::Sessions(_)
        ));
        assert!(matches!(
            parse_args(["--replay", "s"]).unwrap(),
            CliCommand::Replay {
                since: 0,
                json_events: false,
                ..
            }
        ));
        assert!(matches!(
            parse_args(["--replay", "s", "--since", "18446744073709551615"]).unwrap(),
            CliCommand::Replay {
                since: u64::MAX,
                ..
            }
        ));
    }

    #[test]
    fn replay_options_reject_bad_sequences_and_execution_mode_conflicts() {
        for sequence in ["-1", "18446744073709551616", "1.0", "x"] {
            assert!(matches!(
                parse_args(["--replay", "s", "--since", sequence]),
                Err(CliError::InvalidValue { .. })
            ));
        }
        for args in [
            vec!["--replay"],
            vec!["--replay", "s", "--since"],
            vec!["--replay", "s", "--since", "0", "--since", "1"],
            vec!["--replay", "s", "--json-events", "--json-events"],
            vec!["--replay", "s", "--run", "task"],
            vec!["--replay", "s", "--workflow", "plan.json"],
            vec!["--replay", "s", "--check-shell"],
            vec!["--replay", "s", "--headless"],
            vec!["--sessions", "--tui"],
            vec!["--journal-dir"],
        ] {
            assert!(parse_args(args).is_err());
        }
        for args in [
            vec!["--run", "task", "--json-events"],
            vec!["--sessions", "--since", "0"],
            vec!["--json-events"],
        ] {
            assert_eq!(parse_args(args), Err(CliError::ReplayOptions));
        }
    }

    #[test]
    fn cli_attention_overrides_file_values_regardless_of_argument_order() {
        let path = std::env::temp_dir().join(format!(
            "native-agent-attention-{}.json",
            std::process::id()
        ));
        std::fs::write(&path, br#"{"model":{"quiet_ms":100,"attention_ms":200},"tool":{"quiet_ms":300,"attention_ms":400}}"#).unwrap();
        for args in [
            vec![
                "--attention-model".to_owned(),
                "5,10".into(),
                "--attention-config".into(),
                path.to_string_lossy().into_owned(),
            ],
            vec![
                "--attention-config".to_owned(),
                path.to_string_lossy().into_owned(),
                "--attention-model".into(),
                "5,10".into(),
            ],
        ] {
            let CliCommand::Tui { config, .. } = parse_args(args).unwrap() else {
                panic!()
            };
            assert_eq!(config.attention.model.quiet_ms, 5);
            assert_eq!(config.attention.model.source, ConfigSource::Cli);
            assert_eq!(config.attention.tool.source, ConfigSource::Global);
            assert_eq!(config.attention.children.source, ConfigSource::Default);
        }
        std::fs::remove_file(path).unwrap();
        for value in [
            "0,1",
            "1,1",
            "2,1",
            "1,604800001",
            "a,1",
            "1",
            "1,2,3",
            "-1,1",
        ] {
            assert!(parse_args(["--attention-model", value]).is_err());
        }
    }

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

    #[test]
    fn workflow_modes_are_explicit_and_cannot_mix_with_single_task_modes() {
        assert!(matches!(
            parse_args(["--workflow", "plan.json"]).unwrap(),
            CliCommand::Workflow {
                headless: false,
                ..
            }
        ));
        assert!(matches!(
            parse_args(["--headless", "--workflow", "plan.json"]).unwrap(),
            CliCommand::Workflow { headless: true, .. }
        ));
        for args in [
            vec!["--headless"],
            vec!["--workflow"],
            vec!["--workflow", "plan.json", "--run", "a"],
            vec!["--workflow", "plan.json", "--tui"],
            vec!["--headless", "--headless", "--workflow", "plan.json"],
        ] {
            assert!(parse_args(args).is_err());
        }
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
