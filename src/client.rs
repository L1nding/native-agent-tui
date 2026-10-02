use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;
use tokio::time::Instant;

use crate::agents::AgentInfo;
use crate::app_server::{self, AppServer, AppServerError};
use crate::config::Config;
use crate::gate::{ChildOutcome, CompletionGate, GateEvent, PendingGate, WaitRequest, WaitToken};
use crate::interactions::{ApprovalDecision, RequestView};
use crate::journal::{Journal, JournalError, StoredSnapshot};
use crate::observation::{AttentionClass, ChildFact, ObservationFacts, Observer};
use crate::protocol::{self, Envelope, RpcId};
use crate::scheduler::{
    ExternalTurn, InterruptEffect, RootTaskSpec, Scheduler, SchedulerCommand, TaskAttempt,
    TaskKind, TaskSnapshot,
};
use crate::state::{CoreSnapshot, GateSnapshot, SessionPhase, SessionState, MESSAGE_BYTES};
use crate::transport::{PipeTransport, TransportError};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    SubmitRootInput {
        text: String,
    },
    QueueRootTasks {
        tasks: Vec<RootTaskSpec>,
    },
    Schedule(SchedulerCommand),
    ScheduleTask {
        attempt: TaskAttempt,
        command: SchedulerCommand,
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
    ConfigureAttention {
        class: AttentionClass,
        quiet_ms: u64,
        attention_ms: u64,
    },
    HeadlessRequest {
        request_id: RpcId,
        thread_id: String,
        turn_id: String,
    },
    OutputUnavailable,
    UnconfirmedHeadlessInteraction,
    Quit,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExitReport {
    pub final_phase: SessionPhase,
    pub error: Option<String>,
    pub cleanup_error: Option<String>,
    pub journal_error: Option<JournalError>,
}

#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    #[error(transparent)]
    AppServer(#[from] AppServerError),
    #[error(transparent)]
    Journal(#[from] JournalError),
}

pub struct ClientHandle {
    pub commands: mpsc::Sender<Command>,
    pub snapshots: watch::Receiver<Arc<CoreSnapshot>>,
    pub join: JoinHandle<ExitReport>,
}

impl ClientHandle {
    pub async fn spawn(config: Config) -> Result<Self, ClientError> {
        Self::launch(config, false).await
    }

    pub async fn check_shell(config: Config) -> Result<Self, ClientError> {
        Self::launch(config, true).await
    }

    async fn launch(config: Config, check_only: bool) -> Result<Self, ClientError> {
        let config = app_server::normalize_config(config)?;
        let observer = Observer::new(config.attention.clone());
        let initial = Self::initial(&config, &observer);
        let journal = Journal::open(
            &config.journal,
            &config.cwd,
            StoredSnapshot::capture(&initial),
        )?;
        let mut server = match AppServer::spawn(&config).await {
            Ok(server) => server,
            Err(error) => {
                let mut failure = StoredSnapshot::capture(&initial);
                failure.phase = SessionPhase::Failed;
                failure.issue = Some(crate::journal::PersistenceIssue::StartupFailed);
                failure.close(SessionPhase::Failed, true);
                journal.finish(failure).await?;
                return Err(error.into());
            }
        };
        let pipe = server.pipe.take().expect("new app-server has a transport");
        Ok(Self::start(
            pipe,
            config,
            Some(server),
            check_only,
            Some((observer, journal)),
        ))
    }

    fn initial(config: &Config, observer: &Observer) -> CoreSnapshot {
        CoreSnapshot {
            phase: SessionPhase::Launching,
            cwd: config.cwd.display().to_string(),
            sandbox: config.sandbox.clone(),
            approval_policy: config.approval_policy.clone(),
            observation: observer.snapshot_at(0, Instant::now()),
            ..Default::default()
        }
    }

    fn start(
        pipe: PipeTransport,
        config: Config,
        server: Option<AppServer>,
        check_only: bool,
        prepared: Option<(Observer, Journal)>,
    ) -> Self {
        let (commands, command_rx) = mpsc::channel(32);
        let (observer, journal) = match prepared {
            Some((observer, journal)) => (observer, Some(journal)),
            None => (Observer::new(config.attention.clone()), None),
        };
        let mut initial = Self::initial(&config, &observer);
        initial.journal = journal.as_ref().map(Journal::view);
        let (snapshot_tx, snapshots) = watch::channel(Arc::new(initial.clone()));
        let join = tokio::spawn(
            Core {
                pipe,
                observer,
                journal,
                journal_error: None,
                root_attempt: None,
                root_observed: None,
                config,
                server,
                command_rx,
                snapshot_tx,
                state: SessionState {
                    view: initial,
                    ..Default::default()
                },
                pending: HashMap::new(),
                next_id: 1,
                scheduler: Scheduler::default(),
                child_interrupts: VecDeque::new(),
                interrupt_requested: false,
                interrupt_sent: false,
                preflight_passed: false,
                generation: 0,
                retired_turns: VecDeque::new(),
                gate: PendingGate::default(),
                wait: None,
                ingress_seq: 0,
                collab_starts: HashMap::new(),
                completed_collab: VecDeque::new(),
                completed_waits: VecDeque::new(),
                identity_pending: BTreeSet::new(),
                identity_requested: BTreeSet::new(),
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
    AgentRead,
    StartTurn { generation: u64 },
    Interrupt { generation: u64 },
    ChildInterrupt { attempt: TaskAttempt },
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
    identity_thread: Option<String>,
}

struct Core {
    observer: Observer,
    journal: Option<Journal>,
    journal_error: Option<JournalError>,
    root_attempt: Option<TaskAttempt>,
    root_observed: Option<TaskSnapshot>,
    pipe: PipeTransport,
    server: Option<AppServer>,
    config: Config,
    command_rx: mpsc::Receiver<Command>,
    snapshot_tx: watch::Sender<Arc<CoreSnapshot>>,
    state: SessionState,
    pending: HashMap<RpcId, PendingRpc>,
    next_id: i64,
    scheduler: Scheduler,
    child_interrupts: VecDeque<InterruptEffect>,
    interrupt_requested: bool,
    interrupt_sent: bool,
    preflight_passed: bool,
    generation: u64,
    retired_turns: VecDeque<String>,
    gate: PendingGate,
    wait: Option<PendingTool>,
    ingress_seq: u64,
    collab_starts: HashMap<String, u64>,
    completed_collab: VecDeque<String>,
    completed_waits: VecDeque<(RpcId, String, String)>,
    identity_pending: BTreeSet<String>,
    identity_requested: BTreeSet<String>,
    check_only: bool,
}

struct PendingTool {
    request_id: RpcId,
    call_id: String,
    parent_turn: String,
    generation: u64,
    token: WaitToken,
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
        clock.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut observation_clock = tokio::time::interval(Duration::from_secs(1));
        observation_clock.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut journal_status = self.journal.as_ref().map(|journal| journal.status.clone());
        loop {
            tokio::select! {
                changed = async {
                    match &mut journal_status { Some(status) => Some(status.changed().await), None => std::future::pending().await }
                } => {
                    let error = if matches!(changed, Some(Err(_))) { Some(JournalError::Closed) }
                        else { self.persistence_error() };
                    if let Some(error) = error {
                        self.journal_error = Some(error.clone());
                        self.state.error(SessionPhase::Unknown, error.to_string());
                        self.scheduler.disconnected();
                        self.publish();
                        break;
                    }
                    self.publish_journal_status();
                    continue;
                }
                _ = observation_clock.tick(), if self.observer.has_timed_activity() => {
                    // Observation must not dispatch, retry, flush writes, or release a Gate.
                    self.publish();
                    if self.state.view.phase == SessionPhase::Unknown { break; }
                    continue;
                }
                command = self.command_rx.recv() => {
                    match command {
                        Some(Command::Quit) | None => break,
                        Some(Command::OutputUnavailable) => {
                            self.command(Command::Interrupt);
                            self.state.view.last_headless_action = Some(crate::interactions::HeadlessAction::StopForOutput);
                            if !self.state.view.phase.can_submit() {
                                self.state.error(SessionPhase::Unknown, "JSONL output unavailable; any active external outcome is unknown");
                            }
                            self.scheduler.disconnected();
                            self.publish();
                            break;
                        }
                        Some(Command::UnconfirmedHeadlessInteraction) => {
                            if !self.state.view.phase.can_submit() {
                                self.state.error(SessionPhase::Unknown, "Headless interaction is unavailable; interruption was not confirmed");
                            }
                            self.scheduler.disconnected();
                            self.publish();
                            break;
                        }
                        Some(command) => self.command(command),
                    }
                }
                envelope = self.pipe.recv() => {
                    match envelope {
                        Ok(envelope) => self.envelope(envelope),
                        Err(error) => {
                            self.state.error(SessionPhase::Disconnected, format!("{error}; any active external outcome is unknown"));
                            self.scheduler.disconnected();
                            self.publish();
                            break;
                        }
                    }
                }
                _ = clock.tick(), if !self.pending.is_empty() || !self.child_interrupts.is_empty() => {
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
            self.flush_child_interrupt();
            self.dispatch_root();
            if matches!(
                self.state.view.phase,
                SessionPhase::Unknown | SessionPhase::Disconnected
            ) {
                self.scheduler.disconnected();
            }
            self.publish();
            if self.state.view.phase == SessionPhase::Unknown {
                // A late reply cannot turn an uncertain side effect into a fresh success.
                self.pending.clear();
                break;
            }
        }

        let outcome = self.state.view.phase;
        self.scheduler.disconnected();
        self.gate.disconnect();
        self.wait = None;
        if let Some(gate) = &mut self.state.view.gate {
            gate.pending = false;
        }
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
        if let Some(journal) = self.journal.take() {
            let mut final_snapshot = StoredSnapshot::capture(&self.state.view);
            final_snapshot.close(
                if self.journal_error.is_some() {
                    SessionPhase::Unknown
                } else {
                    outcome
                },
                cleanup_error.is_none(),
            );
            if self.journal_error.is_some() {
                final_snapshot.issue = Some(crate::journal::PersistenceIssue::JournalUnavailable);
            }
            match journal.finish(final_snapshot).await {
                Ok(view) => self.state.view.journal = Some(view),
                Err(error) => {
                    self.journal_error = Some(error.clone());
                    self.state.error(SessionPhase::Unknown, error.to_string());
                }
            }
            self.publish_journal_status();
        }
        ExitReport {
            final_phase: self.state.view.phase,
            error: self.state.view.last_error.clone(),
            cleanup_error,
            journal_error: self.journal_error,
        }
    }

    fn publish(&mut self) {
        if self.wait.is_some() {
            let targets = self.gate.targets();
            if let Some(gate) = &mut self.state.view.gate {
                gate.targets = targets;
            }
        }
        self.scheduler.interactions(&self.state.view.requests);
        self.state.view.scheduler = self.scheduler.snapshot();
        self.state.view.queued_inputs = self.state.view.scheduler.queued_roots;
        self.reconcile_observation();
        self.state.view.observation = self
            .observer
            .snapshot_at(self.state.view.version + 1, Instant::now());
        if self.journal_error.is_none() {
            if let Some(journal) = &mut self.journal {
                if let Err(error) = journal.append(StoredSnapshot::capture(&self.state.view)) {
                    self.journal_error = Some(error.clone());
                    self.state.error(SessionPhase::Unknown, error.to_string());
                    self.scheduler.disconnected();
                    self.state.view.scheduler = self.scheduler.snapshot();
                    self.observer.execution_unavailable(Instant::now());
                    self.state.view.observation = self
                        .observer
                        .snapshot_at(self.state.view.version + 1, Instant::now());
                }
            }
        }
        self.state.view.journal = self.journal.as_ref().map(Journal::view).or(self
            .state
            .view
            .journal
            .take());
        if let Some(view) = &mut self.state.view.journal {
            view.error = self.journal_error.clone().or(view.error.take());
        }
        self.snapshot_tx.send_replace(self.state.snapshot());
    }

    fn publish_journal_status(&mut self) {
        if let Some(journal) = &self.journal {
            self.state.view.journal = Some(journal.view());
        }
        if let Some(view) = &mut self.state.view.journal {
            view.error = self.journal_error.clone().or(view.error.take());
        }
        self.state.view.observation.snapshot_version = self.state.view.version + 1;
        self.snapshot_tx.send_replace(self.state.snapshot());
    }

    fn reconcile_observation(&mut self) {
        let children = self.state.agents.snapshots();
        if let Some(task) = self
            .root_attempt
            .and_then(|attempt| self.scheduler.task(attempt.task))
            .filter(|task| Some(task.attempt) == self.root_attempt.map(|attempt| attempt.attempt))
        {
            self.root_observed = Some(task.clone());
        }
        let facts = ObservationFacts {
            phase: self.state.view.phase,
            root_thread: self.state.view.thread_id.as_deref(),
            root_generation: self.generation,
            root_task: self.root_observed.as_ref(),
            children: children
                .iter()
                .map(|agent| ChildFact {
                    agent,
                    task: self.scheduler.child_task(&agent.info.id),
                })
                .collect(),
            requests: &self.state.view.requests,
            gate: self.state.view.gate.as_ref(),
        };
        if let Err(error) = self.observer.reconcile(facts, Instant::now()) {
            self.state.error(SessionPhase::Unknown, error.to_string());
            self.observer.execution_unavailable(Instant::now());
        }
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
            RpcKind::StartTurn { .. } | RpcKind::AgentRead | RpcKind::ChildInterrupt { .. } => {
                return Err(TransportError::Failed(
                    "start-turn requires typed user input".into(),
                ))
            }
        };
        self.pipe.send(envelope)?;
        if matches!(kind, RpcKind::Preflight) {
            self.state.view.phase = SessionPhase::CheckingShell;
            self.state.view.notice =
                Some("Checking the sandbox shell; no model turn has started.".into());
        }
        self.pending.insert(
            id,
            PendingRpc {
                kind,
                deadline: Instant::now() + Duration::from_secs(30),
                identity_thread: None,
            },
        );
        Ok(())
    }

    fn command(&mut self, command: Command) {
        match command {
            Command::HeadlessRequest {
                request_id,
                thread_id,
                turn_id,
            } => {
                let Some(request) = self
                    .state
                    .view
                    .requests
                    .iter()
                    .find(|request| {
                        request.id == request_id
                            && request.thread_id == thread_id
                            && request.turn_id == turn_id
                            && !request.responding
                    })
                    .cloned()
                else {
                    return;
                };
                let action = if matches!(
                    request.kind,
                    crate::interactions::RequestKind::UserInput { .. }
                ) {
                    crate::interactions::HeadlessAction::InterruptForInput {
                        request_id,
                        thread_id: request.thread_id,
                        turn_id: request.turn_id,
                    }
                } else if !request.allow_decline {
                    crate::interactions::HeadlessAction::InterruptForApproval {
                        request_id,
                        thread_id: request.thread_id,
                        turn_id: request.turn_id,
                    }
                } else {
                    crate::interactions::HeadlessAction::DeclineApproval {
                        request_id: request_id.clone(),
                        thread_id: request.thread_id,
                        turn_id: request.turn_id,
                    }
                };
                if matches!(
                    action,
                    crate::interactions::HeadlessAction::InterruptForInput { .. }
                        | crate::interactions::HeadlessAction::InterruptForApproval { .. }
                ) {
                    self.command(Command::Interrupt);
                } else {
                    self.command(Command::AnswerApproval {
                        request_id: request.id,
                        decision: ApprovalDecision::Decline,
                    });
                }
                self.state.view.last_headless_action = Some(action);
            }
            Command::ConfigureAttention {
                class,
                quiet_ms,
                attention_ms,
            } => {
                self.state.view.notice = Some(
                    match self.observer.configure(class, quiet_ms, attention_ms) {
                        Ok(()) => format!("{class:?} attention settings applied for this session."),
                        Err(error) => error.to_string(),
                    },
                );
            }
            Command::SubmitRootInput { text } => {
                if text.trim().is_empty() || text.len() > MESSAGE_BYTES {
                    self.state.view.notice =
                        Some(format!("Enter a task of 1-{MESSAGE_BYTES} UTF-8 bytes."));
                    return;
                }
                let initializing = matches!(
                    self.state.view.phase,
                    SessionPhase::Launching
                        | SessionPhase::Initializing
                        | SessionPhase::CheckingShell
                );
                if !self.preflight_passed && !initializing {
                    self.state.view.notice = Some(
                        "Shell preflight has not passed; restart after fixing the startup error."
                            .into(),
                    );
                    return;
                }
                if !initializing && self.wait.is_none() && !self.state.view.phase.can_submit() {
                    self.state.view.notice = Some(
                        "Wait for the current turn to finish, or interrupt it with Ctrl+C.".into(),
                    );
                    return;
                }
                let mut spec = RootTaskSpec::input(text);
                if self.wait.is_some() {
                    if let Some(dependency) = self
                        .scheduler
                        .last_pending_root()
                        .or(self.scheduler.active_root().map(|attempt| attempt.task))
                    {
                        spec.dependencies.push(dependency);
                    }
                }
                self.queue_tasks(vec![spec]);
            }
            Command::QueueRootTasks { tasks } => self.queue_tasks(tasks),
            Command::Schedule(command) => self.schedule(command),
            Command::ScheduleTask { attempt, command } => {
                let target = match command {
                    SchedulerCommand::Pause(id)
                    | SchedulerCommand::Resume(id)
                    | SchedulerCommand::Cancel(id)
                    | SchedulerCommand::Retry(id) => Some(id),
                    SchedulerCommand::Reprioritize { task_id, .. } => Some(task_id),
                    _ => None,
                };
                if target != Some(attempt.task)
                    || self
                        .scheduler
                        .task(attempt.task)
                        .is_none_or(|task| task.attempt != attempt.attempt)
                {
                    self.state.view.notice = Some(
                        "Scheduler command rejected: the selected task attempt changed.".into(),
                    );
                    return;
                }
                self.schedule(command);
            }
            Command::Interrupt => {
                if matches!(
                    self.state.view.phase,
                    SessionPhase::StartingTurn | SessionPhase::Running | SessionPhase::GatePending
                ) && !self.interrupt_requested
                {
                    if let Some(attempt) = self.scheduler.active_root() {
                        // Ctrl+C retains its existing root-turn interrupt semantics.
                        let _ = self
                            .scheduler
                            .command(SchedulerCommand::Cancel(attempt.task));
                    }
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
            Command::Quit
            | Command::OutputUnavailable
            | Command::UnconfirmedHeadlessInteraction => {}
        }
    }

    fn queue_tasks(&mut self, tasks: Vec<RootTaskSpec>) {
        if self.check_only
            || (!self.preflight_passed
                && !matches!(
                    self.state.view.phase,
                    SessionPhase::Launching
                        | SessionPhase::Initializing
                        | SessionPhase::CheckingShell
                ))
        {
            self.state.view.notice =
                Some("Task dispatch requires a successful shell preflight.".into());
            return;
        }
        match self.scheduler.enqueue(tasks) {
            Ok(ids) => {
                self.state.view.notice = Some(format!(
                    "Queued {} root tasks; dependencies remain enforced.",
                    ids.len()
                ))
            }
            Err(error) => self.state.view.notice = Some(error.to_string()),
        }
    }

    fn schedule(&mut self, command: SchedulerCommand) {
        match self.scheduler.command(command) {
            Ok(effects) => {
                self.state.view.notice = Some("Scheduler command accepted; running turns stop only on a server terminal event.".into());
                for effect in effects {
                    match effect.kind {
                        TaskKind::RootTurn => {
                            self.interrupt_requested = true;
                            self.issue_interrupt();
                        }
                        TaskKind::NativeChild => self.queue_child_interrupt(effect),
                    }
                }
                self.flush_gate();
            }
            Err(error) => {
                self.state.view.notice = Some(format!("Scheduler command rejected: {error}"))
            }
        }
    }

    fn dispatch_root(&mut self) {
        if self.persistence_error().is_some() {
            return;
        }
        if !self.preflight_passed
            || self.check_only
            || self.wait.is_some()
            || !self.state.view.phase.can_submit()
            || self.state.view.thread_id.is_none()
        {
            return;
        }
        if let Some(dispatch) = self.scheduler.dispatch() {
            self.root_attempt = Some(dispatch.attempt);
            self.submit(&dispatch.text);
        }
    }

    fn persistence_error(&self) -> Option<JournalError> {
        self.journal_error.clone().or_else(|| {
            self.journal.as_ref().and_then(|journal| {
                let status = journal.status.borrow();
                status
                    .error
                    .clone()
                    .or_else(|| status.closed.then_some(JournalError::Closed))
            })
        })
    }

    fn flush_child_interrupt(&mut self) {
        // Limit writes per Core turn; StopWorkflow can target many observed children.
        if self
            .pending
            .values()
            .any(|rpc| matches!(rpc.kind, RpcKind::ChildInterrupt { .. }))
        {
            return;
        }
        let Some(effect) = self.child_interrupts.pop_front() else {
            return;
        };
        let Some(task) = self.scheduler.task(effect.attempt.task) else {
            return;
        };
        if task.attempt != effect.attempt.attempt || !task.state.active() || !task.cancel_requested
        {
            return;
        }
        let Some(external) = effect.external else {
            return;
        }; // Deferred until turn/started.
        if task.external.as_ref() != Some(&external) {
            return;
        }
        let id = RpcId::Number(self.next_id);
        self.next_id += 1;
        match self.pipe.send(app_server::interrupt(
            id.clone(),
            &external.thread_id,
            &external.turn_id,
        )) {
            Ok(()) => {
                self.pending.insert(
                    id,
                    PendingRpc {
                        kind: RpcKind::ChildInterrupt {
                            attempt: effect.attempt,
                        },
                        deadline: Instant::now() + Duration::from_secs(30),
                        identity_thread: None,
                    },
                );
            }
            Err(error) => self.state.error(SessionPhase::Unknown, error.to_string()),
        }
    }

    fn queue_child_interrupt(&mut self, effect: InterruptEffect) {
        // Only the latest intent for each bounded task record can remain queued.
        self.child_interrupts
            .retain(|old| old.attempt.task != effect.attempt.task);
        self.child_interrupts.push_back(effect);
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
        if self.wait.is_some() {
            self.state.error(
                SessionPhase::Unknown,
                "Root submission attempted while a child Gate is pending",
            );
            return;
        }
        let id = RpcId::Number(self.next_id);
        self.next_id += 1;
        let thread = self.state.view.thread_id.as_deref().unwrap();
        match self
            .pipe
            .send(app_server::turn_start(id.clone(), thread, text))
        {
            Ok(()) => {
                self.state.view.root_start_requests += 1;
                self.generation += 1;
                self.state.view.turn_id = None;
                self.state.submission(text);
                self.interrupt_requested = false;
                self.interrupt_sent = false;
                self.collab_starts.clear();
                self.completed_collab.clear();
                self.pending
                    .retain(|_, rpc| rpc.kind.generation().is_none());
                self.pending.insert(
                    id,
                    PendingRpc {
                        kind: RpcKind::StartTurn {
                            generation: self.generation,
                        },
                        deadline: Instant::now() + Duration::from_secs(30),
                        identity_thread: None,
                    },
                );
            }
            Err(error) => self.state.error(SessionPhase::Unknown, error.to_string()),
        }
    }

    fn issue_interrupt(&mut self) {
        if !self.interrupt_requested
            || self.interrupt_sent
            || !matches!(
                self.state.view.phase,
                SessionPhase::Running | SessionPhase::GatePending
            )
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
        if let Some(attempt) = self.scheduler.active_root() {
            if let Err(error) = self.scheduler.started_root(
                attempt,
                ExternalTurn {
                    thread_id: self.state.view.thread_id.clone().unwrap_or_default(),
                    turn_id: id.into(),
                    generation: self.generation,
                },
            ) {
                self.state.error(SessionPhase::Unknown, error.to_string());
                return;
            }
        }
        self.issue_interrupt();
    }

    fn envelope(&mut self, envelope: Envelope) {
        self.observer.raw_message();
        self.ingress_seq += 1;
        match (envelope.method.as_deref(), envelope.id.clone()) {
            (Some(method), Some(id)) => {
                let params = envelope.params.unwrap_or(Value::Null);
                if method == "item/tool/call" {
                    self.accept_wait(id, params);
                    return;
                }
                match RequestView::decode(id.clone(), method, &params) {
                    Ok(request) => {
                        let root_request = request.thread_id
                            == self.state.view.thread_id.as_deref().unwrap_or("")
                            && request.turn_id == self.state.view.turn_id.as_deref().unwrap_or("");
                        let child_request = self
                            .state
                            .agents
                            .active_turn(&request.thread_id, &request.turn_id);
                        if !(child_request && self.preflight_passed
                            || root_request
                                && matches!(
                                    self.state.view.phase,
                                    SessionPhase::Running | SessionPhase::GatePending
                                ))
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
                if let RpcKind::ChildInterrupt { attempt } = pending.kind {
                    if !self
                        .scheduler
                        .task(attempt.task)
                        .is_some_and(|task| task.attempt == attempt.attempt && task.state.active())
                    {
                        return;
                    }
                    if let Some(error) = envelope.error {
                        self.scheduler.interrupt_rejected(attempt);
                        self.state.view.notice = Some(format!(
                            "Child interrupt was not accepted: {}",
                            error
                                .get("message")
                                .and_then(Value::as_str)
                                .unwrap_or("RPC failed")
                        ));
                    }
                    // Acknowledgement never completes a child or releases its Gate.
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
                        if let Some(attempt) = self.scheduler.active_root() {
                            self.scheduler.interrupt_rejected(attempt);
                        }
                        self.state.view.notice = Some(format!("Interrupt was not accepted: {message}. Waiting for turn status; Ctrl+C can request another interrupt."));
                        return;
                    }
                    let phase = if matches!(pending.kind, RpcKind::AgentRead)
                        || (matches!(pending.kind, RpcKind::StartTurn { .. })
                            && self.state.view.turn_id.is_some())
                    {
                        SessionPhase::Unknown
                    } else {
                        SessionPhase::Failed
                    };
                    if phase == SessionPhase::Failed
                        && matches!(pending.kind, RpcKind::StartTurn { .. })
                    {
                        if let Some(attempt) = self.scheduler.active_root() {
                            self.scheduler.finish(attempt, ChildOutcome::Failed);
                        }
                    }
                    self.state.error(phase, message);
                    return;
                }
                if let Some(thread) = pending.identity_thread {
                    self.identity_pending.remove(&thread);
                    let result = envelope.result.unwrap_or(Value::Null);
                    match protocol::decode_agent_info(&result) {
                        Ok(Some(info)) if info.id == thread => {
                            self.confirm_agent(info);
                        }
                        _ => self.state.error(
                            SessionPhase::Unknown,
                            "Child metadata does not confirm its parent identity",
                        ),
                    }
                } else {
                    self.response(pending.kind, envelope.result.unwrap_or(Value::Null));
                }
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
                self.state.view.notice = None;
                self.preflight_passed = true;
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
            RpcKind::AgentRead => None, // The response is handled with its captured thread identity.
            RpcKind::ChildInterrupt { .. } => None,
        };
        if let Some(kind) = action {
            if let Err(error) = self.send_rpc(kind) {
                self.state.error(SessionPhase::Failed, error.to_string());
            }
        }
    }

    fn accept_wait(&mut self, id: RpcId, params: Value) {
        if params["tool"].as_str() != Some(protocol::WAIT_TOOL) || !params["namespace"].is_null() {
            self.reject_wait(id, "Unsupported dynamic tool".into());
            self.state.view.notice =
                Some("Unsupported dynamic tool; interrupting the root turn.".into());
            self.command(Command::Interrupt);
            return;
        }
        let call = match protocol::decode_wait_call(params) {
            Ok(call) => call,
            Err(error) => {
                self.reject_wait(id, error.to_string());
                return;
            }
        };
        if self
            .completed_waits
            .iter()
            .any(|(request, call_id, parent_turn)| {
                request == &id && call_id == &call.call_id && parent_turn == &call.turn_id
            })
        {
            return;
        }
        if let Some(wait) = &self.wait {
            if wait.request_id == id
                && wait.call_id == call.call_id
                && wait.parent_turn == call.turn_id
            {
                return;
            }
            self.reject_wait(id, "A child wait is already pending".into());
            return;
        }
        if self.state.view.thread_id.as_deref() != Some(&call.thread_id)
            || self.state.view.turn_id.as_deref() != Some(&call.turn_id)
            || self.state.view.phase != SessionPhase::Running
        {
            self.reject_wait(id, "Only the active root turn can wait for children".into());
            return;
        }
        let targets = match self
            .state
            .agents
            .capture(&call.thread_id, &call.arguments.targets)
        {
            Ok(targets) => targets,
            Err(error) => {
                self.reject_wait(id, error.to_string());
                return;
            }
        };
        let Some(attempt) = self.scheduler.active_root() else {
            self.reject_wait(id, "No scheduler attempt owns the root turn".into());
            return;
        };
        if let Err(error) = self.scheduler.wait_for_children(attempt, &targets) {
            self.reject_wait(id, error.to_string());
            return;
        }
        let token = match self.gate.accept_wait(WaitRequest {
            targets: targets.clone(),
        }) {
            Ok(token) => token,
            Err(error) => {
                let _ = self.scheduler.released_wait(attempt);
                self.reject_wait(id, format!("Invalid child wait: {error:?}"));
                return;
            }
        };
        self.state.view.gate = Some(GateSnapshot {
            targets,
            pending: true,
            root_starts_at_enter: self.state.view.root_start_requests,
            root_starts_at_release: None,
        });
        self.wait = Some(PendingTool {
            request_id: id,
            call_id: call.call_id,
            parent_turn: call.turn_id,
            generation: self.generation,
            token,
        });
        self.state.view.phase = SessionPhase::GatePending;
        self.flush_gate();
    }

    fn reject_wait(&mut self, id: RpcId, reason: String) {
        if let Err(error) = self.pipe.send(Envelope::error_response(id, -32602, reason)) {
            self.state.error(SessionPhase::Unknown, error.to_string());
        }
    }

    fn apply_child_event(&mut self, event: GateEvent) {
        if let Err(error) = self.scheduler.child_event(&event) {
            self.state.error(SessionPhase::Unknown, error.to_string());
            return;
        }
        if let Some(task) = self.scheduler.child_task(&event.target) {
            let attempt = TaskAttempt {
                task: task.id,
                attempt: task.attempt,
            };
            if task.cancel_requested && event.outcome.is_none() {
                self.queue_child_interrupt(InterruptEffect {
                    attempt,
                    external: task.external.clone(),
                    kind: TaskKind::NativeChild,
                });
            }
            if event.outcome.is_some() {
                self.pending.retain(|_, rpc| !matches!(rpc.kind, RpcKind::ChildInterrupt { attempt: pending } if pending == attempt));
            }
            // A newer authoritative turn retires control RPCs for the old attempt too.
            self.pending.retain(|_, rpc| !matches!(rpc.kind, RpcKind::ChildInterrupt { attempt: pending } if pending.task == attempt.task && pending.attempt != attempt.attempt));
        }
        self.gate.apply(&event);
        self.flush_gate();
    }

    fn flush_gate(&mut self) {
        let Some(attempt) = self.scheduler.active_root() else {
            return;
        };
        if !self.scheduler.can_release_wait(attempt) {
            return;
        }
        if self
            .gate
            .targets()
            .iter()
            .any(|target| self.identity_pending.contains(&target.id))
        {
            return;
        }
        let Some((token, targets)) = self.gate.take_result() else {
            return;
        };
        let Some(wait) = self.wait.take() else {
            self.state.error(
                SessionPhase::Unknown,
                "Gate result has no owning dynamic request",
            );
            return;
        };
        if wait.token != token
            || wait.generation != self.generation
            || self.state.view.turn_id.as_deref() != Some(&wait.parent_turn)
        {
            self.state.error(
                SessionPhase::Unknown,
                "Gate result belongs to an obsolete root attempt",
            );
            return;
        }
        let starts = self.state.view.root_start_requests;
        if let Some(gate) = &mut self.state.view.gate {
            if gate.root_starts_at_enter != starts {
                self.state.error(
                    SessionPhase::Unknown,
                    "Root request count changed while Gate was pending",
                );
                return;
            }
            gate.targets = targets.clone();
            gate.root_starts_at_release = Some(starts);
            gate.pending = false;
        }
        let outcomes:Vec<_> = targets.into_iter().map(|target| json!({"threadId":target.id,"turnId":target.turn_id,"generation":target.generation,"outcome":target.outcome})).collect();
        let result = protocol::dynamic_tool_result(
            true,
            json!({"status":"released","targets":outcomes,"rootTurnStartsWhilePending":0}),
        );
        match self
            .pipe
            .send(Envelope::response(wait.request_id.clone(), Some(result)))
        {
            Ok(()) => {
                self.completed_waits
                    .push_back((wait.request_id, wait.call_id, wait.parent_turn));
                if self.completed_waits.len() > 128 {
                    self.completed_waits.pop_front();
                }
                self.state.view.phase = SessionPhase::Running;
                if let Err(error) = self.scheduler.released_wait(attempt) {
                    self.state.error(SessionPhase::Unknown, error.to_string());
                }
            }
            Err(error) => self.state.error(SessionPhase::Unknown, error.to_string()),
        }
    }

    fn cancel_wait(&mut self) {
        self.gate.cancel();
        if let Some(wait) = self.wait.take() {
            self.completed_waits
                .push_back((wait.request_id, wait.call_id, wait.parent_turn));
            if self.completed_waits.len() > 128 {
                self.completed_waits.pop_front();
            }
        }
        if let Some(gate) = &mut self.state.view.gate {
            gate.pending = false;
        }
        if self.state.view.phase == SessionPhase::GatePending {
            self.state.view.phase = SessionPhase::Running;
        }
        if let Some(attempt) = self.scheduler.active_root() {
            let _ = self.scheduler.released_wait(attempt);
        }
    }

    fn observe_collab(&mut self, method: &str, item: &Value) {
        let Some(id) = item["id"].as_str() else {
            return;
        };
        if id.len() > 1024
            || item["senderThreadId"].as_str() != self.state.view.thread_id.as_deref()
        {
            return;
        }
        let Some(receivers) = item["receiverThreadIds"].as_array() else {
            return;
        };
        if receivers.len() > 64 {
            self.state
                .error(SessionPhase::Unknown, "Too many collaboration targets");
            return;
        }
        let rearm = matches!(
            item["tool"].as_str(),
            Some("sendInput" | "followupTask" | "resumeAgent")
        );
        if method == "item/started" {
            if self.completed_collab.iter().any(|old| old == id) {
                return;
            }
            if !self.collab_starts.contains_key(id) && self.collab_starts.len() >= 128 {
                self.state.error(
                    SessionPhase::Unknown,
                    "Too many pending collaboration activities",
                );
                return;
            }
            if !self.collab_starts.contains_key(id) && self.collab_starts.len() < 128 {
                self.collab_starts.insert(id.into(), self.ingress_seq);
                if rearm {
                    for receiver in receivers.iter().filter_map(Value::as_str) {
                        self.state.agents.rearm(receiver, self.ingress_seq);
                    }
                }
            }
            return;
        }
        if self.completed_collab.iter().any(|old| old == id) {
            return;
        }
        let seq = self.collab_starts.remove(id).unwrap_or(self.ingress_seq);
        self.completed_collab.push_back(id.into());
        if self.completed_collab.len() > 128 {
            self.completed_collab.pop_front();
        }
        if item["status"] != "completed" {
            if rearm {
                for receiver in receivers.iter().filter_map(Value::as_str) {
                    self.state.agents.cancel_rearm(receiver, seq);
                }
            }
            return;
        }
        for receiver in receivers.iter().filter_map(Value::as_str) {
            if item["tool"] == "spawnAgent" && !self.state.agents.known(receiver) {
                let info = AgentInfo {
                    id: receiver.into(),
                    parent_id: self.state.view.thread_id.clone().unwrap(),
                    path: None,
                    nickname: None,
                    role: None,
                    model: item["model"].as_str().map(str::to_owned),
                    confirmed: true,
                };
                if let Err(error) = self.state.agents.register(info) {
                    self.state.error(SessionPhase::Unknown, error.to_string());
                    return;
                }
            } else if rearm {
                self.state.agents.rearm(receiver, seq);
            }
            if self.state.agents.known(receiver) {
                self.register_task_child(receiver, self.scheduler.active_root(), receiver);
            }
        }
    }

    fn observe_activity(&mut self, method: &str, parent: &str, item: &Value) {
        let (Some(id), Some(path)) = (item["agentThreadId"].as_str(), item["agentPath"].as_str())
        else {
            return;
        };
        if !self.state.agents.known(id) {
            let info = AgentInfo {
                id: id.into(),
                parent_id: parent.into(),
                path: Some(path.into()),
                nickname: None,
                role: None,
                model: None,
                confirmed: false,
            };
            if let Err(error) = self.state.agents.register(info) {
                self.state.error(SessionPhase::Unknown, error.to_string());
                return;
            }
        }
        if !self.state.agents.confirmed(id) {
            self.identity_pending.insert(id.into());
        }
        self.register_task_child(id, self.scheduler.active_root(), path);
        if item["kind"] == "interacted" {
            if let Some(item_id) = item["id"].as_str() {
                if item_id.is_empty() || item_id.len() > 1024 {
                    self.state.error(
                        SessionPhase::Unknown,
                        "Invalid collaboration activity identity",
                    );
                    return;
                }
                if self.completed_collab.iter().any(|old| old == item_id) {
                    return;
                }
                if method == "item/started" {
                    if !self.collab_starts.contains_key(item_id) && self.collab_starts.len() >= 128
                    {
                        self.state.error(
                            SessionPhase::Unknown,
                            "Too many pending collaboration activities",
                        );
                        return;
                    }
                    let seq = *self
                        .collab_starts
                        .entry(item_id.into())
                        .or_insert(self.ingress_seq);
                    self.state.agents.rearm(id, seq);
                } else if !self.completed_collab.iter().any(|old| old == item_id) {
                    let seq = self
                        .collab_starts
                        .remove(item_id)
                        .unwrap_or(self.ingress_seq);
                    self.state.agents.rearm(id, seq);
                    self.completed_collab.push_back(item_id.into());
                    if self.completed_collab.len() > 128 {
                        self.completed_collab.pop_front();
                    }
                }
            }
        }
    }

    fn read_agent_identity(&mut self, id: &str) {
        if !self.identity_pending.contains(id) || self.identity_requested.contains(id) {
            return;
        }
        self.identity_requested.insert(id.into());
        let request_id = RpcId::Number(self.next_id);
        self.next_id += 1;
        let request = Envelope::request(
            request_id.clone(),
            "thread/read",
            Some(json!({"threadId":id,"includeTurns":false})),
        );
        if let Err(error) = self.pipe.send(request) {
            self.state.error(SessionPhase::Unknown, error.to_string());
            return;
        }
        self.pending.insert(
            request_id,
            PendingRpc {
                kind: RpcKind::AgentRead,
                deadline: Instant::now() + Duration::from_secs(30),
                identity_thread: Some(id.into()),
            },
        );
    }

    fn confirm_agent(&mut self, info: AgentInfo) {
        let id = info.id.clone();
        let title = info
            .path
            .clone()
            .or(info.nickname.clone())
            .unwrap_or_else(|| id.clone());
        if let Err(error) = self.state.agents.register(info) {
            self.state.error(SessionPhase::Unknown, error.to_string());
            return;
        }
        // thread/started proves thread parentage, not which submitted root task created it.
        self.register_task_child(&id, None, &title);
        self.identity_pending.remove(&id);
        self.pending
            .retain(|_, rpc| rpc.identity_thread.as_deref() != Some(&id));
        if self.wait.is_some() {
            let ids: Vec<_> = self
                .gate
                .targets()
                .into_iter()
                .map(|target| target.id)
                .collect();
            if let Err(error) = self
                .state
                .agents
                .capture(self.state.view.thread_id.as_deref().unwrap_or(""), &ids)
            {
                if let Some(wait) = &self.wait {
                    self.reject_wait(wait.request_id.clone(), error.to_string());
                }
                self.cancel_wait();
                return;
            }
        }
        self.flush_gate();
    }

    fn register_task_child(&mut self, thread: &str, parent: Option<TaskAttempt>, title: &str) {
        if let Err(error) = self.scheduler.register_child(thread, parent, title) {
            self.state.error(SessionPhase::Unknown, error.to_string());
        }
    }

    fn notification(&mut self, method: &str, params: Value) {
        if method == "thread/started" {
            match protocol::decode_agent_info(&params) {
                Ok(Some(info))
                    if self.state.view.thread_id.as_deref() == Some(&info.parent_id)
                        || self.state.agents.known(&info.parent_id) =>
                {
                    self.confirm_agent(info);
                }
                Err(error) => self.state.error(SessionPhase::Unknown, error.to_string()),
                _ => {}
            }
            return;
        }
        if method == "serverRequest/resolved" {
            let thread = params["threadId"].as_str().unwrap_or("");
            if Some(thread) != self.state.view.thread_id.as_deref()
                && !self.state.agents.known(thread)
            {
                return;
            }
            if let Ok(id) = serde_json::from_value::<RpcId>(params["requestId"].clone()) {
                if self
                    .state
                    .view
                    .requests
                    .iter()
                    .any(|request| request.id == id && request.thread_id == thread)
                {
                    self.observer.request_resolved(thread, &id, Instant::now());
                }
                self.state
                    .view
                    .requests
                    .retain(|r| r.id != id || r.thread_id != thread);
                if Some(thread) == self.state.view.thread_id.as_deref()
                    && self.wait.as_ref().is_some_and(|wait| wait.request_id == id)
                {
                    self.cancel_wait();
                }
            }
            return;
        }
        let thread = params.get("threadId").and_then(Value::as_str);
        // Decode observation only for an owned current turn. Unrelated malformed
        // telemetry cannot make this execution uncertain.
        if let (Some(thread), Some(turn)) = (thread, params["turnId"].as_str()) {
            let owned = (Some(thread) == self.state.view.thread_id.as_deref()
                && Some(turn) == self.state.view.turn_id.as_deref()
                && matches!(
                    self.state.view.phase,
                    SessionPhase::Running | SessionPhase::GatePending
                ))
                || self.state.agents.active_turn(thread, turn);
            if owned {
                if let Some(notice) = protocol::decode_observed_tool(method, &params) {
                    match notice {
                        Ok(notice) => {
                            if let Err(error) = self.observer.tool(&notice, Instant::now()) {
                                self.state.error(SessionPhase::Unknown, error.to_string());
                            }
                        }
                        Err(error) => self.state.error(SessionPhase::Unknown, error.to_string()),
                    }
                }
                if let Some(output) = protocol::decode_observed_output(method, &params) {
                    match output {
                        Ok(output) => match output.kind {
                            protocol::ObservedOutputKind::Reasoning => {
                                if let Err(error) = self.observer.output(
                                    thread,
                                    turn,
                                    &output.item_id,
                                    output.bytes,
                                    false,
                                    Instant::now(),
                                ) {
                                    self.state.error(SessionPhase::Unknown, error.to_string());
                                }
                            }
                            protocol::ObservedOutputKind::Tool(category) => {
                                if let Err(error) = self.observer.tool_output(
                                    thread,
                                    turn,
                                    &output.item_id,
                                    output.bytes,
                                    category,
                                    Instant::now(),
                                ) {
                                    self.state.error(SessionPhase::Unknown, error.to_string());
                                }
                            }
                        },
                        Err(error) => self.state.error(SessionPhase::Unknown, error.to_string()),
                    }
                }
            }
        }
        if thread != self.state.view.thread_id.as_deref() {
            if let Some(thread) = thread.filter(|id| self.state.agents.known(id)) {
                if let Some(turn) = params["turnId"].as_str() {
                    if method == "item/agentMessage/delta" {
                        if let (Some(id), Some(text)) =
                            (params["itemId"].as_str(), params["delta"].as_str())
                        {
                            if self.state.child_message(thread, turn, id, text, false) {
                                if let Err(error) = self.observer.output(
                                    thread,
                                    turn,
                                    id,
                                    text.len(),
                                    false,
                                    Instant::now(),
                                ) {
                                    self.state.error(SessionPhase::Unknown, error.to_string());
                                }
                            }
                        }
                    } else if method == "item/completed" && params["item"]["type"] == "agentMessage"
                    {
                        if let (Some(id), Some(text)) = (
                            params["item"]["id"].as_str(),
                            params["item"]["text"].as_str(),
                        ) {
                            if self.state.child_message(thread, turn, id, text, true) {
                                if let Err(error) = self.observer.output(
                                    thread,
                                    turn,
                                    id,
                                    text.len(),
                                    true,
                                    Instant::now(),
                                ) {
                                    self.state.error(SessionPhase::Unknown, error.to_string());
                                }
                            }
                        }
                    }
                }
                if let Some(turn) = params.pointer("/turn/id").and_then(Value::as_str) {
                    let event = match method {
                        "turn/started" => {
                            let event = self.state.agents.started(thread, turn, self.ingress_seq);
                            if event.is_some() {
                                self.read_agent_identity(thread);
                            }
                            event
                        }
                        "turn/completed" => {
                            if !self.state.agents.current_turn(thread, turn) {
                                return;
                            }
                            let outcome =
                                match params.pointer("/turn/status").and_then(Value::as_str) {
                                    Some("completed") => ChildOutcome::Completed,
                                    Some("failed") => ChildOutcome::Failed,
                                    Some("interrupted") => ChildOutcome::Interrupted,
                                    _ => {
                                        self.state.error(
                                            SessionPhase::Unknown,
                                            "Invalid child turn terminal status",
                                        );
                                        return;
                                    }
                                };
                            let event = self.state.agents.completed(thread, turn, outcome);
                            if event.is_some() {
                                self.state.view.requests.retain(|request| {
                                    request.thread_id != thread || request.turn_id != turn
                                });
                            }
                            event
                        }
                        _ => None,
                    };
                    if let Some(event) = event {
                        self.apply_child_event(event);
                    }
                }
            }
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
                self.state.view.requests.retain(|r| {
                    Some(r.thread_id.as_str()) != self.state.view.thread_id.as_deref()
                        || r.turn_id != id.unwrap_or("")
                });
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
                self.cancel_wait();
                if let Some(attempt) = self.scheduler.active_root() {
                    let outcome = match self.state.view.phase {
                        SessionPhase::Completed => ChildOutcome::Completed,
                        SessionPhase::Interrupted => ChildOutcome::Interrupted,
                        _ => ChildOutcome::Failed,
                    };
                    self.scheduler.finish(attempt, outcome);
                    if self.state.view.phase != SessionPhase::Completed
                        && self.scheduler.pending_count() > 0
                    {
                        self.state.view.notice = Some("Dependent tasks remain blocked; retry the failed task or cancel queued work in F4.".into());
                    }
                }
            }
            "item/agentMessage/delta" => {
                if let (Some(turn), Some(id), Some(delta)) =
                    (turn, params["itemId"].as_str(), params["delta"].as_str())
                {
                    if self.state.message(turn, id, delta, false) {
                        if let Err(error) = self.observer.output(
                            self.state.view.thread_id.as_deref().unwrap_or(""),
                            turn,
                            id,
                            delta.len(),
                            false,
                            Instant::now(),
                        ) {
                            self.state.error(SessionPhase::Unknown, error.to_string());
                        }
                    }
                }
            }
            "item/started" | "item/completed" => {
                if turn != self.state.view.turn_id.as_deref() {
                    return;
                }
                let item = &params["item"];
                if item["type"] == "collabAgentToolCall" {
                    self.observe_collab(method, item);
                }
                if item["type"] == "subAgentActivity" {
                    let root = self.state.view.thread_id.clone().unwrap();
                    self.observe_activity(method, &root, item);
                }
                if item["type"] == "agentMessage" && method == "item/completed" {
                    if let (Some(turn), Some(id), Some(text)) =
                        (turn, item["id"].as_str(), item["text"].as_str())
                    {
                        if self.state.message(turn, id, text, true) {
                            if let Err(error) = self.observer.output(
                                self.state.view.thread_id.as_deref().unwrap_or(""),
                                turn,
                                id,
                                text.len(),
                                true,
                                Instant::now(),
                            ) {
                                self.state.error(SessionPhase::Unknown, error.to_string());
                            }
                        }
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
    use crate::journal::{tests::Fixture as JournalFixture, Payload, Replay};
    use serde_json::json;
    use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};

    async fn journal_harness(
        fixture: &JournalFixture,
    ) -> (
        ClientHandle,
        BufReader<tokio::io::DuplexStream>,
        Config,
        String,
    ) {
        let config = Config {
            journal: fixture.settings(),
            ..Default::default()
        };
        let observer = Observer::new(config.attention.clone());
        let initial = ClientHandle::initial(&config, &observer);
        let session = initial.observation.session_id.clone();
        let journal = Journal::open(
            &config.journal,
            &config.cwd,
            StoredSnapshot::capture(&initial),
        )
        .unwrap();
        let (client, server) = tokio::io::duplex(65536);
        let (read, write) = tokio::io::split(client);
        (
            ClientHandle::start(
                PipeTransport::new(read, write),
                config.clone(),
                None,
                false,
                Some((observer, journal)),
            ),
            BufReader::new(server),
            config,
            session,
        )
    }

    async fn journal_committed(client: &mut ClientHandle) {
        tokio::time::timeout(
            Duration::from_secs(3),
            client.snapshots.wait_for(|s| {
                s.journal
                    .as_ref()
                    .is_some_and(|j| j.committed_seq == j.submitted_seq)
            }),
        )
        .await
        .unwrap()
        .unwrap();
    }

    #[tokio::test]
    async fn history_export_failure_cannot_stop_a_live_turn_or_answer_its_pending_request() {
        use crate::history::{HistoryError, HistoryHandle, HistoryRequest};
        let fixture = JournalFixture::new();
        let (mut client, mut server, config, session) = journal_harness(&fixture).await;
        running_root(&mut client, &mut server).await;
        observation_event(&mut client, &mut server, json!({"id":7,"method":"item/commandExecution/requestApproval","params":{"threadId":"root","turnId":"root-turn","command":"PRIVATE_COMMAND"}})).await;
        journal_committed(&mut client).await;
        let mut history = HistoryHandle::start(config.journal.clone(), config.cwd.clone()).unwrap();
        let id = history
            .request(HistoryRequest::Preview { session, since: 0 })
            .unwrap();
        history.response(id).await.unwrap();
        let id = history
            .request(HistoryRequest::Export {
                destination: fixture.root.join("missing-directory/export.jsonl"),
            })
            .unwrap();
        assert!(matches!(
            history.response(id).await,
            Err(HistoryError::ExportIo)
        ));
        assert_eq!(client.snapshots.borrow().phase, SessionPhase::Running);
        assert_eq!(client.snapshots.borrow().root_start_requests, 1);
        assert!(!client.snapshots.borrow().requests[0].responding);
        assert!(
            tokio::time::timeout(Duration::from_millis(50), next(&mut server))
                .await
                .is_err()
        );
        client
            .commands
            .send(Command::AnswerApproval {
                request_id: RpcId::Number(7),
                decision: ApprovalDecision::Decline,
            })
            .await
            .unwrap();
        assert_eq!(next(&mut server).await["result"]["decision"], "decline");
        send(&mut server, json!({"method":"turn/completed","params":{"threadId":"root","turn":{"id":"root-turn","status":"completed"}}})).await;
        phase(&mut client, SessionPhase::Completed).await;
        client.commands.send(Command::Quit).await.unwrap();
        assert!(client.join.await.unwrap().journal_error.is_none());
        history.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn workflow_journal_reports_earlier_failure_after_the_last_root_succeeds() {
        use crate::scheduler::TaskState;
        let fixture = JournalFixture::new();
        let (mut client, mut server, config, session) = journal_harness(&fixture).await;
        ready(&mut server).await;
        phase(&mut client, SessionPhase::Ready).await;
        client
            .commands
            .send(Command::QueueRootTasks {
                tasks: vec![
                    RootTaskSpec::input("first".into()),
                    RootTaskSpec::input("last".into()),
                ],
            })
            .await
            .unwrap();
        for (turn, status) in [("first", "failed"), ("last", "completed")] {
            let start = next(&mut server).await;
            assert_eq!(start["method"], "turn/start");
            send(
                &mut server,
                json!({"id":start["id"],"result":{"turn":{"id":turn}}}),
            )
            .await;
            phase(&mut client, SessionPhase::Running).await;
            send(&mut server, json!({"method":"turn/completed","params":{"threadId":"root","turn":{"id":turn,"status":status}}})).await;
        }
        phase(&mut client, SessionPhase::Completed).await;
        client.commands.send(Command::Quit).await.unwrap();
        assert!(client.join.await.unwrap().journal_error.is_none());
        let replay = Replay::open(&config.journal, &config.cwd, &session, 0).unwrap();
        assert_eq!(replay.info.execution_result, Some(SessionPhase::Failed));
        assert_eq!(
            replay
                .latest_state()
                .tasks
                .iter()
                .map(|task| task.state)
                .collect::<Vec<_>>(),
            [TaskState::Failed, TaskState::Succeeded]
        );
    }

    #[tokio::test]
    async fn journal_core_replays_a_durable_terminal_without_private_text_or_ack_feedback() {
        let fixture = JournalFixture::new();
        let (mut client, mut server, config, session) = journal_harness(&fixture).await;
        ready(&mut server).await;
        phase(&mut client, SessionPhase::Ready).await;
        journal_committed(&mut client).await;
        let settled = client.snapshots.borrow().journal.clone().unwrap();
        tokio::time::sleep(Duration::from_millis(80)).await;
        assert_eq!(
            client
                .snapshots
                .borrow()
                .journal
                .as_ref()
                .unwrap()
                .submitted_seq,
            settled.submitted_seq,
            "Persistence acknowledgments must not append new records"
        );
        client
            .commands
            .send(Command::SubmitRootInput {
                text: "PRIVATE_PROMPT".into(),
            })
            .await
            .unwrap();
        let start = next(&mut server).await;
        send(
            &mut server,
            json!({"id":start["id"],"result":{"turn":{"id":"journal-turn"}}}),
        )
        .await;
        phase(&mut client, SessionPhase::Running).await;
        observation_event(&mut client, &mut server, json!({"id":"approval-id","method":"item/commandExecution/requestApproval","params":{"threadId":"root","turnId":"journal-turn","command":"PRIVATE_COMMAND"}})).await;
        journal_committed(&mut client).await;
        let mut bytes = Vec::new();
        Replay::open(&config.journal, &config.cwd, &session, 0)
            .unwrap()
            .write_jsonl(&mut bytes)
            .unwrap();
        let history = String::from_utf8(bytes).unwrap();
        assert!(history.contains("approval-id") && history.contains("journal-turn"));
        assert!(!history.contains("PRIVATE_"));
        client
            .commands
            .send(Command::AnswerApproval {
                request_id: RpcId::String("approval-id".into()),
                decision: ApprovalDecision::Decline,
            })
            .await
            .unwrap();
        let answer = next(&mut server).await;
        assert_eq!(answer["id"], "approval-id");
        observation_event(&mut client, &mut server, json!({"method":"serverRequest/resolved","params":{"threadId":"root","requestId":"approval-id"}})).await;
        observation_event(&mut client, &mut server, json!({"method":"item/agentMessage/delta","params":{"threadId":"root","turnId":"journal-turn","itemId":"message","delta":"PRIVATE_OUTPUT"}})).await;
        send(&mut server, json!({"method":"turn/completed","params":{"threadId":"root","turn":{"id":"journal-turn","status":"completed"}}})).await;
        phase(&mut client, SessionPhase::Completed).await;
        client.commands.send(Command::Quit).await.unwrap();
        let report = client.join.await.unwrap();
        assert_eq!(report.journal_error, None);
        let replay = Replay::open(&config.journal, &config.cwd, &session, 0).unwrap();
        assert!(replay.info.session_closed && !replay.info.needs_recovery);
        assert_eq!(
            replay.latest_state().execution_result,
            Some(SessionPhase::Completed)
        );
        assert_eq!(replay.latest_state().cleanup_confirmed, Some(true));
        assert_eq!(replay.latest_state().root_start_requests, 1);
        assert_eq!(
            client
                .snapshots
                .borrow()
                .journal
                .as_ref()
                .unwrap()
                .committed_seq,
            replay.info.committed_seq
        );
        let mut bytes = Vec::new();
        replay.write_jsonl(&mut bytes).unwrap();
        assert!(!String::from_utf8(bytes).unwrap().contains("PRIVATE_"));
    }

    #[tokio::test]
    async fn journal_commit_failure_stops_core_without_dispatching_another_root() {
        let fixture = JournalFixture::new();
        let (mut client, mut server, config, session) = journal_harness(&fixture).await;
        running_root(&mut client, &mut server).await;
        journal_committed(&mut client).await;
        let cursor = std::fs::read_dir(&fixture.root)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .find(|path| {
                path.extension()
                    .is_some_and(|extension| extension == "cursor")
            })
            .unwrap();
        std::fs::create_dir(cursor.with_extension("cursor.new")).unwrap();
        client
            .commands
            .send(Command::SubmitRootInput {
                text: "PRIVATE_QUEUED".into(),
            })
            .await
            .unwrap();
        let report = tokio::time::timeout(Duration::from_secs(4), client.join)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(report.final_phase, SessionPhase::Unknown);
        assert!(report.journal_error.is_some());
        assert_eq!(client.snapshots.borrow().root_start_requests, 1);
        assert!(client
            .snapshots
            .borrow()
            .observation
            .activities
            .iter()
            .all(|a| !matches!(
                a.execution_state,
                crate::observation::ExecutionState::Running
                    | crate::observation::ExecutionState::Waiting
            )));
        let replay = Replay::open(&config.journal, &config.cwd, &session, 0).unwrap();
        assert!(
            replay.info.needs_recovery && !replay.info.session_closed && replay.uncommitted_tail
        );
        assert_eq!(replay.info.execution_result, None);
        let mut bytes = Vec::new();
        replay.write_jsonl(&mut bytes).unwrap();
        let end: crate::journal::Record =
            serde_json::from_str(String::from_utf8(bytes).unwrap().lines().last().unwrap())
                .unwrap();
        assert!(
            matches!(end.payload, Payload::ReplayEnd(end) if end.needs_recovery && !end.live_attached)
        );
        let mut trailing = String::new();
        server.read_to_string(&mut trailing).await.unwrap();
        assert!(trailing.is_empty());
    }

    #[tokio::test]
    async fn journal_startup_failure_precedes_any_app_server_launch() {
        let fixture = JournalFixture::new();
        let invalid = fixture.root.join("file-not-directory");
        std::fs::write(&invalid, b"occupied").unwrap();
        let config = Config {
            executable: fixture.root.join("must-never-launch"),
            journal: crate::journal::JournalSettings {
                root: Some(invalid),
                ..Default::default()
            },
            ..Default::default()
        };
        assert!(matches!(
            ClientHandle::spawn(config).await,
            Err(ClientError::Journal(JournalError::Io))
        ));
    }

    #[derive(Clone)]
    struct JsonBuffer(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);
    impl std::io::Write for JsonBuffer {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn output_failure_preserves_a_previously_confirmed_terminal_result() {
        let fixture = JournalFixture::new();
        let (mut client, mut server, config, session) = journal_harness(&fixture).await;
        running_root(&mut client, &mut server).await;
        send(&mut server, json!({"method":"turn/completed","params":{"threadId":"root","turn":{"id":"root-turn","status":"completed"}}})).await;
        phase(&mut client, SessionPhase::Completed).await;
        client
            .commands
            .send(Command::OutputUnavailable)
            .await
            .unwrap();
        client.join.await.unwrap();
        let replay = Replay::open(&config.journal, &config.cwd, &session, 0).unwrap();
        assert_eq!(replay.info.execution_result, Some(SessionPhase::Completed));
        assert_eq!(
            replay.latest_state().issue,
            Some(crate::journal::PersistenceIssue::OutputUnavailable)
        );
        assert!(replay.info.needs_recovery);
        assert_eq!(
            replay.latest_state().tasks[0].state,
            crate::scheduler::TaskState::Succeeded
        );
    }

    #[tokio::test]
    async fn headless_approval_without_decline_requests_interruption_and_records_its_reason() {
        let fixture = JournalFixture::new();
        let (mut client, mut server, config, session) = journal_harness(&fixture).await;
        running_root(&mut client, &mut server).await;
        observation_event(&mut client, &mut server, json!({"id":7,"method":"item/commandExecution/requestApproval","params":{"threadId":"root","turnId":"root-turn","command":"PRIVATE_COMMAND","availableDecisions":["accept"]}})).await;
        client
            .commands
            .send(Command::HeadlessRequest {
                request_id: RpcId::Number(7),
                thread_id: "root".into(),
                turn_id: "root-turn".into(),
            })
            .await
            .unwrap();
        let interrupt = next(&mut server).await;
        assert_eq!(interrupt["method"], "turn/interrupt");
        send(&mut server, json!({"id":interrupt["id"],"result":{}})).await;
        send(&mut server, json!({"method":"turn/completed","params":{"threadId":"root","turn":{"id":"root-turn","status":"interrupted"}}})).await;
        phase(&mut client, SessionPhase::Interrupted).await;
        client.commands.send(Command::Quit).await.unwrap();
        client.join.await.unwrap();
        let replay = Replay::open(&config.journal, &config.cwd, &session, 0).unwrap();
        assert_eq!(
            replay.info.execution_result,
            Some(SessionPhase::Interrupted)
        );
        assert!(matches!(
            replay.latest_state().last_headless_action,
            Some(crate::interactions::HeadlessAction::InterruptForApproval {
                request_id: RpcId::Number(7),
                ..
            })
        ));
    }

    #[tokio::test]
    async fn headless_jsonl_declines_approvals_and_interrupts_secret_input_with_durable_reasons() {
        let fixture = JournalFixture::new();
        let (client, mut server, config, session) = journal_harness(&fixture).await;
        let bytes = JsonBuffer(Default::default());
        let output = crate::json_events::LiveOutput::start(
            &config.journal,
            &config.cwd,
            &session,
            bytes.clone(),
            Duration::from_secs(5),
        )
        .unwrap();
        let runner = tokio::spawn(crate::headless::run(
            client,
            vec![RootTaskSpec::input("PRIVATE_PROMPT".into())],
            false,
            Some(output),
        ));
        ready(&mut server).await;
        let start = next(&mut server).await;
        assert_eq!(start["method"], "turn/start");
        send(
            &mut server,
            json!({"id":start["id"],"result":{"turn":{"id":"headless-turn"}}}),
        )
        .await;
        send(&mut server, json!({"id":7,"method":"item/commandExecution/requestApproval","params":{"threadId":"root","turnId":"headless-turn","command":"PRIVATE_COMMAND"}})).await;
        let reply = next(&mut server).await;
        assert_eq!(reply["id"], 7);
        assert_eq!(reply["result"]["decision"], "decline");
        send(
            &mut server,
            json!({"method":"serverRequest/resolved","params":{"threadId":"root","requestId":7}}),
        )
        .await;
        send(&mut server, json!({"id":"input","method":"item/tool/requestUserInput","params":{"threadId":"root","turnId":"headless-turn","questions":[{"id":"q","header":"PRIVATE_HEADER","question":"PRIVATE_QUESTION","isSecret":true}]}})).await;
        let interrupt = next(&mut server).await;
        assert_eq!(interrupt["method"], "turn/interrupt");
        send(&mut server, json!({"id":interrupt["id"],"result":{}})).await;
        send(&mut server, json!({"method":"turn/completed","params":{"threadId":"root","turn":{"id":"headless-turn","status":"interrupted"}}})).await;
        let result = tokio::time::timeout(Duration::from_secs(4), runner)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(result.unwrap_err().0, 130);
        let text = String::from_utf8(bytes.0.lock().unwrap().clone()).unwrap();
        assert!(!text.contains("PRIVATE_"));
        assert!(text.contains("declineApproval") && text.contains("interruptForInput"));
        let replay = Replay::open(&config.journal, &config.cwd, &session, 0).unwrap();
        assert_eq!(
            replay.info.execution_result,
            Some(SessionPhase::Interrupted)
        );
        assert!(replay.latest_state().session_closed);
        assert_eq!(replay.latest_state().cleanup_confirmed, Some(true));
        assert_eq!(replay.latest_state().root_start_requests, 1);
    }

    #[tokio::test]
    async fn headless_jsonl_broken_writer_interrupts_the_owner_and_preserves_unknown() {
        struct Broken;
        impl std::io::Write for Broken {
            fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
                Err(std::io::ErrorKind::BrokenPipe.into())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let fixture = JournalFixture::new();
        let (mut client, mut server, config, session) = journal_harness(&fixture).await;
        running_root(&mut client, &mut server).await;
        let output = crate::json_events::LiveOutput::start(
            &config.journal,
            &config.cwd,
            &session,
            Broken,
            Duration::from_secs(5),
        )
        .unwrap();
        let runner = tokio::spawn(crate::headless::run(
            client,
            Vec::new(),
            false,
            Some(output),
        ));
        assert_eq!(next(&mut server).await["method"], "turn/interrupt");
        let result = tokio::time::timeout(Duration::from_secs(4), runner)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(result.unwrap_err().0, 4);
        let replay = Replay::open(&config.journal, &config.cwd, &session, 0).unwrap();
        assert_eq!(replay.info.execution_result, Some(SessionPhase::Unknown));
        assert_eq!(
            replay.latest_state().issue,
            Some(crate::journal::PersistenceIssue::OutputUnavailable)
        );
        assert_eq!(replay.latest_state().root_start_requests, 1);
        assert_eq!(replay.latest_state().cleanup_confirmed, Some(true));
        assert!(replay.info.needs_recovery);
    }

    #[tokio::test]
    async fn headless_jsonl_stalled_output_never_holds_core_cleanup_or_starts_another_root() {
        struct Held(std::sync::Arc<(std::sync::Mutex<bool>, std::sync::Condvar)>);
        impl std::io::Write for Held {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                let (lock, condition) = &*self.0;
                let mut released = lock.lock().unwrap();
                while !*released {
                    released = condition.wait(released).unwrap();
                }
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let fixture = JournalFixture::new();
        let (mut client, mut server, config, session) = journal_harness(&fixture).await;
        running_root(&mut client, &mut server).await;
        let mut snapshots = client.snapshots.clone();
        let gate = std::sync::Arc::new((std::sync::Mutex::new(false), std::sync::Condvar::new()));
        let output = crate::json_events::LiveOutput::start(
            &config.journal,
            &config.cwd,
            &session,
            Held(gate.clone()),
            Duration::from_millis(100),
        )
        .unwrap();
        let runner = tokio::spawn(crate::headless::run(
            client,
            Vec::new(),
            false,
            Some(output),
        ));
        tokio::time::timeout(
            Duration::from_secs(3),
            snapshots.wait_for(|s| s.phase == SessionPhase::Unknown),
        )
        .await
        .unwrap()
        .unwrap();
        // Core reached Unknown while stdout was still held by the consumer.
        assert_eq!(snapshots.borrow().root_start_requests, 1);
        *gate.0.lock().unwrap() = true;
        gate.1.notify_all();
        assert_eq!(next(&mut server).await["method"], "turn/interrupt");
        let result = tokio::time::timeout(Duration::from_secs(4), runner)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(result.unwrap_err().0, 4);
        let replay = Replay::open(&config.journal, &config.cwd, &session, 0).unwrap();
        assert_eq!(replay.info.execution_result, Some(SessionPhase::Unknown));
        assert_eq!(replay.latest_state().cleanup_confirmed, Some(true));
        assert_eq!(replay.latest_state().root_start_requests, 1);
    }

    #[tokio::test]
    async fn headless_request_actions_reject_stale_turns_before_declining() {
        let (mut client, mut server) = harness().await;
        running_root(&mut client, &mut server).await;
        observation_event(&mut client, &mut server, json!({"id":7,"method":"item/commandExecution/requestApproval","params":{"threadId":"root","turnId":"root-turn","command":"PRIVATE_COMMAND"}})).await;
        client
            .commands
            .send(Command::HeadlessRequest {
                request_id: RpcId::Number(7),
                thread_id: "root".into(),
                turn_id: "old-turn".into(),
            })
            .await
            .unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(50), next(&mut server))
                .await
                .is_err()
        );
        assert!(client.snapshots.borrow().last_headless_action.is_none());
        client
            .commands
            .send(Command::HeadlessRequest {
                request_id: RpcId::Number(7),
                thread_id: "root".into(),
                turn_id: "root-turn".into(),
            })
            .await
            .unwrap();
        assert_eq!(next(&mut server).await["result"]["decision"], "decline");
        client.commands.send(Command::Quit).await.unwrap();
        client.join.await.unwrap();
    }

    async fn observation_event(
        client: &mut ClientHandle,
        server: &mut BufReader<tokio::io::DuplexStream>,
        event: Value,
    ) {
        let raw = client.snapshots.borrow().observation.raw_message_count;
        send(server, event).await;
        client
            .snapshots
            .wait_for(|snapshot| snapshot.observation.raw_message_count > raw)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn queued_retry_retains_the_last_executed_attempt_until_actual_dispatch() {
        use crate::observation::{ActivityScope, ExecutionState};
        let (mut client, mut server) = harness().await;
        running_root(&mut client, &mut server).await;
        observation_event(&mut client, &mut server, json!({"method":"turn/completed","params":{"threadId":"root","turn":{"id":"root-turn","status":"interrupted"}}})).await;
        let attempt = client.snapshots.borrow().scheduler.tasks[0].id;
        client
            .commands
            .send(Command::Schedule(SchedulerCommand::PauseWorkflow))
            .await
            .unwrap();
        client
            .snapshots
            .wait_for(|snapshot| snapshot.scheduler.paused)
            .await
            .unwrap();
        client
            .commands
            .send(Command::Schedule(SchedulerCommand::Retry(attempt)))
            .await
            .unwrap();
        client
            .snapshots
            .wait_for(|snapshot| snapshot.scheduler.tasks[0].attempt == 2)
            .await
            .unwrap();
        {
            let snapshot = client.snapshots.borrow();
            let root = snapshot
                .observation
                .activities
                .iter()
                .find(|activity| {
                    activity.scope == ActivityScope::Turn && activity.identity.agent_id == "root"
                })
                .unwrap();
            assert_eq!(root.execution_state, ExecutionState::Interrupted);
            assert_eq!(root.identity.attempt_id, Some(1));
            assert_eq!(root.identity.turn_id.as_deref(), Some("root-turn"));
        }
        assert!(
            tokio::time::timeout(Duration::from_millis(50), next(&mut server))
                .await
                .is_err()
        );
        client
            .commands
            .send(Command::Schedule(SchedulerCommand::ResumeWorkflow))
            .await
            .unwrap();
        let start = next(&mut server).await;
        assert_eq!(start["method"], "turn/start");
        send(
            &mut server,
            json!({"id":start["id"],"result":{"turn":{"id":"retry-turn"}}}),
        )
        .await;
        phase(&mut client, SessionPhase::Running).await;
        assert!(client
            .snapshots
            .borrow()
            .observation
            .activities
            .iter()
            .any(|activity| activity.identity.attempt_id == Some(2)
                && activity.identity.turn_id.as_deref() == Some("retry-turn")));
        client.commands.send(Command::Quit).await.unwrap();
        client.join.await.unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn observation_uses_current_turn_evidence_and_preserves_execution_on_configuration() {
        use crate::observation::{ActivityScope, AttentionLevel, ConfigSource, ExecutionState};
        let (mut client, mut server) = harness().await;
        running_root(&mut client, &mut server).await;
        let initial = client
            .snapshots
            .borrow()
            .observation
            .accepted_evidence_count;
        for params in [
            json!({"threadId":"unrelated","turnId":"root-turn","item":{"type":"commandExecution"}}),
            json!({"threadId":"root","turnId":"old","item":{"type":"commandExecution"}}),
        ] {
            observation_event(
                &mut client,
                &mut server,
                json!({"method":"item/started","params":params}),
            )
            .await;
        }
        assert_eq!(client.snapshots.borrow().phase, SessionPhase::Running);
        assert_eq!(
            client
                .snapshots
                .borrow()
                .observation
                .accepted_evidence_count,
            initial
        );
        for id in ["one", "two"] {
            observation_event(&mut client, &mut server, json!({"method":"item/started","params":{"threadId":"root","turnId":"root-turn","item":{"id":id,"type":"commandExecution","status":"inProgress","command":"PRIVATE_COMMAND"}}})).await;
        }
        let after_tools = client
            .snapshots
            .borrow()
            .observation
            .accepted_evidence_count;
        observation_event(&mut client, &mut server, json!({"method":"item/started","params":{"threadId":"root","turnId":"root-turn","item":{"id":"one","type":"commandExecution"}}})).await;
        observation_event(&mut client, &mut server, json!({"method":"item/reasoning/textDelta","params":{"threadId":"root","turnId":"root-turn","itemId":"reasoning","delta":""}})).await;
        assert_eq!(
            client
                .snapshots
                .borrow()
                .observation
                .accepted_evidence_count,
            after_tools
        );
        client
            .commands
            .send(Command::ConfigureAttention {
                class: AttentionClass::Tool,
                quiet_ms: 10,
                attention_ms: 20,
            })
            .await
            .unwrap();
        client
            .snapshots
            .wait_for(|snapshot| snapshot.observation.settings.tool.source == ConfigSource::Tui)
            .await
            .unwrap();
        assert_eq!(
            client
                .snapshots
                .borrow()
                .observation
                .accepted_evidence_count,
            after_tools
        );
        tokio::time::advance(Duration::from_secs(1)).await;
        tokio::task::yield_now().await;
        observation_event(&mut client, &mut server, json!({"method":"item/commandExecution/outputDelta","params":{"threadId":"root","turnId":"root-turn","itemId":"one","delta":"PRIVATE_OUTPUT"}})).await;
        {
            let snapshot = client.snapshots.borrow();
            let activities = &snapshot.observation.activities;
            assert_eq!(
                activities
                    .iter()
                    .find(|activity| activity.item_id.as_deref() == Some("one"))
                    .unwrap()
                    .attention
                    .level,
                AttentionLevel::Active
            );
            assert_eq!(
                activities
                    .iter()
                    .find(|activity| activity.item_id.as_deref() == Some("two"))
                    .unwrap()
                    .attention
                    .level,
                AttentionLevel::AttentionNeeded
            );
            assert_eq!(snapshot.root_start_requests, 1);
            assert_eq!(snapshot.observation.snapshot_version, snapshot.version);
            let encoded = serde_json::to_string(&snapshot.observation).unwrap();
            assert!(!encoded.contains("PRIVATE_COMMAND") && !encoded.contains("PRIVATE_OUTPUT"));
        }
        for _ in 0..2 {
            observation_event(&mut client, &mut server, json!({"method":"item/completed","params":{"threadId":"root","turnId":"root-turn","item":{"id":"answer","type":"agentMessage","text":"final"}}})).await;
        }
        let finalized = client
            .snapshots
            .borrow()
            .observation
            .accepted_evidence_count;
        observation_event(&mut client, &mut server, json!({"method":"item/completed","params":{"threadId":"root","turnId":"root-turn","item":{"id":"answer","type":"agentMessage","text":"final"}}})).await;
        assert_eq!(
            client
                .snapshots
                .borrow()
                .observation
                .accepted_evidence_count,
            finalized
        );
        observation_event(&mut client, &mut server, json!({"method":"turn/completed","params":{"threadId":"root","turn":{"id":"root-turn","status":"interrupted"}}})).await;
        assert_eq!(
            client
                .snapshots
                .borrow()
                .observation
                .activities
                .iter()
                .find(|activity| activity.scope == ActivityScope::Turn)
                .unwrap()
                .execution_state,
            ExecutionState::Interrupted
        );
        assert!(
            tokio::time::timeout(Duration::from_millis(50), next(&mut server))
                .await
                .is_err()
        );
        client.commands.send(Command::Quit).await.unwrap();
        client.join.await.unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn attention_changes_and_child_output_cannot_release_a_paused_gate_or_hide_approval() {
        use crate::observation::{ActivityScope, AttentionLevel, ConfigSource};
        let (mut client, mut server) = harness().await;
        running_root(&mut client, &mut server).await;
        child(&mut server, "a", "a-1").await;
        child(&mut server, "b", "b-1").await;
        wait_call(&mut server, "observation-wait", vec![]).await;
        phase(&mut client, SessionPhase::GatePending).await;
        client
            .commands
            .send(Command::Schedule(SchedulerCommand::PauseWorkflow))
            .await
            .unwrap();
        client
            .snapshots
            .wait_for(|snapshot| snapshot.scheduler.paused)
            .await
            .unwrap();
        let initial = client
            .snapshots
            .borrow()
            .observation
            .accepted_evidence_count;
        client
            .commands
            .send(Command::ConfigureAttention {
                class: AttentionClass::Children,
                quiet_ms: 10,
                attention_ms: 20,
            })
            .await
            .unwrap();
        client
            .snapshots
            .wait_for(|snapshot| snapshot.observation.settings.children.source == ConfigSource::Tui)
            .await
            .unwrap();
        assert_eq!(
            client
                .snapshots
                .borrow()
                .observation
                .accepted_evidence_count,
            initial
        );
        tokio::time::advance(Duration::from_secs(130)).await;
        tokio::task::yield_now().await;
        observation_event(&mut client, &mut server, json!({"method":"item/agentMessage/delta","params":{"threadId":"a","turnId":"a-1","itemId":"a-message","delta":"activity"}})).await;
        {
            let snapshot = client.snapshots.borrow();
            let main = |agent: &str| {
                snapshot
                    .observation
                    .activities
                    .iter()
                    .find(|activity| {
                        activity.scope == ActivityScope::Turn && activity.identity.agent_id == agent
                    })
                    .unwrap()
            };
            assert_eq!(
                main("root").attention.level,
                AttentionLevel::AttentionNeeded
            );
            assert_eq!(main("a").attention.level, AttentionLevel::Active);
            assert_eq!(main("b").attention.level, AttentionLevel::AttentionNeeded);
            assert_eq!(main("root").wait_targets.len(), 2);
        }
        observation_event(&mut client, &mut server, json!({"id":"approval","method":"item/commandExecution/requestApproval","params":{"threadId":"b","turnId":"b-1","command":"PRIVATE"}})).await;
        observation_event(&mut client, &mut server, json!({"method":"item/reasoning/textDelta","params":{"threadId":"b","turnId":"b-1","itemId":"reasoning","delta":"activity"}})).await;
        assert!(client
            .snapshots
            .borrow()
            .observation
            .activities
            .iter()
            .any(
                |activity| activity.attention.requires_action && activity.identity.agent_id == "b"
            ));
        assert!(client.snapshots.borrow().gate.as_ref().unwrap().pending);
        assert_eq!(client.snapshots.borrow().root_start_requests, 1);
        assert!(
            tokio::time::timeout(Duration::from_millis(50), next(&mut server))
                .await
                .is_err()
        );
        client.commands.send(Command::Quit).await.unwrap();
        client.join.await.unwrap();
    }

    #[cfg(windows)]
    #[tokio::test]
    #[ignore = "requires Codex 0.159.2 and Python; all model traffic stays on localhost"]
    async fn live_app_server_gate_has_zero_provider_requests_while_a_child_is_pending() {
        use std::io::{Read, Write};
        struct FixtureHome(std::path::PathBuf);
        impl Drop for FixtureHome {
            fn drop(&mut self) {
                let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("target")
                    .canonicalize()
                    .unwrap();
                if let Ok(path) = self.0.canonicalize() {
                    if path.starts_with(&root)
                        && path
                            .file_name()
                            .unwrap()
                            .to_string_lossy()
                            .starts_with("gate-fixture-")
                    {
                        let _ = std::fs::remove_dir_all(path);
                    }
                }
            }
        }
        fn http(port: u16, method: &str, path: &str) -> Value {
            let mut stream = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            write!(
                stream,
                "{method} {path} HTTP/1.0\r\nHost: localhost\r\nContent-Length: 0\r\n\r\n"
            )
            .unwrap();
            let mut response = String::new();
            stream.read_to_string(&mut response).unwrap();
            let body = response.split_once("\r\n\r\n").unwrap().1;
            if body.is_empty() {
                Value::Null
            } else {
                serde_json::from_str(body).unwrap()
            }
        }
        let mut provider_command = tokio::process::Command::new(
            std::env::var_os("NATIVE_AGENT_TUI_PYTHON").unwrap_or_else(|| "python".into()),
        );
        provider_command
            .arg("-u")
            .arg(
                std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("tests/fixtures/gate_provider.py"),
            )
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::inherit())
            .kill_on_drop(true)
            .creation_flags(0x08000000);
        let mut provider = provider_command.spawn().unwrap();
        let mut port_line = String::new();
        let stdout = provider.stdout.take().unwrap();
        tokio::time::timeout(
            Duration::from_secs(5),
            BufReader::new(stdout).read_line(&mut port_line),
        )
        .await
        .unwrap()
        .unwrap();
        let port: u16 = port_line.trim().parse().unwrap();
        let home = FixtureHome(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("target")
                .join(format!("gate-fixture-{}", std::process::id())),
        );
        std::fs::create_dir(&home.0).unwrap();
        let journal_fixture = JournalFixture::new();
        let config = app_server::normalize_config(Config {
            journal: journal_fixture.settings(),
            windows_sandbox: Some("unelevated".into()),
            sandbox: "read-only".into(),
            model: Some("gpt-6.1-sol".into()),
            ..Default::default()
        })
        .unwrap();
        let catalog_output = tokio::process::Command::new(&config.executable)
            .env("CODEX_HOME", &home.0)
            .args(["debug", "models", "--bundled"])
            .output()
            .await
            .unwrap();
        assert!(catalog_output.status.success());
        let mut catalog: Value = serde_json::from_slice(&catalog_output.stdout).unwrap();
        for model in catalog["models"].as_array_mut().unwrap() {
            // The fixture serves ordinary Responses SSE, not the optional lite wire format.
            model["use_responses_lite"] = json!(false);
        }
        let catalog =
            app_server::DirectCatalog::from_bytes(&serde_json::to_vec(&catalog).unwrap()).unwrap();
        let mut command = crate::owned_process::Command::new(&config.executable);
        command
            .env("CODEX_HOME", &home.0)
            .env_remove("OPENAI_API_KEY")
            .env_remove("CODEX_API_KEY");
        command.args(["-c", &catalog.config_override()]);
        command.args(["-c", "windows.sandbox=\"unelevated\"", "-c", "model_provider=\"gate_fixture\"", "-c", &format!("model_providers.gate_fixture={{name=\"Gate fixture\",base_url=\"http://127.0.0.1:{port}/v1\",wire_api=\"responses\",requires_openai_auth=false}}"), "app-server", "--strict-config", "--listen", "stdio://"]).current_dir(&config.cwd);
        let observer = Observer::new(config.attention.clone());
        let initial = ClientHandle::initial(&config, &observer);
        let session = initial.observation.session_id.clone();
        let journal = Journal::open(
            &config.journal,
            &config.cwd,
            StoredSnapshot::capture(&initial),
        )
        .unwrap();
        let replay_config = config.clone();
        let mut server = AppServer::spawn_command(command).unwrap();
        let pipe = server.pipe.take().unwrap();
        let mut client =
            ClientHandle::start(pipe, config, Some(server), false, Some((observer, journal)));
        client
            .commands
            .send(Command::SubmitRootInput {
                text: "ROOT_TASK: Spawn the fixture worker, then use the registered wait tool."
                    .into(),
            })
            .await
            .unwrap();
        let observation: Result<(), String> = async {
            for round in 0..2u64 {
                let gate = tokio::time::timeout(
                    Duration::from_secs(20),
                    client.snapshots.wait_for(|s| {
                        s.phase == SessionPhase::GatePending
                            && s.gate.as_ref().is_some_and(|gate| {
                                gate.targets
                                    .iter()
                                    .any(|target| target.generation == round + 1)
                            })
                            || matches!(
                                s.phase,
                                SessionPhase::Failed
                                    | SessionPhase::Unknown
                                    | SessionPhase::Disconnected
                                    | SessionPhase::Completed
                            )
                    }),
                )
                .await
                .map_err(|_| "fixture did not reach GatePending".to_owned())?
                .map_err(|error| error.to_string())?
                .clone();
                if gate.phase != SessionPhase::GatePending || gate.agents.len() != 1 {
                    return Err(format!(
                        "expected one waiting child: phase={:?}, error={:?}",
                        gate.phase, gate.last_error
                    ));
                }
                tokio::time::timeout(Duration::from_secs(5), async {
                    while http(port, "GET", "/stats")["child_requests"] != round + 1 {
                        tokio::time::sleep(Duration::from_millis(50)).await;
                    }
                })
                .await
                .map_err(|_| "child provider request missing".to_owned())?;
                client.commands.send(Command::ConfigureAttention { class: AttentionClass::Children, quiet_ms: 10, attention_ms: 20 }).await.map_err(|error| error.to_string())?;
                let attention = tokio::time::timeout(Duration::from_secs(3), client.snapshots.wait_for(|snapshot| {
                    snapshot.observation.activities.iter().any(|activity| activity.identity.agent_id == "root"
                        && activity.kind == crate::observation::ActivityKind::WaitingChildren
                        && activity.attention.level == crate::observation::AttentionLevel::AttentionNeeded)
                })).await.map_err(|_| "Gate attention did not rise".to_owned())?.map_err(|error| error.to_string())?.clone();
                let root_progress = attention.observation.activities.iter().find(|activity| activity.identity.agent_id == "root" && activity.scope == crate::observation::ActivityScope::Turn).unwrap().progress_seq;
                // Sample AFTER a held interval: an early sample cannot prove the quiet window.
                tokio::time::sleep(Duration::from_millis(300)).await;
                let counts = http(port, "GET", "/stats");
                if counts["root_requests"] != 2 * (round + 1)
                    || counts["child_requests"] != round + 1
                    || counts["root_requests_while_child_held"] != 0
                {
                    return Err(format!("requests changed during Gate: {counts}"));
                }
                if client.snapshots.borrow().phase != SessionPhase::GatePending {
                    return Err("Gate released before the child response was released".into());
                }
                if client.snapshots.borrow().observation.activities.iter().find(|activity| activity.identity.agent_id == "root" && activity.scope == crate::observation::ActivityScope::Turn).unwrap().progress_seq != root_progress {
                    return Err("Observation ticking invented Gate evidence".into());
                }
                client
                    .commands
                    .send(Command::Schedule(SchedulerCommand::PauseWorkflow))
                    .await
                    .map_err(|error| error.to_string())?;
                tokio::time::timeout(
                    Duration::from_secs(3),
                    client.snapshots.wait_for(|s| s.scheduler.paused),
                )
                .await
                .map_err(|_| "pause was not applied".to_owned())?
                .map_err(|error| error.to_string())?;
                if round == 0 {
                    http(port, "POST", "/release/0");
                } else {
                    let task = client
                        .snapshots
                        .borrow()
                        .scheduler
                        .tasks
                        .iter()
                        .find(|task| task.kind == TaskKind::NativeChild)
                        .unwrap()
                        .id;
                    client
                        .commands
                        .send(Command::Schedule(SchedulerCommand::Cancel(task)))
                        .await
                        .map_err(|error| error.to_string())?;
                }
                let terminal = tokio::time::timeout(
                    Duration::from_secs(5),
                    client.snapshots.wait_for(|s| {
                        s.agents[0].outcome.is_some()
                            || matches!(s.phase, SessionPhase::Unknown | SessionPhase::Disconnected)
                    }),
                )
                .await
                .map_err(|_| "child did not terminate after completion/interrupt".to_owned())?
                .map_err(|error| error.to_string())?
                .clone();
                let expected = if round == 0 {
                    ChildOutcome::Completed
                } else {
                    ChildOutcome::Interrupted
                };
                if terminal.agents[0].outcome != Some(expected)
                    || terminal.phase != SessionPhase::GatePending
                {
                    return Err(format!(
                        "wrong child terminal fact while paused: {:?}; {:?}",
                        terminal.phase, terminal.agents[0].outcome
                    ));
                }
                tokio::time::sleep(Duration::from_millis(300)).await;
                if http(port, "GET", "/stats")["root_requests"] != 2 * (round + 1) {
                    return Err("ready Gate resumed the parent while paused".into());
                }
                if round == 1 {
                    http(port, "POST", "/release/1");
                }
                client
                    .commands
                    .send(Command::Schedule(SchedulerCommand::ResumeWorkflow))
                    .await
                    .map_err(|error| error.to_string())?;
            }
            let done = tokio::time::timeout(
                Duration::from_secs(10),
                client.snapshots.wait_for(|s| {
                    matches!(
                        s.phase,
                        SessionPhase::Completed
                            | SessionPhase::Unknown
                            | SessionPhase::Failed
                            | SessionPhase::Disconnected
                    )
                }),
            )
            .await
            .map_err(|_| "root did not finish".to_owned())?
            .map_err(|error| error.to_string())?
            .clone();
            if done.phase != SessionPhase::Completed
                || !done
                    .messages
                    .iter()
                    .any(|message| message.text == "GATE_DONE")
                || http(port, "GET", "/stats")["root_requests"] != 5
            {
                return Err(format!(
                    "root failed to resume: {:?}; {:?}",
                    done.phase, done.last_error
                ));
            }
            let tasks = crate::scheduler::WorkflowPlan::parse(br#"{"tasks":[{"id":100,"text":"WORKFLOW_FIRST"},{"id":101,"text":"WORKFLOW_SECOND","dependencies":[100]}]}"#)?.tasks;
            client.commands.send(Command::QueueRootTasks { tasks }).await.map_err(|error| error.to_string())?;
            let workflow = tokio::time::timeout(Duration::from_secs(10), client.snapshots.wait_for(|s| s.scheduler.tasks.iter().filter(|task| task.id.0 >= 100).filter(|task| task.state == crate::scheduler::TaskState::Succeeded).count() == 2 || matches!(s.phase, SessionPhase::Unknown | SessionPhase::Failed | SessionPhase::Disconnected))).await.map_err(|_| "sequential workflow did not complete".to_owned())?.map_err(|error| error.to_string())?.clone();
            if workflow.phase != SessionPhase::Completed || workflow.root_start_requests != 3 || http(port, "GET", "/stats")["root_requests"] != 7 {
                return Err(format!("workflow did not dispatch exactly two more turns: {:?}; {:?}", workflow.phase, workflow.last_error));
            }
            Ok(())
        }
        .await;
        let counts = http(port, "GET", "/stats");
        // Clean up the owned server and provider before reporting any assertion failure.
        http(port, "POST", "/release/0");
        http(port, "POST", "/release/1");
        let _ = client.commands.send(Command::Quit).await;
        let exit = client.join.await;
        provider.stdin.take();
        let provider_exit = tokio::time::timeout(Duration::from_secs(3), provider.wait()).await;
        observation.unwrap_or_else(|error| panic!("{error}; provider counters: {counts}"));
        let exit = exit.unwrap();
        assert!(exit.cleanup_error.is_none() && exit.journal_error.is_none());
        let replay = Replay::open(&replay_config.journal, &replay_config.cwd, &session, 0).unwrap();
        assert!(replay.info.session_closed && !replay.info.needs_recovery);
        assert_eq!(replay.info.execution_result, Some(SessionPhase::Completed));
        assert_eq!(replay.latest_state().root_start_requests, 3);
        let mut bytes = Vec::new();
        replay.write_jsonl(&mut bytes).unwrap();
        let text = String::from_utf8(bytes).unwrap();
        assert!(
            !text.contains("ROOT_TASK:")
                && !text.contains("WORKFLOW_FIRST")
                && !text.contains("GATE_DONE")
        );
        assert!(provider_exit.unwrap().unwrap().success());
    }

    #[tokio::test]
    async fn paused_gate_observes_children_and_approvals_but_replies_only_after_resume() {
        use crate::scheduler::TaskState;
        let (mut client, mut server) = harness().await;
        running_root(&mut client, &mut server).await;
        child(&mut server, "a", "a-1").await;
        wait_call(&mut server, "pause-gate", vec![]).await;
        phase(&mut client, SessionPhase::GatePending).await;
        assert_eq!(client.snapshots.borrow().scheduler.root_slots_reserved, 0);
        client
            .commands
            .send(Command::Schedule(SchedulerCommand::PauseWorkflow))
            .await
            .unwrap();
        client
            .snapshots
            .wait_for(|s| s.scheduler.paused)
            .await
            .unwrap();
        send(&mut server, json!({"id":"approval-paused","method":"item/commandExecution/requestApproval","params":{"threadId":"a","turnId":"a-1","itemId":"shell","command":"echo test","cwd":"."}})).await;
        client
            .snapshots
            .wait_for(|s| !s.requests.is_empty())
            .await
            .unwrap();
        client
            .commands
            .send(Command::AnswerApproval {
                request_id: RpcId::String("approval-paused".into()),
                decision: ApprovalDecision::Accept,
            })
            .await
            .unwrap();
        assert_eq!(next(&mut server).await["id"], "approval-paused");
        send(&mut server, json!({"method":"turn/completed","params":{"threadId":"a","turn":{"id":"a-1","status":"completed"}}})).await;
        client
            .snapshots
            .wait_for(|s| s.agents[0].outcome.is_some())
            .await
            .unwrap();
        assert_eq!(
            client.snapshots.borrow().scheduler.tasks[1].state,
            TaskState::Succeeded
        );
        assert!(client.snapshots.borrow().gate.as_ref().unwrap().pending);
        assert!(
            tokio::time::timeout(Duration::from_millis(50), next(&mut server))
                .await
                .is_err()
        );
        client
            .commands
            .send(Command::Schedule(SchedulerCommand::ResumeWorkflow))
            .await
            .unwrap();
        assert_eq!(next(&mut server).await["id"], "pause-gate");
        phase(&mut client, SessionPhase::Running).await;
        assert_eq!(client.snapshots.borrow().scheduler.root_slots_reserved, 1);
        client.commands.send(Command::Quit).await.unwrap();
        client.join.await.unwrap();
    }

    #[tokio::test]
    async fn native_cancel_is_deferred_until_identity_and_acknowledgement_never_releases_gate() {
        use crate::scheduler::TaskState;
        let (mut client, mut server) = harness().await;
        running_root(&mut client, &mut server).await;
        send(&mut server, json!({"method":"thread/started","params":{"thread":{"id":"a","parentThreadId":"root"}}})).await;
        let task = client
            .snapshots
            .wait_for(|s| s.scheduler.tasks.len() == 2)
            .await
            .unwrap()
            .scheduler
            .tasks[1]
            .id;
        client
            .commands
            .send(Command::Schedule(SchedulerCommand::Cancel(task)))
            .await
            .unwrap();
        client
            .snapshots
            .wait_for(|s| s.scheduler.tasks[1].cancel_requested)
            .await
            .unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(50), next(&mut server))
                .await
                .is_err()
        );
        send(
            &mut server,
            json!({"method":"turn/started","params":{"threadId":"a","turn":{"id":"a-1"}}}),
        )
        .await;
        let interrupt = next(&mut server).await;
        assert_eq!(interrupt["method"], "turn/interrupt");
        assert_eq!(interrupt["params"], json!({"threadId":"a","turnId":"a-1"}));
        send(&mut server, json!({"id":interrupt["id"],"result":{}})).await;
        wait_call(&mut server, "cancel-child", vec![]).await;
        phase(&mut client, SessionPhase::GatePending).await;
        assert_eq!(
            client.snapshots.borrow().scheduler.tasks[1].state,
            TaskState::Cancelling
        );
        assert_eq!(client.snapshots.borrow().agents[0].outcome, None);
        assert!(
            tokio::time::timeout(Duration::from_millis(50), next(&mut server))
                .await
                .is_err()
        );
        send(&mut server, json!({"method":"turn/completed","params":{"threadId":"a","turn":{"id":"a-1","status":"interrupted"}}})).await;
        assert_eq!(next(&mut server).await["id"], "cancel-child");
        phase(&mut client, SessionPhase::Running).await;
        assert_eq!(
            client.snapshots.borrow().scheduler.tasks[1].state,
            TaskState::Cancelled
        );
        client.commands.send(Command::Quit).await.unwrap();
        client.join.await.unwrap();
    }

    #[tokio::test]
    async fn dag_dispatch_waits_for_preflight_pause_and_dependencies_and_uses_new_retry_attempts() {
        use crate::scheduler::{TaskId, TaskState};
        let (mut client, mut server) = harness().await;
        let mut first = RootTaskSpec::input("first".into());
        first.id = Some(TaskId(1));
        let mut second = RootTaskSpec::input("second".into());
        second.id = Some(TaskId(2));
        second.dependencies.push(TaskId(1));
        second.priority = 1000;
        client
            .commands
            .send(Command::Schedule(SchedulerCommand::PauseWorkflow))
            .await
            .unwrap();
        client
            .commands
            .send(Command::QueueRootTasks {
                tasks: vec![second, first],
            })
            .await
            .unwrap();
        let preflight = initialized(&mut server).await;
        client
            .snapshots
            .wait_for(|s| s.scheduler.tasks.len() == 2)
            .await
            .unwrap();
        assert_eq!(client.snapshots.borrow().root_start_requests, 0);
        send(&mut server, json!({"id":preflight["id"],"result":{"exitCode":0,"stdout":"native-agent-tui-shell-ok"}})).await;
        phase(&mut client, SessionPhase::Ready).await;
        assert!(
            tokio::time::timeout(Duration::from_millis(50), next(&mut server))
                .await
                .is_err()
        );
        client
            .commands
            .send(Command::Schedule(SchedulerCommand::ResumeWorkflow))
            .await
            .unwrap();
        let first = next(&mut server).await;
        assert_eq!(first["params"]["input"][0]["text"], "first");
        send(
            &mut server,
            json!({"id":first["id"],"result":{"turn":{"id":"one"}}}),
        )
        .await;
        phase(&mut client, SessionPhase::Running).await;
        send(&mut server, json!({"method":"turn/completed","params":{"threadId":"root","turn":{"id":"one","status":"failed"}}})).await;
        phase(&mut client, SessionPhase::Failed).await;
        assert_eq!(
            client.snapshots.borrow().scheduler.tasks[1].state,
            TaskState::Blocked
        );
        client
            .commands
            .send(Command::Schedule(SchedulerCommand::Retry(TaskId(1))))
            .await
            .unwrap();
        let retry = next(&mut server).await;
        assert_eq!(retry["params"]["input"][0]["text"], "first");
        send(
            &mut server,
            json!({"id":retry["id"],"result":{"turn":{"id":"one-retry"}}}),
        )
        .await;
        phase(&mut client, SessionPhase::Running).await;
        assert_eq!(
            client
                .snapshots
                .borrow()
                .scheduler
                .active_root
                .unwrap()
                .attempt,
            2
        );
        client
            .commands
            .send(Command::ScheduleTask {
                attempt: TaskAttempt {
                    task: TaskId(1),
                    attempt: 1,
                },
                command: SchedulerCommand::Cancel(TaskId(1)),
            })
            .await
            .unwrap();
        tokio::time::timeout(
            Duration::from_secs(3),
            client.snapshots.wait_for(|s| {
                s.notice
                    .as_deref()
                    .is_some_and(|notice| notice.contains("attempt changed"))
            }),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(
            client.snapshots.borrow().scheduler.tasks[0].state,
            TaskState::Running
        );
        send(&mut server, json!({"method":"turn/completed","params":{"threadId":"root","turn":{"id":"one","status":"completed"}}})).await;
        assert!(
            tokio::time::timeout(Duration::from_millis(50), next(&mut server))
                .await
                .is_err()
        );
        send(&mut server, json!({"method":"turn/completed","params":{"threadId":"root","turn":{"id":"one-retry","status":"completed"}}})).await;
        let second = next(&mut server).await;
        assert_eq!(second["params"]["input"][0]["text"], "second");
        client.commands.send(Command::Quit).await.unwrap();
        client.join.await.unwrap();
    }

    #[tokio::test]
    async fn stopping_cancels_the_root_and_each_observed_child_without_a_writer_burst() {
        use crate::scheduler::TaskState;
        let (mut client, mut server) = harness().await;
        running_root(&mut client, &mut server).await;
        for name in ["a", "b", "c"] {
            child(&mut server, name, &format!("{name}-1")).await;
        }
        wait_call(&mut server, "stop-gate", vec![]).await;
        phase(&mut client, SessionPhase::GatePending).await;
        client
            .commands
            .send(Command::Schedule(SchedulerCommand::StopWorkflow))
            .await
            .unwrap();
        let root_interrupt = next(&mut server).await;
        assert_eq!(root_interrupt["params"]["threadId"], "root");
        send(&mut server, json!({"id":root_interrupt["id"],"result":{}})).await;
        for name in ["a", "b", "c"] {
            let interrupt = next(&mut server).await;
            assert_eq!(interrupt["method"], "turn/interrupt");
            assert_eq!(interrupt["params"]["threadId"], name);
            send(&mut server, json!({"id":interrupt["id"],"result":{}})).await;
            send(&mut server, json!({"method":"turn/completed","params":{"threadId":name,"turn":{"id":format!("{name}-1"),"status":"interrupted"}}})).await;
        }
        send(&mut server, json!({"method":"turn/completed","params":{"threadId":"root","turn":{"id":"root-turn","status":"interrupted"}}})).await;
        phase(&mut client, SessionPhase::Interrupted).await;
        assert!(client.snapshots.borrow().scheduler.stopping);
        assert!(client
            .snapshots
            .borrow()
            .scheduler
            .tasks
            .iter()
            .all(|task| task.state == TaskState::Cancelled));
        assert_eq!(client.snapshots.borrow().root_start_requests, 1);
        assert_eq!(
            client
                .snapshots
                .borrow()
                .gate
                .as_ref()
                .unwrap()
                .root_starts_at_release,
            None
        );
        // A late spawn is also stopped once its real external turn arrives.
        child(&mut server, "late", "late-1").await;
        let interrupt = next(&mut server).await;
        assert_eq!(interrupt["params"]["threadId"], "late");
        send(&mut server, json!({"id":interrupt["id"],"result":{}})).await;
        client.commands.send(Command::Quit).await.unwrap();
        client.join.await.unwrap();
    }

    async fn running_root(
        client: &mut ClientHandle,
        server: &mut BufReader<tokio::io::DuplexStream>,
    ) {
        ready(server).await;
        phase(client, SessionPhase::Ready).await;
        client
            .commands
            .send(Command::SubmitRootInput {
                text: "coordinate".into(),
            })
            .await
            .unwrap();
        let start = next(server).await;
        send(
            server,
            json!({"id":start["id"],"result":{"turn":{"id":"root-turn"}}}),
        )
        .await;
        phase(client, SessionPhase::Running).await;
    }
    async fn child(server: &mut BufReader<tokio::io::DuplexStream>, id: &str, turn: &str) {
        send(server, json!({"method":"thread/started","params":{"thread":{"id":id,"parentThreadId":"root","source":{"subAgent":{"thread_spawn":{"parent_thread_id":"root","depth":1,"agent_path":format!("/root/{id}")}}}}}})).await;
        send(
            server,
            json!({"method":"turn/started","params":{"threadId":id,"turn":{"id":turn}}}),
        )
        .await;
    }
    async fn wait_call(
        server: &mut BufReader<tokio::io::DuplexStream>,
        id: &str,
        targets: Vec<&str>,
    ) {
        send(server, json!({"id":id,"method":"item/tool/call","params":{"threadId":"root","turnId":"root-turn","callId":id,"tool":protocol::WAIT_TOOL,"arguments":{"targets":targets}}})).await;
    }

    async fn activity(
        server: &mut BufReader<tokio::io::DuplexStream>,
        method: &str,
        item: &str,
        kind: &str,
    ) {
        send(server, json!({"method":method,"params":{"threadId":"root","turnId":"root-turn","item":{"id":item,"type":"subAgentActivity","agentThreadId":"a","agentPath":"/root/a","kind":kind}}})).await;
    }

    #[tokio::test]
    async fn native_activity_defers_identity_read_and_keeps_completed_child_pending_until_confirmed(
    ) {
        let (mut client, mut server) = harness().await;
        running_root(&mut client, &mut server).await;
        activity(&mut server, "item/started", "spawn-a", "started").await;
        activity(&mut server, "item/completed", "spawn-a", "started").await;
        wait_call(&mut server, "native-wait", vec![]).await;
        phase(&mut client, SessionPhase::GatePending).await;
        assert!(
            tokio::time::timeout(Duration::from_millis(50), next(&mut server))
                .await
                .is_err()
        );
        send(
            &mut server,
            json!({"method":"turn/started","params":{"threadId":"a","turn":{"id":"a-1"}}}),
        )
        .await;
        let identity = next(&mut server).await;
        assert_eq!(identity["method"], "thread/read");
        assert_eq!(identity["params"]["includeTurns"], false);
        send(&mut server, json!({"method":"item/agentMessage/delta","params":{"threadId":"a","turnId":"a-1","itemId":"shared-item","delta":"child partial"}})).await;
        send(&mut server, json!({"method":"item/completed","params":{"threadId":"a","turnId":"a-1","item":{"type":"agentMessage","id":"shared-item","text":"CHILD_DONE"}}})).await;
        send(&mut server, json!({"method":"turn/completed","params":{"threadId":"a","turn":{"id":"a-1","status":"completed"}}})).await;
        assert!(
            tokio::time::timeout(Duration::from_millis(50), next(&mut server))
                .await
                .is_err()
        );
        assert_eq!(client.snapshots.borrow().phase, SessionPhase::GatePending);
        send(
            &mut server,
            json!({"id":identity["id"],"result":{"thread":{"id":"a","parentThreadId":"root"}}}),
        )
        .await;
        assert_eq!(next(&mut server).await["id"], "native-wait");
        phase(&mut client, SessionPhase::Running).await;
        let snapshot = client.snapshots.borrow().clone();
        assert!(snapshot.agents[0].info.confirmed);
        assert_eq!(
            snapshot
                .messages
                .iter()
                .filter(|message| message.thread_id == "a")
                .count(),
            1
        );
        assert_eq!(
            snapshot
                .messages
                .iter()
                .find(|message| message.thread_id == "a")
                .unwrap()
                .text,
            "CHILD_DONE"
        );
        activity(&mut server, "item/completed", "child-done", "completed").await;
        wait_call(&mut server, "again", vec!["a"]).await;
        assert_eq!(next(&mut server).await["id"], "again");
        client.commands.send(Command::Quit).await.unwrap();
        client.join.await.unwrap();
    }

    #[tokio::test]
    async fn native_followup_handles_both_start_orders_and_duplicate_activity_without_rearming_twice(
    ) {
        for started_before_completed in [false, true] {
            let (mut client, mut server) = harness().await;
            running_root(&mut client, &mut server).await;
            child(&mut server, "a", "old").await;
            send(&mut server, json!({"method":"turn/completed","params":{"threadId":"a","turn":{"id":"old","status":"completed"}}})).await;
            activity(&mut server, "item/started", "followup", "interacted").await;
            if started_before_completed {
                send(
                    &mut server,
                    json!({"method":"turn/started","params":{"threadId":"a","turn":{"id":"new"}}}),
                )
                .await;
            }
            activity(&mut server, "item/completed", "followup", "interacted").await;
            activity(&mut server, "item/started", "followup", "interacted").await;
            activity(&mut server, "item/completed", "followup", "interacted").await;
            wait_call(&mut server, "wait-followup", vec!["a"]).await;
            phase(&mut client, SessionPhase::GatePending).await;
            send(&mut server, json!({"method":"turn/completed","params":{"threadId":"a","turn":{"id":"old","status":"completed"}}})).await;
            assert!(
                tokio::time::timeout(Duration::from_millis(50), next(&mut server))
                    .await
                    .is_err()
            );
            if !started_before_completed {
                send(
                    &mut server,
                    json!({"method":"turn/started","params":{"threadId":"a","turn":{"id":"new"}}}),
                )
                .await;
            }
            send(&mut server, json!({"method":"turn/completed","params":{"threadId":"a","turn":{"id":"new","status":"completed"}}})).await;
            let result = next(&mut server).await;
            let data: Value = serde_json::from_str(
                result["result"]["contentItems"][0]["text"]
                    .as_str()
                    .unwrap(),
            )
            .unwrap();
            assert_eq!(data["targets"][0]["turnId"], "new");
            assert_eq!(data["targets"][0]["generation"], 2);
            client.commands.send(Command::Quit).await.unwrap();
            client.join.await.unwrap();
        }
    }

    #[tokio::test(start_paused = true)]
    async fn gate_has_no_deadline_and_root_input_remains_queued_after_an_hour() {
        let (mut client, mut server) = harness().await;
        running_root(&mut client, &mut server).await;
        child(&mut server, "a", "a-1").await;
        wait_call(&mut server, "long-wait", vec![]).await;
        phase(&mut client, SessionPhase::GatePending).await;
        let version = client.snapshots.borrow().version;
        let evidence = client
            .snapshots
            .borrow()
            .observation
            .accepted_evidence_count;
        tokio::time::advance(Duration::from_secs(3600)).await;
        tokio::task::yield_now().await;
        {
            let snapshot = client.snapshots.borrow();
            assert!(
                snapshot.version > version && snapshot.version <= version + 2,
                "Missed ticks must not cause a burst"
            );
            assert_eq!(snapshot.observation.accepted_evidence_count, evidence);
            assert_eq!(snapshot.observation.snapshot_version, snapshot.version);
            let root = snapshot
                .observation
                .activities
                .iter()
                .find(|activity| activity.identity.agent_id == "root")
                .unwrap();
            assert_eq!(
                root.attention.level,
                crate::observation::AttentionLevel::AttentionNeeded
            );
            assert!(snapshot.gate.as_ref().unwrap().pending);
        }
        client
            .commands
            .send(Command::SubmitRootInput {
                text: "later".into(),
            })
            .await
            .unwrap();
        client
            .snapshots
            .wait_for(|s| s.queued_inputs == 1)
            .await
            .unwrap();
        assert_eq!(client.snapshots.borrow().phase, SessionPhase::GatePending);
        assert_eq!(client.snapshots.borrow().root_start_requests, 1);
        assert!(
            tokio::time::timeout(Duration::from_millis(50), next(&mut server))
                .await
                .is_err()
        );
        send(&mut server, json!({"method":"turn/completed","params":{"threadId":"a","turn":{"id":"a-1","status":"completed"}}})).await;
        assert_eq!(next(&mut server).await["id"], "long-wait");
        client.commands.send(Command::Quit).await.unwrap();
        client.join.await.unwrap();
    }

    #[tokio::test]
    async fn interrupt_and_disconnect_cancel_the_gate_without_fabricating_child_completion() {
        for disconnect in [false, true] {
            let (mut client, mut server) = harness().await;
            running_root(&mut client, &mut server).await;
            child(&mut server, "a", "a-1").await;
            wait_call(&mut server, "cancel-wait", vec![]).await;
            phase(&mut client, SessionPhase::GatePending).await;
            if disconnect {
                drop(server);
                assert_eq!(
                    client.join.await.unwrap().final_phase,
                    SessionPhase::Disconnected
                );
            } else {
                client
                    .commands
                    .send(Command::SubmitRootInput {
                        text: "later".into(),
                    })
                    .await
                    .unwrap();
                client
                    .snapshots
                    .wait_for(|s| s.queued_inputs == 1)
                    .await
                    .unwrap();
                client.commands.send(Command::Interrupt).await.unwrap();
                let interrupt = next(&mut server).await;
                assert_eq!(interrupt["method"], "turn/interrupt");
                send(&mut server, json!({"id":interrupt["id"],"result":{}})).await;
                send(&mut server, json!({"method":"turn/completed","params":{"threadId":"root","turn":{"id":"root-turn","status":"interrupted"}}})).await;
                phase(&mut client, SessionPhase::Interrupted).await;
                assert_eq!(client.snapshots.borrow().queued_inputs, 1);
                assert_eq!(
                    client
                        .snapshots
                        .borrow()
                        .scheduler
                        .tasks
                        .last()
                        .unwrap()
                        .state,
                    crate::scheduler::TaskState::Blocked
                );
                assert!(
                    tokio::time::timeout(Duration::from_millis(50), next(&mut server))
                        .await
                        .is_err()
                );
                client.commands.send(Command::Quit).await.unwrap();
                client.join.await.unwrap();
            }
            let snapshot = client.snapshots.borrow();
            assert_eq!(snapshot.agents[0].outcome, None);
            assert!(!snapshot.gate.as_ref().unwrap().pending);
            assert_eq!(snapshot.gate.as_ref().unwrap().root_starts_at_release, None);
        }
    }

    #[tokio::test]
    async fn invalid_wait_arguments_and_non_direct_targets_are_rejected_without_another_root_turn()
    {
        let (mut client, mut server) = harness().await;
        running_root(&mut client, &mut server).await;
        child(&mut server, "a", "a-1").await;
        send(&mut server, json!({"method":"thread/started","params":{"thread":{"id":"grandchild","parentThreadId":"a"}}})).await;
        for (id, arguments) in [
            ("unknown", json!({"targets":["missing"]})),
            ("grandchild", json!({"targets":["grandchild"]})),
            ("malformed", json!({"targets":[],"timeout":1})),
        ] {
            send(&mut server, json!({"id":id,"method":"item/tool/call","params":{"threadId":"root","turnId":"root-turn","callId":id,"tool":protocol::WAIT_TOOL,"arguments":arguments}})).await;
            let result = next(&mut server).await;
            assert_eq!(result["id"], id);
            assert_eq!(result["error"]["code"], -32602);
        }
        assert_eq!(client.snapshots.borrow().root_start_requests, 1);
        client.commands.send(Command::Quit).await.unwrap();
        client.join.await.unwrap();
    }

    #[tokio::test]
    async fn failed_child_metadata_hydration_closes_execution_with_an_unknown_outcome() {
        let (mut client, mut server) = harness().await;
        running_root(&mut client, &mut server).await;
        activity(&mut server, "item/completed", "spawn", "started").await;
        wait_call(&mut server, "unconfirmed-wait", vec![]).await;
        phase(&mut client, SessionPhase::GatePending).await;
        send(
            &mut server,
            json!({"method":"turn/started","params":{"threadId":"a","turn":{"id":"a-1"}}}),
        )
        .await;
        let identity = next(&mut server).await;
        send(
            &mut server,
            json!({"id":identity["id"],"error":{"code":-1,"message":"metadata unavailable"}}),
        )
        .await;
        let exit = client.join.await.unwrap();
        assert_eq!(exit.final_phase, SessionPhase::Unknown);
        assert_eq!(client.snapshots.borrow().agents[0].outcome, None);
        assert!(!client.snapshots.borrow().gate.as_ref().unwrap().pending);
    }

    #[tokio::test]
    async fn authoritative_metadata_rejects_a_captured_child_with_a_non_direct_parent() {
        for notification in [false, true] {
            let (mut client, mut server) = harness().await;
            running_root(&mut client, &mut server).await;
            child(&mut server, "b", "b-1").await;
            activity(&mut server, "item/completed", "spawn-a", "started").await;
            wait_call(&mut server, "parent-check", vec![]).await;
            phase(&mut client, SessionPhase::GatePending).await;
            send(
                &mut server,
                json!({"method":"turn/started","params":{"threadId":"a","turn":{"id":"a-1"}}}),
            )
            .await;
            let identity = next(&mut server).await;
            for child in ["a", "b"] {
                send(&mut server, json!({"method":"turn/completed","params":{"threadId":child,"turn":{"id":format!("{child}-1"),"status":"completed"}}})).await;
            }
            let metadata = json!({"thread":{"id":"a","parentThreadId":"b"}});
            if notification {
                send(
                    &mut server,
                    json!({"method":"thread/started","params":metadata}),
                )
                .await;
            } else {
                send(&mut server, json!({"id":identity["id"],"result":metadata})).await;
            }
            let result = next(&mut server).await;
            assert_eq!(result["id"], "parent-check");
            assert_eq!(result["error"]["code"], -32602);
            phase(&mut client, SessionPhase::Running).await;
            assert_eq!(
                client
                    .snapshots
                    .borrow()
                    .gate
                    .as_ref()
                    .unwrap()
                    .root_starts_at_release,
                None
            );
            wait_call(&mut server, "parent-check", vec![]).await;
            assert!(
                tokio::time::timeout(Duration::from_millis(50), next(&mut server))
                    .await
                    .is_err()
            );
            client.commands.send(Command::Quit).await.unwrap();
            client.join.await.unwrap();
        }
    }

    #[tokio::test]
    async fn failure_in_eight_children_releases_early_and_remaining_approvals_survive_root_completion(
    ) {
        let (mut client, mut server) = harness().await;
        running_root(&mut client, &mut server).await;
        for index in 0..8 {
            child(&mut server, &format!("c{index}"), &format!("c{index}-1")).await;
        }
        wait_call(&mut server, "eight", vec![]).await;
        phase(&mut client, SessionPhase::GatePending).await;
        send(&mut server, json!({"method":"turn/completed","params":{"threadId":"c0","turn":{"id":"c0-1","status":"failed"}}})).await;
        let result = next(&mut server).await;
        let data: Value = serde_json::from_str(
            result["result"]["contentItems"][0]["text"]
                .as_str()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(data["targets"].as_array().unwrap().len(), 8);
        assert_eq!(data["targets"][0]["outcome"], "failed");
        assert_eq!(data["targets"][7]["outcome"], Value::Null);
        send(&mut server, json!({"method":"turn/completed","params":{"threadId":"root","turn":{"id":"root-turn","status":"completed"}}})).await;
        phase(&mut client, SessionPhase::Completed).await;
        send(&mut server, json!({"id":"remaining-approval","method":"item/commandExecution/requestApproval","params":{"threadId":"c7","turnId":"c7-1","command":"test"}})).await;
        client
            .snapshots
            .wait_for(|s| s.requests.len() == 1)
            .await
            .unwrap();
        client
            .commands
            .send(Command::AnswerApproval {
                request_id: RpcId::String("remaining-approval".into()),
                decision: ApprovalDecision::Decline,
            })
            .await
            .unwrap();
        let approval = next(&mut server).await;
        assert_eq!(approval["id"], "remaining-approval");
        assert_eq!(approval["result"]["decision"], "decline");
        assert_eq!(client.snapshots.borrow().root_start_requests, 1);
        client.commands.send(Command::Quit).await.unwrap();
        client.join.await.unwrap();
    }

    #[tokio::test]
    async fn reused_wait_ids_in_a_new_root_turn_are_not_mistaken_for_old_deliveries() {
        let (mut client, mut server) = harness().await;
        running_root(&mut client, &mut server).await;
        wait_call(&mut server, "reused", vec![]).await;
        assert_eq!(next(&mut server).await["id"], "reused");
        send(&mut server, json!({"method":"turn/completed","params":{"threadId":"root","turn":{"id":"root-turn","status":"completed"}}})).await;
        phase(&mut client, SessionPhase::Completed).await;
        client
            .commands
            .send(Command::SubmitRootInput {
                text: "new root turn".into(),
            })
            .await
            .unwrap();
        let start = next(&mut server).await;
        send(
            &mut server,
            json!({"id":start["id"],"result":{"turn":{"id":"root-next"}}}),
        )
        .await;
        phase(&mut client, SessionPhase::Running).await;
        send(&mut server, json!({"id":"reused","method":"item/tool/call","params":{"threadId":"root","turnId":"root-next","callId":"reused","tool":protocol::WAIT_TOOL,"arguments":{"targets":[]}}})).await;
        assert_eq!(next(&mut server).await["result"]["success"], true);
        wait_call(&mut server, "reused", vec![]).await; // A late old-turn delivery still gets no second reply.
        assert!(
            tokio::time::timeout(Duration::from_millis(50), next(&mut server))
                .await
                .is_err()
        );
        client.commands.send(Command::Quit).await.unwrap();
        client.join.await.unwrap();
    }

    #[tokio::test]
    async fn gate_keeps_root_requests_zero_routes_child_approvals_and_queues_root_input() {
        let (mut client, mut server) = harness().await;
        running_root(&mut client, &mut server).await;
        child(&mut server, "a", "a-1").await;
        wait_call(&mut server, "wait-1", vec!["/root/a"]).await;
        phase(&mut client, SessionPhase::GatePending).await;
        client
            .commands
            .send(Command::SubmitRootInput {
                text: "queued task".into(),
            })
            .await
            .unwrap();
        send(&mut server, json!({"method":"thread/status/changed","params":{"threadId":"a","status":{"type":"idle"}}})).await;
        send(&mut server, json!({"method":"turn/completed","params":{"threadId":"a","turn":{"id":"obsolete","status":"completed"}}})).await;
        send(&mut server, json!({"id":"child-approval","method":"item/commandExecution/requestApproval","params":{"threadId":"a","turnId":"a-1","command":"test"}})).await;
        tokio::time::timeout(
            Duration::from_secs(3),
            client
                .snapshots
                .wait_for(|s| s.queued_inputs == 1 && s.requests.len() == 1),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(100), next(&mut server))
                .await
                .is_err()
        );
        client
            .commands
            .send(Command::AnswerApproval {
                request_id: RpcId::String("child-approval".into()),
                decision: ApprovalDecision::Decline,
            })
            .await
            .unwrap();
        let approval = next(&mut server).await;
        assert_eq!(approval["id"], "child-approval");
        assert_eq!(approval["result"]["decision"], "decline");
        send(&mut server, json!({"method":"turn/completed","params":{"threadId":"a","turn":{"id":"a-1","status":"completed"}}})).await;
        let released = next(&mut server).await;
        assert_eq!(released["id"], "wait-1");
        assert_eq!(released["result"]["success"], true);
        phase(&mut client, SessionPhase::Running).await;
        let gate = client.snapshots.borrow().gate.clone().unwrap();
        assert_eq!(gate.root_starts_at_enter, 1);
        assert_eq!(gate.root_starts_at_release, Some(1));
        send(&mut server, json!({"method":"turn/completed","params":{"threadId":"a","turn":{"id":"a-1","status":"completed"}}})).await;
        assert!(
            tokio::time::timeout(Duration::from_millis(100), next(&mut server))
                .await
                .is_err()
        );
        send(&mut server, json!({"method":"turn/completed","params":{"threadId":"root","turn":{"id":"root-turn","status":"completed"}}})).await;
        let start = next(&mut server).await;
        assert_eq!(start["method"], "turn/start");
        assert_eq!(start["params"]["input"][0]["text"], "queued task");
        client.commands.send(Command::Quit).await.unwrap();
        client.join.await.unwrap();
    }

    #[tokio::test]
    async fn followup_wait_ignores_old_completion_until_a_new_child_turn_is_bound() {
        let (mut client, mut server) = harness().await;
        running_root(&mut client, &mut server).await;
        child(&mut server, "a", "old").await;
        send(&mut server, json!({"method":"turn/completed","params":{"threadId":"a","turn":{"id":"old","status":"completed"}}})).await;
        for method in ["item/started", "item/completed"] {
            send(&mut server, json!({"method":method,"params":{"threadId":"root","turnId":"root-turn","item":{"id":"followup","type":"collabAgentToolCall","senderThreadId":"root","receiverThreadIds":["a"],"tool":"followupTask","status":"completed","agentsStates":{"a":{"status":"completed"}}}}})).await;
        }
        wait_call(&mut server, "wait-new", vec!["a"]).await;
        phase(&mut client, SessionPhase::GatePending).await;
        send(&mut server, json!({"method":"turn/completed","params":{"threadId":"a","turn":{"id":"old","status":"completed"}}})).await;
        assert!(
            tokio::time::timeout(Duration::from_millis(100), next(&mut server))
                .await
                .is_err()
        );
        send(
            &mut server,
            json!({"method":"turn/started","params":{"threadId":"a","turn":{"id":"new"}}}),
        )
        .await;
        send(&mut server, json!({"method":"turn/completed","params":{"threadId":"a","turn":{"id":"old","status":"completed"}}})).await;
        assert!(
            tokio::time::timeout(Duration::from_millis(100), next(&mut server))
                .await
                .is_err()
        );
        send(&mut server, json!({"method":"turn/completed","params":{"threadId":"a","turn":{"id":"new","status":"completed"}}})).await;
        let result = next(&mut server).await;
        let data: Value = serde_json::from_str(
            result["result"]["contentItems"][0]["text"]
                .as_str()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(data["targets"][0]["turnId"], "new");
        assert_eq!(data["targets"][0]["generation"], 2);
        client.commands.send(Command::Quit).await.unwrap();
        client.join.await.unwrap();
    }

    #[tokio::test]
    async fn empty_targets_capture_known_children_and_failure_releases_once() {
        let (mut client, mut server) = harness().await;
        running_root(&mut client, &mut server).await;
        child(&mut server, "a", "a-1").await;
        child(&mut server, "b", "b-1").await;
        wait_call(&mut server, "wait-all", vec![]).await;
        phase(&mut client, SessionPhase::GatePending).await;
        child(&mut server, "later", "later-1").await;
        send(&mut server, json!({"method":"turn/completed","params":{"threadId":"a","turn":{"id":"a-1","status":"failed"}}})).await;
        let result = next(&mut server).await;
        let data: Value = serde_json::from_str(
            result["result"]["contentItems"][0]["text"]
                .as_str()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(result["result"]["success"], true);
        assert_eq!(data["targets"].as_array().unwrap().len(), 2);
        assert_eq!(data["targets"][0]["outcome"], "failed");
        assert_eq!(data["targets"][1]["outcome"], Value::Null);
        wait_call(&mut server, "wait-all", vec![]).await;
        assert!(
            tokio::time::timeout(Duration::from_millis(100), next(&mut server))
                .await
                .is_err()
        );
        client.commands.send(Command::Quit).await.unwrap();
        client.join.await.unwrap();
    }

    #[cfg(windows)]
    #[tokio::test]
    #[ignore = "requires the installed authenticated Codex app-server"]
    async fn live_windows_core_reaches_ready_without_a_model_turn() {
        let fixture = JournalFixture::new();
        let config = Config {
            journal: fixture.settings(),
            windows_sandbox: Some("unelevated".into()),
            sandbox: "read-only".into(),
            ..Default::default()
        };
        let mut client = ClientHandle::spawn(config).await.unwrap();
        let outcome = tokio::time::timeout(
            Duration::from_secs(35),
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
        let snapshot = outcome.unwrap_or_else(|_| {
            let latest = client.snapshots.borrow();
            panic!("live startup did not reach a result within 35 seconds; phase={:?}, notice={:?}, error={:?}", latest.phase, latest.notice, latest.last_error)
        });
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
                None,
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
