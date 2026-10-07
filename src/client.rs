use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;
use tokio::time::Instant;

use crate::agents::{AgentInfo, AgentLimits, AgentRegistry};
use crate::app_server::{self, AppServer, AppServerError};
use crate::config::Config;
use crate::gate::{ChildOutcome, CompletionGate, GateEvent, PendingGate, WaitRequest, WaitToken};
use crate::interactions::{files::FilePreviews, ApprovalDecision, RequestRef, RequestView};
use crate::journal::{Journal, JournalError, StoredSnapshot};
use crate::observation::{AttentionClass, ChildFact, ObservationFacts, Observer};
use crate::outbox::{Outbox, OutboxError, OutboxIntent};
use crate::protocol::{self, Envelope, RpcId};
use crate::scheduler::{
    ExternalTurn, InterruptEffect, RootTaskSpec, Scheduler, SchedulerCommand, TaskAttempt,
    TaskKind, TaskSnapshot,
};
use crate::skills::SkillRefreshSource;
use crate::state::{
    CoreSnapshot, FactSource, GateSnapshot, PersistenceState, SessionPhase, SessionState,
    TokenBudgetSnapshot, UsageFact, UsageIdentity, UsageSummary, MESSAGE_BYTES,
};
use crate::tool_details::{ToolDetailLocator, ToolDetails, ToolLifecycle};
use crate::transport::{PipeTransport, TransportError};

#[cfg(all(test, windows))]
mod live_requests;

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
        request: RequestRef,
        decision: ApprovalDecision,
    },
    AnswerUserInput {
        request: RequestRef,
        answers: BTreeMap<String, Vec<String>>,
    },
    Interrupt,
    ConfigureAttention {
        class: AttentionClass,
        quiet_ms: u64,
        attention_ms: u64,
    },
    HeadlessRequest {
        request: RequestRef,
    },
    RefreshSkills,
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
    #[error(transparent)]
    Outbox(#[from] OutboxError),
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
        let mut initial = Self::initial(&config, &observer);
        // Journal::open 已同步提交首个快照。
        initial.persistence = PersistenceState::Committed;
        let journal = Journal::open(
            &config.journal,
            &config.cwd,
            StoredSnapshot::capture(&initial),
        )?;
        let outbox = match Outbox::open(
            config
                .journal
                .outbox_path(&config.cwd, &initial.observation.session_id)?,
        ) {
            Ok(outbox) => outbox,
            Err(error) => {
                let mut failure = StoredSnapshot::capture(&initial);
                failure.phase = SessionPhase::Failed;
                failure.issue = Some(crate::journal::PersistenceIssue::JournalUnavailable);
                failure.close(SessionPhase::Failed, false);
                let _ = journal.finish(failure).await;
                return Err(error.into());
            }
        };
        let mut server = match AppServer::spawn(&config).await {
            Ok(server) => server,
            Err(error) => {
                let cleanup_confirmed = !matches!(error, AppServerError::StartupCleanup);
                let mut failure = StoredSnapshot::capture(&initial);
                failure.phase = SessionPhase::Failed;
                failure.issue = Some(if cleanup_confirmed {
                    crate::journal::PersistenceIssue::StartupFailed
                } else {
                    crate::journal::PersistenceIssue::CleanupUncertain
                });
                failure.close(SessionPhase::Failed, cleanup_confirmed);
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
            Some((observer, journal, outbox)),
        ))
    }

    fn initial(config: &Config, observer: &Observer) -> CoreSnapshot {
        CoreSnapshot {
            phase: SessionPhase::Launching,
            cwd: config.cwd.display().to_string(),
            sandbox: config.sandbox.clone(),
            approval_policy: config.approval_policy.clone(),
            scheduler: crate::scheduler::SchedulerSnapshot {
                native_slot_capacity: Some(config.max_native_turns.max(1)),
                ..Default::default()
            },
            token_budget: TokenBudgetSnapshot {
                limit: config.max_total_tokens,
                per_agent_limit: config.max_agent_tokens,
                ..Default::default()
            },
            observation: observer.snapshot_at(0, Instant::now()),
            timeline: observer.timeline_snapshot(),
            ..Default::default()
        }
    }

    fn start(
        pipe: PipeTransport,
        config: Config,
        server: Option<AppServer>,
        check_only: bool,
        prepared: Option<(Observer, Journal, Outbox)>,
    ) -> Self {
        let shell_source = if cfg!(windows) && !check_only {
            server
                .as_ref()
                .and_then(AppServer::shell_peer)
                .map(crate::shell_check::Source::Peer)
        } else {
            None
        };
        Self::start_with_shell_source(pipe, config, server, check_only, prepared, shell_source)
    }

    fn start_with_shell_source(
        pipe: PipeTransport,
        config: Config,
        server: Option<AppServer>,
        check_only: bool,
        prepared: Option<(Observer, Journal, Outbox)>,
        shell_source: Option<crate::shell_check::Source>,
    ) -> Self {
        let agent_limits = AgentLimits {
            max_children: config.max_native_children,
            max_depth: config.max_native_depth,
            max_active_turns: config.max_native_turns,
        };
        let (commands, command_rx) = mpsc::channel(32);
        let (observer, journal, outbox) = match prepared {
            Some((observer, journal, outbox)) => (observer, Some(journal), Some(outbox)),
            None => (Observer::new(config.attention.clone()), None, None),
        };
        let mut initial = Self::initial(&config, &observer);
        initial.journal = journal.as_ref().map(Journal::view);
        initial.persistence = if initial.journal.is_some() {
            PersistenceState::Committed
        } else {
            PersistenceState::Uncertain
        };
        let (snapshot_tx, snapshots) = watch::channel(Arc::new(initial.clone()));
        let mut scheduler = Scheduler::default();
        scheduler
            .set_native_slot_capacity(Some(config.max_native_turns.max(1)))
            .expect("initial native slot capacity must fit an empty scheduler");
        let join = tokio::spawn(
            Core {
                pipe,
                observer,
                journal,
                outbox,
                journal_error: None,
                outbox_error: None,
                root_attempt: None,
                root_observed: None,
                config,
                server,
                command_rx,
                snapshot_tx,
                state: SessionState {
                    view: initial,
                    agents: AgentRegistry::with_limits(agent_limits),
                    usage_facts: Default::default(),
                    tool_lines: Default::default(),
                },
                pending: HashMap::new(),
                next_id: 1,
                next_outbox_id: 1,
                outbox_pending: HashMap::new(),
                scheduler,
                child_interrupts: VecDeque::new(),
                interrupt_requested: false,
                interrupt_sent: false,
                token_budget_stop_started: false,
                agent_budget_interrupts: BTreeMap::new(),
                preflight_passed: false,
                shell_source,
                shell_check: None,
                peer_cleanup_uncertain: false,
                generation: 0,
                retired_turns: VecDeque::new(),
                gate: PendingGate::default(),
                wait: None,
                ingress_seq: 0,
                file_previews: FilePreviews::default(),
                tool_details: ToolDetails::default(),
                collab_starts: HashMap::new(),
                completed_collab: VecDeque::new(),
                completed_waits: VecDeque::new(),
                identity_pending: BTreeSet::new(),
                identity_requested: BTreeSet::new(),
                check_only,
                skills_generation: 0,
                skills_refresh_queued: false,
                skills_refresh_force_reload: false,
                skills_refresh_source: None,
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
    StartTurn {
        generation: u64,
    },
    Interrupt {
        generation: u64,
    },
    ChildInterrupt {
        attempt: TaskAttempt,
    },
    SkillsList {
        generation: u64,
        force_reload: bool,
        source: SkillRefreshSource,
    },
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
    skills_cwd: Option<String>,
}

struct Core {
    observer: Observer,
    journal: Option<Journal>,
    outbox: Option<Outbox>,
    journal_error: Option<JournalError>,
    outbox_error: Option<OutboxError>,
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
    next_outbox_id: u64,
    outbox_pending: HashMap<RpcId, u64>,
    scheduler: Scheduler,
    child_interrupts: VecDeque<InterruptEffect>,
    interrupt_requested: bool,
    interrupt_sent: bool,
    token_budget_stop_started: bool,
    /// 已触发的 agent/turn 预算键；同一代只允许一次中断。
    agent_budget_interrupts: BTreeMap<String, (String, u64)>,
    preflight_passed: bool,
    shell_source: Option<crate::shell_check::Source>,
    shell_check: Option<crate::shell_check::ShellCheck>,
    peer_cleanup_uncertain: bool,
    generation: u64,
    retired_turns: VecDeque<String>,
    gate: PendingGate,
    wait: Option<PendingTool>,
    ingress_seq: u64,
    file_previews: FilePreviews,
    tool_details: ToolDetails,
    collab_starts: HashMap<String, u64>,
    completed_collab: VecDeque<String>,
    completed_waits: VecDeque<(RpcId, String, String)>,
    identity_pending: BTreeSet<String>,
    identity_requested: BTreeSet<String>,
    check_only: bool,
    skills_generation: u64,
    skills_refresh_queued: bool,
    skills_refresh_force_reload: bool,
    skills_refresh_source: Option<SkillRefreshSource>,
}

struct PendingTool {
    request_id: RpcId,
    call_id: String,
    parent_turn: String,
    generation: u64,
    token: WaitToken,
}

fn persistence_state(
    view: Option<&crate::journal::JournalView>,
    journal_error: Option<&JournalError>,
) -> PersistenceState {
    match view {
        Some(view) if journal_error.is_some() || view.error.is_some() => {
            PersistenceState::Uncertain
        }
        Some(view) if view.committed_seq >= view.submitted_seq => PersistenceState::Committed,
        Some(_) => PersistenceState::Submitted,
        None => PersistenceState::Uncertain,
    }
}

fn rpc_id_text(id: &RpcId) -> String {
    match id {
        RpcId::Number(value) => value.to_string(),
        RpcId::String(value) => value.clone(),
    }
}

impl Core {
    fn confirmed_session_usage(&self) -> (Option<u64>, bool) {
        let root = (self.state.view.usage.source == FactSource::ServerConfirmed)
            .then_some(self.state.view.usage.total_tokens)
            .flatten();
        let child_usages = self.state.agents.snapshots();
        let children_complete = child_usages.iter().all(|agent| {
            agent.usage.source == FactSource::ServerConfirmed && agent.usage.total_tokens.is_some()
        });
        let child_total = child_usages
            .iter()
            .filter(|agent| agent.usage.source == FactSource::ServerConfirmed)
            .filter_map(|agent| agent.usage.total_tokens)
            .fold(0, u64::saturating_add);
        let has_any_confirmed_total = root.is_some()
            || child_usages.iter().any(|agent| {
                agent.usage.source == FactSource::ServerConfirmed
                    && agent.usage.total_tokens.is_some()
            });
        let complete = root.is_some() && children_complete;
        if !has_any_confirmed_total {
            return (None, false);
        }
        (
            Some(root.unwrap_or_default().saturating_add(child_total)),
            complete,
        )
    }

    fn update_token_budget_snapshot(&mut self) {
        let (confirmed_total_tokens, confirmed_complete) = self.confirmed_session_usage();
        self.state.view.token_budget = TokenBudgetSnapshot {
            confirmed_total_tokens,
            confirmed_complete,
            limit: self.config.max_total_tokens,
            stop_triggered: self.token_budget_stop_started,
            per_agent_limit: self.config.max_agent_tokens,
            per_agent_stop_triggered: !self.agent_budget_interrupts.is_empty(),
        };
    }

    fn shell_timeout(&mut self) {
        let message = self.shell_timeout_message();
        self.state.error(SessionPhase::Unknown, message);
    }

    /// 超时后只给出可操作的建议，不自动放宽沙箱或重试。
    fn shell_timeout_message(&self) -> String {
        let advice = if !cfg!(windows) {
            "Check the configured shell and sandbox."
        } else if self.config.windows_sandbox.as_deref() == Some("unelevated") {
            "Check the Codex Windows sandbox setup."
        } else {
            "The elevated Codex sandbox may need one-time admin setup: run `codex sandbox -- cmd /c echo ok` in a terminal and approve the UAC prompt. Or restart with --windows-sandbox unelevated (keep it with `[windows] sandbox = \"unelevated\"` in the Codex config.toml)."
        };
        format!("Shell preflight timed out; no model turn was started. {advice} Automatic retry is disabled.")
    }

    fn token_budget_exhausted(&self) -> Option<u64> {
        let limit = self.config.max_total_tokens?;
        let root = if self.state.view.usage.source == FactSource::ServerConfirmed {
            self.state.view.usage.total_tokens.unwrap_or_default()
        } else {
            0
        };
        (root.saturating_add(self.state.agents.confirmed_total_tokens()) >= limit).then_some(limit)
    }

    fn current_agent_budget(&self, thread: &str, turn: &str) -> Option<(u64, u64)> {
        let limit = self.config.max_agent_tokens?;
        if self.state.view.thread_id.as_deref() == Some(thread) {
            if self.state.view.turn_id.as_deref() != Some(turn)
                || !matches!(
                    self.state.view.phase,
                    SessionPhase::StartingTurn | SessionPhase::Running | SessionPhase::GatePending
                )
            {
                return None;
            }
            let total = (self.state.view.usage.source == FactSource::ServerConfirmed)
                .then_some(self.state.view.usage.total_tokens)
                .flatten()?;
            return (total >= limit).then_some((limit, self.generation));
        }
        if !self.state.agents.active_turn(thread, turn) {
            return None;
        }
        let agent = self
            .state
            .agents
            .snapshots()
            .into_iter()
            .find(|agent| agent.info.id == thread && agent.turn_id.as_deref() == Some(turn))?;
        let total = (agent.usage.source == FactSource::ServerConfirmed)
            .then_some(agent.usage.total_tokens)
            .flatten()?;
        (total >= limit).then_some((limit, agent.generation))
    }

    fn interrupt_for_agent_budget(
        &mut self,
        thread: &str,
        turn: &str,
        limit: u64,
        generation: u64,
    ) {
        if self
            .agent_budget_interrupts
            .get(thread)
            .is_some_and(|(old_turn, old_generation)| {
                old_turn == turn && *old_generation == generation
            })
        {
            return;
        }
        self.agent_budget_interrupts
            .insert(thread.to_owned(), (turn.to_owned(), generation));
        self.state.view.notice = Some(format!(
            "Agent token budget reached: limit={limit} thread={thread} turn={turn}; interrupt requested; waiting for the server's terminal event."
        ));
        if self.state.view.thread_id.as_deref() == Some(thread) {
            if let Some(attempt) = self.scheduler.active_root() {
                let _ = self
                    .scheduler
                    .command(SchedulerCommand::Cancel(attempt.task));
            }
            self.interrupt_requested = true;
            self.issue_interrupt();
            return;
        }
        let Some(task) = self.scheduler.child_task(thread).cloned() else {
            return;
        };
        if !task.state.active()
            || task.external.as_ref().is_none_or(|external| {
                external.thread_id != thread
                    || external.turn_id != turn
                    || external.generation != generation
            })
        {
            return;
        }
        if let Ok(effects) = self.scheduler.command(SchedulerCommand::Cancel(task.id)) {
            for effect in effects {
                self.queue_child_interrupt(effect);
            }
        }
    }

    fn interrupt_for_token_budget(&mut self, limit: u64) {
        if self.token_budget_stop_started {
            return;
        }
        self.token_budget_stop_started = true;
        if let Ok(effects) = self.scheduler.command(SchedulerCommand::StopWorkflow) {
            for effect in effects {
                match effect.kind {
                    TaskKind::RootTurn => {
                        self.interrupt_requested = true;
                        self.issue_interrupt();
                    }
                    TaskKind::NativeChild => self.queue_child_interrupt(effect),
                }
            }
        }
        if matches!(
            self.state.view.phase,
            SessionPhase::StartingTurn | SessionPhase::Running | SessionPhase::GatePending
        ) {
            self.interrupt_requested = true;
            self.issue_interrupt();
        }
        self.state.view.notice = Some(format!(
            "Token budget {limit} reached; interrupt requested; waiting for the server's terminal event."
        ));
    }

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
                report = async { self.shell_check.as_mut().expect("active check").wait().await }, if self.shell_check.is_some() => {
                    let expired = self.shell_check.as_ref().is_some_and(|check| check.deadline <= Instant::now());
                    self.shell_check.take();
                    self.peer_cleanup_uncertain |= !report.cleanup_confirmed;
                    if !report.cleanup_confirmed {
                        self.state.error(SessionPhase::Unknown, "Isolated shell preflight process cleanup could not be confirmed");
                    } else if expired {
                        self.shell_timeout();
                    } else {
                        use crate::shell_check::Outcome;
                        match report.outcome {
                            Outcome::Passed => {
                                self.state.view.phase = SessionPhase::Ready;
                                self.state.view.notice = None;
                                self.preflight_passed = true;
                                self.skills_generation = self.skills_generation.wrapping_add(1);
                                self.queue_skills_refresh(SkillRefreshSource::Initial, false);
                            }
                            Outcome::TimedOut => self.shell_timeout(),
                            Outcome::Cancelled => self.state.error(SessionPhase::Failed, "Shell preflight cancelled; no model turn was started"),
                            Outcome::TransportUnavailable | Outcome::ProtocolRejected => self.state.error(SessionPhase::Unknown, "Isolated shell preflight has no verified response; its external outcome is unknown. No model turn was started"),
                            Outcome::BackendRejected => self.state.error(SessionPhase::Failed, "Isolated shell preflight backend version or launch was rejected; no model turn was started"),
                            Outcome::InitializeRejected => self.state.error(SessionPhase::Failed, "Isolated shell preflight initialize response failed verification; no model turn was started"),
                            Outcome::ShellRejected => self.state.error(SessionPhase::Failed, "Isolated shell preflight returned an unsuccessful check; no model turn was started"),
                        }
                    }
                }
                _ = clock.tick(), if !self.pending.is_empty() || !self.child_interrupts.is_empty() || self.shell_check.is_some() => {
                    if self.shell_check.as_ref().is_some_and(|check| check.deadline <= Instant::now()) {
                        self.shell_timeout();
                    }
                    let expired = self.pending.iter().find(|(_, rpc)| rpc.deadline <= Instant::now()).map(|(id, rpc)| (id.clone(), rpc.kind));
                    if let Some((id, kind)) = expired {
                        self.pending.remove(&id);
                        self.unknown_outbox(&id);
                        if let RpcKind::SkillsList { generation, .. } = kind {
                            // 旧代次超时只丢弃结果；保留排队刷新并在本轮循环末重发。
                            if generation == self.skills_generation {
                                self.skills_failed(crate::skills::SkillAvailability::TimedOut);
                            }
                        } else {
                            let message = if matches!(kind, RpcKind::Preflight) {
                            self.shell_timeout_message()
                        } else {
                            format!("app-server {kind:?} response timed out; automatic retry is disabled")
                        };
                            self.state.error(SessionPhase::Unknown, message);
                        }
                    }
                }
            }
            self.flush_child_interrupt();
            self.flush_skills_refresh();
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
                self.unknown_pending_outbox();
                self.pending.clear();
                break;
            }
        }

        self.unknown_pending_outbox();
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
        if let Some(check) = &self.shell_check {
            check.cancel();
        }
        self.pipe.close_writer().await;
        let mut cleanup_error = if let Some(server) = &mut self.server {
            server.shutdown().await.err().map(|error| error.to_string())
        } else {
            None
        };
        if let Some(mut check) = self.shell_check.take() {
            self.peer_cleanup_uncertain |= !check.wait().await.cleanup_confirmed;
        }
        if self.peer_cleanup_uncertain {
            cleanup_error =
                Some("Isolated shell preflight process cleanup could not be confirmed".into());
        }
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
        self.update_token_budget_snapshot();
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
        self.state.view.timeline = self.observer.timeline_snapshot();
        if matches!(
            self.state.view.phase,
            SessionPhase::Unknown
                | SessionPhase::Disconnected
                | SessionPhase::Stopping
                | SessionPhase::Stopped
        ) {
            self.tool_details.execution_unavailable();
        }
        self.state.view.tool_details = self.tool_details.snapshot();
        let transport = self.pipe.stats();
        self.state.view.diagnostics.transport_bytes_in = transport.bytes_in;
        self.state.view.diagnostics.transport_bytes_out = transport.bytes_out;
        if let Some(view) = &mut self.state.view.journal {
            view.error = self.journal_error.clone().or(view.error.take());
        }
        self.state.view.startup_blocked = !self.preflight_passed
            && !matches!(
                self.state.view.phase,
                SessionPhase::Created
                    | SessionPhase::Launching
                    | SessionPhase::Initializing
                    | SessionPhase::CheckingShell
            );
        self.project_persistence_state();
        self.snapshot_tx.send_replace(self.state.snapshot());
    }

    fn publish_journal_status(&mut self) {
        if let Some(journal) = &self.journal {
            self.state.view.journal = Some(journal.view());
        }
        if let Some(view) = &mut self.state.view.journal {
            view.error = self.journal_error.clone().or(view.error.take());
        }
        self.project_persistence_state();
        self.state.view.observation.snapshot_version = self.state.view.version + 1;
        self.snapshot_tx.send_replace(self.state.snapshot());
    }

    /// 将 journal 的提交水位投影为 Core 可消费的脱敏状态。
    fn project_persistence_state(&mut self) {
        self.state.view.persistence = persistence_state(
            self.state.view.journal.as_ref(),
            self.journal_error.as_ref(),
        );
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

    fn skills_idle_for_refresh(&self) -> bool {
        let phase_allows_query = matches!(
            self.state.view.phase,
            SessionPhase::Ready | SessionPhase::Completed | SessionPhase::Interrupted
        );
        let scheduler = self.scheduler.snapshot();
        !self.check_only
            && self.preflight_passed
            && phase_allows_query
            && self.wait.is_none()
            && self
                .state
                .view
                .gate
                .as_ref()
                .is_none_or(|gate| !gate.pending)
            && self.state.view.requests.is_empty()
            && !scheduler.stopping
            && !scheduler.tasks.iter().any(|task| task.state.active())
    }

    fn queue_skills_refresh(&mut self, source: SkillRefreshSource, force_reload: bool) {
        self.skills_refresh_source = Some(match (self.skills_refresh_source, source) {
            (Some(SkillRefreshSource::Manual), _) | (_, SkillRefreshSource::Manual) => {
                SkillRefreshSource::Manual
            }
            (Some(SkillRefreshSource::Changed), _) | (_, SkillRefreshSource::Changed) => {
                SkillRefreshSource::Changed
            }
            _ => SkillRefreshSource::Initial,
        });
        self.skills_refresh_queued = true;
        self.skills_refresh_force_reload |= force_reload;
        self.state.view.skills.freshness = crate::skills::SkillFreshness::Queued;
    }

    fn flush_skills_refresh(&mut self) {
        if !self.skills_refresh_queued
            || !self.skills_idle_for_refresh()
            || self
                .pending
                .values()
                .any(|rpc| matches!(rpc.kind, RpcKind::SkillsList { .. }))
        {
            return;
        }
        let kind = RpcKind::SkillsList {
            generation: self.skills_generation,
            force_reload: self.skills_refresh_force_reload,
            source: self
                .skills_refresh_source
                .unwrap_or(SkillRefreshSource::Initial),
        };
        if let Err(error) = self.send_rpc(kind) {
            self.skills_failed(crate::skills::SkillAvailability::TransportUnavailable);
            self.state
                .error(SessionPhase::Disconnected, error.to_string());
        } else {
            self.skills_refresh_queued = false;
            self.skills_refresh_force_reload = false;
            self.skills_refresh_source = None;
        }
    }

    fn skills_failed(&mut self, reason: crate::skills::SkillAvailability) {
        self.state.view.skills.availability = reason;
        self.state.view.skills.freshness = if self.skills_refresh_queued {
            crate::skills::SkillFreshness::Queued
        } else {
            crate::skills::SkillFreshness::Stale
        };
    }

    fn skills_rpc_is_current(&self, pending: &PendingRpc) -> bool {
        let RpcKind::SkillsList { generation, .. } = pending.kind else {
            return false;
        };
        pending.skills_cwd.as_deref() == Some(self.config.cwd.display().to_string().as_str())
            && generation == self.skills_generation
    }

    fn outbox_failure(error: OutboxError) -> TransportError {
        TransportError::Failed(format!("durable outbox failed: {error}"))
    }

    /// Record an effect before handing it to the single transport writer.
    ///
    /// A successful `PipeTransport::send` means the frame entered the bounded
    /// writer queue, not that the server processed it. Callers that expect a
    /// response retain the returned intent id and confirm it at the response
    /// boundary; unresolved ids become `unknown` on disconnect or timeout.
    fn send_effect(
        &mut self,
        envelope: Envelope,
        task: Option<TaskAttempt>,
    ) -> Result<Option<u64>, TransportError> {
        let outbox_id = if let Some(outbox) = self.outbox.as_mut() {
            let id = self.next_outbox_id;
            self.next_outbox_id = self
                .next_outbox_id
                .checked_add(1)
                .ok_or_else(|| TransportError::Failed("outbox id exhausted".into()))?;
            let payload = serde_json::to_vec(&envelope)
                .map_err(|error| TransportError::Failed(error.to_string()))?;
            let request_id = envelope
                .id
                .as_ref()
                .map(rpc_id_text)
                .unwrap_or_else(|| format!("outbox-{id}"));
            let method = envelope.method.clone().unwrap_or_else(|| "response".into());
            let intent = OutboxIntent::new(
                id,
                self.state.view.observation.session_id.clone(),
                task,
                request_id,
                method,
                &payload,
            );
            outbox.record_intent(intent).map_err(Self::outbox_failure)?;
            Some(id)
        } else {
            None
        };

        if let Err(error) = self.pipe.send(envelope) {
            if let Some(id) = outbox_id {
                if let Some(outbox) = &mut self.outbox {
                    let _ = outbox.mark_unknown(id);
                }
            }
            return Err(error);
        }
        if let Some(id) = outbox_id {
            if let Some(outbox) = &mut self.outbox {
                if let Err(error) = outbox.mark_sent(id) {
                    let _ = outbox.mark_unknown(id);
                    return Err(Self::outbox_failure(error));
                }
            }
        }
        Ok(outbox_id)
    }

    fn track_outbox(&mut self, request_id: &RpcId, outbox_id: Option<u64>) {
        if let Some(outbox_id) = outbox_id {
            self.outbox_pending.insert(request_id.clone(), outbox_id);
        }
    }

    fn confirm_outbox(&mut self, request_id: &RpcId) {
        let Some(outbox_id) = self.outbox_pending.remove(request_id) else {
            return;
        };
        if let Some(outbox) = &mut self.outbox {
            if let Err(error) = outbox.mark_confirmed(outbox_id) {
                self.outbox_error = Some(error);
                self.state.error(
                    SessionPhase::Unknown,
                    "Durable outbox confirmation failed; external outcome is unknown",
                );
            }
        }
    }

    fn unknown_outbox(&mut self, request_id: &RpcId) {
        let Some(outbox_id) = self.outbox_pending.remove(request_id) else {
            return;
        };
        if let Some(outbox) = &mut self.outbox {
            if let Err(error) = outbox.mark_unknown(outbox_id) {
                self.outbox_error = Some(error);
            }
        }
    }

    fn unknown_pending_outbox(&mut self) {
        let ids: Vec<RpcId> = self.outbox_pending.keys().cloned().collect();
        for request_id in ids {
            self.unknown_outbox(&request_id);
        }
    }

    fn send_rpc(&mut self, kind: RpcKind) -> Result<(), TransportError> {
        if matches!(kind, RpcKind::Preflight) {
            if let Some(source) = self.shell_source.take() {
                self.state.view.phase = SessionPhase::CheckingShell;
                self.state.view.notice =
                    Some("Checking the sandbox shell; no model turn has started.".into());
                self.shell_check = Some(crate::shell_check::ShellCheck::start(
                    source,
                    self.config.clone(),
                ));
                return Ok(());
            }
        }
        let id = RpcId::Number(self.next_id);
        self.next_id += 1;
        let envelope = match kind {
            RpcKind::Initialize => app_server::initialize(id.clone()),
            RpcKind::ThreadStart => app_server::thread_start(id.clone(), &self.config),
            RpcKind::Preflight => app_server::preflight(id.clone(), &self.config),
            RpcKind::SkillsList { force_reload, .. } => {
                app_server::skills_list(id.clone(), &self.config.cwd, force_reload)
            }
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
        let outbox_id = self.send_effect(envelope, None)?;
        self.track_outbox(&id, outbox_id);
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
                skills_cwd: matches!(kind, RpcKind::SkillsList { .. })
                    .then(|| self.config.cwd.display().to_string()),
            },
        );
        Ok(())
    }

    fn command(&mut self, command: Command) {
        match command {
            Command::HeadlessRequest { request: reference } => {
                let Some(request) = self
                    .state
                    .view
                    .requests
                    .iter()
                    .find(|request| {
                        request.matches(&reference)
                            && !request.responding
                            && self.request_is_live(request)
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
                        request_id: reference.id.clone(),
                        thread_id: request.thread_id,
                        turn_id: request.turn_id,
                    }
                } else if !request.allow_decline {
                    crate::interactions::HeadlessAction::InterruptForApproval {
                        request_id: reference.id.clone(),
                        thread_id: request.thread_id,
                        turn_id: request.turn_id,
                    }
                } else {
                    crate::interactions::HeadlessAction::DeclineApproval {
                        request_id: reference.id.clone(),
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
                        request: reference,
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
            Command::RefreshSkills => {
                self.skills_generation = self.skills_generation.wrapping_add(1);
                self.state.view.skills.freshness = crate::skills::SkillFreshness::Stale;
                self.queue_skills_refresh(SkillRefreshSource::Manual, true);
            }
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
            Command::AnswerApproval { request, decision } => {
                let result = self
                    .state
                    .view
                    .requests
                    .iter()
                    .find(|r| r.matches(&request) && self.request_is_live(r))
                    .ok_or_else(|| "Request already resolved.".to_owned())
                    .and_then(|request| {
                        request.approval_result(decision).map_err(|e| e.to_string())
                    });
                self.respond(request.id, result);
            }
            Command::AnswerUserInput { request, answers } => {
                let result = self
                    .state
                    .view
                    .requests
                    .iter()
                    .find(|r| r.matches(&request) && self.request_is_live(r))
                    .ok_or_else(|| "Request already resolved.".to_owned())
                    .and_then(|request| request.input_result(&answers).map_err(|e| e.to_string()));
                self.respond(request.id, result);
            }
            Command::Quit
            | Command::OutputUnavailable
            | Command::UnconfirmedHeadlessInteraction => {}
        }
    }

    fn request_is_live(&self, request: &RequestView) -> bool {
        self.preflight_passed
            && !matches!(
                self.state.view.phase,
                SessionPhase::Unknown
                    | SessionPhase::Disconnected
                    | SessionPhase::Stopping
                    | SessionPhase::ClosingTransport
                    | SessionPhase::Stopped
            )
            && ((self.state.view.thread_id.as_deref() == Some(&request.thread_id)
                && self.state.view.turn_id.as_deref() == Some(&request.turn_id)
                && matches!(
                    self.state.view.phase,
                    SessionPhase::Running | SessionPhase::GatePending
                ))
                || self
                    .state
                    .agents
                    .active_turn(&request.thread_id, &request.turn_id))
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
        if let Some(limit) = self.token_budget_exhausted() {
            if self.interrupt_requested
                || matches!(
                    self.state.view.phase,
                    SessionPhase::StartingTurn | SessionPhase::Running | SessionPhase::GatePending
                )
            {
                return;
            }
            self.token_budget_stop_started = true;
            let _ = self.scheduler.command(SchedulerCommand::StopWorkflow);
            self.state.error(
                SessionPhase::Failed,
                format!("Token budget {limit} reached; start a new session."),
            );
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
        let request = app_server::interrupt(id.clone(), &external.thread_id, &external.turn_id);
        match self.send_effect(request, Some(effect.attempt)) {
            Ok(outbox_id) => {
                self.track_outbox(&id, outbox_id);
                self.pending.insert(
                    id,
                    PendingRpc {
                        kind: RpcKind::ChildInterrupt {
                            attempt: effect.attempt,
                        },
                        deadline: Instant::now() + Duration::from_secs(30),
                        identity_thread: None,
                        skills_cwd: None,
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
            Ok(result) => {
                match self.send_effect(Envelope::response(id.clone(), Some(result)), None) {
                    Ok(_) => {
                        if let Some(request) =
                            self.state.view.requests.iter_mut().find(|r| r.id == id)
                        {
                            request.responding = true;
                        }
                    }
                    Err(error) => self.state.error(SessionPhase::Unknown, error.to_string()),
                }
            }
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
        let thread = self.state.view.thread_id.clone().unwrap();
        // 新 root turn 尚未收到 usage；旧 turn 事实不能重新绑定到这一代。
        self.state.clear_usage_fact(&thread);
        let request = app_server::turn_start(id.clone(), &thread, text);
        let outbox_id = match self.send_effect(request, self.root_attempt) {
            Ok(outbox_id) => outbox_id,
            Err(error) => {
                self.state.error(SessionPhase::Unknown, error.to_string());
                return;
            }
        };
        self.track_outbox(&id, outbox_id);
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
                skills_cwd: None,
            },
        );
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
        self.state
            .view
            .diagnostics
            .record_incoming(envelope.method.as_deref(), envelope.id.is_some());
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
                    Ok(mut request) => {
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
                            if let Err(error) = self.send_effect(
                                Envelope::error_response(
                                    id,
                                    -32602,
                                    "Request does not belong to the active turn",
                                ),
                                None,
                            ) {
                                self.state.error(SessionPhase::Unknown, error.to_string());
                            }
                        } else if self.state.view.requests.iter().any(|r| r.id == id) {
                            // A repeated delivery is still the same pending interaction.
                        } else if self.state.view.requests.len() < 64 {
                            request.received_seq = self.ingress_seq;
                            self.file_previews.attach(&mut request);
                            self.state.view.requests.push(request);
                        } else {
                            let _ = self.send_effect(
                                Envelope::error_response(
                                    id,
                                    -32000,
                                    "Too many pending interaction requests",
                                ),
                                None,
                            );
                            self.state.error(
                                SessionPhase::Unknown,
                                "Too many pending interaction requests",
                            );
                        }
                    }
                    Err(error) => {
                        let result = self.send_effect(
                            Envelope::error_response(id, -32601, error.to_string()),
                            None,
                        );
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
                self.confirm_outbox(&id);
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
                if let Some(error) = &envelope.error {
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
                    if matches!(pending.kind, RpcKind::SkillsList { .. }) {
                        if !self.skills_rpc_is_current(&pending) {
                            return;
                        }
                        let reason = if envelope
                            .error
                            .as_ref()
                            .and_then(|error| error.get("code"))
                            .and_then(Value::as_i64)
                            == Some(-32601)
                        {
                            crate::skills::SkillAvailability::Unsupported
                        } else {
                            crate::skills::SkillAvailability::RpcError
                        };
                        self.skills_failed(reason);
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
                    if matches!(pending.kind, RpcKind::SkillsList { .. }) {
                        let Some(cwd) = pending.skills_cwd.as_deref() else {
                            return;
                        };
                        if !self.skills_rpc_is_current(&pending) {
                            return;
                        }
                        let RpcKind::SkillsList { source, .. } = pending.kind else {
                            return;
                        };
                        self.skills_refresh_queued = false;
                        if let Some(skills) = crate::skills::parse_result(
                            &envelope.result.unwrap_or(Value::Null),
                            cwd,
                        ) {
                            let mut skills = skills;
                            skills.refresh_source = Some(source);
                            self.state.view.skills = skills;
                            self.skills_refresh_source = None;
                        } else {
                            self.skills_failed(crate::skills::SkillAvailability::Malformed);
                        }
                        return;
                    }
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
                if !self.config.backend.is_acp() {
                    if let Err(error) = crate::compatibility::verify_initialize(&result) {
                        self.state.error(SessionPhase::Failed, error.to_string());
                        return;
                    }
                }
                if let Err(error) =
                    self.send_effect(Envelope::notification("initialized", None), None)
                {
                    self.state.error(SessionPhase::Failed, error.to_string());
                    return;
                }
                Some(if self.check_only && !self.config.backend.is_acp() {
                    RpcKind::Preflight
                } else {
                    RpcKind::ThreadStart
                })
            }
            RpcKind::ThreadStart => {
                if !self.config.backend.is_acp() {
                    if let Err(error) = crate::compatibility::verify_thread_start(&result) {
                        self.state.error(SessionPhase::Failed, error.to_string());
                        return;
                    }
                }
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
                if self.config.backend.is_acp() {
                    // ACP has no Codex command/exec preflight. The session/new
                    // response is the explicit backend readiness fact.
                    self.state.view.phase = SessionPhase::Ready;
                    self.state.view.notice = None;
                    self.preflight_passed = true;
                    None
                } else {
                    Some(RpcKind::Preflight)
                }
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
                if !self.check_only {
                    self.skills_generation = self.skills_generation.wrapping_add(1);
                    self.queue_skills_refresh(SkillRefreshSource::Initial, false);
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
            RpcKind::AgentRead => None, // The response is handled with its captured thread identity.
            RpcKind::ChildInterrupt { .. } => None,
            RpcKind::SkillsList { .. } => None,
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
        if let Err(error) = self.send_effect(Envelope::error_response(id, -32602, reason), None) {
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

    fn tool_locator(&self, thread: &str, turn: &str, item: &str) -> Option<ToolDetailLocator> {
        let (agent_id, task_id, attempt_id, generation) =
            if self.state.view.thread_id.as_deref() == Some(thread) {
                let attempt = self.root_attempt?;
                (
                    "root".to_owned(),
                    Some(attempt.task),
                    Some(attempt.attempt),
                    Some(self.generation),
                )
            } else {
                let agent = self.state.agents.snapshots().into_iter().find(|agent| {
                    agent.info.id == thread && agent.turn_id.as_deref() == Some(turn)
                })?;
                let task = self.scheduler.child_task(thread)?;
                let external = task.external.as_ref()?;
                if external.thread_id != thread
                    || external.turn_id != turn
                    || external.generation != agent.generation
                    || task.attempt != agent.generation
                {
                    return None;
                }
                (
                    thread.to_owned(),
                    Some(task.id),
                    Some(task.attempt),
                    Some(agent.generation),
                )
            };
        Some(ToolDetailLocator {
            session_id: self.state.view.observation.session_id.clone(),
            identity: crate::observation::ActivityIdentity {
                agent_id,
                task_id,
                attempt_id,
                thread_id: Some(thread.to_owned()),
                turn_id: Some(turn.to_owned()),
                generation,
            },
            item_id: item.to_owned(),
        })
    }

    fn observe_tool_detail_event(&mut self, notice: &protocol::ObservedTool, params: &Value) {
        let Some(locator) = self.tool_locator(&notice.thread_id, &notice.turn_id, &notice.item_id)
        else {
            return;
        };
        let fields = protocol::decode_observed_tool_details(&params["item"], notice.category);
        if notice.outcome.is_none() {
            self.tool_details
                .observe_started(locator.clone(), notice.category, fields);
        } else {
            let lifecycle = match notice.outcome {
                Some(protocol::ObservedToolOutcome::Completed) => ToolLifecycle::Completed,
                Some(protocol::ObservedToolOutcome::CompletedUnknown) => {
                    ToolLifecycle::EndedUnknown
                }
                Some(protocol::ObservedToolOutcome::Failed) => ToolLifecycle::Failed,
                Some(protocol::ObservedToolOutcome::Interrupted) => ToolLifecycle::Interrupted,
                _ => ToolLifecycle::Unknown,
            };
            self.tool_details.observe_completed(
                &locator,
                notice.category,
                lifecycle,
                fields,
                notice.outcome != Some(protocol::ObservedToolOutcome::Unknown),
            );
        }
        if let Some(detail) = self.tool_details.get(&locator) {
            // 工作目录内的路径显示为相对路径，摘要更短。
            let prefix = format!("{}{}", self.config.cwd.display(), std::path::MAIN_SEPARATOR);
            let line = crate::tool_details::summary_line(&detail).replace(&prefix, "");
            self.state
                .tool_line(&notice.thread_id, &notice.turn_id, &notice.item_id, line);
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
        let outbox_id = match self.send_effect(request, None) {
            Ok(outbox_id) => outbox_id,
            Err(error) => {
                self.state.error(SessionPhase::Unknown, error.to_string());
                return;
            }
        };
        self.track_outbox(&request_id, outbox_id);
        self.pending.insert(
            request_id,
            PendingRpc {
                kind: RpcKind::AgentRead,
                deadline: Instant::now() + Duration::from_secs(30),
                identity_thread: Some(id.into()),
                skills_cwd: None,
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
        if method == "skills/changed" {
            self.skills_generation = self.skills_generation.wrapping_add(1);
            self.skills_refresh_force_reload |= self.pending.values().any(|rpc| {
                matches!(
                    rpc.kind,
                    RpcKind::SkillsList {
                        force_reload: true,
                        ..
                    }
                )
            });
            self.queue_skills_refresh(SkillRefreshSource::Changed, false);
            return;
        }
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
                if let Some(request) = self
                    .state
                    .view
                    .requests
                    .iter()
                    .find(|request| request.id == id && request.thread_id == thread)
                {
                    self.observer
                        .request_resolved(&request.reference(), Instant::now());
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
        if method == "thread/tokenUsage/updated" {
            let Some(thread) = params.get("threadId").and_then(Value::as_str) else {
                return;
            };
            let Some(turn) = params.get("turnId").and_then(Value::as_str) else {
                return;
            };
            let Some(usage) = decode_usage(&params) else {
                return;
            };
            let accepted = if self.state.view.thread_id.as_deref() == Some(thread) {
                if self.state.view.turn_id.as_deref() != Some(turn) {
                    return;
                }
                let usage = preserve_cumulative_total(self.state.view.usage, usage);
                self.state.view.usage = usage;
                self.state.set_usage_fact(UsageFact {
                    summary: usage,
                    identity: UsageIdentity {
                        thread_id: Some(thread.to_owned()),
                        turn_id: Some(turn.to_owned()),
                        generation: Some(self.generation),
                    },
                });
                true
            } else {
                if !self.state.agents.current_turn(thread, turn) {
                    return;
                }
                let previous = self
                    .state
                    .agents
                    .snapshots()
                    .into_iter()
                    .find(|agent| agent.info.id == thread)
                    .map_or_else(UsageSummary::default, |agent| agent.usage);
                let usage = preserve_cumulative_total(previous, usage);
                let accepted = self.state.agents.update_usage(thread, turn, usage);
                if accepted {
                    let generation = self
                        .state
                        .agents
                        .snapshots()
                        .into_iter()
                        .find(|agent| agent.info.id == thread)
                        .map(|agent| agent.generation);
                    self.state.set_usage_fact(UsageFact {
                        summary: usage,
                        identity: UsageIdentity {
                            thread_id: Some(thread.to_owned()),
                            turn_id: Some(turn.to_owned()),
                            generation,
                        },
                    });
                }
                accepted
            };
            if accepted {
                if let Some(limit) = self.token_budget_exhausted() {
                    self.interrupt_for_token_budget(limit);
                }
                if let Some((limit, generation)) = self.current_agent_budget(thread, turn) {
                    self.interrupt_for_agent_budget(thread, turn, limit, generation);
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
                        Ok(notice) => match self.observer.tool(&notice, Instant::now()) {
                            Ok(true) => self.observe_tool_detail_event(&notice, &params),
                            Ok(false) => {}
                            Err(error) => {
                                self.state.error(SessionPhase::Unknown, error.to_string());
                            }
                        },
                        Err(error) => self.state.error(SessionPhase::Unknown, error.to_string()),
                    }
                }
                if let Some(Ok(snapshot)) = protocol::decode_file_change_snapshot(method, &params) {
                    let accepted = self
                        .observer
                        .file_change_snapshot(&snapshot, Instant::now());
                    if snapshot.source != protocol::FileChangeSnapshotSource::ItemStarted
                        || accepted
                    {
                        self.file_previews.observe(
                            &snapshot,
                            self.ingress_seq,
                            &mut self.state.view.requests,
                        );
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
                                if let Some(locator) =
                                    self.tool_locator(thread, turn, &output.item_id)
                                {
                                    self.tool_details.observe_output(&locator, &output.text);
                                }
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
                            let previous_turn = self
                                .state
                                .agents
                                .snapshots()
                                .into_iter()
                                .find(|agent| agent.info.id == thread)
                                .and_then(|agent| {
                                    agent.turn_id.map(|turn| (turn, agent.generation))
                                });
                            match self.state.agents.started(thread, turn, self.ingress_seq) {
                                Ok(event) => {
                                    if event.is_some() {
                                        // 子代理进入新 generation 后，旧 usage 只保留在历史累计中，
                                        // 当前 turn 必须等服务端再次确认。
                                        self.state.clear_usage_fact(thread);
                                        if let Some((old_turn, generation)) =
                                            previous_turn.filter(|(old_turn, _)| old_turn != turn)
                                        {
                                            self.observer.retire_child_turn(
                                                thread,
                                                &old_turn,
                                                generation,
                                                Instant::now(),
                                            );
                                            let session = &self.state.view.observation.session_id;
                                            self.tool_details.retire(session, thread, &old_turn);
                                            self.file_previews.retire(thread, &old_turn);
                                            self.state.view.requests.retain(|request| {
                                                request.thread_id != thread
                                                    || request.turn_id != old_turn
                                            });
                                        }
                                        self.read_agent_identity(thread);
                                    }
                                    event
                                }
                                Err(error) => {
                                    self.state.error(SessionPhase::Unknown, error.to_string());
                                    None
                                }
                            }
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
                                self.tool_details.retire(
                                    &self.state.view.observation.session_id,
                                    thread,
                                    turn,
                                );
                                self.file_previews.retire(thread, turn);
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
                if let Some(thread) = self.state.view.thread_id.as_deref() {
                    self.tool_details.retire(
                        &self.state.view.observation.session_id,
                        thread,
                        id.unwrap(),
                    );
                    self.file_previews.retire(thread, id.unwrap());
                }
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

fn decode_usage(params: &Value) -> Option<UsageSummary> {
    let usage = params.get("tokenUsage").unwrap_or(params);
    let number = |paths: &[&str]| {
        paths.iter().find_map(|path| {
            usage
                .pointer(path)
                .and_then(Value::as_u64)
                .or_else(|| params.pointer(path).and_then(Value::as_u64))
        })
    };
    let input_tokens = number(&[
        "/total/inputTokens",
        "/total/input_tokens",
        "/inputTokens",
        "/input_tokens",
    ]);
    let cached_input_tokens = number(&[
        "/total/cachedInputTokens",
        "/total/cached_input_tokens",
        "/total/inputTokensDetails/cachedTokens",
        "/total/input_tokens_details/cached_tokens",
        "/cachedInputTokens",
        "/cached_input_tokens",
        "/inputTokensDetails/cachedTokens",
        "/input_tokens_details/cached_tokens",
    ]);
    let output_tokens = number(&[
        "/total/outputTokens",
        "/total/output_tokens",
        "/outputTokens",
        "/output_tokens",
    ]);
    let reasoning_tokens = number(&[
        "/total/reasoningOutputTokens",
        "/total/reasoning_output_tokens",
        "/total/outputTokensDetails/reasoningTokens",
        "/total/output_tokens_details/reasoning_tokens",
        "/reasoningOutputTokens",
        "/reasoning_output_tokens",
        "/outputTokensDetails/reasoningTokens",
        "/output_tokens_details/reasoning_tokens",
    ]);
    let total_tokens = number(&[
        "/total/totalTokens",
        "/total/total_tokens",
        "/totalTokens",
        "/total_tokens",
    ]);
    let context_window = number(&[
        "/modelContextWindow",
        "/model_context_window",
        "/contextWindow",
        "/context_window",
        "/total/modelContextWindow",
        "/total/model_context_window",
    ]);
    let summary = UsageSummary {
        input_tokens,
        cached_input_tokens,
        output_tokens,
        reasoning_tokens,
        total_tokens,
        context_window,
        source: FactSource::ServerConfirmed,
    };
    summary.has_value().then_some(summary)
}

fn preserve_cumulative_total(previous: UsageSummary, mut current: UsageSummary) -> UsageSummary {
    if previous.source == FactSource::ServerConfirmed
        && current.source == FactSource::ServerConfirmed
    {
        current.total_tokens = match (previous.total_tokens, current.total_tokens) {
            (Some(previous), Some(current)) => Some(previous.max(current)),
            (Some(previous), None) => Some(previous),
            (None, current) => current,
        };
    }
    current
}

#[cfg(test)]
mod tests {
    mod file_progress;
    mod tool_details;

    use super::*;
    use crate::journal::{tests::Fixture as JournalFixture, JournalView, Payload, Replay};
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
        let outbox =
            Outbox::open(config.journal.outbox_path(&config.cwd, &session).unwrap()).unwrap();
        let (client, server) = tokio::io::duplex(65536);
        let (read, write) = tokio::io::split(client);
        (
            ClientHandle::start(
                PipeTransport::new(read, write),
                config.clone(),
                None,
                false,
                Some((observer, journal, outbox)),
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
    async fn durable_outbox_confirms_root_submission_without_persisting_prompt() {
        let fixture = JournalFixture::new();
        let (mut client, mut server, config, session) = journal_harness(&fixture).await;
        ready(&mut server).await;
        let prompt = "PRIVATE_ROOT_PROMPT";
        client
            .commands
            .send(Command::SubmitRootInput {
                text: prompt.into(),
            })
            .await
            .unwrap();
        let turn = next(&mut server).await;
        assert_eq!(turn["method"], "turn/start");
        let path = config.journal.outbox_path(&config.cwd, &session).unwrap();
        let sent = Outbox::open(&path).unwrap();
        let record = sent
            .records()
            .find(|record| record.intent.method == "turn/start")
            .expect("turn/start intent must be durable before sending");
        assert_eq!(record.status, crate::outbox::OutboxStatus::Sent);
        assert!(!std::fs::read_to_string(&path).unwrap().contains(prompt));

        send(
            &mut server,
            json!({"id":turn["id"],"result":{"turn":{"id":"turn-1"}}}),
        )
        .await;
        phase(&mut client, SessionPhase::Running).await;
        let confirmed = Outbox::open(&path).unwrap();
        let record = confirmed
            .records()
            .find(|record| record.intent.method == "turn/start")
            .unwrap();
        assert_eq!(record.status, crate::outbox::OutboxStatus::Confirmed);

        client.commands.send(Command::Quit).await.unwrap();
        client.join.await.unwrap();
    }

    #[test]
    fn persistence_projection_distinguishes_submission_commit_and_uncertainty() {
        let mut view = JournalView {
            session_id: "session".into(),
            submitted_seq: 2,
            committed_seq: 1,
            committed_version: 1,
            error: None,
        };
        assert_eq!(
            persistence_state(Some(&view), None),
            PersistenceState::Submitted
        );
        view.committed_seq = 2;
        assert_eq!(
            persistence_state(Some(&view), None),
            PersistenceState::Committed
        );
        view.error = Some(JournalError::Io);
        assert_eq!(
            persistence_state(Some(&view), None),
            PersistenceState::Uncertain
        );
        view.error = None;
        assert_eq!(
            persistence_state(Some(&view), Some(&JournalError::Closed)),
            PersistenceState::Uncertain
        );
        assert_eq!(persistence_state(None, None), PersistenceState::Uncertain);
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
                request: current_request(&client, RpcId::Number(7)),
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
                request: current_request(&client, RpcId::String("approval-id".into())),
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
        assert!(
            client
                .snapshots
                .borrow()
                .timeline
                .entries
                .iter()
                .any(|entry| entry.evidence.kind
                    == crate::observation::EvidenceKind::ExecutionUnknown)
        );
        assert_eq!(
            client.snapshots.borrow().timeline.high_water,
            client
                .snapshots
                .borrow()
                .observation
                .accepted_evidence_count
        );
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
                request: current_request(&client, RpcId::Number(7)),
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
                request: RequestRef {
                    turn_id: "old-turn".into(),
                    ..current_request(&client, RpcId::Number(7))
                },
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
                request: current_request(&client, RpcId::Number(7)),
            })
            .await
            .unwrap();
        assert_eq!(next(&mut server).await["result"]["decision"], "decline");
        client.commands.send(Command::Quit).await.unwrap();
        client.join.await.unwrap();
    }

    fn current_request(client: &ClientHandle, id: RpcId) -> RequestRef {
        client
            .snapshots
            .borrow()
            .requests
            .iter()
            .find(|request| request.id == id)
            .unwrap()
            .reference()
    }

    #[tokio::test]
    async fn reused_request_delivery_restores_actionable_observation() {
        use crate::observation::{ActivityKind, EvidenceKind, InteractionState};

        let (mut client, mut server) = harness().await;
        running_root(&mut client, &mut server).await;
        for id in [json!(7), json!("7")] {
            let request = json!({"id":id,"method":"item/commandExecution/requestApproval",
                "params":{"threadId":"root","turnId":"root-turn"}});
            observation_event(&mut client, &mut server, request.clone()).await;
            let old_delivery = client.snapshots.borrow().requests[0].reference();
            let first = client.snapshots.borrow().observation.clone();
            let old = first
                .activities
                .iter()
                .find(|activity| activity.attention.requires_action)
                .unwrap();
            let old_id = old.activity_id.clone();
            observation_event(
                &mut client,
                &mut server,
                json!({"method":"serverRequest/resolved","params":{
                    "threadId":"root","requestId":id}}),
            )
            .await;
            let resolved = client.snapshots.borrow().observation.clone();
            assert!(!resolved
                .activities
                .iter()
                .any(|a| a.attention.requires_action));

            observation_event(&mut client, &mut server, request.clone()).await;
            let new_delivery = client.snapshots.borrow().requests[0].reference();
            assert_ne!(old_delivery.received_seq, new_delivery.received_seq);
            assert!(!client.snapshots.borrow().requests[0].responding);
            let recreated = client.snapshots.borrow().observation.clone();
            let pending: Vec<_> = recreated
                .activities
                .iter()
                .filter(|activity| activity.attention.requires_action)
                .collect();
            assert_eq!(
                pending.len(),
                1,
                "a new delivery must restore requires_action"
            );
            assert_ne!(pending[0].activity_id, old_id);
            assert_eq!(pending[0].kind, ActivityKind::WaitingApproval);
            assert_eq!(
                pending[0].interaction_state,
                Some(InteractionState::Pending)
            );
            assert_eq!(
                pending[0].last_evidence.as_ref().unwrap().kind,
                EvidenceKind::RequestCreated
            );
            assert_eq!(
                recreated
                    .activities
                    .iter()
                    .flat_map(|activity| &activity.recent_evidence)
                    .filter(|evidence| evidence.id > resolved.accepted_evidence_count
                        && evidence.kind == EvidenceKind::RequestCreated)
                    .count(),
                2
            );
            assert!(recreated
                .activities
                .iter()
                .any(|activity| activity.activity_id == old_id
                    && activity.interaction_state == Some(InteractionState::Resolved)));
            // A duplicate wire request remains the same accepted delivery.
            observation_event(&mut client, &mut server, request).await;
            assert_eq!(
                client
                    .snapshots
                    .borrow()
                    .observation
                    .accepted_evidence_count,
                recreated.accepted_evidence_count
            );
            observation_event(
                &mut client,
                &mut server,
                json!({"method":"serverRequest/resolved","params":{
                    "threadId":"root","requestId":id}}),
            )
            .await;
        }
        assert_eq!(client.snapshots.borrow().root_start_requests, 1);
        assert!(
            tokio::time::timeout(Duration::from_millis(50), next(&mut server))
                .await
                .is_err()
        );
        client.commands.send(Command::Quit).await.unwrap();
        client.join.await.unwrap();
    }

    #[tokio::test]
    async fn reused_request_ids_cannot_receive_stale_approval_input_or_headless_actions() {
        let (mut client, mut server) = harness().await;
        running_root(&mut client, &mut server).await;
        let approval = json!({"id":7,"method":"item/commandExecution/requestApproval","params":{"threadId":"root","turnId":"root-turn","command":"first"}});
        observation_event(&mut client, &mut server, approval.clone()).await;
        let old = current_request(&client, RpcId::Number(7));
        observation_event(
            &mut client,
            &mut server,
            json!({"method":"serverRequest/resolved","params":{"threadId":"root","requestId":7}}),
        )
        .await;
        observation_event(&mut client, &mut server, approval.clone()).await;
        let current = current_request(&client, RpcId::Number(7));
        assert_ne!(old.received_seq, current.received_seq);
        observation_event(&mut client, &mut server, approval).await;
        assert_eq!(current_request(&client, RpcId::Number(7)), current);
        for reference in [
            old.clone(),
            RequestRef {
                thread_id: "other".into(),
                ..current.clone()
            },
            RequestRef {
                turn_id: "old-turn".into(),
                ..current.clone()
            },
        ] {
            client
                .commands
                .send(Command::AnswerApproval {
                    request: reference,
                    decision: ApprovalDecision::Accept,
                })
                .await
                .unwrap();
        }
        client
            .commands
            .send(Command::AnswerUserInput {
                request: old.clone(),
                answers: BTreeMap::new(),
            })
            .await
            .unwrap();
        client
            .commands
            .send(Command::HeadlessRequest { request: old })
            .await
            .unwrap();
        client
            .commands
            .send(Command::ConfigureAttention {
                class: AttentionClass::Model,
                quiet_ms: 5000,
                attention_ms: 10000,
            })
            .await
            .unwrap();
        client
            .snapshots
            .wait_for(|s| {
                s.notice
                    .as_ref()
                    .is_some_and(|notice| notice.contains("settings applied"))
            })
            .await
            .unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(50), next(&mut server))
                .await
                .is_err()
        );
        assert!(!client.snapshots.borrow().requests[0].responding);
        assert!(client.snapshots.borrow().last_headless_action.is_none());
        assert_eq!(client.snapshots.borrow().root_start_requests, 1);
        client
            .commands
            .send(Command::AnswerApproval {
                request: current.clone(),
                decision: ApprovalDecision::Decline,
            })
            .await
            .unwrap();
        assert_eq!(next(&mut server).await["result"]["decision"], "decline");
        observation_event(
            &mut client,
            &mut server,
            json!({"method":"serverRequest/resolved","params":{"threadId":"root","requestId":7}}),
        )
        .await;
        observation_event(&mut client, &mut server, json!({"id":7,"method":"item/tool/requestUserInput","params":{"threadId":"root","turnId":"root-turn","questions":[{"id":"secret","header":"Secret","question":"Value?","isSecret":true}]}})).await;
        let answers = BTreeMap::from([("secret".into(), vec!["answer".into()])]);
        client
            .commands
            .send(Command::AnswerUserInput {
                request: current,
                answers: answers.clone(),
            })
            .await
            .unwrap();
        client
            .snapshots
            .wait_for(|s| s.notice.as_deref() == Some("Request already resolved."))
            .await
            .unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(50), next(&mut server))
                .await
                .is_err()
        );
        client
            .commands
            .send(Command::AnswerUserInput {
                request: current_request(&client, RpcId::Number(7)),
                answers,
            })
            .await
            .unwrap();
        assert_eq!(
            next(&mut server).await["result"]["answers"]["secret"]["answers"][0],
            "answer"
        );
        client.commands.send(Command::Quit).await.unwrap();
        client.join.await.unwrap();
    }

    #[tokio::test]
    async fn approval_cancel_rejects_stale_actions_and_waits_for_server_resolution_and_terminal() {
        let (mut client, mut server) = harness().await;
        running_root(&mut client, &mut server).await;
        let approval = json!({"id":7,"method":"item/commandExecution/requestApproval","params":{
            "threadId":"root","turnId":"root-turn","availableDecisions":["accept","cancel"]}});
        observation_event(&mut client, &mut server, approval.clone()).await;
        let old = current_request(&client, RpcId::Number(7));
        observation_event(
            &mut client,
            &mut server,
            json!({"method":"serverRequest/resolved","params":{"threadId":"root","requestId":7}}),
        )
        .await;
        observation_event(&mut client, &mut server, approval).await;
        let current = current_request(&client, RpcId::Number(7));
        assert_ne!(old.received_seq, current.received_seq);
        for reference in [
            old,
            RequestRef {
                thread_id: "other".into(),
                ..current.clone()
            },
            RequestRef {
                turn_id: "old-turn".into(),
                ..current.clone()
            },
        ] {
            client
                .commands
                .send(Command::AnswerApproval {
                    request: reference,
                    decision: ApprovalDecision::Cancel,
                })
                .await
                .unwrap();
        }
        client
            .commands
            .send(Command::AnswerApproval {
                request: current.clone(),
                decision: ApprovalDecision::Decline,
            })
            .await
            .unwrap();
        client
            .snapshots
            .wait_for(|s| {
                s.notice.as_deref() == Some("this decision is not available for the request")
            })
            .await
            .unwrap();
        assert!(!client.snapshots.borrow().requests[0].responding);
        let cancel = Command::AnswerApproval {
            request: current.clone(),
            decision: ApprovalDecision::Cancel,
        };
        client.commands.send(cancel.clone()).await.unwrap();
        client.commands.send(cancel.clone()).await.unwrap();
        assert_eq!(
            next(&mut server).await,
            json!({"id":7,"result":{"decision":"cancel"}})
        );
        client
            .snapshots
            .wait_for(|s| s.requests[0].responding)
            .await
            .unwrap();
        assert_eq!(client.snapshots.borrow().phase, SessionPhase::Running);
        assert!(
            tokio::time::timeout(Duration::from_millis(50), next(&mut server))
                .await
                .is_err()
        );
        observation_event(
            &mut client,
            &mut server,
            json!({"method":"serverRequest/resolved","params":{"threadId":"root","requestId":7}}),
        )
        .await;
        let snapshot = client.snapshots.borrow().clone();
        assert!(snapshot.requests.is_empty());
        assert_eq!(snapshot.phase, SessionPhase::Running);
        assert!(snapshot.observation.activities.iter().any(|a| a.request_id
            == Some(RpcId::Number(7))
            && a.interaction_state == Some(crate::observation::InteractionState::Resolved)
            && a.recent_evidence
                .iter()
                .any(|e| e.kind == crate::observation::EvidenceKind::RequestResolved)));
        client.commands.send(cancel).await.unwrap();
        client
            .snapshots
            .wait_for(|s| s.notice.as_deref() == Some("Request already resolved."))
            .await
            .unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(50), next(&mut server))
                .await
                .is_err()
        );
        observation_event(
            &mut client,
            &mut server,
            json!({"method":"turn/completed","params":{
            "threadId":"root","turn":{"id":"root-turn","status":"interrupted"}}}),
        )
        .await;
        phase(&mut client, SessionPhase::Interrupted).await;
        assert_eq!(client.snapshots.borrow().root_start_requests, 1);
        client.commands.send(Command::Quit).await.unwrap();
        client.join.await.unwrap();
    }

    #[tokio::test]
    async fn file_approval_context_is_owned_live_only_and_retired_with_its_turn() {
        let (mut client, mut server) = harness().await;
        running_root(&mut client, &mut server).await;
        let item = json!({"id":"file-1","type":"fileChange","status":"inProgress","changes":[{"path":"PRIVATE_FILE.rs","kind":{"type":"update","move_path":null},"diff":"PRIVATE_DIFF"}]});
        observation_event(&mut client, &mut server, json!({"method":"item/started","params":{"threadId":"other","turnId":"root-turn","item":item}})).await;
        observation_event(&mut client, &mut server, json!({"id":7,"method":"item/fileChange/requestApproval","params":{"threadId":"root","turnId":"root-turn","itemId":"file-1","reason":"PRIVATE_REASON","grantRoot":"PRIVATE_ROOT"}})).await;
        assert!(client.snapshots.borrow().requests[0]
            .details
            .file_preview
            .is_none());
        observation_event(&mut client, &mut server, json!({"method":"item/started","params":{"threadId":"root","turnId":"old-turn","item":item}})).await;
        assert!(client.snapshots.borrow().requests[0]
            .details
            .file_preview
            .is_none());
        observation_event(&mut client, &mut server, json!({"method":"item/started","params":{"threadId":"root","turnId":"root-turn","item":item}})).await;
        observation_event(&mut client, &mut server, json!({"method":"item/fileChange/patchUpdated","params":{"threadId":"root","turnId":"root-turn","itemId":"file-1","changes":[{"path":"PRIVATE_FILE.rs","kind":{"type":"update","move_path":null},"diff":"PRIVATE_PATCH_UPDATED"}]}})).await;
        let latest_preview = client.snapshots.borrow().requests[0]
            .details
            .file_preview
            .clone();
        observation_event(&mut client, &mut server, json!({"method":"item/started","params":{"threadId":"root","turnId":"root-turn","item":{"id":"file-1","type":"fileChange","status":"inProgress","changes":[{"path":"PRIVATE_FILE.rs","kind":{"type":"update","move_path":null},"diff":"PRIVATE_DIFF"}]}}})).await;
        assert_eq!(
            client.snapshots.borrow().requests[0].details.file_preview,
            latest_preview,
            "a repeated start snapshot must not replace the newer approval preview"
        );
        observation_event(&mut client, &mut server, json!({"method":"item/started","params":{"threadId":"root","turnId":"root-turn","item":{"id":"file-1","type":"fileChange","status":"inProgress"}}})).await;
        assert_eq!(
            client.snapshots.borrow().requests[0].details.file_preview,
            latest_preview,
            "a repeated start without changes must preserve an available preview"
        );
        observation_event(&mut client, &mut server, json!({"method":"item/fileChange/patchUpdated","params":{"threadId":"root","turnId":"root-turn","itemId":"file-1"}})).await;
        assert_eq!(
            client.snapshots.borrow().requests[0].details.file_preview,
            latest_preview,
            "a patch update without changes must preserve the cached preview"
        );
        observation_event(&mut client, &mut server, json!({"method":"item/fileChange/patchUpdated","params":{"threadId":"root","turnId":"root-turn","itemId":"file-1","changes":[{"path":"PRIVATE_FILE.rs","kind":{"type":"update","move_path":42},"diff":"PRIVATE_MALFORMED_PATCH"}]}})).await;
        observation_event(&mut client, &mut server, json!({"method":"item/fileChange/patchUpdated","params":{"threadId":"root","turnId":"old-turn","itemId":"file-1","changes":[{"path":"PRIVATE_FILE.rs","kind":{"type":"update","move_path":null},"diff":"PRIVATE_STALE_PATCH"}]}})).await;
        assert!(client.snapshots.borrow().requests[0]
            .details
            .file_preview
            .as_ref()
            .unwrap()
            .text
            .contains("PRIVATE_PATCH_UPDATED"));
        assert!(!client.snapshots.borrow().requests[0]
            .details
            .file_preview
            .as_ref()
            .unwrap()
            .text
            .contains("PRIVATE_MALFORMED_PATCH"));
        assert!(!client.snapshots.borrow().requests[0]
            .details
            .file_preview
            .as_ref()
            .unwrap()
            .text
            .contains("PRIVATE_STALE_PATCH"));
        observation_event(&mut client, &mut server, json!({"id":8,"method":"item/fileChange/requestApproval","params":{"threadId":"root","turnId":"root-turn","itemId":"file-1"}})).await;
        assert!(client.snapshots.borrow().requests[1]
            .details
            .file_preview
            .is_some());
        let current = client.snapshots.borrow().clone();
        let observation = serde_json::to_string(&current.observation).unwrap();
        let timeline = format!("{:?}", current.timeline);
        let journal =
            serde_json::to_string(&StoredSnapshot::capture(&client.snapshots.borrow())).unwrap();
        for private in [
            "PRIVATE_FILE",
            "PRIVATE_DIFF",
            "PRIVATE_PATCH_UPDATED",
            "PRIVATE_MALFORMED_PATCH",
            "PRIVATE_STALE_PATCH",
            "PRIVATE_REASON",
            "PRIVATE_ROOT",
        ] {
            assert!(
                !observation.contains(private),
                "observation leaked {private}"
            );
            assert!(!timeline.contains(private), "timeline leaked {private}");
            assert!(
                !journal.contains(private),
                "journal snapshot leaked {private}"
            );
        }
        observation_event(&mut client, &mut server, json!({"method":"turn/completed","params":{"threadId":"root","turn":{"id":"root-turn","status":"completed"}}})).await;
        assert!(client.snapshots.borrow().requests.is_empty());
        client
            .commands
            .send(Command::SubmitRootInput {
                text: "next".into(),
            })
            .await
            .unwrap();
        let start = next(&mut server).await;
        send(
            &mut server,
            json!({"id":start["id"],"result":{"turn":{"id":"next-turn"}}}),
        )
        .await;
        phase(&mut client, SessionPhase::Running).await;
        observation_event(&mut client, &mut server, json!({"id":7,"method":"item/fileChange/requestApproval","params":{"threadId":"root","turnId":"next-turn","itemId":"file-1"}})).await;
        assert!(client.snapshots.borrow().requests[0]
            .details
            .file_preview
            .is_none());
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
    async fn timeline_archives_accepted_evidence_without_tick_configuration_or_secret_content() {
        use crate::observation::{EvidenceKind, ExecutionState};
        let (mut client, mut server) = harness().await;
        running_root(&mut client, &mut server).await;
        let initial = client.snapshots.borrow().timeline.clone();
        assert_eq!(
            initial.high_water,
            client
                .snapshots
                .borrow()
                .observation
                .accepted_evidence_count
        );
        client
            .commands
            .send(Command::ConfigureAttention {
                class: AttentionClass::Model,
                quiet_ms: 10,
                attention_ms: 20,
            })
            .await
            .unwrap();
        client
            .snapshots
            .wait_for(|snapshot| snapshot.observation.settings.model.quiet_ms == 10)
            .await
            .unwrap();
        tokio::time::advance(Duration::from_secs(1)).await;
        tokio::task::yield_now().await;
        assert!(std::sync::Arc::ptr_eq(
            &initial.entries,
            &client.snapshots.borrow().timeline.entries
        ));
        observation_event(&mut client, &mut server, json!({"method":"item/agentMessage/delta",
            "params":{"threadId":"root","turnId":"old-turn","itemId":"stale","delta":"PRIVATE_STALE"}})).await;
        assert_eq!(
            client.snapshots.borrow().timeline.high_water,
            initial.high_water
        );
        for _ in 0..20 {
            observation_event(&mut client, &mut server, json!({"method":"item/agentMessage/delta",
                "params":{"threadId":"root","turnId":"root-turn","itemId":"message","delta":"PRIVATE_BODY中文👋"}})).await;
        }
        observation_event(
            &mut client,
            &mut server,
            json!({"method":"item/started",
            "params":{"threadId":"root","turnId":"root-turn","item":{
                "id":"tool","type":"commandExecution","command":"PRIVATE_TOOL"}}}),
        )
        .await;
        observation_event(
            &mut client,
            &mut server,
            json!({"method":"item/completed",
            "params":{"threadId":"root","turnId":"root-turn","item":{
                "id":"tool","type":"commandExecution","status":"completed"}}}),
        )
        .await;
        observation_event(
            &mut client,
            &mut server,
            json!({"id":7,"method":"item/commandExecution/requestApproval",
            "params":{"threadId":"root","turnId":"root-turn","command":"PRIVATE_COMMAND"}}),
        )
        .await;
        let reference = client.snapshots.borrow().requests[0].reference();
        let archive = client.snapshots.borrow().timeline.clone();
        let metrics = client.snapshots.borrow().diagnostics.clone();
        assert!(metrics.transport_bytes_in > 0);
        assert!(metrics.transport_bytes_out > 0);
        assert!(metrics.telemetry_events >= 20);
        assert!(metrics.control_events >= 4);
        assert!(archive
            .entries
            .iter()
            .any(|entry| entry.evidence.kind == EvidenceKind::ToolCompleted
                && entry.scope == crate::observation::ActivityScope::Tool
                && entry.item_id.as_deref() == Some("tool")));
        assert_eq!(
            archive.high_water,
            client
                .snapshots
                .borrow()
                .observation
                .accepted_evidence_count
        );
        assert!(archive.entries.len() > 8);
        assert!(archive
            .entries
            .iter()
            .any(|entry| entry.request.as_ref() == Some(&reference)
                && entry.evidence.kind == EvidenceKind::RequestCreated));
        let diagnostic = format!("{archive:?}");
        let stored =
            serde_json::to_string(&StoredSnapshot::capture(&client.snapshots.borrow())).unwrap();
        for private in [
            "PRIVATE_BODY",
            "PRIVATE_COMMAND",
            "PRIVATE_STALE",
            "PRIVATE_TOOL",
        ] {
            assert!(!diagnostic.contains(private));
            assert!(!stored.contains(private));
        }
        assert!(!stored.contains("\"timeline\""));
        assert!(archive
            .entries
            .iter()
            .zip(archive.entries.iter().skip(1))
            .all(|(previous, next)| previous.evidence.id < next.evidence.id));
        assert_eq!(initial.entries.len(), initial.high_water as usize);
        observation_event(
            &mut client,
            &mut server,
            json!({"method":"serverRequest/resolved",
            "params":{"threadId":"root","requestId":7}}),
        )
        .await;
        observation_event(
            &mut client,
            &mut server,
            json!({"method":"turn/completed",
            "params":{"threadId":"root","turn":{"id":"root-turn","status":"completed"}}}),
        )
        .await;
        let completed = client.snapshots.borrow().timeline.clone();
        assert!(completed
            .entries
            .iter()
            .any(|entry| entry.evidence.kind == EvidenceKind::TurnCompleted
                && entry.execution_state == ExecutionState::Completed));
        assert_eq!(client.snapshots.borrow().root_start_requests, 1);
        assert!(
            tokio::time::timeout(Duration::from_millis(50), next(&mut server))
                .await
                .is_err()
        );
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

    #[tokio::test]
    async fn compaction_observation_is_inert_for_root_and_child_gate_execution() {
        use crate::observation::{ActivityScope, EvidenceKind, EvidenceSource, ExecutionState};
        let (mut client, mut server) = harness().await;
        running_root(&mut client, &mut server).await;
        child(&mut server, "a", "a-1").await;
        wait_call(&mut server, "compaction-wait", vec!["a"]).await;
        phase(&mut client, SessionPhase::GatePending).await;
        for (thread, turn, id) in [
            ("root", "root-turn", "root-compaction"),
            ("a", "a-1", "child-compaction"),
        ] {
            observation_event(&mut client, &mut server, json!({"method":"item/started","params":{"threadId":thread,"turnId":turn,"item":{"id":id,"type":"contextCompaction"}}})).await;
            observation_event(&mut client, &mut server, json!({"method":"item/completed","params":{"threadId":thread,"turnId":turn,"item":{"id":id,"type":"contextCompaction"}}})).await;
        }
        let before_duplicate = client
            .snapshots
            .borrow()
            .observation
            .accepted_evidence_count;
        for (thread, turn, id) in [
            ("root", "root-turn", "root-compaction"),
            ("a", "a-1", "child-compaction"),
        ] {
            observation_event(&mut client, &mut server, json!({"method":"item/started","params":{"threadId":thread,"turnId":turn,"item":{"id":id,"type":"contextCompaction"}}})).await;
            observation_event(&mut client, &mut server, json!({"method":"item/completed","params":{"threadId":thread,"turnId":turn,"item":{"id":id,"type":"contextCompaction"}}})).await;
        }
        let snapshot = client.snapshots.borrow().clone();
        assert_eq!(snapshot.phase, SessionPhase::GatePending);
        assert!(snapshot.gate.as_ref().unwrap().pending);
        assert_eq!(snapshot.root_start_requests, 1);
        assert_eq!(
            snapshot.observation.accepted_evidence_count,
            before_duplicate
        );
        for (thread, turn, id) in [
            ("root", "old-root-turn", "stale-root-compaction"),
            ("a", "old-child-turn", "stale-child-compaction"),
            ("foreign", "root-turn", "foreign-compaction"),
        ] {
            observation_event(&mut client, &mut server, json!({"method":"item/completed","params":{"threadId":thread,"turnId":turn,"item":{"id":id,"type":"contextCompaction"}}})).await;
        }
        let snapshot = client.snapshots.borrow().clone();
        assert_eq!(
            snapshot.observation.accepted_evidence_count,
            before_duplicate
        );
        assert!(snapshot.observation.activities.iter().all(|activity| {
            !matches!(
                activity.item_id.as_deref(),
                Some("stale-root-compaction")
                    | Some("stale-child-compaction")
                    | Some("foreign-compaction")
            )
        }));
        assert_eq!(snapshot.root_start_requests, 1);
        for (id, owner) in [("root-compaction", "root"), ("child-compaction", "a")] {
            let activity = snapshot
                .observation
                .activities
                .iter()
                .find(|activity| activity.item_id.as_deref() == Some(id))
                .unwrap();
            assert_eq!(activity.identity.agent_id, owner);
            assert_eq!(activity.scope, ActivityScope::Tool);
            assert_eq!(
                activity.tool_category,
                Some(protocol::ToolCategory::Compaction)
            );
            assert_eq!(activity.execution_state, ExecutionState::Completed);
            assert_eq!(
                activity.last_evidence.as_ref().unwrap().source,
                EvidenceSource::AppServer
            );
            assert!(activity
                .recent_evidence
                .iter()
                .any(|evidence| evidence.kind == EvidenceKind::ToolCompleted));
        }
        assert!(
            tokio::time::timeout(Duration::from_millis(50), next(&mut server))
                .await
                .is_err(),
            "compaction evidence cannot release Gate or send another root request"
        );
        client.commands.send(Command::Quit).await.unwrap();
        client.join.await.unwrap();
    }

    #[tokio::test]
    async fn unfinished_compaction_is_unknown_when_its_root_or_child_turn_ends() {
        use crate::observation::{EvidenceSource, ExecutionState};
        let (mut client, mut server) = harness().await;
        running_root(&mut client, &mut server).await;
        child(&mut server, "a", "a-1").await;
        for (thread, turn, id) in [
            ("root", "root-turn", "root-open-compaction"),
            ("a", "a-1", "child-open-compaction"),
        ] {
            observation_event(&mut client, &mut server, json!({"method":"item/started","params":{"threadId":thread,"turnId":turn,"item":{"id":id,"type":"contextCompaction"}}})).await;
        }
        observation_event(&mut client, &mut server, json!({"method":"turn/completed","params":{"threadId":"a","turn":{"id":"a-1","status":"completed"}}})).await;
        let child = client
            .snapshots
            .borrow()
            .observation
            .activities
            .iter()
            .find(|activity| activity.item_id.as_deref() == Some("child-open-compaction"))
            .unwrap()
            .clone();
        assert_eq!(child.execution_state, ExecutionState::Unknown);
        assert_eq!(
            child.last_evidence.as_ref().unwrap().source,
            EvidenceSource::Core
        );

        observation_event(&mut client, &mut server, json!({"method":"turn/completed","params":{"threadId":"root","turn":{"id":"root-turn","status":"completed"}}})).await;
        let root = client
            .snapshots
            .borrow()
            .observation
            .activities
            .iter()
            .find(|activity| activity.item_id.as_deref() == Some("root-open-compaction"))
            .unwrap()
            .clone();
        assert_eq!(root.execution_state, ExecutionState::Unknown);
        assert_eq!(
            root.last_evidence.as_ref().unwrap().source,
            EvidenceSource::Core
        );
        client.commands.send(Command::Quit).await.unwrap();
        client.join.await.unwrap();
    }

    #[tokio::test]
    async fn unfinished_compaction_is_unknown_on_transport_disconnect() {
        use crate::observation::{EvidenceSource, ExecutionState};
        let (mut client, mut server) = harness().await;
        running_root(&mut client, &mut server).await;
        observation_event(&mut client, &mut server, json!({"method":"item/started","params":{"threadId":"root","turnId":"root-turn","item":{"id":"open-compaction","type":"contextCompaction"}}})).await;
        drop(server);
        phase(&mut client, SessionPhase::Disconnected).await;
        let activity = client
            .snapshots
            .borrow()
            .observation
            .activities
            .iter()
            .find(|activity| activity.item_id.as_deref() == Some("open-compaction"))
            .unwrap()
            .clone();
        assert_eq!(activity.execution_state, ExecutionState::Unknown);
        assert_eq!(
            activity.last_evidence.as_ref().unwrap().source,
            EvidenceSource::Core
        );
        assert_eq!(
            activity.last_evidence.as_ref().unwrap().kind,
            crate::observation::EvidenceKind::ExecutionUnknown
        );
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
        let outbox =
            Outbox::open(config.journal.outbox_path(&config.cwd, &session).unwrap()).unwrap();
        let replay_config = config.clone();
        let mut server = AppServer::spawn_command(command).unwrap();
        let pipe = server.pipe.take().unwrap();
        let mut client = ClientHandle::start(
            pipe,
            config,
            Some(server),
            false,
            Some((observer, journal, outbox)),
        );
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
                request: current_request(&client, RpcId::String("approval-paused".into())),
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
        let skills = next(&mut server).await;
        assert_eq!(skills["method"], "skills/list");
        send(&mut server, json!({"id":skills["id"],"result":{"data":[{"cwd":skills["params"]["cwds"][0],"errors":[],"skills":[]}]}})).await;
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
            .wait_for(|s| s.scheduler.queued_roots == 1)
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
                    .wait_for(|s| s.scheduler.queued_roots == 1)
                    .await
                    .unwrap();
                client.commands.send(Command::Interrupt).await.unwrap();
                let interrupt = next(&mut server).await;
                assert_eq!(interrupt["method"], "turn/interrupt");
                send(&mut server, json!({"id":interrupt["id"],"result":{}})).await;
                send(&mut server, json!({"method":"turn/completed","params":{"threadId":"root","turn":{"id":"root-turn","status":"interrupted"}}})).await;
                phase(&mut client, SessionPhase::Interrupted).await;
                assert_eq!(client.snapshots.borrow().scheduler.queued_roots, 1);
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
    async fn configured_native_child_capacity_stops_execution_with_an_unknown_outcome() {
        let (mut client, mut server) = harness_with_config(Config {
            max_native_children: 1,
            ..Default::default()
        })
        .await;
        running_root(&mut client, &mut server).await;
        child(&mut server, "a", "a-1").await;
        send(
            &mut server,
            json!({
                "method":"thread/started",
                "params":{"thread":{"id":"b","parentThreadId":"root"}}
            }),
        )
        .await;
        let snapshot = client
            .snapshots
            .wait_for(|snapshot| snapshot.phase == SessionPhase::Unknown)
            .await
            .unwrap()
            .clone();
        assert!(snapshot
            .last_error
            .as_deref()
            .is_some_and(|error| error.contains("capacity reached")));
        assert_eq!(snapshot.agents.len(), 1);
        client.join.await.unwrap();
    }

    #[tokio::test]
    async fn configured_native_turn_capacity_rejects_a_second_active_turn() {
        let (mut client, mut server) = harness_with_config(Config {
            max_native_children: 2,
            max_native_turns: 1,
            ..Default::default()
        })
        .await;
        running_root(&mut client, &mut server).await;
        assert_eq!(
            client.snapshots.borrow().scheduler.native_slot_capacity,
            Some(1)
        );
        child(&mut server, "a", "a-1").await;
        send(
            &mut server,
            json!({
                "method":"thread/started",
                "params":{"thread":{"id":"b","parentThreadId":"root","source":{"subAgent":{"thread_spawn":{"parent_thread_id":"root","depth":1,"agent_path":"/root/b"}}}}}
            }),
        )
        .await;
        send(
            &mut server,
            json!({"method":"turn/started","params":{"threadId":"b","turn":{"id":"b-1"}}}),
        )
        .await;
        let snapshot = client
            .snapshots
            .wait_for(|snapshot| snapshot.phase == SessionPhase::Unknown)
            .await
            .unwrap()
            .clone();
        assert!(snapshot
            .last_error
            .as_deref()
            .is_some_and(|error| error.contains("active turn capacity")));
        assert_eq!(snapshot.agents.len(), 2);
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
                request: current_request(&client, RpcId::String("remaining-approval".into())),
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
                .wait_for(|s| s.scheduler.queued_roots == 1 && s.requests.len() == 1),
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
                request: current_request(&client, RpcId::String("child-approval".into())),
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
    async fn rearmed_active_child_keeps_capacity_slot_for_other_agents() {
        let (mut client, mut server) = harness_with_config(Config {
            max_native_children: 2,
            max_native_turns: 1,
            ..Default::default()
        })
        .await;
        running_root(&mut client, &mut server).await;
        send(
            &mut server,
            json!({
                "method":"thread/started",
                "params":{"thread":{"id":"a","parentThreadId":"root","source":{"subAgent":{}}}}
            }),
        )
        .await;
        send(
            &mut server,
            json!({
                "method":"item/started",
                "params":{"threadId":"root","turnId":"root-turn","item":{
                    "id":"spawn-a","type":"subAgentActivity","agentThreadId":"a",
                    "agentPath":"/root/a","kind":"started"
                }}
            }),
        )
        .await;
        send(
            &mut server,
            json!({"method":"turn/started","params":{"threadId":"a","turn":{"id":"old"}}}),
        )
        .await;
        let identity = next(&mut server).await;
        assert_eq!(identity["method"], "thread/read");
        send(
            &mut server,
            json!({
                "id":identity["id"],
                "result":{"thread":{"id":"a","parentThreadId":"root"}}
            }),
        )
        .await;
        send(
            &mut server,
            json!({
                "method":"item/started",
                "params":{"threadId":"root","turnId":"root-turn","item":{
                    "id":"followup-capacity","type":"collabAgentToolCall","senderThreadId":"root",
                    "receiverThreadIds":["a"],"tool":"followupTask","status":"in_progress"
                }}
            }),
        )
        .await;
        send(
            &mut server,
            json!({
                "method":"item/completed",
                "params":{"threadId":"root","turnId":"root-turn","item":{
                    "id":"followup-capacity","type":"collabAgentToolCall","senderThreadId":"root",
                    "receiverThreadIds":["a"],"tool":"followupTask","status":"completed",
                    "agentsStates":{"a":{"status":"completed"}}
                }}
            }),
        )
        .await;
        child(&mut server, "b", "b-1").await;
        let snapshot = client
            .snapshots
            .wait_for(|snapshot| snapshot.phase == SessionPhase::Unknown)
            .await
            .unwrap()
            .clone();
        assert!(snapshot
            .last_error
            .as_deref()
            .is_some_and(|error| error.contains("active turn capacity")));
        client.join.await.unwrap();
    }

    #[tokio::test]
    async fn rearmed_active_child_replacement_reuses_one_capacity_slot() {
        let (mut client, mut server) = harness_with_config(Config {
            max_native_children: 2,
            max_native_turns: 1,
            ..Default::default()
        })
        .await;
        running_root(&mut client, &mut server).await;
        send(
            &mut server,
            json!({
                "method":"thread/started",
                "params":{"thread":{"id":"a","parentThreadId":"root","source":{"subAgent":{}}}}
            }),
        )
        .await;
        send(
            &mut server,
            json!({
                "method":"item/started",
                "params":{"threadId":"root","turnId":"root-turn","item":{
                    "id":"spawn-a-replace","type":"subAgentActivity","agentThreadId":"a",
                    "agentPath":"/root/a","kind":"started"
                }}
            }),
        )
        .await;
        send(
            &mut server,
            json!({"method":"turn/started","params":{"threadId":"a","turn":{"id":"old"}}}),
        )
        .await;
        let identity = next(&mut server).await;
        assert_eq!(identity["method"], "thread/read");
        send(
            &mut server,
            json!({
                "id":identity["id"],
                "result":{"thread":{"id":"a","parentThreadId":"root"}}
            }),
        )
        .await;
        send(
            &mut server,
            json!({
                "method":"item/started",
                "params":{"threadId":"root","turnId":"root-turn","item":{
                    "id":"followup-replace","type":"collabAgentToolCall","senderThreadId":"root",
                    "receiverThreadIds":["a"],"tool":"followupTask","status":"in_progress"
                }}
            }),
        )
        .await;
        send(
            &mut server,
            json!({
                "method":"item/completed",
                "params":{"threadId":"root","turnId":"root-turn","item":{
                    "id":"followup-replace","type":"collabAgentToolCall","senderThreadId":"root",
                    "receiverThreadIds":["a"],"tool":"followupTask","status":"completed",
                    "agentsStates":{"a":{"status":"completed"}}
                }}
            }),
        )
        .await;
        send(
            &mut server,
            json!({
                "method":"turn/started",
                "params":{"threadId":"a","turn":{"id":"new"}}
            }),
        )
        .await;
        let snapshot = client
            .snapshots
            .wait_for(|snapshot| {
                snapshot.scheduler.tasks.iter().any(|task| {
                    task.external.as_ref().is_some_and(|external| {
                        external.thread_id == "a"
                            && external.turn_id == "new"
                            && task.native_slot_reserved
                    })
                })
            })
            .await
            .unwrap()
            .clone();
        assert_eq!(snapshot.scheduler.native_slot_capacity, Some(1));
        assert_eq!(snapshot.scheduler.native_slots_reserved, 1);
        assert_eq!(snapshot.phase, SessionPhase::Running);
        send(
            &mut server,
            json!({"method":"turn/completed","params":{"threadId":"a","turn":{"id":"old","status":"completed"}}}),
        )
        .await;
        send(
            &mut server,
            json!({"method":"turn/completed","params":{"threadId":"a","turn":{"id":"new","status":"completed"}}}),
        )
        .await;
        let snapshot = client
            .snapshots
            .wait_for(|snapshot| snapshot.scheduler.native_slots_reserved == 0)
            .await
            .unwrap();
        assert_eq!(snapshot.scheduler.native_slot_capacity, Some(1));
        drop(snapshot);
        send(
            &mut server,
            json!({"method":"turn/completed","params":{"threadId":"root","turn":{"id":"root-turn","status":"completed"}}}),
        )
        .await;
        phase(&mut client, SessionPhase::Completed).await;
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
    async fn live_app_server_skills_list_finds_fixture_without_a_model_turn() {
        struct Fixture {
            root: std::path::PathBuf,
            previous_home: Option<std::ffi::OsString>,
        }
        impl Drop for Fixture {
            fn drop(&mut self) {
                if let Some(home) = self.previous_home.take() {
                    std::env::set_var("CODEX_HOME", home);
                } else {
                    std::env::remove_var("CODEX_HOME");
                }
                let target = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("target")
                    .canonicalize()
                    .unwrap();
                if let Ok(root) = self.root.canonicalize() {
                    if root.starts_with(&target)
                        && root
                            .file_name()
                            .unwrap()
                            .to_string_lossy()
                            .starts_with("skills-fixture-")
                    {
                        let _ = std::fs::remove_dir_all(root);
                    }
                }
            }
        }
        static NEXT_FIXTURE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let target = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("target");
        let root = loop {
            let candidate = target.join(format!(
                "skills-fixture-{}-{}",
                std::process::id(),
                NEXT_FIXTURE.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            ));
            match std::fs::create_dir(&candidate) {
                Ok(()) => break candidate,
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => panic!("could not create skills smoke fixture: {error}"),
            }
        };
        let workspace = root.join("workspace");
        let home = root.join("home");
        let skill = workspace
            .join(".agents")
            .join("skills")
            .join("fixture-skill");
        std::fs::create_dir_all(&skill).unwrap();
        std::fs::create_dir_all(&home).unwrap();
        std::fs::write(
            skill.join("SKILL.md"),
            "---\nname: fixture-skill\ndescription: isolated skills list smoke fixture\n---\n# Fixture skill\n",
        )
        .unwrap();
        let fixture = Fixture {
            root,
            previous_home: std::env::var_os("CODEX_HOME"),
        };
        std::env::set_var("CODEX_HOME", home);
        let journal = JournalFixture::new();
        let config = Config {
            cwd: workspace,
            journal: journal.settings(),
            sandbox: "read-only".into(),
            windows_sandbox: Some("unelevated".into()),
            ..Default::default()
        };
        let mut client = ClientHandle::spawn(config).await.unwrap();
        let snapshot = tokio::time::timeout(
            Duration::from_secs(45),
            client.snapshots.wait_for(|snapshot| {
                snapshot.phase == SessionPhase::Ready
                    && snapshot.skills.freshness == crate::skills::SkillFreshness::Current
            }),
        )
        .await
        .expect("live skills/list did not return within 45 seconds")
        .unwrap()
        .clone();
        assert!(matches!(
            snapshot.skills.availability,
            crate::skills::SkillAvailability::Available | crate::skills::SkillAvailability::Partial
        ));
        assert!(snapshot
            .skills
            .entries
            .iter()
            .any(|entry| entry.name == "fixture-skill"));
        assert_eq!(snapshot.root_turn_count, 0);
        assert_eq!(snapshot.root_start_requests, 0);
        let second_skill = fixture
            .root
            .join("workspace/.agents/skills/explicit-refresh");
        std::fs::create_dir_all(&second_skill).unwrap();
        std::fs::write(
            second_skill.join("SKILL.md"),
            "---\nname: explicit-refresh\ndescription: explicit reload fixture\n---\n# Explicit refresh\n",
        )
        .unwrap();
        client.commands.send(Command::RefreshSkills).await.unwrap();
        let refreshed = tokio::time::timeout(
            Duration::from_secs(15),
            client.snapshots.wait_for(|snapshot| {
                snapshot.skills.freshness == crate::skills::SkillFreshness::Current
                    && snapshot
                        .skills
                        .entries
                        .iter()
                        .any(|entry| entry.name == "explicit-refresh")
            }),
        )
        .await
        .expect("explicit forceReload did not find the newly added fixture skill")
        .unwrap()
        .clone();
        assert_eq!(refreshed.root_turn_count, 0);
        assert_eq!(refreshed.root_start_requests, 0);
        client.commands.send(Command::Quit).await.unwrap();
        let report = client.join.await.unwrap();
        assert!(report.cleanup_error.is_none());
        drop(fixture);
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
        let retained_config = config.clone();
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
        let report = client.join.await.unwrap();
        let snapshot = outcome.unwrap_or_else(|_| {
            let latest = client.snapshots.borrow();
            panic!("live startup did not reach a result within 35 seconds; phase={:?}, notice={:?}, error={:?}", latest.phase, latest.notice, latest.last_error)
        });
        let session = client
            .snapshots
            .borrow()
            .journal
            .as_ref()
            .expect("startup must retain its journal")
            .session_id
            .clone();
        let replay =
            Replay::open(&retained_config.journal, &retained_config.cwd, &session, 0).unwrap();
        let final_state = replay.latest_state();
        eprintln!(
            "[startup-check] {}",
            json!({
                "phase": format!("{:?}", snapshot.phase),
                "ready": snapshot.phase == SessionPhase::Ready,
                "root_turn_count": snapshot.root_turn_count,
                "root_start_requests": final_state.root_start_requests,
                "cleanup_confirmed": report.cleanup_error.is_none() && final_state.cleanup_confirmed == Some(true),
                "journal_confirmed": report.journal_error.is_none() && final_state.session_closed,
                "shell_preflight_timeout": snapshot.last_error.as_deref().is_some_and(|error| error.contains("Shell preflight timed out")),
            })
        );
        assert_eq!(final_state.root_start_requests, 0);
        assert_eq!(snapshot.root_turn_count, 0);
        assert!(report.cleanup_error.is_none());
        assert!(report.journal_error.is_none());
        assert_eq!(final_state.cleanup_confirmed, Some(true));
        assert!(final_state.session_closed);
        assert_eq!(
            snapshot.phase,
            SessionPhase::Ready,
            "{:?}",
            snapshot.last_error
        );
    }

    async fn harness() -> (ClientHandle, BufReader<tokio::io::DuplexStream>) {
        harness_with_config(Config::default()).await
    }

    async fn harness_with_config(
        config: Config,
    ) -> (ClientHandle, BufReader<tokio::io::DuplexStream>) {
        let (client, server) = tokio::io::duplex(65536);
        let (read, write) = tokio::io::split(client);
        (
            ClientHandle::start(PipeTransport::new(read, write), config, None, false, None),
            BufReader::new(server),
        )
    }

    async fn isolated_harness(
        thread_version: &str,
    ) -> (
        ClientHandle,
        BufReader<tokio::io::DuplexStream>,
        BufReader<tokio::io::DuplexStream>,
    ) {
        isolated_harness_with_cleanup(thread_version, None).await
    }

    async fn isolated_harness_with_cleanup(
        thread_version: &str,
        cleanup: Option<tokio::sync::oneshot::Receiver<bool>>,
    ) -> (
        ClientHandle,
        BufReader<tokio::io::DuplexStream>,
        BufReader<tokio::io::DuplexStream>,
    ) {
        let (main_client, main_server) = tokio::io::duplex(65536);
        let (main_read, main_write) = tokio::io::split(main_client);
        let (peer_client, peer_server) = tokio::io::duplex(65536);
        let (peer_read, peer_write) = tokio::io::split(peer_client);
        let peer = PipeTransport::new(peer_read, peer_write);
        let source = match cleanup {
            Some(confirmation) => crate::shell_check::Source::ScriptedCleanup(peer, confirmation),
            None => crate::shell_check::Source::Pipe(peer),
        };
        let mut client = ClientHandle::start_with_shell_source(
            PipeTransport::new(main_read, main_write),
            Config::default(),
            None,
            false,
            None,
            Some(source),
        );
        let mut main = BufReader::new(main_server);
        let mut initialize: Value = serde_json::from_str(include_str!(
            "../tests/fixtures/codex-0.159.2/initialize.json"
        ))
        .unwrap();
        initialize["id"] = next(&mut main).await["id"].clone();
        send(&mut main, initialize).await;
        assert_eq!(next(&mut main).await["method"], "initialized");
        let start = next(&mut main).await;
        assert_eq!(start["method"], "thread/start");
        send(
            &mut main,
            json!({"id":start["id"],"result":{"thread":{"id":"root","cliVersion":thread_version}}}),
        )
        .await;
        phase(
            &mut client,
            if thread_version == "0.159.2" {
                SessionPhase::CheckingShell
            } else {
                SessionPhase::Failed
            },
        )
        .await;
        (client, main, BufReader::new(peer_server))
    }

    async fn initialize_peer(peer: &mut BufReader<tokio::io::DuplexStream>) -> Value {
        let request = next(peer).await;
        assert_eq!(request["method"], "initialize");
        let mut response: Value = serde_json::from_str(include_str!(
            "../tests/fixtures/codex-0.159.2/initialize.json"
        ))
        .unwrap();
        response["id"] = request["id"].clone();
        send(peer, response).await;
        assert_eq!(next(peer).await["method"], "initialized");
        let preflight = next(peer).await;
        assert_eq!(preflight["method"], "command/exec");
        preflight
    }

    #[tokio::test]
    async fn isolated_preflight_keeps_main_rpc_ids_separate_and_blocks_queued_tasks_until_cleanup()
    {
        let (client, mut main, mut peer) = isolated_harness("0.159.2").await;
        client
            .commands
            .send(Command::SubmitRootInput {
                text: "after check".into(),
            })
            .await
            .unwrap();
        let preflight = initialize_peer(&mut peer).await;
        // Main connection cannot satisfy the auxiliary request, even with matching numeric IDs.
        send(&mut main, json!({"id":preflight["id"],"result":{"exitCode":0,"stdout":"native-agent-tui-shell-ok"}})).await;
        assert!(
            tokio::time::timeout(Duration::from_millis(30), next(&mut main))
                .await
                .is_err()
        );
        assert_eq!(client.snapshots.borrow().phase, SessionPhase::CheckingShell);
        assert_eq!(client.snapshots.borrow().root_start_requests, 0);
        send(&mut peer, json!({"id":preflight["id"],"result":{"exitCode":0,"stdout":"native-agent-tui-shell-ok"}})).await;
        let skills = next(&mut main).await;
        assert_eq!(skills["method"], "skills/list");
        send(&mut main, json!({"id":skills["id"],"result":{"data":[{"cwd":skills["params"]["cwds"][0],"errors":[],"skills":[]}]}})).await;
        let turn = next(&mut main).await;
        assert_eq!(turn["method"], "turn/start");
        assert_eq!(turn["params"]["threadId"], "root");
        let mut line = String::new();
        assert_eq!(peer.read_line(&mut line).await.unwrap(), 0);
        client.commands.send(Command::Quit).await.unwrap();
        assert!(client.join.await.unwrap().cleanup_error.is_none());
    }

    #[tokio::test]
    async fn incompatible_main_thread_cannot_launch_isolated_preflight() {
        let (client, _main, mut peer) = isolated_harness("PRIVATE_VERSION").await;
        assert!(
            tokio::time::timeout(Duration::from_millis(30), next(&mut peer))
                .await
                .is_err()
        );
        assert_eq!(client.snapshots.borrow().root_start_requests, 0);
        client.commands.send(Command::Quit).await.unwrap();
        client.join.await.unwrap();
    }

    #[tokio::test]
    async fn invalid_isolated_initialize_cannot_execute_shell_or_dispatch_model() {
        let (mut client, _main, mut peer) = isolated_harness("0.159.2").await;
        let initialize = next(&mut peer).await;
        send(
            &mut peer,
            json!({"id":initialize["id"],"result":{"userAgent":"PRIVATE_METADATA"}}),
        )
        .await;
        phase(&mut client, SessionPhase::Failed).await;
        assert_eq!(client.snapshots.borrow().root_start_requests, 0);
        assert!(!client
            .snapshots
            .borrow()
            .last_error
            .as_deref()
            .unwrap()
            .contains("PRIVATE_"));
        let mut line = String::new();
        assert_eq!(peer.read_line(&mut line).await.unwrap(), 0);
        client.commands.send(Command::Quit).await.unwrap();
        client.join.await.unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn isolated_preflight_timeout_remains_unknown_and_cannot_retry_queued_task() {
        let (mut client, mut main, mut peer) = isolated_harness("0.159.2").await;
        client
            .commands
            .send(Command::SubmitRootInput {
                text: "must remain queued".into(),
            })
            .await
            .unwrap();
        initialize_peer(&mut peer).await;
        tokio::time::advance(crate::shell_check::DEADLINE + Duration::from_secs(1)).await;
        phase(&mut client, SessionPhase::Unknown).await;
        {
            let snapshot = client.snapshots.borrow();
            let error = snapshot.last_error.as_deref().unwrap();
            assert!(error.contains("Shell preflight timed out"), "{error}");
            if cfg!(windows) {
                assert!(error.contains("--windows-sandbox unelevated"), "{error}");
            }
            assert_eq!(snapshot.notice, None);
        }
        let report = client.join.await.unwrap();
        assert!(report.cleanup_error.is_none());
        assert_eq!(client.snapshots.borrow().root_start_requests, 0);
        let mut line = String::new();
        assert_eq!(main.read_line(&mut line).await.unwrap(), 0);
        assert_eq!(peer.read_line(&mut line).await.unwrap(), 0);
    }

    #[tokio::test]
    async fn isolated_preflight_uncertain_responses_cannot_dispatch_or_reveal_private_text() {
        for response in [
            json!({"id":999,"result":{"exitCode":0,"stdout":"native-agent-tui-shell-ok"}}),
            json!({"id":2,"error":{"code":-1,"message":"PRIVATE_ERROR"}}),
            json!({"id":2,"result":{"exitCode":0}}),
            json!({"id":2,"result":{"exitCode":"PRIVATE_CODE","stdout":"native-agent-tui-shell-ok"}}),
            json!({"id":"PRIVATE_REQUEST","method":"PRIVATE_INTERACTION","params":{}}),
        ] {
            let (mut client, mut main, mut peer) = isolated_harness("0.159.2").await;
            client
                .commands
                .send(Command::SubmitRootInput {
                    text: "PRIVATE_TASK".into(),
                })
                .await
                .unwrap();
            initialize_peer(&mut peer).await;
            send(&mut peer, response).await;
            phase(&mut client, SessionPhase::Unknown).await;
            assert!(client.join.await.unwrap().cleanup_error.is_none());
            assert_eq!(client.snapshots.borrow().root_start_requests, 0);
            assert!(!client
                .snapshots
                .borrow()
                .last_error
                .as_deref()
                .unwrap()
                .contains("PRIVATE_"));
            let mut line = String::new();
            assert_eq!(main.read_line(&mut line).await.unwrap(), 0);
        }
    }

    #[tokio::test]
    async fn isolated_preflight_eof_preserves_unknown_shell_outcome() {
        let (mut client, mut main, mut peer) = isolated_harness("0.159.2").await;
        initialize_peer(&mut peer).await;
        drop(peer);
        phase(&mut client, SessionPhase::Unknown).await;
        assert!(client.join.await.unwrap().cleanup_error.is_none());
        assert_eq!(client.snapshots.borrow().root_start_requests, 0);
        let mut line = String::new();
        assert_eq!(main.read_line(&mut line).await.unwrap(), 0);
    }

    #[tokio::test(start_paused = true)]
    async fn isolated_preflight_success_cannot_release_tasks_after_cleanup_crosses_deadline() {
        let (confirmation, cleanup) = tokio::sync::oneshot::channel();
        let (mut client, mut main, mut peer) =
            isolated_harness_with_cleanup("0.159.2", Some(cleanup)).await;
        let request = initialize_peer(&mut peer).await;
        send(&mut peer, json!({"id":request["id"],"result":{"exitCode":0,"stdout":"native-agent-tui-shell-ok"}})).await;
        // EOF confirms the worker has consumed success and entered its cleanup stage.
        let mut line = String::new();
        assert_eq!(peer.read_line(&mut line).await.unwrap(), 0);
        assert_eq!(client.snapshots.borrow().phase, SessionPhase::CheckingShell);
        tokio::time::advance(crate::shell_check::DEADLINE + Duration::from_secs(1)).await;
        confirmation.send(true).unwrap();
        phase(&mut client, SessionPhase::Unknown).await;
        let report = client.join.await.unwrap();
        assert!(report.cleanup_error.is_none());
        assert_eq!(client.snapshots.borrow().root_start_requests, 0);
        assert_eq!(main.read_line(&mut line).await.unwrap(), 0);
    }

    #[tokio::test]
    async fn quitting_during_isolated_preflight_joins_its_cancelled_owner() {
        let (client, _main, mut peer) = isolated_harness("0.159.2").await;
        initialize_peer(&mut peer).await;
        client.commands.send(Command::Quit).await.unwrap();
        let report = tokio::time::timeout(Duration::from_secs(2), client.join)
            .await
            .unwrap()
            .unwrap();
        assert!(report.cleanup_error.is_none());
        assert_eq!(client.snapshots.borrow().root_start_requests, 0);
        let mut line = String::new();
        assert_eq!(peer.read_line(&mut line).await.unwrap(), 0);
    }

    #[tokio::test]
    async fn isolated_preflight_cannot_become_ready_before_cleanup_or_after_uncertain_cleanup() {
        let (confirmation, cleanup) = tokio::sync::oneshot::channel();
        let (mut client, mut main, mut peer) =
            isolated_harness_with_cleanup("0.159.2", Some(cleanup)).await;
        client
            .commands
            .send(Command::SubmitRootInput {
                text: "must remain blocked".into(),
            })
            .await
            .unwrap();
        let request = initialize_peer(&mut peer).await;
        send(&mut peer,json!({"id":request["id"],"result":{"exitCode":0,"stdout":"native-agent-tui-shell-ok"}})).await;
        assert!(
            tokio::time::timeout(Duration::from_millis(30), next(&mut main))
                .await
                .is_err()
        );
        assert_eq!(client.snapshots.borrow().phase, SessionPhase::CheckingShell);
        confirmation.send(false).unwrap();
        phase(&mut client, SessionPhase::Unknown).await;
        let report = client.join.await.unwrap();
        assert!(report.cleanup_error.is_some());
        assert_eq!(client.snapshots.borrow().root_start_requests, 0);
    }
    async fn next(server: &mut BufReader<tokio::io::DuplexStream>) -> Value {
        let mut line = String::new();
        tokio::time::timeout(Duration::from_secs(3), server.read_line(&mut line))
            .await
            .expect("scripted app-server request timed out")
            .unwrap();
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
        let skills = next(server).await;
        assert_eq!(skills["method"], "skills/list");
        send(server, json!({"id":skills["id"],"result":{"data":[{"cwd":skills["params"]["cwds"][0],"errors":[],"skills":[]}]}})).await;
    }

    #[tokio::test]
    async fn changed_inventory_refresh_coalesces_and_ignores_stale_results() {
        let (mut client, mut server) = harness().await;
        ready(&mut server).await;
        tokio::time::timeout(
            Duration::from_secs(3),
            client.snapshots.wait_for(|snapshot| {
                snapshot.skills.availability == crate::skills::SkillAvailability::Available
                    && snapshot.skills.freshness == crate::skills::SkillFreshness::Current
            }),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(
            client.snapshots.borrow().skills.refresh_source,
            Some(crate::skills::SkillRefreshSource::Initial)
        );

        send(&mut server, json!({"method":"skills/changed","params":{}})).await;
        let changed_request = tokio::time::timeout(Duration::from_secs(3), next(&mut server))
            .await
            .unwrap();
        assert_eq!(changed_request["method"], "skills/list");
        assert_eq!(changed_request["params"]["forceReload"], false);
        send(
            &mut server,
            json!({"id":changed_request["id"],"result":{"data":[{"cwd":changed_request["params"]["cwds"][0],"errors":[],"skills":[{"name":"changed","enabled":true,"path":"/workspace/changed/SKILL.md","scope":"repo"}]}]}}),
        )
        .await;
        let changed = tokio::time::timeout(
            Duration::from_secs(3),
            client.snapshots.wait_for(|snapshot| {
                snapshot.skills.freshness == crate::skills::SkillFreshness::Current
                    && snapshot.skills.refresh_source
                        == Some(crate::skills::SkillRefreshSource::Changed)
                    && snapshot
                        .skills
                        .entries
                        .iter()
                        .any(|entry| entry.name == "changed")
            }),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(
            changed.skills.refresh_source,
            Some(crate::skills::SkillRefreshSource::Changed)
        );
        drop(changed);

        send(&mut server, json!({"method":"skills/changed","params":{}})).await;
        let stale_request = tokio::time::timeout(Duration::from_secs(3), next(&mut server))
            .await
            .unwrap();
        assert_eq!(stale_request["method"], "skills/list");
        assert_eq!(stale_request["params"]["forceReload"], false);
        client.commands.send(Command::RefreshSkills).await.unwrap();
        tokio::time::timeout(
            Duration::from_secs(3),
            client.snapshots.wait_for(|snapshot| {
                snapshot.skills.freshness == crate::skills::SkillFreshness::Queued
            }),
        )
        .await
        .unwrap()
        .unwrap();
        send(&mut server, json!({"method":"skills/changed","params":{}})).await;
        send(
            &mut server,
            json!({"id":stale_request["id"],"result":{"data":[{"cwd":stale_request["params"]["cwds"][0],"errors":[],"skills":[{"name":"stale","description":"PRIVATE_DESCRIPTION","enabled":true,"path":"/workspace/stale/SKILL.md","scope":"repo"}]}]}}),
        )
        .await;
        let current_request = tokio::time::timeout(Duration::from_secs(3), next(&mut server))
            .await
            .unwrap();
        assert_eq!(current_request["method"], "skills/list");
        assert_eq!(current_request["params"]["forceReload"], true);
        send(
            &mut server,
            json!({"id":current_request["id"],"result":{"data":[{"cwd":current_request["params"]["cwds"][0],"errors":[],"skills":[{"name":"current","description":"PRIVATE_DESCRIPTION","enabled":true,"path":"/workspace/current/SKILL.md","scope":"repo","interface":{"defaultPrompt":"PRIVATE_PROMPT"}}]}]}}),
        )
        .await;
        let current = tokio::time::timeout(
            Duration::from_secs(3),
            client.snapshots.wait_for(|snapshot| {
                snapshot.skills.freshness == crate::skills::SkillFreshness::Current
                    && snapshot
                        .skills
                        .entries
                        .iter()
                        .any(|entry| entry.name == "current")
            }),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(current.skills.entries.len(), 1);
        assert_eq!(
            current.skills.refresh_source,
            Some(crate::skills::SkillRefreshSource::Manual)
        );
        assert_eq!(current.phase, SessionPhase::Ready);
        assert_eq!(current.root_turn_count, 0);
        assert_eq!(current.root_start_requests, 0);
        drop(current);
        client.commands.send(Command::Quit).await.unwrap();
        tokio::time::timeout(Duration::from_secs(2), client.join)
            .await
            .expect("Core did not stop after Quit")
            .unwrap();
    }

    #[tokio::test]
    async fn changed_inventory_refresh_records_changed_source() {
        let (mut client, mut server) = harness().await;
        ready(&mut server).await;
        client
            .snapshots
            .wait_for(|snapshot| {
                snapshot.skills.freshness == crate::skills::SkillFreshness::Current
            })
            .await
            .unwrap();

        send(&mut server, json!({"method":"skills/changed","params":{}})).await;
        let request = next(&mut server).await;
        assert_eq!(request["method"], "skills/list");
        assert_eq!(request["params"]["forceReload"], false);
        send(
            &mut server,
            json!({"id":request["id"],"result":{"data":[{"cwd":request["params"]["cwds"][0],"errors":[],"skills":[]}]}}),
        )
        .await;

        let snapshot = client
            .snapshots
            .wait_for(|snapshot| {
                snapshot.skills.freshness == crate::skills::SkillFreshness::Current
                    && snapshot.skills.refresh_source
                        == Some(crate::skills::SkillRefreshSource::Changed)
            })
            .await
            .unwrap();
        assert_eq!(
            snapshot.skills.refresh_source,
            Some(crate::skills::SkillRefreshSource::Changed)
        );
        drop(snapshot);
        client.commands.send(Command::Quit).await.unwrap();
        client.join.await.unwrap();
    }

    #[tokio::test]
    async fn stale_skills_rpc_error_keeps_confirmed_inventory_while_refresh_is_queued() {
        let (mut client, mut server) = harness().await;
        ready(&mut server).await;
        tokio::time::timeout(
            Duration::from_secs(3),
            client.snapshots.wait_for(|snapshot| {
                snapshot.skills.freshness == crate::skills::SkillFreshness::Current
            }),
        )
        .await
        .expect("initial inventory response was not applied")
        .unwrap();

        send(&mut server, json!({"method":"skills/changed","params":{}})).await;
        let stale_request = tokio::time::timeout(Duration::from_secs(3), next(&mut server))
            .await
            .expect("changed notification did not request an inventory refresh");
        let version_before_refresh = client.snapshots.borrow().version;
        client.commands.send(Command::RefreshSkills).await.unwrap();
        tokio::time::timeout(
            Duration::from_secs(3),
            client.snapshots.wait_for(|snapshot| {
                snapshot.version > version_before_refresh
                    && snapshot.skills.freshness == crate::skills::SkillFreshness::Queued
            }),
        )
        .await
        .expect("explicit refresh command was not applied")
        .unwrap();
        send(
            &mut server,
            json!({"id":stale_request["id"],"error":{"code":-32601,"message":"PRIVATE_STALE_ERROR"}}),
        )
        .await;

        let current_request = tokio::time::timeout(Duration::from_secs(3), next(&mut server))
            .await
            .expect("stale RPC error blocked the current inventory refresh");
        assert_eq!(current_request["method"], "skills/list");
        assert_eq!(current_request["params"]["forceReload"], true);
        {
            let snapshot = client.snapshots.borrow();
            assert_eq!(
                snapshot.skills.availability,
                crate::skills::SkillAvailability::Available
            );
            assert_eq!(
                snapshot.skills.freshness,
                crate::skills::SkillFreshness::Queued
            );
        }
        send(
            &mut server,
            json!({"id":current_request["id"],"result":{"data":[{"cwd":current_request["params"]["cwds"][0],"errors":[],"skills":[{"name":"current-after-stale-error","enabled":true,"path":"/workspace/current/SKILL.md","scope":"repo"}]}]}}),
        )
        .await;
        let snapshot = tokio::time::timeout(
            Duration::from_secs(3),
            client.snapshots.wait_for(|snapshot| {
                snapshot.skills.freshness == crate::skills::SkillFreshness::Current
                    && snapshot
                        .skills
                        .entries
                        .iter()
                        .any(|entry| entry.name == "current-after-stale-error")
            }),
        )
        .await
        .expect("current inventory result was not applied")
        .unwrap();
        {
            assert_eq!(
                snapshot.skills.availability,
                crate::skills::SkillAvailability::Available
            );
            assert_eq!(snapshot.phase, SessionPhase::Ready);
        }
        drop(snapshot);
        client.commands.send(Command::Quit).await.unwrap();
        tokio::time::timeout(Duration::from_secs(2), client.join)
            .await
            .expect("Quit did not stop Core after stale RPC error")
            .unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn skills_refresh_waits_for_gate_and_coalesces_until_root_and_child_are_idle() {
        let (mut client, mut server) = harness().await;
        running_root(&mut client, &mut server).await;
        child(&mut server, "a", "a-1").await;
        wait_call(&mut server, "skills-gate-wait", vec!["a"]).await;
        phase(&mut client, SessionPhase::GatePending).await;

        let before = client.snapshots.borrow().clone();
        let gate_before = before.gate.clone().unwrap();
        let root_turn_before = before
            .observation
            .activities
            .iter()
            .find(|activity| {
                activity.identity.agent_id == "root"
                    && activity.scope == crate::observation::ActivityScope::Turn
            })
            .unwrap()
            .clone();
        let waiting_before = before
            .observation
            .activities
            .iter()
            .find(|activity| {
                activity.identity.agent_id == "root"
                    && activity.kind == crate::observation::ActivityKind::WaitingChildren
            })
            .unwrap()
            .clone();
        send(&mut server, json!({"method":"skills/changed","params":{}})).await;
        client
            .snapshots
            .wait_for(|snapshot| {
                snapshot.observation.raw_message_count > before.observation.raw_message_count
            })
            .await
            .unwrap();
        client.commands.send(Command::RefreshSkills).await.unwrap();
        client
            .snapshots
            .wait_for(|snapshot| {
                snapshot.version > before.version
                    && snapshot.skills.freshness == crate::skills::SkillFreshness::Queued
            })
            .await
            .unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(50), next(&mut server))
                .await
                .is_err(),
            "skills refresh must not emit RPCs while the Gate is pending"
        );
        {
            let snapshot = client.snapshots.borrow();
            assert_eq!(snapshot.phase, SessionPhase::GatePending);
            assert_eq!(snapshot.root_start_requests, before.root_start_requests);
            assert_eq!(snapshot.gate.as_ref().unwrap(), &gate_before);
            assert_eq!(
                snapshot.observation.accepted_evidence_count,
                before.observation.accepted_evidence_count
            );
            let root_turn = snapshot
                .observation
                .activities
                .iter()
                .find(|activity| {
                    activity.identity.agent_id == "root"
                        && activity.scope == crate::observation::ActivityScope::Turn
                })
                .unwrap();
            let waiting = snapshot
                .observation
                .activities
                .iter()
                .find(|activity| {
                    activity.identity.agent_id == "root"
                        && activity.kind == crate::observation::ActivityKind::WaitingChildren
                })
                .unwrap();
            assert_eq!(root_turn.progress_seq, root_turn_before.progress_seq);
            assert_eq!(root_turn.last_evidence, root_turn_before.last_evidence);
            assert_eq!(root_turn.attention, root_turn_before.attention);
            assert_eq!(waiting.progress_seq, waiting_before.progress_seq);
            assert_eq!(waiting.last_evidence, waiting_before.last_evidence);
            assert_eq!(waiting.attention, waiting_before.attention);
        }

        observation_event(
            &mut client,
            &mut server,
            json!({"method":"turn/completed","params":{"threadId":"a","turn":{"id":"a-1","status":"completed"}}}),
        ).await;
        let gate_release = next(&mut server).await;
        assert_eq!(gate_release["id"], "skills-gate-wait");
        phase(&mut client, SessionPhase::Running).await;
        observation_event(
            &mut client,
            &mut server,
            json!({"method":"turn/completed","params":{"threadId":"root","turn":{"id":"root-turn","status":"completed"}}}),
        ).await;
        phase(&mut client, SessionPhase::Completed).await;

        let refresh = next(&mut server).await;
        assert_eq!(refresh["method"], "skills/list");
        assert_eq!(refresh["params"]["forceReload"], true);
        send(
            &mut server,
            json!({"id":refresh["id"],"result":{"data":[{"cwd":refresh["params"]["cwds"][0],"errors":[],"skills":[]}]}}),
        ).await;
        client
            .snapshots
            .wait_for(|snapshot| {
                snapshot.skills.freshness == crate::skills::SkillFreshness::Current
            })
            .await
            .unwrap();
        assert_eq!(client.snapshots.borrow().phase, SessionPhase::Completed);
        assert_eq!(
            client.snapshots.borrow().root_start_requests,
            before.root_start_requests
        );
        assert!(
            tokio::time::timeout(Duration::from_millis(50), next(&mut server))
                .await
                .is_err(),
            "changed and explicit refresh must coalesce to one request"
        );
        client.commands.send(Command::Quit).await.unwrap();
        client.join.await.unwrap();
    }

    #[tokio::test]
    async fn unanswered_initial_skills_query_does_not_block_root_start_or_quit() {
        let (mut client, mut server) = harness().await;
        let preflight = initialized(&mut server).await;
        send(&mut server, json!({"id":preflight["id"],"result":{"exitCode":0,"stdout":"native-agent-tui-shell-ok"}})).await;
        phase(&mut client, SessionPhase::Ready).await;
        let skills = next(&mut server).await;
        assert_eq!(skills["method"], "skills/list");

        client
            .commands
            .send(Command::SubmitRootInput {
                text: "task while inventory is pending".into(),
            })
            .await
            .unwrap();
        let start = next(&mut server).await;
        assert_eq!(start["method"], "turn/start");
        send(
            &mut server,
            json!({"id":start["id"],"result":{"turn":{"id":"task-turn"}}}),
        )
        .await;
        phase(&mut client, SessionPhase::Running).await;
        assert_eq!(client.snapshots.borrow().root_start_requests, 1);
        client.commands.send(Command::Quit).await.unwrap();
        tokio::time::timeout(Duration::from_secs(2), client.join)
            .await
            .expect("Quit must stop Core with an unanswered optional skills query")
            .unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn stale_skills_rpc_timeout_dispatches_queued_refresh_without_other_events() {
        let (mut client, mut server) = harness().await;
        ready(&mut server).await;
        client
            .snapshots
            .wait_for(|snapshot| {
                snapshot.skills.freshness == crate::skills::SkillFreshness::Current
            })
            .await
            .unwrap();
        send(&mut server, json!({"method":"skills/changed","params":{}})).await;
        let old_request = next(&mut server).await;
        client.commands.send(Command::RefreshSkills).await.unwrap();
        client
            .snapshots
            .wait_for(|snapshot| snapshot.skills.freshness == crate::skills::SkillFreshness::Queued)
            .await
            .unwrap();

        tokio::time::advance(Duration::from_secs(31)).await;
        let refresh = next(&mut server).await;
        assert_eq!(refresh["method"], "skills/list");
        assert_ne!(refresh["id"], old_request["id"]);
        let snapshot = client.snapshots.borrow();
        assert_eq!(snapshot.phase, SessionPhase::Ready);
        assert_eq!(snapshot.root_turn_count, 0);
        assert_eq!(snapshot.root_start_requests, 0);
        drop(snapshot);

        send(&mut server, json!({"id":refresh["id"],"result":{"data":[{"cwd":refresh["params"]["cwds"][0],"errors":[],"skills":[]}]}})).await;
        client.commands.send(Command::Quit).await.unwrap();
        client.join.await.unwrap();
    }

    #[tokio::test]
    async fn unsupported_skills_list_keeps_execution_ready() {
        let (mut client, mut server) = harness().await;
        ready(&mut server).await;
        client
            .snapshots
            .wait_for(|snapshot| {
                snapshot.skills.freshness == crate::skills::SkillFreshness::Current
            })
            .await
            .unwrap();
        send(&mut server, json!({"method":"skills/changed","params":{}})).await;
        let request = next(&mut server).await;
        send(
            &mut server,
            json!({"id":request["id"],"error":{"code":-32601,"message":"PRIVATE_SERVER_ERROR"}}),
        )
        .await;
        let snapshot = client
            .snapshots
            .wait_for(|snapshot| {
                snapshot.skills.availability == crate::skills::SkillAvailability::Unsupported
            })
            .await
            .unwrap();
        assert_eq!(snapshot.phase, SessionPhase::Ready);
        assert!(snapshot.last_error.is_none());
        assert_eq!(snapshot.root_start_requests, 0);
        drop(snapshot);
        client.commands.send(Command::Quit).await.unwrap();
        client.join.await.unwrap();
    }
    async fn initialized(server: &mut BufReader<tokio::io::DuplexStream>) -> Value {
        let init = next(server).await;
        assert_eq!(init["method"], "initialize");
        let mut response: Value = serde_json::from_str(include_str!(
            "../tests/fixtures/codex-0.159.2/initialize.json"
        ))
        .unwrap();
        response["id"] = init["id"].clone();
        send(server, response).await;
        assert_eq!(next(server).await["method"], "initialized");
        let thread = next(server).await;
        assert_eq!(thread["method"], "thread/start");
        send(
            server,
            json!({"id":thread["id"],"result":{"thread":{"id":"root","cliVersion":"0.159.2"},"model":"test-model"}}),
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
    async fn pinned_startup_transcript_reaches_ready_with_zero_model_turns() {
        let (mut client, mut server) = harness().await;
        let records: Vec<Value> = include_str!("../tests/fixtures/codex-0.159.2/startup.jsonl")
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        for (method, response) in ["initialize", "thread/start", "command/exec"]
            .into_iter()
            .zip(records)
        {
            let request = next(&mut server).await;
            assert_eq!(request["method"], method);
            assert_eq!(request["id"], response["id"]);
            send(&mut server, response).await;
            if method == "initialize" {
                assert_eq!(next(&mut server).await["method"], "initialized");
            }
        }
        phase(&mut client, SessionPhase::Ready).await;
        assert_eq!(client.snapshots.borrow().root_turn_count, 0);
        assert_eq!(client.snapshots.borrow().root_start_requests, 0);
        client.commands.send(Command::Quit).await.unwrap();
        client.join.await.unwrap();
    }

    #[tokio::test]
    async fn invalid_initialize_cannot_start_a_thread_preflight_or_queued_task() {
        for result in [
            json!({}),
            json!({"userAgent":"PRIVATE_METADATA"}),
            Value::Null,
        ] {
            let (mut client, mut server) = harness().await;
            let request = next(&mut server).await;
            client
                .commands
                .send(Command::SubmitRootInput {
                    text: "PRIVATE_PROMPT".into(),
                })
                .await
                .unwrap();
            send(&mut server, json!({"id":request["id"],"result":result})).await;
            phase(&mut client, SessionPhase::Failed).await;
            client
                .commands
                .send(Command::SubmitRootInput {
                    text: "PRIVATE_RETRY".into(),
                })
                .await
                .unwrap();
            assert!(
                tokio::time::timeout(Duration::from_millis(30), next(&mut server))
                    .await
                    .is_err()
            );
            let snapshot = client.snapshots.borrow();
            assert_eq!(snapshot.root_start_requests, 0);
            assert_eq!(snapshot.root_turn_count, 0);
            assert!(snapshot.thread_id.is_none());
            assert!(!snapshot.last_error.as_deref().unwrap().contains("PRIVATE_"));
            drop(snapshot);
            client.commands.send(Command::Quit).await.unwrap();
            client.join.await.unwrap();
        }
    }

    #[tokio::test]
    async fn a_mismatched_new_thread_release_cannot_start_preflight_or_a_model_turn() {
        let (mut client, mut server) = harness().await;
        let request = next(&mut server).await;
        let mut response: Value = serde_json::from_str(include_str!(
            "../tests/fixtures/codex-0.159.2/initialize.json"
        ))
        .unwrap();
        response["id"] = request["id"].clone();
        send(&mut server, response).await;
        assert_eq!(next(&mut server).await["method"], "initialized");
        let request = next(&mut server).await;
        assert_eq!(request["method"], "thread/start");
        client
            .commands
            .send(Command::SubmitRootInput {
                text: "PRIVATE_PROMPT".into(),
            })
            .await
            .unwrap();
        send(&mut server, json!({"id":request["id"],"result":{"thread":{"id":"root","cliVersion":"PRIVATE_VERSION"}}})).await;
        phase(&mut client, SessionPhase::Failed).await;
        assert!(
            tokio::time::timeout(Duration::from_millis(30), next(&mut server))
                .await
                .is_err()
        );
        let snapshot = client.snapshots.borrow();
        assert_eq!(snapshot.root_start_requests, 0);
        assert!(snapshot.thread_id.is_none());
        assert!(!snapshot.last_error.as_deref().unwrap().contains("PRIVATE_"));
        drop(snapshot);
        client.commands.send(Command::Quit).await.unwrap();
        client.join.await.unwrap();
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
                request: current_request(&client, RpcId::String("approval".into())),
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
        assert!(client.snapshots.borrow().startup_blocked);
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
        send(&mut server, json!({"method":"thread/tokenUsage/updated","params":{"threadId":"root","turnId":"current","tokenUsage":{"total":{"inputTokens":62,"cachedInputTokens":40,"outputTokens":18,"reasoningOutputTokens":3,"totalTokens":83},"modelContextWindow":128}}})).await;
        tokio::time::timeout(
            Duration::from_secs(3),
            client
                .snapshots
                .wait_for(|s| s.usage.total_tokens == Some(83)),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(client.snapshots.borrow().usage.input_tokens, Some(62));
        assert_eq!(
            client.snapshots.borrow().usage.cached_input_tokens,
            Some(40)
        );
        assert_eq!(client.snapshots.borrow().usage.output_tokens, Some(18));
        assert_eq!(client.snapshots.borrow().usage.reasoning_tokens, Some(3));
        assert_eq!(client.snapshots.borrow().usage.context_window, Some(128));
        assert_eq!(
            client.snapshots.borrow().usage.source,
            FactSource::ServerConfirmed
        );
        let snapshot = client.snapshots.borrow().clone();
        let usage_fact = snapshot
            .usage_facts
            .iter()
            .find(|fact| fact.identity.thread_id.as_deref() == Some("root"))
            .expect("accepted root usage keeps typed identity");
        assert_eq!(usage_fact.identity.turn_id.as_deref(), Some("current"));
        assert_eq!(usage_fact.identity.generation, Some(1));
        send(&mut server, json!({"method":"thread/tokenUsage/updated","params":{"threadId":"child","turnId":"child-current","tokenUsage":{"total":{"totalTokens":999}}}})).await;
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert_eq!(client.snapshots.borrow().usage.total_tokens, Some(83));
        assert_eq!(client.snapshots.borrow().requests.len(), 1);
        client
            .commands
            .send(Command::AnswerApproval {
                request: current_request(&client, RpcId::String("active".into())),
                decision: ApprovalDecision::Decline,
            })
            .await
            .unwrap();
        assert_eq!(next(&mut server).await["result"]["decision"], "decline");
        client.commands.send(Command::Quit).await.unwrap();
        client.join.await.unwrap();
    }

    #[test]
    fn usage_decoder_accepts_server_shapes_without_inventing_missing_context() {
        let usage = decode_usage(&json!({
            "tokenUsage": {
                "total": {
                    "input_tokens": 10,
                    "input_tokens_details": {"cached_tokens": 4},
                    "output_tokens": 6,
                    "output_tokens_details": {"reasoning_tokens": 2},
                    "total_tokens": 16
                }
            }
        }))
        .unwrap();
        assert_eq!(usage.input_tokens, Some(10));
        assert_eq!(usage.cached_input_tokens, Some(4));
        assert_eq!(usage.output_tokens, Some(6));
        assert_eq!(usage.reasoning_tokens, Some(2));
        assert_eq!(usage.total_tokens, Some(16));
        assert_eq!(usage.context_window, None);
        assert_eq!(usage.source, FactSource::ServerConfirmed);
        assert!(decode_usage(&json!({"tokenUsage": {}})).is_none());
    }

    #[test]
    fn usage_decoder_reads_thread_cumulative_total_from_schema_total_object() {
        let usage = decode_usage(&json!({
            "tokenUsage": {"total": {"totalTokens": 42}}
        }))
        .unwrap();
        assert_eq!(usage.total_tokens, Some(42));
    }

    #[test]
    fn cumulative_usage_keeps_the_larger_thread_total_across_turn_updates() {
        let confirmed = |total_tokens| UsageSummary {
            total_tokens,
            source: FactSource::ServerConfirmed,
            ..Default::default()
        };
        assert_eq!(
            preserve_cumulative_total(confirmed(Some(9)), confirmed(Some(1))).total_tokens,
            Some(9)
        );
        assert_eq!(
            preserve_cumulative_total(confirmed(Some(9)), confirmed(None)).total_tokens,
            Some(9)
        );
        assert_eq!(
            preserve_cumulative_total(confirmed(Some(9)), confirmed(Some(12))).total_tokens,
            Some(12)
        );
    }

    #[tokio::test]
    async fn token_budget_interrupts_the_active_root_and_blocks_later_dispatch() {
        let (mut client, mut server) = harness_with_config(Config {
            max_total_tokens: Some(10),
            ..Default::default()
        })
        .await;
        ready(&mut server).await;
        phase(&mut client, SessionPhase::Ready).await;
        assert_eq!(client.snapshots.borrow().token_budget.limit, Some(10));
        assert_eq!(
            client
                .snapshots
                .borrow()
                .token_budget
                .confirmed_total_tokens,
            None
        );
        client
            .commands
            .send(Command::SubmitRootInput {
                text: "budgeted work".into(),
            })
            .await
            .unwrap();
        let start = next(&mut server).await;
        send(
            &mut server,
            json!({"id":start["id"],"result":{"turn":{"id":"one"}}}),
        )
        .await;
        send(
            &mut server,
            json!({"method":"turn/started","params":{"threadId":"root","turn":{"id":"one"}}}),
        )
        .await;
        send(
            &mut server,
            json!({"method":"thread/tokenUsage/updated","params":{"threadId":"root","turnId":"one","tokenUsage":{"total":{"totalTokens":10}}}}),
        )
        .await;
        tokio::time::timeout(
            Duration::from_secs(3),
            client.snapshots.wait_for(|snapshot| {
                snapshot.token_budget.confirmed_total_tokens == Some(10)
                    && snapshot.token_budget.stop_triggered
            }),
        )
        .await
        .unwrap()
        .unwrap();
        let interrupt = tokio::time::timeout(Duration::from_secs(3), next(&mut server))
            .await
            .unwrap();
        assert_eq!(interrupt["method"], "turn/interrupt");
        assert!(client
            .snapshots
            .borrow()
            .notice
            .as_deref()
            .is_some_and(|notice| notice.contains("Token budget 10 reached")));
        send(&mut server, json!({"id":interrupt["id"],"result":{}})).await;
        send(
            &mut server,
            json!({"method":"turn/completed","params":{"threadId":"root","turn":{"id":"one","status":"interrupted"}}}),
        )
        .await;
        phase(&mut client, SessionPhase::Failed).await;
        client
            .commands
            .send(Command::SubmitRootInput {
                text: "must not dispatch".into(),
            })
            .await
            .unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(100), next(&mut server))
                .await
                .is_err()
        );
        client.commands.send(Command::Quit).await.unwrap();
        client.join.await.unwrap();
    }

    #[tokio::test]
    async fn stale_root_usage_cannot_lower_current_usage_or_bypass_budget() {
        let (mut client, mut server) = harness_with_config(Config {
            max_total_tokens: Some(10),
            ..Default::default()
        })
        .await;
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
            json!({"id":first["id"],"result":{"turn":{"id":"one"}}}),
        )
        .await;
        send(
            &mut server,
            json!({"method":"turn/started","params":{"threadId":"root","turn":{"id":"one"}}}),
        )
        .await;
        send(
            &mut server,
            json!({"method":"thread/tokenUsage/updated","params":{"threadId":"root","turnId":"one","tokenUsage":{"total":{"totalTokens":9}}}}),
        )
        .await;
        client
            .snapshots
            .wait_for(|snapshot| snapshot.usage.total_tokens == Some(9))
            .await
            .unwrap();
        let first_fact = client
            .snapshots
            .borrow()
            .usage_facts
            .iter()
            .find(|fact| fact.identity.thread_id.as_deref() == Some("root"))
            .cloned()
            .expect("first turn usage fact");
        assert_eq!(first_fact.identity.turn_id.as_deref(), Some("one"));
        assert_eq!(
            client
                .snapshots
                .borrow()
                .token_budget
                .confirmed_total_tokens,
            Some(9)
        );
        assert!(client.snapshots.borrow().token_budget.confirmed_complete);
        send(
            &mut server,
            json!({"method":"turn/completed","params":{"threadId":"root","turn":{"id":"one","status":"completed"}}}),
        )
        .await;
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
            json!({"id":second["id"],"result":{"turn":{"id":"two"}}}),
        )
        .await;
        send(
            &mut server,
            json!({"method":"turn/started","params":{"threadId":"root","turn":{"id":"two"}}}),
        )
        .await;
        phase(&mut client, SessionPhase::Running).await;
        send(
            &mut server,
            json!({"method":"thread/tokenUsage/updated","params":{"threadId":"root","turnId":"two","tokenUsage":{"total":{"totalTokens":9}}}}),
        )
        .await;
        client
            .snapshots
            .wait_for(|snapshot| {
                snapshot.turn_id.as_deref() == Some("two")
                    && snapshot.usage_facts.iter().any(|fact| {
                        fact.identity.thread_id.as_deref() == Some("root")
                            && fact.identity.turn_id.as_deref() == Some("two")
                    })
            })
            .await
            .unwrap();
        let snapshot = client.snapshots.borrow().clone();
        let second_fact = snapshot
            .usage_facts
            .iter()
            .find(|fact| fact.identity.thread_id.as_deref() == Some("root"))
            .expect("second turn usage fact");
        assert_eq!(second_fact.identity.turn_id.as_deref(), Some("two"));
        assert_eq!(second_fact.identity.generation, Some(2));
        let mut version = client.snapshots.borrow().version;
        send(
            &mut server,
            json!({"method":"thread/tokenUsage/updated","params":{"threadId":"root","turnId":"two","tokenUsage":{"total":{"totalTokens":1}}}}),
        )
        .await;
        client
            .snapshots
            .wait_for(|snapshot| snapshot.version > version)
            .await
            .unwrap();
        assert_eq!(client.snapshots.borrow().usage.total_tokens, Some(9));
        assert_eq!(
            client
                .snapshots
                .borrow()
                .token_budget
                .confirmed_total_tokens,
            Some(9)
        );
        assert!(client.snapshots.borrow().token_budget.confirmed_complete);
        version = client.snapshots.borrow().version;
        send(
            &mut server,
            json!({"method":"thread/tokenUsage/updated","params":{"threadId":"root","tokenUsage":{"total":{"totalTokens":1}}}}),
        )
        .await;
        client
            .snapshots
            .wait_for(|snapshot| snapshot.version > version)
            .await
            .unwrap();
        assert_eq!(client.snapshots.borrow().usage.total_tokens, Some(9));
        version = client.snapshots.borrow().version;
        send(
            &mut server,
            json!({"method":"thread/tokenUsage/updated","params":{"threadId":"root","turnId":"one","tokenUsage":{"total":{"totalTokens":1}}}}),
        )
        .await;
        client
            .snapshots
            .wait_for(|snapshot| snapshot.version > version)
            .await
            .unwrap();
        assert_eq!(client.snapshots.borrow().usage.total_tokens, Some(9));

        send(
            &mut server,
            json!({"method":"thread/tokenUsage/updated","params":{"threadId":"root","turnId":"two","tokenUsage":{"total":{"totalTokens":10}}}}),
        )
        .await;
        let interrupt = tokio::time::timeout(Duration::from_secs(3), next(&mut server))
            .await
            .unwrap();
        assert_eq!(interrupt["method"], "turn/interrupt");
        assert_eq!(
            interrupt["params"],
            json!({"threadId":"root","turnId":"two"})
        );
        send(&mut server, json!({"id":interrupt["id"],"result":{}})).await;
        send(
            &mut server,
            json!({"method":"turn/completed","params":{"threadId":"root","turn":{"id":"two","status":"interrupted"}}}),
        )
        .await;
        phase(&mut client, SessionPhase::Failed).await;
        client.commands.send(Command::Quit).await.unwrap();
        client.join.await.unwrap();
    }

    #[tokio::test]
    async fn session_token_budget_sums_confirmed_root_and_child_usage() {
        let (mut client, mut server) = harness_with_config(Config {
            max_total_tokens: Some(10),
            ..Default::default()
        })
        .await;
        running_root(&mut client, &mut server).await;
        child(&mut server, "child", "child-turn").await;
        send(
            &mut server,
            json!({
                "method":"thread/tokenUsage/updated",
                "params":{"threadId":"root","turnId":"root-turn","tokenUsage":{"total":{"totalTokens":5}}}
            }),
        )
        .await;
        tokio::time::timeout(
            Duration::from_secs(3),
            client.snapshots.wait_for(|snapshot| {
                snapshot.token_budget.confirmed_total_tokens == Some(5)
                    && !snapshot.token_budget.confirmed_complete
            }),
        )
        .await
        .unwrap()
        .unwrap();
        send(
            &mut server,
            json!({
                "method":"thread/tokenUsage/updated",
                "params":{"threadId":"child","turnId":"child-turn","tokenUsage":{"total":{"totalTokens":5}}}
            }),
        )
        .await;
        tokio::time::timeout(
            Duration::from_secs(3),
            client.snapshots.wait_for(|snapshot| {
                snapshot.token_budget.confirmed_total_tokens == Some(10)
                    && snapshot.token_budget.confirmed_complete
                    && snapshot.token_budget.limit == Some(10)
                    && snapshot.token_budget.stop_triggered
            }),
        )
        .await
        .unwrap()
        .unwrap();
        let snapshot = client.snapshots.borrow().clone();
        let child_fact = snapshot
            .usage_facts
            .iter()
            .find(|fact| fact.identity.thread_id.as_deref() == Some("child"))
            .expect("child usage carries its server identity");
        assert_eq!(child_fact.identity.turn_id.as_deref(), Some("child-turn"));
        assert_eq!(child_fact.identity.generation, Some(1));

        let mut interrupted_threads = BTreeSet::new();
        for _ in 0..2 {
            let interrupt = tokio::time::timeout(Duration::from_secs(3), next(&mut server))
                .await
                .unwrap_or_else(|_| panic!("budget stop did not interrupt every active turn"));
            assert_eq!(interrupt["method"], "turn/interrupt");
            interrupted_threads
                .insert(interrupt["params"]["threadId"].as_str().unwrap().to_owned());
            send(&mut server, json!({"id":interrupt["id"],"result":{}})).await;
        }
        assert_eq!(
            interrupted_threads,
            BTreeSet::from(["root".into(), "child".into()])
        );
        send(
            &mut server,
            json!({
                "method":"turn/completed",
                "params":{"threadId":"root","turn":{"id":"root-turn","status":"interrupted"}}
            }),
        )
        .await;
        send(
            &mut server,
            json!({
                "method":"turn/completed",
                "params":{"threadId":"child","turn":{"id":"child-turn","status":"interrupted"}}
            }),
        )
        .await;
        phase(&mut client, SessionPhase::Failed).await;
        client
            .commands
            .send(Command::SubmitRootInput {
                text: "blocked".into(),
            })
            .await
            .unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(100), next(&mut server))
                .await
                .is_err()
        );
        client.commands.send(Command::Quit).await.unwrap();
        client.join.await.unwrap();
    }

    #[tokio::test]
    async fn child_new_generation_keeps_usage_unavailable_until_confirmed() {
        let (mut client, mut server) = harness().await;
        running_root(&mut client, &mut server).await;
        child(&mut server, "child", "child-turn").await;
        send(
            &mut server,
            json!({"method":"thread/tokenUsage/updated","params":{"threadId":"child","turnId":"child-turn","tokenUsage":{"total":{"totalTokens":7}}}}),
        )
        .await;
        client
            .snapshots
            .wait_for(|snapshot| {
                snapshot.usage_facts.iter().any(|fact| {
                    fact.identity.thread_id.as_deref() == Some("child")
                        && fact.identity.turn_id.as_deref() == Some("child-turn")
                })
            })
            .await
            .unwrap();
        send(
            &mut server,
            json!({"method":"turn/completed","params":{"threadId":"child","turn":{"id":"child-turn","status":"completed"}}}),
        )
        .await;
        send(
            &mut server,
            json!({"method":"turn/started","params":{"threadId":"child","turn":{"id":"child-next"}}}),
        )
        .await;
        client
            .snapshots
            .wait_for(|snapshot| {
                snapshot.agents.iter().any(|agent| {
                    agent.info.id == "child" && agent.turn_id.as_deref() == Some("child-next")
                })
            })
            .await
            .unwrap();
        let snapshot = client.snapshots.borrow().clone();
        let fact = snapshot
            .usage_facts
            .iter()
            .find(|fact| fact.identity.thread_id.as_deref() == Some("child"))
            .expect("new generation keeps an explicit unavailable fact");
        assert_eq!(fact.summary.source, FactSource::Unknown);
        assert_eq!(fact.identity.turn_id, None);
        assert_eq!(fact.identity.generation, None);
        client.commands.send(Command::Quit).await.unwrap();
        client.join.await.unwrap();
    }

    #[tokio::test]
    async fn per_agent_budget_interrupts_root_once_and_ignores_late_old_turn_usage() {
        let (mut client, mut server) = harness_with_config(Config {
            max_agent_tokens: Some(10),
            ..Default::default()
        })
        .await;
        running_root(&mut client, &mut server).await;
        assert_eq!(
            client.snapshots.borrow().token_budget.per_agent_limit,
            Some(10)
        );
        assert!(
            !client
                .snapshots
                .borrow()
                .token_budget
                .per_agent_stop_triggered
        );
        send(
            &mut server,
            json!({"method":"thread/tokenUsage/updated","params":{"threadId":"root","turnId":"root-turn","tokenUsage":{"total":{"totalTokens":10}}}}),
        )
        .await;
        let interrupt = tokio::time::timeout(Duration::from_secs(3), next(&mut server))
            .await
            .unwrap();
        assert_eq!(interrupt["method"], "turn/interrupt");
        assert!(client
            .snapshots
            .borrow()
            .notice
            .as_deref()
            .is_some_and(|notice| {
                notice.contains("Agent token budget reached")
                    && notice.contains("thread=root")
                    && notice.contains("turn=root-turn")
            }));
        assert!(
            client
                .snapshots
                .borrow()
                .token_budget
                .per_agent_stop_triggered
        );
        send(&mut server, json!({"id":interrupt["id"],"result":{}})).await;
        send(
            &mut server,
            json!({"method":"thread/tokenUsage/updated","params":{"threadId":"root","turnId":"root-turn","tokenUsage":{"total":{"totalTokens":10}}}}),
        )
        .await;
        assert!(
            tokio::time::timeout(Duration::from_millis(100), next(&mut server))
                .await
                .is_err()
        );
        send(
            &mut server,
            json!({"method":"turn/completed","params":{"threadId":"root","turn":{"id":"root-turn","status":"interrupted"}}}),
        )
        .await;
        phase(&mut client, SessionPhase::Interrupted).await;
        send(
            &mut server,
            json!({"method":"thread/tokenUsage/updated","params":{"threadId":"root","turnId":"old-turn","tokenUsage":{"total":{"totalTokens":100}}}}),
        )
        .await;
        assert!(
            tokio::time::timeout(Duration::from_millis(100), next(&mut server))
                .await
                .is_err()
        );
        client.commands.send(Command::Quit).await.unwrap();
        client.join.await.unwrap();
    }

    #[tokio::test]
    async fn per_agent_budget_interrupts_child_once_and_rejects_old_turn_usage() {
        let (mut client, mut server) = harness_with_config(Config {
            max_agent_tokens: Some(10),
            ..Default::default()
        })
        .await;
        running_root(&mut client, &mut server).await;
        child(&mut server, "child", "child-turn").await;
        assert_eq!(
            client.snapshots.borrow().token_budget.per_agent_limit,
            Some(10)
        );
        send(
            &mut server,
            json!({"method":"thread/tokenUsage/updated","params":{"threadId":"child","turnId":"child-turn","tokenUsage":{"total":{"totalTokens":10}}}}),
        )
        .await;
        let interrupt = tokio::time::timeout(Duration::from_secs(3), next(&mut server))
            .await
            .unwrap();
        assert_eq!(interrupt["method"], "turn/interrupt");
        assert_eq!(interrupt["params"]["threadId"], "child");
        assert_eq!(interrupt["params"]["turnId"], "child-turn");
        assert!(
            client
                .snapshots
                .borrow()
                .token_budget
                .per_agent_stop_triggered
        );
        send(&mut server, json!({"id":interrupt["id"],"result":{}})).await;
        send(
            &mut server,
            json!({"method":"thread/tokenUsage/updated","params":{"threadId":"child","turnId":"child-turn","tokenUsage":{"total":{"totalTokens":10}}}}),
        )
        .await;
        assert!(
            tokio::time::timeout(Duration::from_millis(100), next(&mut server))
                .await
                .is_err()
        );
        send(
            &mut server,
            json!({"method":"turn/completed","params":{"threadId":"child","turn":{"id":"child-turn","status":"interrupted"}}}),
        )
        .await;
        send(
            &mut server,
            json!({"method":"thread/tokenUsage/updated","params":{"threadId":"child","turnId":"old-child-turn","tokenUsage":{"total":{"totalTokens":100}}}}),
        )
        .await;
        assert!(
            tokio::time::timeout(Duration::from_millis(100), next(&mut server))
                .await
                .is_err()
        );
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
        send(&mut server, json!({"method":"thread/tokenUsage/updated","params":{"threadId":"root","turnId":"two","tokenUsage":{"total":{"totalTokens":42}}}})).await;
        tokio::time::timeout(
            Duration::from_secs(3),
            client
                .snapshots
                .wait_for(|s| s.usage.total_tokens == Some(42)),
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
