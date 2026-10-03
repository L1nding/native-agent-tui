//! Real pinned app-server requests driven by a deterministic local Responses provider.
use std::io::{Read, Write};
use std::path::Path;
use std::process::Stdio;

use tokio::io::{AsyncBufReadExt, BufReader};

use super::*;
use crate::interactions::RequestKind;
use crate::journal::{tests::Fixture, JournalSettings, Replay};

struct LiveRequests {
    client: ClientHandle,
    provider: tokio::process::Child,
    port: u16,
    config: Config,
    _fixture: Fixture,
}

fn stats(port: u16) -> Result<Value, String> {
    let mut stream =
        std::net::TcpStream::connect(("127.0.0.1", port)).map_err(|e| e.to_string())?;
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .map_err(|e| e.to_string())?;
    stream
        .set_write_timeout(Some(Duration::from_secs(2)))
        .map_err(|e| e.to_string())?;
    write!(stream, "GET /stats HTTP/1.0\r\nHost: localhost\r\n\r\n").map_err(|e| e.to_string())?;
    let mut response = String::new();
    stream
        .take(4096)
        .read_to_string(&mut response)
        .map_err(|e| e.to_string())?;
    serde_json::from_str(
        response
            .split_once("\r\n\r\n")
            .ok_or("invalid provider stats")?
            .1,
    )
    .map_err(|e| e.to_string())
}

impl LiveRequests {
    async fn launch(scenario: &str) -> Result<Self, Box<dyn std::error::Error>> {
        let fixture = Fixture::new();
        let home = fixture.root.join("home");
        let work = fixture.root.join("work");
        std::fs::create_dir(&home)?;
        std::fs::create_dir(&work)?;
        std::fs::write(work.join("file-accepted.txt"), "BASE\n")?;
        let executable = Config::default().executable;
        let query = |args: &'static [&'static str]| {
            let mut command = tokio::process::Command::new(&executable);
            command
                .env("CODEX_HOME", &home)
                .env_remove("OPENAI_API_KEY")
                .env_remove("CODEX_API_KEY")
                .args(args)
                .creation_flags(0x08000000)
                .kill_on_drop(true);
            command
        };
        let version =
            tokio::time::timeout(Duration::from_secs(15), query(&["--version"]).output()).await??;
        crate::compatibility::verify_version(&version.stdout)?;
        if !version.status.success() {
            return Err("version query failed".into());
        }
        let bundled = tokio::time::timeout(
            Duration::from_secs(15),
            query(&["debug", "models", "--bundled"]).output(),
        )
        .await??;
        if !bundled.status.success() {
            return Err("bundled catalog unavailable".into());
        }
        let mut catalog: Value = serde_json::from_slice(&bundled.stdout)?;
        for model in catalog["models"]
            .as_array_mut()
            .ok_or("invalid bundled catalog")?
        {
            // The fixture implements ordinary Responses SSE, preserving the other catalog fields.
            model["use_responses_lite"] = json!(false);
        }
        let catalog_path = home.join("models.json");
        std::fs::write(&catalog_path, serde_json::to_vec(&catalog)?)?;
        let mut command = tokio::process::Command::new(
            std::env::var_os("NATIVE_AGENT_TUI_PYTHON").unwrap_or_else(|| "python".into()),
        );
        command
            .args(["-u"])
            .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/request_provider.py"))
            .args(["--scenario", scenario])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .creation_flags(0x08000000)
            .kill_on_drop(true);
        let mut provider = command.spawn()?;
        let mut line = String::new();
        tokio::time::timeout(
            Duration::from_secs(5),
            BufReader::new(provider.stdout.take().ok_or("missing provider stdout")?)
                .read_line(&mut line),
        )
        .await??;
        let port: u16 = line.trim().parse()?;
        std::fs::write(home.join("config.toml"), format!(
            "model = \"gpt-6.1-sol\"\nmodel_provider = \"request_fixture\"\nmodel_catalog_json = {}\nfeatures.default_mode_request_user_input = true\n[model_providers.request_fixture]\nname = \"Local request fixture\"\nbase_url = \"http://127.0.0.1:{port}/v1\"\nwire_api = \"responses\"\nrequires_openai_auth = false\n",
            serde_json::to_string(&catalog_path.to_string_lossy())?))?;
        // Use normal ClientHandle::spawn, including the production Windows peer preflight.
        // The wrapper confines CODEX_HOME to the fixture and does not change the parent environment.
        let executable = executable.to_string_lossy();
        if executable
            .chars()
            .any(|c| matches!(c, '"' | '%' | '\r' | '\n'))
        {
            return Err("unsafe fixture executable path".into());
        }
        let wrapper = fixture.root.join("codex-fixture.cmd");
        std::fs::write(&wrapper, format!("@echo off\r\nset \"CODEX_HOME={}\"\r\nset \"OPENAI_API_KEY=\"\r\nset \"CODEX_API_KEY=\"\r\ncall \"{executable}\" %*\r\n", home.display()))?;
        let config = Config {
            executable: wrapper,
            cwd: work,
            journal: JournalSettings {
                root: Some(fixture.root.join("journal")),
                ..Default::default()
            },
            windows_sandbox: Some("unelevated".into()),
            sandbox: "read-only".into(),
            approval_policy: "untrusted".into(),
            model: Some("gpt-6.1-sol".into()),
            ..Default::default()
        };
        let client = ClientHandle::spawn(config.clone()).await?;
        Ok(Self {
            client,
            provider,
            port,
            config,
            _fixture: fixture,
        })
    }

    async fn request(
        &mut self,
        previous: Option<&crate::interactions::RequestRef>,
    ) -> Result<RequestView, String> {
        let outcome = tokio::time::timeout(
            Duration::from_secs(20),
            self.client.snapshots.wait_for(|s| {
                s.requests.iter().any(|request| {
                    !request.responding
                        && previous.is_none_or(|reference| !request.matches(reference))
                }) || matches!(
                    s.phase,
                    SessionPhase::Completed
                        | SessionPhase::Unknown
                        | SessionPhase::Failed
                        | SessionPhase::Disconnected
                )
            }),
        )
        .await
        .map(|result| result.map(|snapshot| snapshot.clone()));
        let snapshot = outcome
            .map_err(|_| {
                let state = self.client.snapshots.borrow();
                format!(
                    "request missing; phase={:?}, pending={:?}, notice={:?}; provider stats: {:?}",
                    state.phase,
                    state
                        .requests
                        .iter()
                        .map(|r| (
                            r.received_seq,
                            r.responding,
                            r.allow_accept,
                            r.allow_decline,
                            r.allow_cancel
                        ))
                        .collect::<Vec<_>>(),
                    state.notice,
                    stats(self.port)
                )
            })?
            .map_err(|e| e.to_string())?;
        snapshot
            .requests
            .iter()
            .find(|request| {
                !request.responding && previous.is_none_or(|reference| !request.matches(reference))
            })
            .cloned()
            .ok_or_else(|| {
                format!(
                    "request missing at phase {:?}; provider stats: {:?}",
                    snapshot.phase,
                    stats(self.port)
                )
            })
    }

    async fn resolved(&mut self, request: &RequestView) -> Result<(), String> {
        tokio::time::timeout(
            Duration::from_secs(20),
            self.client.snapshots.wait_for(|s| {
                s.observation.activities.iter().any(|activity| {
                    activity.identity.thread_id.as_deref() == Some(&request.thread_id)
                        && activity.request_id.as_ref() == Some(&request.id)
                        && activity.interaction_state
                            == Some(crate::observation::InteractionState::Resolved)
                        && activity.recent_evidence.iter().any(|evidence| {
                            evidence.kind == crate::observation::EvidenceKind::RequestResolved
                        })
                })
            }),
        )
        .await
        .map_err(|_| "server request resolution evidence missing")?
        .map_err(|e| e.to_string())?;
        Ok(())
    }

    async fn terminal(&mut self, expected: SessionPhase) -> Result<(), String> {
        let snapshot = tokio::time::timeout(
            Duration::from_secs(20),
            self.client.snapshots.wait_for(|s| {
                matches!(
                    s.phase,
                    SessionPhase::Completed
                        | SessionPhase::Interrupted
                        | SessionPhase::Unknown
                        | SessionPhase::Failed
                        | SessionPhase::Disconnected
                )
            }),
        )
        .await
        .map_err(|_| "turn did not complete")?
        .map_err(|e| e.to_string())?
        .clone();
        if snapshot.phase != expected
            || !snapshot.requests.is_empty()
            || snapshot.root_start_requests != 1
        {
            return Err(format!(
                "unexpected final state: {:?}, pending={}, root_starts={}",
                snapshot.phase,
                snapshot.requests.len(),
                snapshot.root_start_requests
            ));
        }
        Ok(())
    }

    async fn finish(mut self, expected: SessionPhase) -> Result<(), String> {
        let session = self
            .client
            .snapshots
            .borrow()
            .observation
            .session_id
            .clone();
        let _ = self.client.commands.send(Command::Quit).await;
        let exit = match tokio::time::timeout(Duration::from_secs(15), &mut self.client.join).await
        {
            Ok(result) => result.map_err(|e| e.to_string()),
            Err(_) => {
                self.client.join.abort();
                let _ = self.client.join.await;
                Err("client cleanup timed out".into())
            }
        };
        self.provider.stdin.take();
        let provider_exit =
            tokio::time::timeout(Duration::from_secs(3), self.provider.wait()).await;
        if provider_exit.is_err() {
            let _ = self.provider.kill().await;
            let _ = self.provider.wait().await;
            return Err("provider cleanup timed out".into());
        }
        if !provider_exit.unwrap().map_err(|e| e.to_string())?.success() {
            return Err("provider failed".into());
        }
        let exit = exit?;
        if exit.cleanup_error.is_some() || exit.journal_error.is_some() {
            return Err("Core cleanup or journal failed".into());
        }
        let replay = Replay::open(&self.config.journal, &self.config.cwd, &session, 0)
            .map_err(|e| e.to_string())?;
        if !replay.info.session_closed
            || replay.latest_state().cleanup_confirmed != Some(true)
            || replay.info.execution_result != Some(expected)
        {
            return Err("durable completion and cleanup not confirmed".into());
        }
        let mut bytes = Vec::new();
        replay.write_jsonl(&mut bytes).map_err(|e| e.to_string())?;
        let text = String::from_utf8(bytes).map_err(|e| e.to_string())?;
        if [
            "SHELL_CANARY",
            "FILE_CANARY",
            "测试回答",
            "REQUESTS_DONE",
            "WriteAllText",
        ]
        .iter()
        .any(|private| text.contains(private))
        {
            return Err("request content leaked into journal".into());
        }
        Ok(())
    }
}

#[tokio::test]
#[ignore = "requires Codex 0.159.2 and Python; localhost provider and isolated fixture files"]
async fn live_codex_file_decisions_and_shell_approval_preserve_the_selected_sandbox() {
    let mut fixture = LiveRequests::launch("approvals").await.unwrap();
    let observed: Result<(), String> = async {
        fixture
            .client
            .commands
            .send(Command::SubmitRootInput {
                text: "Run the request fixture.".into(),
            })
            .await
            .map_err(|e| e.to_string())?;
        let mut previous = None;
        for step in 0..4 {
            let request = fixture.request(previous.as_ref()).await?;
            if request.received_seq == 0
                || request.details.item_id.is_none()
                || request.details.started_at_ms.is_none()
                || matches!(request.kind, RequestKind::UserInput { .. })
                || matches!(request.kind, RequestKind::FileApproval) != matches!(step, 0 | 3)
            {
                return Err(format!(
                    "unexpected request identity or type at step {step}"
                ));
            }
            if let RequestKind::FileApproval = request.kind {
                let preview = request
                    .details
                    .file_preview
                    .as_ref()
                    .ok_or("real file preview missing")?;
                if preview.unavailable || preview.truncated || !preview.text.contains("FILE_CANARY")
                {
                    return Err("invalid real file preview".into());
                }
            } else if !request
                .details
                .fields
                .iter()
                .any(|field| field.label == "Command cwd")
            {
                return Err("real command cwd missing".into());
            }
            tokio::time::sleep(Duration::from_millis(300)).await;
            if stats(fixture.port)?["requests"] != step + 1
                || fixture.config.cwd.join("command-declined.txt").exists()
                || fixture.config.cwd.join("file-declined.txt").exists()
            {
                return Err(
                    "provider continued or declined side effect occurred before an answer".into(),
                );
            }
            let command = Command::AnswerApproval {
                request: request.reference(),
                decision: if step == 0 {
                    ApprovalDecision::Decline
                } else {
                    ApprovalDecision::Accept
                },
            };
            fixture
                .client
                .commands
                .send(command.clone())
                .await
                .map_err(|e| e.to_string())?;
            fixture
                .client
                .commands
                .send(command)
                .await
                .map_err(|e| e.to_string())?;
            fixture.resolved(&request).await?;
            previous = Some(request.reference());
        }
        fixture.terminal(SessionPhase::Completed).await?;
        if stats(fixture.port)?["requests"] != 5
            || stats(fixture.port)?["fixture_error"] != false
            || stats(fixture.port)?["accepted_command_output_count"] != 1
        {
            return Err("unexpected provider request count".into());
        }
        if fixture.config.cwd.join("command-declined.txt").exists()
            || fixture.config.cwd.join("file-declined.txt").exists()
        {
            return Err("declined writes executed".into());
        }
        // A command approval does not grant writes denied by the selected read-only sandbox.
        let snapshot = fixture.client.snapshots.borrow().clone();
        if fixture.config.cwd.join("command-accepted.txt").exists()
            || snapshot.sandbox != "read-only"
            || !snapshot.observation.activities.iter().any(|activity| {
                activity.item_id.as_deref() == Some("command-2")
                    && activity.scope == crate::observation::ActivityScope::Tool
                    && activity.execution_state == crate::observation::ExecutionState::Failed
            })
        {
            return Err("shell approval changed the sandbox or hid the failed write".into());
        }
        if std::fs::read_to_string(fixture.config.cwd.join("file-accepted.txt"))
            .map_err(|e| e.to_string())?
            != "BASE\nFILE_CANARY 中文\n"
        {
            return Err("accepted side effects missing or changed".into());
        }
        Ok(())
    }
    .await;
    let cleanup = fixture.finish(SessionPhase::Completed).await;
    observed.unwrap();
    cleanup.unwrap();
}

#[tokio::test]
#[ignore = "requires Codex 0.159.2 and Python; explicit optional input feature in fixture home"]
async fn live_codex_input_answers_follow_real_question_ids_and_resolve_before_completion() {
    let mut fixture = LiveRequests::launch("input").await.unwrap();
    let observed: Result<(), String> = async {
        fixture
            .client
            .commands
            .send(Command::SubmitRootInput {
                text: "Ask the fixture question.".into(),
            })
            .await
            .map_err(|e| e.to_string())?;
        let request = fixture.request(None).await?;
        let RequestKind::UserInput { questions } = &request.kind else {
            return Err("real user input missing".into());
        };
        if questions.len() != 1
            || questions[0].id != "choice"
            || questions[0]
                .options
                .as_ref()
                .is_none_or(|options| options.len() != 2)
            || request.details.item_id.as_deref() != Some("input-0")
        {
            return Err("real question identity or options missing".into());
        }
        fixture
            .client
            .commands
            .send(Command::AnswerUserInput {
                request: request.reference(),
                answers: BTreeMap::from([("choice".into(), vec!["测试回答中文👋".into()])]),
            })
            .await
            .map_err(|e| e.to_string())?;
        fixture.resolved(&request).await?;
        fixture.terminal(SessionPhase::Completed).await?;
        if request.details.is_blocking != Some(false)
            || request.details.auto_resolution_ms.is_some()
            || !fixture
                .client
                .snapshots
                .borrow()
                .observation
                .activities
                .iter()
                .any(|activity| {
                    activity.request_id.as_ref() == Some(&request.id)
                        && activity.interaction_state
                            == Some(crate::observation::InteractionState::Resolved)
                })
        {
            return Err("real input hints or server resolution evidence missing".into());
        }
        let counters = stats(fixture.port)?;
        if counters["requests"] != 2
            || counters["fixture_answer_received"] != true
            || counters["fixture_error"] != false
        {
            return Err(format!("real input round trip failed: {counters}"));
        }
        Ok(())
    }
    .await;
    let cleanup = fixture.finish(SessionPhase::Completed).await;
    observed.unwrap();
    cleanup.unwrap();
}

#[tokio::test]
#[ignore = "requires Codex 0.159.2 and Python; request-specific cancel and no external model"]
async fn live_codex_command_cancel_rejects_the_write_and_waits_for_interrupted() {
    let mut fixture = LiveRequests::launch("cancel").await.unwrap();
    let observed: Result<(), String> = async {
        fixture
            .client
            .commands
            .send(Command::SubmitRootInput {
                text: "Cancel the fixture command.".into(),
            })
            .await
            .map_err(|e| e.to_string())?;
        let request = fixture.request(None).await?;
        if !matches!(request.kind, RequestKind::CommandApproval)
            || !request.allow_cancel
            || request.allow_decline
        {
            return Err("real command did not advertise cancel-only rejection".into());
        }
        request
            .approval_result(ApprovalDecision::Cancel)
            .map_err(|e| e.to_string())?;
        let command = Command::AnswerApproval {
            request: request.reference(),
            decision: ApprovalDecision::Cancel,
        };
        fixture
            .client
            .commands
            .send(command.clone())
            .await
            .map_err(|e| e.to_string())?;
        fixture
            .client
            .commands
            .send(command)
            .await
            .map_err(|e| e.to_string())?;
        fixture.resolved(&request).await?;
        fixture.terminal(SessionPhase::Interrupted).await?;
        if fixture.config.cwd.join("command-declined.txt").exists()
            || stats(fixture.port)?["requests"] != 1
        {
            return Err("canceled command wrote a file or resumed the provider".into());
        }
        Ok(())
    }
    .await;
    let cleanup = fixture.finish(SessionPhase::Interrupted).await;
    observed.unwrap();
    cleanup.unwrap();
}
