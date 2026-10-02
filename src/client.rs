use std::collections::{BTreeMap, HashMap, VecDeque};
use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;
use tokio::time::Instant;

use crate::app_server::{self, AppServer, AppServerError};
use crate::config::Config;
use crate::interactions::{ApprovalDecision, RequestView};
use crate::protocol::{Envelope, RpcId};
use crate::state::{CoreSnapshot, SessionPhase, SessionState, MESSAGE_BYTES};
use crate::transport::{PipeTransport, TransportError};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    SubmitRootInput {
        text: String,
    },
    AnswerApproval {
        request_id: RpcId,
        decision: ApprovalDecision,
    },
    AnswerUserInput {
        request_id: RpcId,
        answers: BTreeMap<String, Vec<String>>,
    },
    Interrupt,
    Quit,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExitReport {
    pub final_phase: SessionPhase,
    pub error: Option<String>,
    pub cleanup_error: Option<String>,
}

pub struct ClientHandle {
    pub commands: mpsc::Sender<Command>,
    pub snapshots: watch::Receiver<Arc<CoreSnapshot>>,
    pub join: JoinHandle<ExitReport>,
}

impl ClientHandle {
    pub async fn spawn(config: Config) -> Result<Self, AppServerError> {
        let config = app_server::normalize_config(config)?;
        let mut server = AppServer::spawn(&config)?;
        let pipe = server.pipe.take().expect("new app-server has a transport");
        Ok(Self::start(pipe, config, Some(server), false))
    }

    pub async fn check_shell(config: Config) -> Result<Self, AppServerError> {
        let config = app_server::normalize_config(config)?;
        let mut server = AppServer::spawn(&config)?;
        let pipe = server.pipe.take().expect("new app-server has a transport");
        Ok(Self::start(pipe, config, Some(server), true))
    }

    fn start(
        pipe: PipeTransport,
        config: Config,
        server: Option<AppServer>,
        check_only: bool,
    ) -> Self {
        let (commands, command_rx) = mpsc::channel(32);
        let initial = CoreSnapshot {
            phase: SessionPhase::Launching,
            cwd: config.cwd.display().to_string(),
            sandbox: config.sandbox.clone(),
            approval_policy: config.approval_policy.clone(),
            ..Default::default()
        };
        let (snapshot_tx, snapshots) = watch::channel(Arc::new(initial.clone()));
        let join = tokio::spawn(
            Core {
                pipe,
                config,
                server,
                command_rx,
                snapshot_tx,
                state: SessionState { view: initial },
                pending: HashMap::new(),
                next_id: 1,
                initial_input: None,
                interrupt_requested: false,
                interrupt_sent: false,
                preflight_passed: false,
                generation: 0,
                retired_turns: VecDeque::new(),
                check_only,
            }
            .run(),
        );
        Self {
            commands,
            snapshots,
            join,
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum RpcKind {
    Initialize,
    ThreadStart,
    Preflight,
    StartTurn { generation: u64 },
    Interrupt { generation: u64 },
}

impl RpcKind {
    fn generation(self) -> Option<u64> {
        match self {
            Self::StartTurn { generation } | Self::Interrupt { generation } => Some(generation),
            _ => None,
        }
    }
}

struct PendingRpc {
    kind: RpcKind,
    deadline: Instant,
}

struct Core {
    pipe: PipeTransport,
    server: Option<AppServer>,
    config: Config,
    command_rx: mpsc::Receiver<Command>,
    snapshot_tx: watch::Sender<Arc<CoreSnapshot>>,
    state: SessionState,
    pending: HashMap<RpcId, PendingRpc>,
    next_id: i64,
    initial_input: Option<String>,
    interrupt_requested: bool,
    interrupt_sent: bool,
    preflight_passed: bool,
    generation: u64,
    retired_turns: VecDeque<String>,
    check_only: bool,
}

impl Core {
    async fn run(mut self) -> ExitReport {
        if let Err(error) = self.send_rpc(RpcKind::Initialize) {
            self.state.error(SessionPhase::Failed, error.to_string());
        } else {
            self.state.view.phase = SessionPhase::Initializing;
        }
        self.publish();
        let mut clock = tokio::time::interval(Duration::from_millis(250));
        loop {
            tokio::select! {
                command = self.command_rx.recv() => {
                    match command {
                        Some(Command::Quit) | None => break,
                        Some(command) => self.command(command),
                    }
                }
                envelope = self.pipe.recv() => {
                    match envelope {
                        Ok(envelope) => self.envelope(envelope),
                        Err(error) => {
                            self.state.error(SessionPhase::Disconnected, format!("{error}; any active external outcome is unknown"));
                            self.publish();
                            break;
                        }
                    }
                }
                _ = clock.tick(), if !self.pending.is_empty() => {
                    let expired = self.pending.iter().find(|(_, rpc)| rpc.deadline <= Instant::now()).map(|(id, rpc)| (id.clone(), rpc.kind));
                    if let Some((id, kind)) = expired {
                        self.pending.remove(&id);
                        let message = if matches!(kind, RpcKind::Preflight) {
                            let advice = if cfg!(windows) { " Check Codex Windows sandbox setup, or explicitly select --windows-sandbox unelevated." } else { " Check the configured shell and sandbox." };
                            format!("Shell preflight timed out; no model turn was started.{advice} Automatic retry is disabled.")
                        } else {
                            format!("app-server {kind:?} response timed out; automatic retry is disabled")
                        };
                        self.state.error(SessionPhase::Unknown, message);
                    }
                }
            }
            self.publish();
            if self.state.view.phase == SessionPhase::Unknown {
                // A late reply cannot turn an uncertain side effect into a fresh success.
                self.pending.clear();
                break;
            }
        }

        let outcome = self.state.view.phase;
        if !matches!(
            outcome,
            SessionPhase::Failed | SessionPhase::Unknown | SessionPhase::Disconnected
        ) {
            self.state.view.phase = SessionPhase::Stopping;
            self.publish();
        }
        self.pipe.close_writer().await;
        let cleanup_error = if let Some(server) = &mut self.server {
            server.shutdown().await.err().map(|error| error.to_string())
        } else {
            None
        };
        if !matches!(
            outcome,
            SessionPhase::Failed | SessionPhase::Unknown | SessionPhase::Disconnected
        ) {
            self.state.view.phase = SessionPhase::Stopped;
        }
        self.publish();
        ExitReport {
            final_phase: self.state.view.phase,
            error: self.state.view.last_error.clone(),
            cleanup_error,
        }
    }

    fn publish(&mut self) {
        self.snapshot_tx.send_replace(self.state.snapshot());
    }

    fn send_rpc(&mut self, kind: RpcKind) -> Result<(), TransportError> {
        let id = RpcId::Number(self.next_id);
        self.next_id += 1;
        let envelope = match kind {
            RpcKind::Initialize => app_server::initialize(id.clone()),
            RpcKind::ThreadStart => app_server::thread_start(id.clone(), &self.config),
            RpcKind::Preflight => app_server::preflight(id.clone(), &self.config),
            RpcKind::Interrupt { .. } => app_server::interrupt(
                id.clone(),
                self.state.view.thread_id.as_deref().unwrap_or(""),
                self.state.view.turn_id.as_deref().unwrap_or(""),
            ),
            RpcKind::StartTurn { .. } => {
                return Err(TransportError::Failed(
                    "start-turn requires typed user input".into(),
                ))
            }
        };
        self.pipe.send(envelope)?;
        self.pending.insert(
            id,
            PendingRpc {
                kind,
                deadline: Instant::now() + Duration::from_secs(30),
            },
        );
        Ok(())
    }

    fn command(&mut self, command: Command) {
        match command {
            Command::SubmitRootInput { text } => {
                if text.trim().is_empty() || text.len() > MESSAGE_BYTES {
                    self.state.view.notice =
                        Some(format!("Enter a task of 1-{MESSAGE_BYTES} UTF-8 bytes."));
                    return;
                }
                if matches!(
                    self.state.view.phase,
                    SessionPhase::Launching | SessionPhase::Initializing
                ) {
                    if self.initial_input.is_none() {
                        self.initial_input = Some(text);
                    } else {
                        self.state.view.notice = Some("An initial task is already queued.".into());
                    }
                    return;
                }
                if !self.preflight_passed {
                    self.state.view.notice = Some(
                        "Shell preflight has not passed; restart after fixing the startup error."
                            .into(),
                    );
                    return;
                }
                if !self.state.view.phase.can_submit() || self.state.view.thread_id.is_none() {
                    self.state.view.notice = Some(
                        "Wait for the current turn to finish, or interrupt it with Ctrl+C.".into(),
                    );
                    return;
                }
                self.submit(&text);
            }
            Command::Interrupt => {
                if matches!(
                    self.state.view.phase,
                    SessionPhase::StartingTurn | SessionPhase::Running | SessionPhase::GatePending
                ) && !self.interrupt_requested
                {
                    self.interrupt_requested = true;
                    self.state.view.notice = Some(
                        "Interrupt requested; waiting for the server's terminal event.".into(),
                    );
                    self.issue_interrupt();
                }
            }
            Command::AnswerApproval {
                request_id,
                decision,
            } => {
                let result = self
                    .state
                    .view
                    .requests
                    .iter()
                    .find(|r| r.id == request_id)
                    .ok_or_else(|| "Request already resolved.".to_owned())
                    .and_then(|request| {
                        request.approval_result(decision).map_err(|e| e.to_string())
                    });
                self.respond(request_id, result);
            }
            Command::AnswerUserInput {
                request_id,
                answers,
            } => {
                let result = self
                    .state
                    .view
                    .requests
                    .iter()
                    .find(|r| r.id == request_id)
                    .ok_or_else(|| "Request already resolved.".to_owned())
                    .and_then(|request| request.input_result(&answers).map_err(|e| e.to_string()));
                self.respond(request_id, result);
            }
            Command::Quit => {}
        }
    }

    fn respond(&mut self, id: RpcId, result: Result<Value, String>) {
        match result {
            Ok(result) => match self.pipe.send(Envelope::response(id.clone(), Some(result))) {
                Ok(()) => {
                    if let Some(request) = self.state.view.requests.iter_mut().find(|r| r.id == id)
                    {
                        request.responding = true;
                    }
                }
                Err(error) => self.state.error(SessionPhase::Unknown, error.to_string()),
            },
            Err(error) => self.state.view.notice = Some(error),
        }
    }

    fn submit(&mut self, text: &str) {
        let id = RpcId::Number(self.next_id);
        self.next_id += 1;
        let thread = self.state.view.thread_id.as_deref().unwrap();
        match self
            .pipe
            .send(app_server::turn_start(id.clone(), thread, text))
        {
            Ok(()) => {
                self.generation += 1;
                self.state.view.turn_id = None;
                self.state.submission(text);
                self.interrupt_requested = false;
                self.interrupt_sent = false;
                self.pending
                    .retain(|_, rpc| rpc.kind.generation().is_none());
                self.pending.insert(
                    id,
                    PendingRpc {
                        kind: RpcKind::StartTurn {
                            generation: self.generation,
                        },
                        deadline: Instant::now() + Duration::from_secs(30),
                    },
                );
            }
            Err(error) => self.state.error(SessionPhase::Unknown, error.to_string()),
        }
    }

    fn issue_interrupt(&mut self) {
        if !self.interrupt_requested
            || self.interrupt_sent
            || self.state.view.phase != SessionPhase::Running
            || self.state.view.turn_id.is_none()
        {
            return;
        }
        match self.send_rpc(RpcKind::Interrupt {
            generation: self.generation,
        }) {
            Ok(()) => self.interrupt_sent = true,
            Err(error) => self.state.error(SessionPhase::Unknown, error.to_string()),
        }
    }

    fn bind_turn(&mut self, id: &str) {
        if self.retired_turns.iter().any(|old| old == id) {
            return;
        }
        if let Some(current) = self.state.view.turn_id.as_deref() {
            if current != id {
                self.state.error(
                    SessionPhase::Unknown,
                    "Conflicting turn identity for the current submission",
                );
                return;
            }
        } else if self.state.view.phase == SessionPhase::StartingTurn {
            self.state.turn_started(id.into());
        }
        self.issue_interrupt();
    }

    fn envelope(&mut self, envelope: Envelope) {
        match (envelope.method.as_deref(), envelope.id.clone()) {
            (Some(method), Some(id)) => {
                let params = envelope.params.unwrap_or(Value::Null);
                match RequestView::decode(id.clone(), method, &params) {
                    Ok(request) => {
                        if request.thread_id != self.state.view.thread_id.as_deref().unwrap_or("")
                            || request.turn_id != self.state.view.turn_id.as_deref().unwrap_or("")
                            || !matches!(
                                self.state.view.phase,
                                SessionPhase::Running | SessionPhase::GatePending
                            )
                        {
                            if let Err(error) = self.pipe.send(Envelope::error_response(
                                id,
                                -32602,
                                "Request does not belong to the active turn",
                            )) {
                                self.state.error(SessionPhase::Unknown, error.to_string());
                            }
                        } else if self.state.view.requests.iter().any(|r| r.id == id) {
                            // A repeated delivery is still the same pending interaction.
                        } else if self.state.view.requests.len() < 64 {
                            self.state.view.requests.push(request);
                        } else {
                            let _ = self.pipe.send(Envelope::error_response(
                                id,
                                -32000,
                                "Too many pending interaction requests",
                            ));
                            self.state.error(
                                SessionPhase::Unknown,
                                "Too many pending interaction requests",
                            );
                        }
                    }
                    Err(error) => {
                        let result =
                            self.pipe
                                .send(Envelope::error_response(id, -32601, error.to_string()));
                        self.state.view.notice = Some(error.to_string());
                        // Unsupported permissions or dynamic tools must not silently resume the root.
                        if result.is_err() {
                            self.state.error(
                                SessionPhase::Unknown,
                                "Could not reject unsupported request",
                            );
                        }
                        self.command(Command::Interrupt);
                    }
                }
            }
            (Some(method), None) => {
                self.notification(method, envelope.params.unwrap_or(Value::Null))
            }
            (None, Some(id)) => {
                let Some(pending) = self.pending.remove(&id) else {
                    return;
                };
                if pending
                    .kind
                    .generation()
                    .is_some_and(|generation| generation != self.generation)
                {
                    return;
                }
                if let Some(error) = envelope.error {
                    let message = error
                        .get("message")
                        .and_then(Value::as_str)
                        .unwrap_or("RPC failed")
                        .to_owned();
                    if matches!(pending.kind, RpcKind::Interrupt { .. }) {
                        // Rejection proves the interrupt was not accepted, not that the turn ended.
                        self.interrupt_requested = false;
                        self.interrupt_sent = false;
                        self.state.view.notice = Some(format!("Interrupt was not accepted: {message}. Waiting for turn status; Ctrl+C can request another interrupt."));
                        return;
                    }
                    let phase = if matches!(pending.kind, RpcKind::StartTurn { .. })
                        && self.state.view.turn_id.is_some()
                    {
                        SessionPhase::Unknown
                    } else {
                        SessionPhase::Failed
                    };
                    self.state.error(phase, message);
                    return;
                }
                self.response(pending.kind, envelope.result.unwrap_or(Value::Null));
            }
            _ => self
                .state
                .error(SessionPhase::Unknown, "Invalid envelope identity"),
        }
    }

    fn response(&mut self, kind: RpcKind, result: Value) {
        let action = match kind {
            RpcKind::Initialize => {
                if let Err(error) = self.pipe.send(Envelope::notification("initialized", None)) {
                    self.state.error(SessionPhase::Failed, error.to_string());
                    return;
                }
                Some(if self.check_only {
                    RpcKind::Preflight
                } else {
                    RpcKind::ThreadStart
                })
            }
            RpcKind::ThreadStart => {
                let Some(id) = result.pointer("/thread/id").and_then(Value::as_str) else {
                    self.state.error(
                        SessionPhase::Failed,
                        "thread/start response is missing thread.id",
                    );
                    return;
                };
                self.state.view.thread_id = Some(id.into());
                self.state.view.model = result
                    .get("model")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                if let Some(policy) = result.get("approvalPolicy").and_then(Value::as_str) {
                    self.state.view.approval_policy = policy.into();
                }
                if let Some(cwd) = result.get("cwd").and_then(Value::as_str) {
                    self.state.view.cwd = cwd.into();
                }
                Some(RpcKind::Preflight)
            }
            RpcKind::Preflight => {
                if result.get("exitCode").and_then(Value::as_i64) != Some(0)
                    || !result
                        .get("stdout")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .contains("native-agent-tui-shell-ok")
                {
                    self.state.error(
                        SessionPhase::Failed,
                        "Shell preflight failed; no model turn was started",
                    );
                    return;
                }
                self.state.view.phase = SessionPhase::Ready;
                self.preflight_passed = true;
                if let Some(text) = self.initial_input.take() {
                    self.submit(&text);
                }
                None
            }
            RpcKind::StartTurn { .. } => {
                let Some(id) = result.pointer("/turn/id").and_then(Value::as_str) else {
                    self.state.error(
                        SessionPhase::Unknown,
                        "turn/start response is missing turn.id",
                    );
                    return;
                };
                self.bind_turn(id);
                None
            }
            RpcKind::Interrupt { .. } => None, // An RPC acknowledgement is not a terminal turn event.
        };
        if let Some(kind) = action {
            if let Err(error) = self.send_rpc(kind) {
                self.state.error(SessionPhase::Failed, error.to_string());
            }
        }
    }

    fn notification(&mut self, method: &str, params: Value) {
        if method == "serverRequest/resolved" {
            if params["threadId"].as_str() != self.state.view.thread_id.as_deref() {
                return;
            }
            if let Ok(id) = serde_json::from_value::<RpcId>(params["requestId"].clone()) {
                self.state.view.requests.retain(|r| r.id != id);
            }
            return;
        }
        let thread = params.get("threadId").and_then(Value::as_str);
        if thread != self.state.view.thread_id.as_deref() {
            return;
        }
        let turn = params.get("turnId").and_then(Value::as_str);
        match method {
            "turn/started" => {
                if self.state.view.phase == SessionPhase::StartingTurn {
                    if let Some(id) = params.pointer("/turn/id").and_then(Value::as_str) {
                        self.bind_turn(id);
                    }
                }
            }
            "turn/completed" => {
                let id = params.pointer("/turn/id").and_then(Value::as_str);
                if id.is_none()
                    || id != self.state.view.turn_id.as_deref()
                    || !matches!(
                        self.state.view.phase,
                        SessionPhase::Running | SessionPhase::GatePending
                    )
                {
                    return;
                }
                self.state.view.phase = match params.pointer("/turn/status").and_then(Value::as_str)
                {
                    Some("completed") => SessionPhase::Completed,
                    Some("interrupted") => SessionPhase::Interrupted,
                    Some("failed") => SessionPhase::Failed,
                    _ => {
                        self.state
                            .error(SessionPhase::Unknown, "Invalid turn terminal status");
                        return;
                    }
                };
                self.state.view.last_error = params
                    .pointer("/turn/error/message")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                self.state
                    .view
                    .requests
                    .retain(|r| r.turn_id != id.unwrap_or(""));
                self.state.view.tool_activity = None;
                self.state.view.notice = None;
                self.interrupt_requested = false;
                self.interrupt_sent = false;
                self.pending
                    .retain(|_, rpc| rpc.kind.generation() != Some(self.generation));
                self.retired_turns.push_back(id.unwrap().to_owned());
                if self.retired_turns.len() > 128 {
                    self.retired_turns.pop_front();
                }
            }
            "item/agentMessage/delta" => {
                if let (Some(turn), Some(id), Some(delta)) =
                    (turn, params["itemId"].as_str(), params["delta"].as_str())
                {
                    self.state.message(turn, id, delta, false);
                }
            }
            "item/started" | "item/completed" => {
                if turn != self.state.view.turn_id.as_deref() {
                    return;
                }
                let item = &params["item"];
                if item["type"] == "agentMessage" && method == "item/completed" {
                    if let (Some(turn), Some(id), Some(text)) =
                        (turn, item["id"].as_str(), item["text"].as_str())
                    {
                        self.state.message(turn, id, text, true);
                    }
                } else if method == "item/started" && item["type"] != "userMessage" {
                    self.state.view.tool_activity = item["type"].as_str().map(str::to_owned);
                } else if method == "item/completed" {
                    self.state.view.tool_activity = None;
                }
            }
            "thread/tokenUsage/updated" => {
                self.state.view.total_tokens = params
                    .pointer("/tokenUsage/total/totalTokens")
                    .and_then(Value::as_u64);
            }
            "error" => {
                self.state.view.notice = params
                    .pointer("/error/message")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    #[cfg(windows)]
    #[tokio::test]
    #[ignore = "requires the installed authenticated Codex app-server"]
    async fn live_windows_core_reaches_ready_without_a_model_turn() {
        let config = Config {
            windows_sandbox: Some("unelevated".into()),
            sandbox: "read-only".into(),
            ..Default::default()
        };
        let mut client = ClientHandle::spawn(config).await.unwrap();
        let outcome = tokio::time::timeout(
            Duration::from_secs(15),
            client.snapshots.wait_for(|s| {
                matches!(
                    s.phase,
                    SessionPhase::Ready
                        | SessionPhase::Failed
                        | SessionPhase::Unknown
                        | SessionPhase::Disconnected
                )
            }),
        )
        .await
        .map(|result| result.unwrap().clone());
        let _ = client.commands.send(Command::Quit).await;
        client.join.await.unwrap();
        let snapshot = outcome.expect("live startup must finish within 15 seconds");
        assert_eq!(
            snapshot.phase,
            SessionPhase::Ready,
            "{:?}",
            snapshot.last_error
        );
        assert_eq!(snapshot.root_turn_count, 0);
    }

    async fn harness() -> (ClientHandle, BufReader<tokio::io::DuplexStream>) {
        let (client, server) = tokio::io::duplex(65536);
        let (read, write) = tokio::io::split(client);
        (
            ClientHandle::start(
                PipeTransport::new(read, write),
                Config::default(),
                None,
                false,
            ),
            BufReader::new(server),
        )
    }
    async fn next(server: &mut BufReader<tokio::io::DuplexStream>) -> Value {
        let mut line = String::new();
        server.read_line(&mut line).await.unwrap();
        serde_json::from_str(&line).unwrap()
    }
    async fn send(server: &mut BufReader<tokio::io::DuplexStream>, data: Value) {
        server
            .get_mut()
            .write_all(format!("{data}\n").as_bytes())
            .await
            .unwrap();
    }
    async fn ready(server: &mut BufReader<tokio::io::DuplexStream>) {
        let preflight = initialized(server).await;
        send(server, json!({"id":preflight["id"],"result":{"exitCode":0,"stdout":"native-agent-tui-shell-ok","stderr":""}})).await;
    }
    async fn initialized(server: &mut BufReader<tokio::io::DuplexStream>) -> Value {
        let init = next(server).await;
        assert_eq!(init["method"], "initialize");
        send(server, json!({"id":init["id"],"result":{}})).await;
        assert_eq!(next(server).await["method"], "initialized");
        let thread = next(server).await;
        assert_eq!(thread["method"], "thread/start");
        send(
            server,
            json!({"id":thread["id"],"result":{"thread":{"id":"root"},"model":"test-model"}}),
        )
        .await;
        let preflight = next(server).await;
        assert_eq!(preflight["method"], "command/exec");
        preflight
    }
    async fn phase(client: &mut ClientHandle, expected: SessionPhase) {
        tokio::time::timeout(
            Duration::from_secs(3),
            client.snapshots.wait_for(|s| s.phase == expected),
        )
        .await
        .unwrap()
        .unwrap();
    }

    #[tokio::test]
    async fn notifications_before_rpc_response_are_preserved_and_old_turns_cannot_finish_new_turn()
    {
        let (mut client, mut server) = harness().await;
        ready(&mut server).await;
        phase(&mut client, SessionPhase::Ready).await;
        client
            .commands
            .send(Command::SubmitRootInput {
                text: "first".into(),
            })
            .await
            .unwrap();
        let request = next(&mut server).await;
        send(
            &mut server,
            json!({"method":"turn/started","params":{"threadId":"root","turn":{"id":"one"}}}),
        )
        .await;
        send(&mut server, json!({"method":"item/agentMessage/delta","params":{"threadId":"root","turnId":"one","itemId":"a","delta":"中文"}})).await;
        send(&mut server, json!({"method":"turn/completed","params":{"threadId":"root","turn":{"id":"one","status":"completed"}}})).await;
        send(
            &mut server,
            json!({"id":request["id"],"result":{"turn":{"id":"one"}}}),
        )
        .await;
        phase(&mut client, SessionPhase::Completed).await;
        assert_eq!(
            client.snapshots.borrow().messages.last().unwrap().text,
            "中文"
        );
        client
            .commands
            .send(Command::SubmitRootInput {
                text: "second".into(),
            })
            .await
            .unwrap();
        let request = next(&mut server).await;
        send(
            &mut server,
            json!({"id":request["id"],"result":{"turn":{"id":"two"}}}),
        )
        .await;
        send(&mut server, json!({"method":"turn/completed","params":{"threadId":"root","turn":{"id":"one","status":"completed"}}})).await;
        send(&mut server, json!({"method":"item/agentMessage/delta","params":{"threadId":"root","turnId":"two","itemId":"b","delta":"second"}})).await;
        tokio::time::timeout(
            Duration::from_secs(3),
            client.snapshots.wait_for(|s| {
                s.messages
                    .last()
                    .is_some_and(|m| m.text == "second" && m.role == "Agent")
            }),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(client.snapshots.borrow().phase, SessionPhase::Running);
        assert_eq!(client.snapshots.borrow().root_turn_count, 2);
        client.commands.send(Command::Quit).await.unwrap();
        assert_eq!(
            client.join.await.unwrap().final_phase,
            SessionPhase::Stopped
        );
    }

    #[tokio::test]
    async fn interrupt_ack_does_not_fabricate_completion_and_approvals_resolve_once() {
        let (mut client, mut server) = harness().await;
        ready(&mut server).await;
        phase(&mut client, SessionPhase::Ready).await;
        client
            .commands
            .send(Command::SubmitRootInput {
                text: "work".into(),
            })
            .await
            .unwrap();
        let req = next(&mut server).await;
        send(
            &mut server,
            json!({"id":req["id"],"result":{"turn":{"id":"one"}}}),
        )
        .await;
        phase(&mut client, SessionPhase::Running).await;
        send(&mut server, json!({"id":"approval","method":"item/commandExecution/requestApproval","params":{"threadId":"root","turnId":"one","command":"test"}})).await;
        tokio::time::timeout(
            Duration::from_secs(3),
            client.snapshots.wait_for(|s| !s.requests.is_empty()),
        )
        .await
        .unwrap()
        .unwrap();
        client
            .commands
            .send(Command::AnswerApproval {
                request_id: RpcId::String("approval".into()),
                decision: ApprovalDecision::Decline,
            })
            .await
            .unwrap();
        assert_eq!(next(&mut server).await["result"]["decision"], "decline");
        send(&mut server, json!({"method":"serverRequest/resolved","params":{"threadId":"root","requestId":"approval"}})).await;
        client.commands.send(Command::Interrupt).await.unwrap();
        let req = next(&mut server).await;
        assert_eq!(req["method"], "turn/interrupt");
        send(&mut server, json!({"id":req["id"],"result":{}})).await;
        assert_eq!(client.snapshots.borrow().phase, SessionPhase::Running);
        send(&mut server, json!({"method":"turn/completed","params":{"threadId":"root","turn":{"id":"one","status":"interrupted"}}})).await;
        phase(&mut client, SessionPhase::Interrupted).await;
        assert!(client.snapshots.borrow().requests.is_empty());
        client.commands.send(Command::Quit).await.unwrap();
        client.join.await.unwrap();
    }

    #[tokio::test]
    async fn disconnect_is_visible_and_preflight_never_starts_a_model_turn() {
        let (mut client, mut server) = harness().await;
        ready(&mut server).await;
        phase(&mut client, SessionPhase::Ready).await;
        assert_eq!(client.snapshots.borrow().root_turn_count, 0);
        drop(server);
        phase(&mut client, SessionPhase::Disconnected).await;
        assert!(client.join.await.unwrap().error.is_some());
    }

    #[tokio::test]
    async fn failed_preflight_cannot_be_bypassed_by_submitting_a_task() {
        let (mut client, mut server) = harness().await;
        let preflight = initialized(&mut server).await;
        send(
            &mut server,
            json!({"id":preflight["id"],"result":{"exitCode":1,"stdout":"","stderr":""}}),
        )
        .await;
        phase(&mut client, SessionPhase::Failed).await;
        client
            .commands
            .send(Command::SubmitRootInput {
                text: "bypass".into(),
            })
            .await
            .unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(100), next(&mut server))
                .await
                .is_err()
        );
        assert_eq!(client.snapshots.borrow().phase, SessionPhase::Failed);
        assert_eq!(client.snapshots.borrow().root_turn_count, 0);
        client.commands.send(Command::Quit).await.unwrap();
        client.join.await.unwrap();
    }

    #[tokio::test]
    async fn stale_interactions_are_rejected_and_other_threads_cannot_resolve_current_requests() {
        let (mut client, mut server) = harness().await;
        ready(&mut server).await;
        phase(&mut client, SessionPhase::Ready).await;
        client
            .commands
            .send(Command::SubmitRootInput {
                text: "work".into(),
            })
            .await
            .unwrap();
        let start = next(&mut server).await;
        send(
            &mut server,
            json!({"id":start["id"],"result":{"turn":{"id":"current"}}}),
        )
        .await;
        phase(&mut client, SessionPhase::Running).await;
        send(&mut server, json!({"id":"stale","method":"item/commandExecution/requestApproval","params":{"threadId":"root","turnId":"previous"}})).await;
        assert_eq!(next(&mut server).await["error"]["code"], -32602);
        send(&mut server, json!({"id":"active","method":"item/commandExecution/requestApproval","params":{"threadId":"root","turnId":"current"}})).await;
        send(&mut server, json!({"method":"serverRequest/resolved","params":{"threadId":"other","requestId":"active"}})).await;
        send(&mut server, json!({"method":"thread/tokenUsage/updated","params":{"threadId":"root","tokenUsage":{"total":{"totalTokens":1}}}})).await;
        tokio::time::timeout(
            Duration::from_secs(3),
            client.snapshots.wait_for(|s| s.total_tokens == Some(1)),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(client.snapshots.borrow().requests.len(), 1);
        client
            .commands
            .send(Command::AnswerApproval {
                request_id: RpcId::String("active".into()),
                decision: ApprovalDecision::Decline,
            })
            .await
            .unwrap();
        assert_eq!(next(&mut server).await["result"]["decision"], "decline");
        client.commands.send(Command::Quit).await.unwrap();
        client.join.await.unwrap();
    }

    #[tokio::test]
    async fn delayed_start_response_and_old_started_event_cannot_rebind_a_new_attempt() {
        let (mut client, mut server) = harness().await;
        ready(&mut server).await;
        phase(&mut client, SessionPhase::Ready).await;
        client
            .commands
            .send(Command::SubmitRootInput {
                text: "first".into(),
            })
            .await
            .unwrap();
        let first = next(&mut server).await;
        send(
            &mut server,
            json!({"method":"turn/started","params":{"threadId":"root","turn":{"id":"one"}}}),
        )
        .await;
        send(&mut server, json!({"method":"turn/completed","params":{"threadId":"root","turn":{"id":"one","status":"completed"}}})).await;
        phase(&mut client, SessionPhase::Completed).await;
        client
            .commands
            .send(Command::SubmitRootInput {
                text: "second".into(),
            })
            .await
            .unwrap();
        let second = next(&mut server).await;
        send(
            &mut server,
            json!({"method":"turn/started","params":{"threadId":"root","turn":{"id":"one"}}}),
        )
        .await;
        send(
            &mut server,
            json!({"id":second["id"],"result":{"turn":{"id":"two"}}}),
        )
        .await;
        send(
            &mut server,
            json!({"id":first["id"],"result":{"turn":{"id":"one"}}}),
        )
        .await;
        send(&mut server, json!({"method":"thread/tokenUsage/updated","params":{"threadId":"root","tokenUsage":{"total":{"totalTokens":42}}}})).await;
        tokio::time::timeout(
            Duration::from_secs(3),
            client.snapshots.wait_for(|s| s.total_tokens == Some(42)),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(client.snapshots.borrow().turn_id.as_deref(), Some("two"));
        assert_eq!(client.snapshots.borrow().root_turn_count, 2);
        assert_eq!(client.snapshots.borrow().phase, SessionPhase::Running);
        client.commands.send(Command::Quit).await.unwrap();
        client.join.await.unwrap();
    }

    #[tokio::test]
    async fn interrupt_during_start_is_sent_once_as_soon_as_turn_is_bound() {
        let (mut client, mut server) = harness().await;
        ready(&mut server).await;
        phase(&mut client, SessionPhase::Ready).await;
        client
            .commands
            .send(Command::SubmitRootInput {
                text: "work".into(),
            })
            .await
            .unwrap();
        let start = next(&mut server).await;
        client.commands.send(Command::Interrupt).await.unwrap();
        tokio::time::timeout(
            Duration::from_secs(3),
            client.snapshots.wait_for(|s| {
                s.notice
                    .as_deref()
                    .is_some_and(|n| n.starts_with("Interrupt requested"))
            }),
        )
        .await
        .unwrap()
        .unwrap();
        send(
            &mut server,
            json!({"method":"turn/started","params":{"threadId":"root","turn":{"id":"one"}}}),
        )
        .await;
        let interrupt = tokio::time::timeout(Duration::from_millis(250), next(&mut server))
            .await
            .expect("interrupt must not wait for the start RPC response");
        assert_eq!(interrupt["method"], "turn/interrupt");
        send(
            &mut server,
            json!({"id":start["id"],"result":{"turn":{"id":"one"}}}),
        )
        .await;
        send(&mut server, json!({"id":interrupt["id"],"result":{}})).await;
        assert!(
            tokio::time::timeout(Duration::from_millis(100), next(&mut server))
                .await
                .is_err()
        );
        send(&mut server, json!({"method":"turn/completed","params":{"threadId":"root","turn":{"id":"one","status":"interrupted"}}})).await;
        phase(&mut client, SessionPhase::Interrupted).await;
        client.commands.send(Command::Quit).await.unwrap();
        client.join.await.unwrap();
    }

    #[tokio::test]
    async fn rejected_interrupt_preserves_the_session_until_a_real_terminal_event() {
        let (mut client, mut server) = harness().await;
        ready(&mut server).await;
        phase(&mut client, SessionPhase::Ready).await;
        client
            .commands
            .send(Command::SubmitRootInput {
                text: "work".into(),
            })
            .await
            .unwrap();
        let start = next(&mut server).await;
        send(
            &mut server,
            json!({"id":start["id"],"result":{"turn":{"id":"one"}}}),
        )
        .await;
        phase(&mut client, SessionPhase::Running).await;
        client.commands.send(Command::Interrupt).await.unwrap();
        let interrupt = next(&mut server).await;
        send(&mut server, json!({"id":interrupt["id"],"error":{"code":-32600,"message":"no active turn to interrupt"}})).await;
        send(&mut server, json!({"method":"turn/completed","params":{"threadId":"root","turn":{"id":"one","status":"interrupted"}}})).await;
        phase(&mut client, SessionPhase::Interrupted).await;
        client.commands.send(Command::Quit).await.unwrap();
        assert_eq!(
            client.join.await.unwrap().final_phase,
            SessionPhase::Stopped
        );
    }
}
