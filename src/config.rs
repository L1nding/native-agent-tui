use std::io::Read;
use std::path::PathBuf;

use crate::agents::{
    DEFAULT_MAX_NATIVE_CHILDREN, DEFAULT_MAX_NATIVE_DEPTH, DEFAULT_MAX_NATIVE_TURNS,
};
use crate::backend::BackendKind;
use crate::history::search::Category;
use crate::journal::JournalSettings;
use crate::observation::{AttentionClass, AttentionSettings, ConfigSource};
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum McpServerConfig {
    Stdio {
        name: String,
        command: String,
        args: Vec<String>,
        env: Vec<(String, String)>,
    },
    Http {
        name: String,
        url: String,
        headers: Vec<(String, String)>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    pub backend: BackendKind,
    pub cwd: PathBuf,
    pub executable: PathBuf,
    pub dsh_executable: PathBuf,
    pub acp_profile: String,
    pub mcp_servers: Vec<McpServerConfig>,
    pub model: Option<String>,
    /// 会话汇总规则：工作流与 `--run` 要求所有根任务成功（严格）；
    /// 交互 TUI 以最后一轮结果为准，用户中断过的早期轮次不会让会话记为失败。
    pub strict_outcome: bool,
    pub sandbox: String,
    pub approval_policy: String,
    pub windows_sandbox: Option<String>,
    pub max_native_children: usize,
    pub max_native_depth: usize,
    pub max_native_turns: usize,
    pub max_total_tokens: Option<u64>,
    pub max_agent_tokens: Option<u64>,
    pub attention: AttentionSettings,
    pub journal: JournalSettings,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            backend: BackendKind::Codex,
            cwd: PathBuf::from("."),
            executable: std::env::var_os("CODEX_BIN")
                .map(PathBuf::from)
                .unwrap_or_else(|| {
                    PathBuf::from(if cfg!(windows) { "codex.cmd" } else { "codex" })
                }),
            dsh_executable: std::env::var_os("DSH_BIN")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from(if cfg!(windows) { "dsh.cmd" } else { "dsh" })),
            acp_profile: "acp".into(),
            mcp_servers: Vec::new(),
            model: None,
            strict_outcome: false,
            sandbox: "workspace-write".into(),
            approval_policy: "on-request".into(),
            windows_sandbox: None,
            max_native_children: DEFAULT_MAX_NATIVE_CHILDREN,
            max_native_depth: DEFAULT_MAX_NATIVE_DEPTH,
            max_native_turns: DEFAULT_MAX_NATIVE_TURNS,
            max_total_tokens: None,
            max_agent_tokens: None,
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
    Recovery {
        session: String,
        config: Config,
    },
    Search {
        query: String,
        category: Category,
        thread: String,
        turn: String,
        config: Config,
    },
    History {
        session: Option<String>,
        config: Config,
    },
    Export {
        session: String,
        since: u64,
        output: Option<PathBuf>,
        config: Config,
    },
    Replay {
        session: String,
        since: u64,
        json_events: bool,
        config: Config,
    },
    Workflow {
        path: PathBuf,
        headless: bool,
        json_events: bool,
        config: Config,
    },
    Run {
        goal: String,
        json_events: bool,
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
    #[error("choose one execution or history mode; --headless requires --workflow")]
    ConflictingModes,
    #[error("invalid value for {option}: {value}")]
    InvalidValue { option: String, value: String },
    #[error("{0}")]
    Attention(String),
    #[error("--since requires --replay or --export SESSION_ID")]
    ReplayOptions,
    #[error("--output requires --export SESSION_ID")]
    ExportOptions,
    #[error(
        "--json-events requires --run TASK, --workflow FILE --headless, or --replay SESSION_ID"
    )]
    JsonEventsOptions,
    #[error("search filters require --search QUERY")]
    SearchOptions,
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
    let mut output = None;
    let mut search_category = None;
    let mut search_thread = None;
    let mut search_turn = None;
    while index < args.len() {
        let option = &args[index];
        index += 1;
        match option.as_str() {
            "--help" | "-h" if args.len() == 1 => return Ok(CliCommand::Help),
            "--version" | "-V" if args.len() == 1 => return Ok(CliCommand::Version),
            "--run" | "--tui" | "--check-shell" | "--workflow" | "--sessions" | "--replay"
            | "--history" | "--export" | "--search" | "--recovery" => {
                if mode.replace(option.as_str()).is_some() {
                    return Err(CliError::ConflictingModes);
                }
                if option == "--run"
                    || option == "--replay"
                    || option == "--workflow"
                    || option == "--export"
                    || option == "--search"
                    || option == "--recovery"
                    || (matches!(option.as_str(), "--tui" | "--history")
                        && args.get(index).is_some_and(|next| !next.starts_with('-')))
                {
                    goal = Some(value(&args, &mut index, option)?);
                }
            }
            "--headless" if !headless => headless = true,
            "--cwd" => config.cwd = value(&args, &mut index, option)?.into(),
            "--codex" => config.executable = value(&args, &mut index, option)?.into(),
            "--backend" => {
                let raw = value(&args, &mut index, option)?;
                config.backend =
                    raw.parse::<BackendKind>()
                        .map_err(|expected| CliError::InvalidValue {
                            option: option.clone(),
                            value: expected.into(),
                        })?;
            }
            "--dsh" => config.dsh_executable = value(&args, &mut index, option)?.into(),
            "--profile" => config.acp_profile = value(&args, &mut index, option)?,
            "--model" => config.model = Some(value(&args, &mut index, option)?),
            "--journal-dir" => config.journal.root = Some(value(&args, &mut index, option)?.into()),
            "--max-native-children" => {
                config.max_native_children = bounded_usize(&args, &mut index, option, 1, 64)?;
            }
            "--max-native-depth" => {
                config.max_native_depth = bounded_usize(&args, &mut index, option, 1, 8)?;
            }
            "--max-native-turns" => {
                config.max_native_turns = bounded_usize(&args, &mut index, option, 1, 64)?;
            }
            "--max-total-tokens" if config.max_total_tokens.is_none() => {
                config.max_total_tokens = Some(positive_u64(&args, &mut index, option)?);
            }
            "--max-agent-tokens" if config.max_agent_tokens.is_none() => {
                config.max_agent_tokens = Some(positive_u64(&args, &mut index, option)?);
            }
            "--json-events" if !json_events => json_events = true,
            "--search-category" if search_category.is_none() => {
                let raw = value(&args, &mut index, option)?;
                search_category = Some(Category::from_label(&raw).ok_or_else(|| {
                    CliError::InvalidValue {
                        option: option.clone(),
                        value:
                            "expected all, lifecycle, output, tool, compaction, request, or waiting"
                                .into(),
                    }
                })?);
            }
            "--search-thread" if search_thread.is_none() => {
                search_thread = Some(value(&args, &mut index, option)?);
            }
            "--search-turn" if search_turn.is_none() => {
                search_turn = Some(value(&args, &mut index, option)?);
            }
            "--output" if output.is_none() => {
                output = Some(PathBuf::from(value(&args, &mut index, option)?))
            }
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
    if since.is_some() && !matches!(mode, Some("--replay" | "--export")) {
        return Err(CliError::ReplayOptions);
    }
    if output.is_some() && mode != Some("--export") {
        return Err(CliError::ExportOptions);
    }
    if json_events
        && !(matches!(mode, Some("--replay" | "--run")) || mode == Some("--workflow") && headless)
    {
        return Err(CliError::JsonEventsOptions);
    }
    if (search_category.is_some() || search_thread.is_some() || search_turn.is_some())
        && mode != Some("--search")
    {
        return Err(CliError::SearchOptions);
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
        Some("--recovery") => CliCommand::Recovery {
            session: goal.unwrap(),
            config,
        },
        Some("--search") => CliCommand::Search {
            query: goal.unwrap(),
            category: search_category.unwrap_or_default(),
            thread: search_thread.unwrap_or_default(),
            turn: search_turn.unwrap_or_default(),
            config,
        },
        Some("--history") => CliCommand::History {
            session: goal,
            config,
        },
        Some("--export") => CliCommand::Export {
            session: goal.unwrap(),
            since: since.unwrap_or(0),
            output,
            config,
        },
        Some("--replay") => CliCommand::Replay {
            session: goal.unwrap(),
            since: since.unwrap_or(0),
            json_events,
            config,
        },
        Some("--workflow") => CliCommand::Workflow {
            path: goal.unwrap().into(),
            headless,
            json_events,
            config,
        },
        Some("--run") => CliCommand::Run {
            goal: goal.unwrap(),
            json_events,
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

fn bounded_usize(
    args: &[String],
    index: &mut usize,
    option: &str,
    min: usize,
    max: usize,
) -> Result<usize, CliError> {
    let raw = value(args, index, option)?;
    let parsed = raw.parse::<usize>().map_err(|_| CliError::InvalidValue {
        option: option.into(),
        value: format!("expected an integer from {min} to {max}"),
    })?;
    if !(min..=max).contains(&parsed) {
        return Err(CliError::InvalidValue {
            option: option.into(),
            value: format!("expected an integer from {min} to {max}"),
        });
    }
    Ok(parsed)
}

fn positive_u64(args: &[String], index: &mut usize, option: &str) -> Result<u64, CliError> {
    let raw = value(args, index, option)?;
    let parsed = raw.parse::<u64>().map_err(|_| CliError::InvalidValue {
        option: option.into(),
        value: "expected a positive integer".into(),
    })?;
    if parsed == 0 {
        return Err(CliError::InvalidValue {
            option: option.into(),
            value: "expected a positive integer".into(),
        });
    }
    Ok(parsed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn history_and_export_modes_cannot_launch_execution_or_mix_input_channels() {
        assert!(matches!(
            parse_args(["--history"]).unwrap(),
            CliCommand::History { session: None, .. }
        ));
        assert!(
            matches!(parse_args(["--history", "session-1", "--codex", "missing"]).unwrap(), CliCommand::History { session: Some(session), .. } if session == "session-1")
        );
        let CliCommand::Export {
            session,
            since,
            output,
            ..
        } = parse_args([
            "--output",
            "中文 export.jsonl",
            "--export",
            "session-1",
            "--since",
            "2",
        ])
        .unwrap()
        else {
            panic!()
        };
        assert_eq!(session, "session-1");
        assert_eq!(since, 2);
        assert_eq!(output, Some(PathBuf::from("中文 export.jsonl")));
        for args in [
            vec!["--export"],
            vec!["--history", "--run", "task"],
            vec!["--history", "--since", "1"],
            vec!["--export", "s", "--json-events"],
            vec!["--export", "s", "--headless"],
            vec!["--run", "task", "--output", "out"],
            vec!["--output", "out"],
            vec!["--export", "s", "--output", "one", "--output", "two"],
            vec!["--search"],
            vec!["--search", "waiting", "--run", "task"],
            vec!["--search", "waiting", "--since", "1"],
            vec!["--search", "waiting", "--json-events"],
        ] {
            assert!(parse_args(args).is_err());
        }
    }

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
            parse_args(["--search", "waiting"]).unwrap(),
            CliCommand::Search { query, .. } if query == "waiting"
        ));
        let CliCommand::Search {
            query,
            category,
            thread,
            turn,
            ..
        } = parse_args([
            "--search",
            "compaction",
            "--search-category",
            "compaction",
            "--search-thread",
            "root-thread",
            "--search-turn",
            "turn-42",
        ])
        .unwrap()
        else {
            panic!()
        };
        assert_eq!(query, "compaction");
        assert_eq!(category, Category::Compaction);
        assert_eq!(thread, "root-thread");
        assert_eq!(turn, "turn-42");
        assert!(matches!(
            parse_args(["--search", "x", "--search-category", "unknown"]),
            Err(CliError::InvalidValue { .. })
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
        assert!(matches!(
            parse_args(["--recovery", "s", "--journal-dir", "history"]),
            Ok(CliCommand::Recovery { session, config })
                if session == "s" && config.journal.root == Some(PathBuf::from("history"))
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
            vec!["--run", "task", "--since", "0"],
            vec!["--sessions", "--since", "0"],
        ] {
            assert_eq!(parse_args(args), Err(CliError::ReplayOptions));
        }
        for args in [
            vec!["--json-events"],
            vec!["--tui", "--json-events"],
            vec!["--workflow", "plan.json", "--json-events"],
            vec!["--check-shell", "--json-events"],
        ] {
            assert_eq!(parse_args(args), Err(CliError::JsonEventsOptions));
        }
        assert!(matches!(
            parse_args(["--run", "task", "--json-events"]).unwrap(),
            CliCommand::Run {
                json_events: true,
                ..
            }
        ));
        assert!(matches!(
            parse_args(["--workflow", "plan.json", "--headless", "--json-events"]).unwrap(),
            CliCommand::Workflow {
                json_events: true,
                headless: true,
                ..
            }
        ));
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
        let CliCommand::Run { goal, config, .. } = parse_args([
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
    fn parses_and_bounds_native_agent_limits() {
        let CliCommand::Run { config, .. } = parse_args([
            "--run",
            "任务",
            "--max-native-children",
            "3",
            "--max-native-depth",
            "1",
            "--max-native-turns",
            "2",
            "--max-total-tokens",
            "1000",
        ])
        .unwrap() else {
            panic!()
        };
        assert_eq!(config.max_native_children, 3);
        assert_eq!(config.max_native_depth, 1);
        assert_eq!(config.max_native_turns, 2);
        assert_eq!(config.max_total_tokens, Some(1000));
        assert_eq!(config.max_agent_tokens, None);
        for args in [
            vec!["--run", "task", "--max-native-children", "0"],
            vec!["--run", "task", "--max-native-children", "65"],
            vec!["--run", "task", "--max-native-depth", "0"],
            vec!["--run", "task", "--max-native-depth", "9"],
            vec!["--run", "task", "--max-native-turns", "0"],
            vec!["--run", "task", "--max-native-turns", "65"],
            vec!["--run", "task", "--max-native-depth", "bad"],
            vec!["--run", "task", "--max-total-tokens", "0"],
            vec!["--run", "task", "--max-total-tokens", "bad"],
            vec!["--run", "task", "--max-agent-tokens", "0"],
            vec!["--run", "task", "--max-agent-tokens", "bad"],
        ] {
            assert!(matches!(
                parse_args(args),
                Err(CliError::InvalidValue { .. })
            ));
        }
    }

    #[test]
    fn parses_positive_per_agent_token_budget() {
        let CliCommand::Run { config, .. } =
            parse_args(["--run", "task", "--max-agent-tokens", "42"]).unwrap()
        else {
            panic!()
        };
        assert_eq!(config.max_agent_tokens, Some(42));
        for value in ["0", "-1", "bad", "18446744073709551616"] {
            assert!(matches!(
                parse_args(["--run", "task", "--max-agent-tokens", value]),
                Err(CliError::InvalidValue { .. })
            ));
        }
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
    fn selects_deepseek_acp_without_changing_codex_defaults() {
        let CliCommand::Run { config, .. } = parse_args([
            "--backend",
            "deepseek-acp",
            "--dsh",
            "custom-dsh",
            "--profile",
            "acp-test",
            "--run",
            "hello",
        ])
        .unwrap() else {
            panic!()
        };
        assert_eq!(config.backend, BackendKind::DeepSeekAcp);
        assert_eq!(config.dsh_executable, PathBuf::from("custom-dsh"));
        assert_eq!(config.acp_profile, "acp-test");
        assert_eq!(Config::default().backend, BackendKind::Codex);
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
