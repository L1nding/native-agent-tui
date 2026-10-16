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
use crate::gate::{ChildOutcome, CompletionGate, GateEvent, WaitRequest, WaitToken};
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    PreviewEvent {
        epoch: u64,
        event: crate::preview::Event,
    },
    SubmitRootInput {
        text: String,
    },
    /// 列出工作区里能补全 `@路径` 的条目；只读，限一层目录。
    CompletePath {
        query: String,
    },
    /// 向 dsh 询问它自己的斜杠命令。
    ListBackendCommands,
    /// 在空闲的会话上执行一条 dsh 斜杠命令，例如 `/compact`。
    RunBackendCommand {
        line: String,
    },
    /// 引导正在运行的 root 轮次；不开始新轮次，也不排队。
    SteerRoot {
        text: String,
    },
    ConfirmExtension {
        confirmation: crate::extensions::ExtensionConfirmation,
    },
    OpenDomainImage {
        binding: crate::domain::ReviewImageBinding,
    },
    DomainImagesChecked {
        attempt_id: u64,
        generation: u64,
        results: Vec<(crate::domain::ReviewImageBinding, Result<(), String>)>,
    },
    DomainImageOpened {
        attempt_id: u64,
        generation: u64,
        binding: crate::domain::ReviewImageBinding,
        outcome: crate::domain::review::ImageOpenOutcome,
    },
    DomainAuthorizationFinished {
        attempt_id: u64,
        generation: u64,
        root_turn_count: u64,
        binding: crate::domain::ProposalBinding,
        review_set_sha256: Option<String>,
        policy: crate::config::DomainApprovalPolicy,
        outcome: Result<String, String>,
    },
    DomainFactReadFinished {
        attempt_id: u64,
        generation: u64,
        outcome: Result<crate::domain::DomainFactData, String>,
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
    /// Query the backend's model inventory for the picker.
    ListModels,
    /// Use `model` from the next root turn on; rejected while a turn is active.
    SelectModel {
        model: String,
    },
    /// Add or edit a provider and model in the dsh profile, then reload it.
    /// With `test`, also send one short message through the saved model.
    AddModelProvider {
        spec: crate::backend::dsh_config::ProviderSpec,
        test: bool,
    },
    /// Send one short message through `model` in a separate dsh process.
    TestModel {
        model: String,
    },
    /// Sent by Core's own test task when it finishes; not a user command.
    ModelTestFinished {
        model: String,
        outcome: Result<u64, String>,
    },
    /// Restart dsh and resume the session so edited configuration is read.
    ReloadBackend,
    /// Read the saved conversations of this workspace for the sessions page.
    ListConversations,
    /// Read dsh's permission presets and the one this session runs under.
    DescribePermission,
    /// Switch this session's permission preset and make it dsh's default.
    SetPermission {
        preset: String,
    },
    /// Continue a saved conversation in this session; rejected while a turn is active.
    ResumeConversation {
        id: String,
    },
    /// Create a fresh dsh conversation in the running agent process.
    NewConversation,
    /// Store a `/setting` choice for the next start.
    SaveSetting {
        setting: crate::preferences::Setting,
        value: String,
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
        Self::launch(config).await
    }

    async fn launch(config: Config) -> Result<Self, ClientError> {
        let mut config = app_server::normalize_config(config)?;
        // 命令行参数优先；否则沿用上次在 /model、/setting 里保存的选择。
        let saved = crate::preferences::path(&config.journal)
            .map(|path| {
                // 设置原先放在 journal 目录旁边；第一次启动时挪到新位置。
                crate::preferences::migrate(&config.journal, &path);
                crate::preferences::Preferences::load(&path)
            })
            .unwrap_or_default();
        {
            if config.model.is_none() {
                if let Some(model) = saved.model(crate::backend::PREFERENCE_KEY) {
                    config.model = Some(model.to_owned());
                    config.model_is_preference = true;
                }
            }
            for class in AttentionClass::ALL {
                let default =
                    config.attention.get(class).source == crate::observation::ConfigSource::Default;
                if let (true, Some((quiet, attention))) =
                    (default, saved.attention(&format!("{class:?}")))
                {
                    // 保存的值失效时保留默认阈值。
                    let _ = config.attention.set(
                        class,
                        quiet,
                        attention,
                        crate::observation::ConfigSource::Global,
                    );
                }
            }
        }
        let observer = Observer::new(config.attention.clone());
        let mut initial = Self::initial(&config, &observer);
        for setting in crate::preferences::Setting::ALL {
            if let Some(value) = saved.setting(setting) {
                initial.saved_settings.insert(setting, value.into());
            }
        }
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
            Some((observer, journal, outbox)),
        ))
    }

    fn initial(config: &Config, observer: &Observer) -> CoreSnapshot {
        CoreSnapshot {
            phase: SessionPhase::Launching,
            cwd: config.cwd.display().to_string(),
            branch: crate::git_branch::current(&config.cwd),
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
            extensions: vec![crate::extensions::ExtensionSnapshot::Modeling(
                crate::domain::DomainFacts::new(config.domain_setup.clone()).snapshot,
            )],
            ..Default::default()
        }
    }

    fn start(
        pipe: PipeTransport,
        config: Config,
        server: Option<AppServer>,
        prepared: Option<(Observer, Journal, Outbox)>,
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
                domain: crate::domain::DomainFacts::new(config.domain_setup.clone()),
                domain_authorization_task: None,
                domain_image_check_task: None,
                domain_image_open_task: None,
                domain_image_attempt: 0,
                domain_generation: 0,
                domain_auto_approval_attempt: None,
                domain_fact_attempt: 0,
                domain_fact_task: None,
                domain_fact_queue: VecDeque::new(),
                steer: None,
                steer_count: 0,
                backend_command: None,
                backend_command_count: 0,
                preview_bridge: None,
                preview_epoch: 0,
                preview_restarts: 0,
                preview_target: None,
                config,
                server,
                command_rx,
                command_tx: commands.downgrade(),
                snapshot_tx,
                state: SessionState {
                    view: initial,
                    agents: AgentRegistry::with_limits(agent_limits),
                    usage_facts: Default::default(),
                    tool_lines: Default::default(),
                    turn_timing: None,
                    tool_started: Default::default(),
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
                session_ready: false,
                generation: 0,
                retired_turns: VecDeque::new(),
                gate: CompletionGate::default(),
                wait: None,
                ingress_seq: 0,
                file_previews: FilePreviews::default(),
                tool_details: ToolDetails::default(),
                collab_starts: HashMap::new(),
                completed_collab: VecDeque::new(),
                completed_waits: VecDeque::new(),
                identity_pending: BTreeSet::new(),
                identity_requested: BTreeSet::new(),
                skills_generation: 0,
                skills_refresh_queued: false,
                skills_refresh_force_reload: false,
                skills_refresh_source: None,
                startup_permission: false,
                saving_provider: None,
                resuming: None,
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
    NewThread,
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
    /// Guidance for the running root turn.
    Steer {
        generation: u64,
    },
    /// dsh's own slash commands for this session.
    CommandList,
    /// One of dsh's slash commands, running on the idle session.
    CommandRun,
    ModelList,
    /// dsh's permission presets and this session's choice.
    PermissionDescribe,
    PermissionSet,
    /// Adds or changes a provider through dsh's settings.
    ProviderSave,
    /// Providers dsh holds, for the edit form.
    ProviderList,
    /// dsh's sessions of this workspace, for the sessions page.
    ThreadList,
    /// Readable messages of the session just resumed.
    ThreadHistory,
    ModelSelect,
    BackendReload,
    ThreadResume,
}

impl RpcKind {
    fn generation(self) -> Option<u64> {
        match self {
            Self::StartTurn { generation }
            | Self::Interrupt { generation }
            | Self::Steer { generation } => Some(generation),
            _ => None,
        }
    }
}

struct PendingSteer {
    notice_id: String,
    text: String,
    /// dsh 给这条消息的身份；应答到达前未知。
    message_id: Option<String>,
}

struct PendingRpc {
    kind: RpcKind,
    deadline: Instant,
    identity_thread: Option<String>,
    skills_cwd: Option<String>,
}

impl PendingRpc {
    fn new(kind: RpcKind) -> Self {
        Self {
            kind,
            deadline: Instant::now() + Duration::from_secs(30),
            identity_thread: None,
            skills_cwd: None,
        }
    }
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
    /// For Core's own background tasks; weak, so dropping the handle still ends Core.
    command_tx: mpsc::WeakSender<Command>,
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
    /// The backend confirmed a session; tasks may be dispatched to it.
    session_ready: bool,
    generation: u64,
    retired_turns: VecDeque<String>,
    gate: CompletionGate,
    wait: Option<PendingTool>,
    ingress_seq: u64,
    file_previews: FilePreviews,
    tool_details: ToolDetails,
    domain: crate::domain::DomainFacts,
    domain_authorization_task: Option<JoinHandle<()>>,
    domain_image_check_task: Option<(u64, JoinHandle<()>)>,
    domain_image_open_task: Option<(u64, JoinHandle<()>)>,
    domain_image_attempt: u64,
    domain_generation: u64,
    domain_auto_approval_attempt: Option<crate::domain::ProposalBinding>,
    domain_fact_attempt: u64,
    domain_fact_task: Option<PendingDomainFact>,
    domain_fact_queue: VecDeque<(u64, crate::domain::receipt::FactReceipt)>,
    /// 已提交、尚未确认送达的一次引导；同一时刻只允许一个。
    steer: Option<PendingSteer>,
    steer_count: u64,
    /// 正在执行的 dsh 命令：(对话里的提示 id, 命令行)。同一时刻只有一条。
    backend_command: Option<(String, String)>,
    backend_command_count: u64,
    preview_bridge: Option<crate::preview::Bridge>,
    preview_epoch: u64,
    /// Unexpected preview exits restarted in this session; bounded so a broken
    /// service cannot respawn forever.
    preview_restarts: u8,
    preview_target: Option<crate::preview::WorkTarget>,
    collab_starts: HashMap<String, u64>,
    completed_collab: VecDeque<String>,
    completed_waits: VecDeque<(RpcId, String, String)>,
    identity_pending: BTreeSet<String>,
    identity_requested: BTreeSet<String>,
    skills_generation: u64,
    skills_refresh_queued: bool,
    skills_refresh_force_reload: bool,
    skills_refresh_source: Option<SkillRefreshSource>,
    /// The session waits for dsh to confirm `--permission` before it is ready.
    startup_permission: bool,
    /// `provider/model` of the save dsh has not answered yet, and whether to test it afterwards.
    saving_provider: Option<(String, bool)>,
    /// The dsh session a pending resume switches to, until its messages are shown.
    resuming: Option<crate::conversations::ConversationSummary>,
}

struct PendingTool {
    request_id: RpcId,
    call_id: String,
    parent_turn: String,
    generation: u64,
    token: WaitToken,
}

struct PendingDomainFact {
    attempt_id: u64,
    generation: u64,
    receipt: crate::domain::receipt::FactReceipt,
    task: JoinHandle<()>,
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
        self.start_preview();
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
                            let disconnected = format!("{error}; any active external outcome is unknown");
                            // 失败响应后紧接 EOF 时，保留已确认原因并照实标记断连。
                            let message = match (&self.state.view.phase, &self.state.view.last_error) {
                                (SessionPhase::Failed, Some(cause)) => format!("{cause}; {disconnected}"),
                                _ => disconnected,
                            };
                            self.state.error(SessionPhase::Disconnected, message);
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
                        self.unknown_outbox(&id);
                        if let RpcKind::SkillsList { generation, .. } = kind {
                            // 旧代次超时只丢弃结果；保留排队刷新并在本轮循环末重发。
                            if generation == self.skills_generation {
                                self.skills_failed(crate::skills::SkillAvailability::TimedOut);
                            }
                        } else if matches!(kind, RpcKind::ModelList) {
                            self.state.view.models.availability =
                                crate::models::ModelsAvailability::TimedOut;
                        } else if matches!(kind, RpcKind::PermissionDescribe) {
                            // 没读到：界面照旧显示“未读取”。
                        } else if matches!(kind, RpcKind::PermissionSet) && self.startup_permission {
                            // 启动时要求的预设没有得到确认：不在未知的预设下执行任务。
                            self.state.view.permission.switching = None;
                            self.state.error(
                                SessionPhase::Failed,
                                "dsh did not confirm --permission in time; no task was started",
                            );
                        } else if matches!(kind, RpcKind::PermissionSet) {
                            // 没有答复：不知道 dsh 是否已经切换，重新读取才算数。
                            self.state.view.permission.switching = None;
                            self.state.view.permission.current.clear();
                            self.state.view.notice = Some(
                                "The permission preset change was not confirmed in time; open /setting to read it again.".into(),
                            );
                        } else if matches!(kind, RpcKind::ProviderList) {
                            // 只影响编辑表单的预填；清单本身照常可用。
                        } else if matches!(kind, RpcKind::ProviderSave) {
                            // 没有答复：dsh 可能已经保存，也可能没有。
                            let test = self.saving_provider.take();
                            self.provider_not_saved(
                                test,
                                "dsh did not answer in time; check the model list before saving again",
                            );
                        } else if matches!(kind, RpcKind::ThreadList) {
                            self.state.view.notice =
                                Some("Sessions were not listed in time.".into());
                        } else if matches!(kind, RpcKind::ThreadHistory) {
                            // 会话已经切换成功；只是之前的消息没能显示。
                            self.resuming = None;
                            self.state.view.resuming = None;
                            self.state.view.notice = Some(
                                "Resumed; its earlier messages were not loaded in time.".into(),
                            );
                        } else if matches!(kind, RpcKind::CommandList) {
                            self.state.view.notice =
                                Some("dsh commands were not listed in time.".into());
                        } else if matches!(kind, RpcKind::CommandRun) {
                            // 没有答复：命令可能仍在执行或已经生效；不重发。
                            if let Some((notice, line)) = self.backend_command.take() {
                                self.state.command_notice(
                                    &notice,
                                    format!("{line} was not confirmed in time; it may still finish."),
                                    true,
                                );
                            }
                        } else if matches!(kind, RpcKind::Steer { .. }) {
                            // 没有答复：dsh 可能已经收下，也可能没有；不重发。
                            if let Some(steer) = self.steer.take() {
                                self.state.steer_notice(
                                    &steer.notice_id,
                                    "Steering was not confirmed in time; it may still reach the model.".into(),
                                    true,
                                );
                            }
                        } else if matches!(kind, RpcKind::NewThread) {
                            self.state.view.creating_conversation = false;
                            self.state.error(SessionPhase::Unknown,
                                "New conversation was not confirmed in time; automatic retry is disabled");
                        } else if matches!(kind, RpcKind::ModelSelect) {
                            // 未确认的切换不改变已知模型；用户可以重新选择。
                            self.state.view.models.switching = None;
                            self.state.view.notice = Some(
                                "Model switch was not confirmed in time; the current model is unchanged as far as known.".into(),
                            );
                        } else {
                            self.state.error(
                                SessionPhase::Unknown,
                                format!("dsh {kind:?} response timed out; automatic retry is disabled"),
                            );
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

        self.preview_bridge.take();
        if let Some((_, task)) = self.domain_image_check_task.take() {
            task.abort();
        }
        if let Some((_, task)) = self.domain_image_open_task.take() {
            task.abort();
        }
        if let Some(task) = self.domain_authorization_task.take() {
            task.abort();
            let _ = task.await;
        }
        if let Some(pending) = self.domain_fact_task.take() {
            pending.task.abort();
            let _ = pending.task.await;
        }
        self.domain_fact_queue.clear();
        self.domain.synchronizing = false;
        self.domain.snapshot.authorizing = false;
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
            let outcome = if self.journal_error.is_some() {
                SessionPhase::Unknown
            } else {
                outcome
            };
            if self.config.strict_outcome {
                final_snapshot.close(outcome, cleanup_error.is_none());
            } else {
                final_snapshot.close_last_turn(outcome, cleanup_error.is_none());
            }
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
        self.check_domain_images();
        self.maybe_auto_approve_domain();
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
        if matches!(
            self.state.view.phase,
            SessionPhase::Running | SessionPhase::GatePending
        ) {
            self.state.refresh_turn_timing(Instant::now());
        } else {
            // 断连、停止等没有服务端终态的结束：固定数值，不写完成时刻。
            self.state.finish_turn_timing(Instant::now(), None);
        }
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
        self.project_journal();
        self.state.view.timeline = self.observer.timeline_snapshot();
        if matches!(
            self.state.view.phase,
            SessionPhase::Unknown
                | SessionPhase::Disconnected
                | SessionPhase::Stopping
                | SessionPhase::Stopped
        ) {
            self.tool_details.execution_unavailable();
            self.state.finish_all_tool_lines();
            self.state.finish_all_reasoning();
            self.state.finish_all_notices();
            self.steer = None;
            self.backend_command = None;
        }
        self.state.view.tool_details = self.tool_details.snapshot();
        self.state.view.extensions = vec![crate::extensions::ExtensionSnapshot::Modeling(
            self.domain.snapshot.clone(),
        )];
        if let (Some(bridge), Some(setup)) = (&self.preview_bridge, &self.config.domain_setup) {
            let preparing = self
                .state
                .view
                .thread_id
                .as_deref()
                .zip(self.state.view.turn_id.as_deref())
                .and_then(|(thread, turn)| {
                    crate::preview::prepare_target(
                        &self.domain.snapshot,
                        &self.state.view.tool_details,
                        thread,
                        turn,
                    )
                });
            bridge.activity(
                setup,
                self.state.view.phase,
                self.preview_target.as_ref().or(preparing.as_ref()),
                !self.state.view.requests.is_empty(),
            );
        }
        self.domain
            .project_control_messages(&mut self.state.view.messages);
        let transport = self.pipe.stats();
        self.state.view.diagnostics.transport_bytes_in = transport.bytes_in;
        self.state.view.diagnostics.transport_bytes_out = transport.bytes_out;
        self.state.view.startup_blocked = !self.session_ready
            && !matches!(
                self.state.view.phase,
                SessionPhase::Created
                    | SessionPhase::Launching
                    | SessionPhase::Initializing
                    | SessionPhase::CheckingShell
            );
        self.snapshot_tx.send_replace(self.state.snapshot());
    }

    fn publish_journal_status(&mut self) {
        self.project_journal();
        self.state.view.observation.snapshot_version = self.state.view.version + 1;
        self.snapshot_tx.send_replace(self.state.snapshot());
    }

    /// 将 journal 视图、错误和提交水位投影为 Core 可消费的脱敏状态。
    fn project_journal(&mut self) {
        if let Some(journal) = &self.journal {
            self.state.view.journal = Some(journal.view());
        }
        if let Some(view) = &mut self.state.view.journal {
            view.error = self.journal_error.clone().or(view.error.take());
        }
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
        self.session_ready
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

        // 通知没有响应；写出即完成，不能留在 Sent 被恢复报告当成待检查的外部结果。
        let notification = envelope.id.is_none();
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
                if notification {
                    outbox.mark_confirmed(id).map_err(Self::outbox_failure)?;
                }
            }
        }
        Ok(outbox_id)
    }

    fn next_rpc_id(&mut self) -> RpcId {
        let id = RpcId::Number(self.next_id);
        self.next_id += 1;
        id
    }

    /// 已写出的请求等待响应；响应确认 outbox，超时或断连则标记为 unknown。
    fn expect_response(&mut self, id: RpcId, outbox_id: Option<u64>, rpc: PendingRpc) {
        if let Some(outbox_id) = outbox_id {
            self.outbox_pending.insert(id.clone(), outbox_id);
        }
        self.pending.insert(id, rpc);
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
        let id = self.next_rpc_id();
        let envelope = match kind {
            RpcKind::Initialize => app_server::initialize(id.clone()),
            RpcKind::ThreadStart => app_server::thread_start(id.clone(), &self.config),
            RpcKind::NewThread => app_server::thread_new(id.clone(), &self.config),
            RpcKind::SkillsList { force_reload, .. } => {
                app_server::skills_list(id.clone(), &self.config.cwd, force_reload)
            }
            RpcKind::Interrupt { .. } => app_server::interrupt(
                id.clone(),
                self.state.view.thread_id.as_deref().unwrap_or(""),
                self.state.view.turn_id.as_deref().unwrap_or(""),
            ),
            RpcKind::Steer { .. } => app_server::turn_steer(
                id.clone(),
                self.state.view.thread_id.as_deref().unwrap_or(""),
                self.state.view.turn_id.as_deref().unwrap_or(""),
                self.steer.as_ref().map_or("", |steer| steer.text.as_str()),
            ),
            RpcKind::CommandList => app_server::backend_commands_list(id.clone()),
            RpcKind::CommandRun => app_server::backend_command_run(
                id.clone(),
                self.backend_command
                    .as_ref()
                    .map_or("", |(_, line)| line.as_str()),
            ),
            RpcKind::ModelList => app_server::model_list(id.clone()),
            RpcKind::BackendReload => app_server::backend_reload(id.clone()),
            RpcKind::ThreadResume => app_server::thread_resume(
                id.clone(),
                self.resuming
                    .as_ref()
                    .map_or("", |conversation| conversation.id.as_str()),
            ),
            RpcKind::ProviderList => app_server::provider_list(id.clone()),
            RpcKind::PermissionDescribe => app_server::permission_describe(id.clone()),
            RpcKind::ThreadList => app_server::thread_list(id.clone(), &self.config.cwd),
            RpcKind::ThreadHistory => app_server::thread_history(
                id.clone(),
                self.state.view.thread_id.as_deref().unwrap_or(""),
            ),
            RpcKind::StartTurn { .. }
            | RpcKind::AgentRead
            | RpcKind::ChildInterrupt { .. }
            | RpcKind::ProviderSave
            | RpcKind::PermissionSet
            | RpcKind::ModelSelect => {
                return Err(TransportError::Failed(
                    "start-turn requires typed user input".into(),
                ))
            }
        };
        let outbox_id = self.send_effect(envelope, None)?;
        let skills_cwd = matches!(kind, RpcKind::SkillsList { .. })
            .then(|| self.config.cwd.display().to_string());
        let mut pending = PendingRpc {
            skills_cwd,
            ..PendingRpc::new(kind)
        };
        if matches!(kind, RpcKind::CommandRun) {
            // 手动压缩要等模型写完摘要，比普通请求慢得多。
            pending.deadline = Instant::now() + Duration::from_secs(300);
        }
        self.expect_response(id, outbox_id, pending);
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
                        Ok(()) => {
                            let saved = self.journal.is_some()
                                && crate::preferences::path(&self.config.journal).is_some_and(
                                    |path| {
                                        crate::preferences::save_attention(
                                            &path,
                                            &format!("{class:?}"),
                                            quiet_ms,
                                            attention_ms,
                                        )
                                        .is_ok()
                                    },
                                );
                            format!(
                                "{class:?} attention settings applied{}.",
                                if saved {
                                    " and saved"
                                } else {
                                    " for this session"
                                }
                            )
                        }
                        Err(error) => error.to_string(),
                    },
                );
            }
            Command::DomainFactReadFinished {
                attempt_id,
                generation,
                outcome,
            } => {
                self.finish_domain_fact_read(attempt_id, generation, outcome);
            }
            Command::PreviewEvent { epoch, event } => {
                if epoch != self.preview_epoch {
                    return;
                }
                match event {
                    crate::preview::Event::Ready { connection_file } => {
                        self.domain.snapshot.preview_connection = Some(connection_file);
                    }
                    crate::preview::Event::Closed => {
                        self.domain.snapshot.preview_connection = None;
                        self.domain.snapshot.preview_selection = None;
                        self.preview_target = None;
                        // The preview is display only: restart it so Blender reconnects
                        // through session discovery, a few times at most.
                        if self.preview_restarts < 3 {
                            self.preview_restarts += 1;
                            self.start_preview();
                            self.domain.snapshot.notice =
                                Some("Blender 预览服务已重新启动；Blender 会自动重新连接".into());
                        } else {
                            self.domain.snapshot.notice =
                                Some("Blender 预览已断开；场景与产物保持原状态".into());
                        }
                    }
                    crate::preview::Event::Feedback(feedback) => {
                        let outcome = if !self.idle_for_backend_change()
                            || self.domain.snapshot.authorizing
                        {
                            Err("平台正在执行或等待交互；请在 idle 后重新提交对象反馈".into())
                        } else {
                            crate::preview::feedback_text(&feedback, &self.domain.snapshot)
                        };
                        match outcome {
                            Ok(text) => {
                                self.preview_target = Some(crate::preview::WorkTarget {
                                    binding: feedback.binding.clone(),
                                    snapshot_id: Some(feedback.snapshot_id.clone()),
                                    parts: feedback.object_ids.clone(),
                                    instances: feedback.instances.clone(),
                                });
                                self.domain.remember_control(&text);
                                let before = self.state.view.root_start_requests;
                                self.command(Command::SubmitRootInput { text });
                                // Queue acceptance is not submission. Confirm the
                                // normal scheduler's owned outbox write first.
                                self.dispatch_root();
                                let status = if self.state.view.root_start_requests > before {
                                    "submitted"
                                } else {
                                    "unknown"
                                };
                                if let Some(bridge) = &self.preview_bridge {
                                    bridge.receipt(&feedback.request_id, status, "Feedback passed to the owning platform session; approval remains in TUI");
                                }
                                self.domain.snapshot.notice =
                                    Some("Blender 对象意见已送入同一平台会话，等待新候选".into());
                            }
                            Err(reason) => {
                                if let Some(bridge) = &self.preview_bridge {
                                    bridge.receipt(&feedback.request_id, "rejected", &reason);
                                }
                                self.domain.snapshot.notice = Some(reason);
                            }
                        }
                    }
                    crate::preview::Event::Selection(selection) => {
                        let old = self.domain.snapshot.preview_selection.as_ref();
                        if selection.valid(&self.domain.snapshot)
                            && old.is_none_or(|s| {
                                s.client_id != selection.client_id
                                    || s.sequence < selection.sequence
                            })
                        {
                            self.domain.snapshot.preview_selection = Some(*selection);
                        }
                    }
                    crate::preview::Event::SelectionClosed { client_id } => {
                        if self
                            .domain
                            .snapshot
                            .preview_selection
                            .as_ref()
                            .is_some_and(|s| s.client_id == client_id)
                        {
                            self.domain.snapshot.preview_selection = None;
                        }
                    }
                    crate::preview::Event::Fact(receipt) => {
                        if self.config.domain_setup.as_ref().is_some_and(|setup| {
                            crate::domain::receipt::validate(setup, &receipt).is_ok()
                        }) {
                            self.queue_domain_fact(*receipt);
                        }
                    }
                }
            }
            Command::ConfirmExtension { confirmation } => match confirmation {
                crate::extensions::ExtensionConfirmation::Modeling(binding) => {
                    self.approve_domain_change(binding)
                }
            },
            Command::OpenDomainImage { binding } => self.open_domain_image(binding),
            Command::DomainImagesChecked {
                attempt_id,
                generation,
                results,
            } => {
                if self
                    .domain_image_check_task
                    .as_ref()
                    .is_none_or(|(owner, _)| *owner != attempt_id)
                {
                    return;
                }
                self.domain_image_check_task.take();
                if generation == self.generation {
                    for (binding, result) in results {
                        if let Some(image) = self
                            .domain
                            .snapshot
                            .review_images
                            .iter_mut()
                            .find(|image| image.binding == binding)
                        {
                            image.file_state = if result.is_ok() {
                                crate::domain::ImageFileState::Available
                            } else {
                                crate::domain::ImageFileState::Unavailable
                            };
                            image.notice = result.err();
                            if image.file_state == crate::domain::ImageFileState::Unavailable {
                                image.externally_opened = false;
                            }
                        }
                    }
                }
            }
            Command::DomainImageOpened {
                attempt_id,
                generation,
                binding,
                outcome,
            } => {
                if self
                    .domain_image_open_task
                    .as_ref()
                    .is_none_or(|(owner, _)| *owner != attempt_id)
                {
                    return;
                }
                self.domain_image_open_task.take();
                if generation == self.generation {
                    if let Some(image) = self
                        .domain
                        .snapshot
                        .review_images
                        .iter_mut()
                        .find(|image| image.binding == binding)
                    {
                        image.finish_open(outcome);
                    }
                }
            }
            Command::DomainAuthorizationFinished {
                attempt_id,
                generation,
                root_turn_count,
                binding,
                review_set_sha256,
                policy,
                outcome,
            } => {
                if attempt_id != self.domain_generation || !self.domain.snapshot.authorizing {
                    return;
                }
                self.domain_authorization_task.take();
                self.domain.snapshot.authorizing = false;
                if generation != self.generation
                    || root_turn_count != self.state.view.root_turn_count
                    || !self.idle_for_backend_change()
                    || !self.domain.can_authorize(&binding)
                    || self.domain.snapshot.review_set_sha256() != review_set_sha256
                    || (policy == crate::config::DomainApprovalPolicy::ReviewedTemplate
                        && !self.domain.can_auto_authorize(&binding))
                {
                    self.domain.snapshot.notice = Some("批准结果已过期；请核对当前方案".into());
                    return;
                }
                match outcome {
                    Ok(text) => {
                        self.domain.remember_control(&text);
                        self.domain.snapshot.notice = Some(
                            if policy == crate::config::DomainApprovalPolicy::ReviewedTemplate {
                                "已审模板策略已自动批准此版本，交给平台 Agent 应用和构建".into()
                            } else {
                                "具体版本已批准，交给平台 Agent 应用和构建".into()
                            },
                        );
                        self.command(Command::SubmitRootInput { text });
                    }
                    Err(reason) => {
                        if reason.starts_with("图片不可用") {
                            for image in &mut self.domain.snapshot.review_images {
                                image.file_state = crate::domain::ImageFileState::Pending;
                                image.externally_opened = false;
                            }
                        }
                        self.domain.snapshot.notice = Some(reason);
                    }
                }
            }
            Command::SubmitRootInput { text } => {
                if self.domain.snapshot.authorizing {
                    self.state.view.notice = Some("正在记录具体版本批准，请等待结果或取消".into());
                    return;
                }
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
                if !self.session_ready && !initializing {
                    self.state.view.notice = Some(
                        "This session did not start; restart after fixing the startup error."
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
                let mut text = text;
                if !self.domain.is_control_message(&text) {
                    self.preview_target = None;
                    if let Some(selection) = &self.domain.snapshot.preview_selection {
                        if let Some(bound) =
                            crate::preview::selected_input(&text, selection, &self.domain.snapshot)
                        {
                            if bound.len() > MESSAGE_BYTES {
                                self.state.view.notice =
                                    Some("对象上下文与意见超过消息预算，请缩短意见".into());
                                return;
                            }
                            self.preview_target = Some(crate::preview::WorkTarget::from(selection));
                            self.domain.remember_control(&bound);
                            text = bound;
                        }
                    }
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
            Command::ListModels => self.list_models(),
            Command::SelectModel { model } => self.select_model(model),
            Command::TestModel { model } => self.test_model(model),
            Command::ModelTestFinished { model, outcome } => {
                use crate::models::ModelTest;
                // 只接受当前这次测试的结果。
                if self.state.view.models.test
                    == (ModelTest::Running {
                        model: model.clone(),
                    })
                {
                    self.state.view.models.test = match outcome {
                        Ok(millis) => ModelTest::Passed { model, millis },
                        Err(message) => ModelTest::Failed { model, message },
                    };
                }
            }
            Command::AddModelProvider { spec, test } => {
                let label = format!("{}/{}", spec.provider, spec.model);
                let refusal = if let Err(reason) = crate::backend::dsh_config::validate(&spec) {
                    Some(reason.to_owned())
                } else if !self.session_ready {
                    Some("dsh is not ready.".to_owned())
                } else if self.saving_provider.is_some() {
                    Some("another save is still waiting for dsh.".to_owned())
                } else {
                    let id = self.next_rpc_id();
                    let sent = self
                        .send_effect(app_server::provider_save(id.clone(), &spec), None)
                        .map(|outbox_id| {
                            self.expect_response(
                                id,
                                outbox_id,
                                PendingRpc::new(RpcKind::ProviderSave),
                            )
                        });
                    sent.err().map(|error| error.to_string())
                };
                match refusal {
                    None => {
                        self.saving_provider = Some((label, test));
                        self.state.view.notice = Some("Saving the provider to dsh...".into());
                    }
                    Some(reason) => self.provider_not_saved(Some((label, test)), &reason),
                }
            }
            Command::DescribePermission => {
                let reading = self
                    .pending
                    .values()
                    .any(|rpc| matches!(rpc.kind, RpcKind::PermissionDescribe));
                if !reading && self.session_ready {
                    // 读不到只是不显示；设置面板会说明。
                    let _ = self.send_rpc(RpcKind::PermissionDescribe);
                }
            }
            Command::SetPermission { preset } => {
                if self.config.domain_setup.is_some() && preset != "read-only" {
                    self.state.view.notice =
                        Some("领域建模会话的通用工具保持只读；写入通过领域工具执行".into());
                    return;
                }
                if let Err(reason) = self.set_permission(&preset, true) {
                    self.state.view.notice =
                        Some(format!("Permission preset was not changed: {reason}"));
                }
            }
            Command::ListConversations => {
                let listing = self
                    .pending
                    .values()
                    .any(|rpc| matches!(rpc.kind, RpcKind::ThreadList));
                if !listing && self.session_ready {
                    if let Err(error) = self.send_rpc(RpcKind::ThreadList) {
                        self.state.view.notice = Some(format!("Sessions were not listed: {error}"));
                    }
                }
            }
            Command::ResumeConversation { id } => {
                if let Err(reason) = self.resume_conversation(&id) {
                    self.state.view.notice =
                        Some(format!("Conversation was not resumed: {reason}"));
                }
            }
            Command::CompletePath { query } => {
                self.state.view.path_hints = crate::path_hints::complete(&self.config.cwd, &query);
            }
            Command::ListBackendCommands => {
                if self.session_ready && self.state.view.thread_id.is_some() {
                    if let Err(error) = self.send_rpc(RpcKind::CommandList) {
                        self.state.view.notice =
                            Some(format!("dsh commands were not listed: {error}"));
                    }
                }
            }
            Command::RunBackendCommand { line } => {
                if let Err(reason) = self.run_backend_command(line) {
                    self.state.view.notice = Some(format!("The command was not run: {reason}"));
                }
            }
            Command::SteerRoot { text } => {
                if let Err(reason) = self.steer_root(text) {
                    self.state.view.notice = Some(format!("Steering was not sent: {reason}"));
                }
            }
            Command::NewConversation => {
                if let Err(reason) = self.start_new_conversation() {
                    self.state.view.notice =
                        Some(format!("New conversation was not started: {reason}"));
                }
            }
            Command::ReloadBackend => {
                self.state.view.notice = Some(match self.reload_backend() {
                    Ok(()) => "Reloading dsh configuration...".into(),
                    Err(reason) => format!("Configuration was not reloaded: {reason}"),
                });
            }
            Command::SaveSetting { setting, value } => {
                if !setting.values().contains(&value.as_str()) {
                    self.state.view.notice = Some("Unsupported setting value.".into());
                    return;
                }
                let stored = self.journal.is_none()
                    || crate::preferences::path(&self.config.journal).is_some_and(|path| {
                        crate::preferences::save_setting(&path, setting, &value).is_ok()
                    });
                self.state.view.notice = Some(if stored {
                    format!("{} set to {value} and saved.", setting.key())
                } else {
                    format!("{} could not be saved.", setting.key())
                });
                if stored {
                    self.state.view.saved_settings.insert(setting, value);
                }
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
                if let Some(task) = self.domain_authorization_task.take() {
                    task.abort();
                    self.domain_generation = self.domain_generation.wrapping_add(1);
                    self.domain.snapshot.authorizing = false;
                    self.domain.snapshot.notice =
                        Some("批准已中断，外部记录结果未知；未提交场景应用".into());
                }
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

    fn list_models(&mut self) {
        use crate::models::ModelsAvailability;
        if self.state.view.thread_id.is_none()
            || self
                .pending
                .values()
                .any(|rpc| matches!(rpc.kind, RpcKind::ModelList))
        {
            return;
        }
        match self.send_rpc(RpcKind::ModelList) {
            Ok(()) => self.state.view.models.availability = ModelsAvailability::Loading,
            Err(_) => {
                self.state.view.models.availability = ModelsAvailability::TransportUnavailable
            }
        }
    }

    /// Remembers a confirmed model for the next start. Only launched sessions
    /// write; a failure leaves the running session unaffected.
    fn remember_model(&self, model: &str) -> &'static str {
        if self.journal.is_none() {
            return "";
        }
        let saved = crate::preferences::path(&self.config.journal).is_some_and(|path| {
            crate::preferences::save_model(&path, crate::backend::PREFERENCE_KEY, model).is_ok()
        });
        if saved {
            " Remembered for the next start."
        } else {
            " It could not be saved for the next start."
        }
    }

    /// A confirmed session is the readiness fact; the sandbox is dsh's own and
    /// is first exercised when the agent runs a command.
    fn reset_domain_session(&mut self) {
        if let Some((_, task)) = self.domain_image_check_task.take() {
            task.abort();
        }
        if let Some((_, task)) = self.domain_image_open_task.take() {
            task.abort();
        }
        if self.config.domain_setup.is_none() {
            return;
        }
        if let Some(task) = self.domain_authorization_task.take() {
            task.abort();
        }
        self.domain_generation = self.domain_generation.wrapping_add(1);
        self.domain = crate::domain::DomainFacts::new(self.config.domain_setup.clone());
        if let Some(pending) = self.domain_fact_task.take() {
            pending.task.abort();
        }
        self.domain_fact_attempt = self.domain_fact_attempt.wrapping_add(1);
        self.domain_fact_queue.clear();
        self.start_preview();
        self.session_ready = false;
        self.state.view.phase = SessionPhase::Initializing;
    }

    fn start_preview(&mut self) {
        self.preview_bridge.take();
        self.preview_target = None;
        self.domain.snapshot.preview_selection = None;
        self.preview_epoch = self.preview_epoch.wrapping_add(1);
        if self.config.domain_preview {
            if let Some(setup) = self.config.domain_setup.clone() {
                self.preview_bridge = Some(crate::preview::Bridge::start(
                    setup,
                    self.preview_epoch,
                    self.command_tx.clone(),
                ));
            }
        }
    }

    fn confirm_configured_permission(&mut self) -> bool {
        if let Some(preset) = self.config.permission.clone() {
            // `--permission`：会话在 dsh 确认这个预设之前不算就绪，
            // 任务不会在别的预设下开始。只改这个会话，不改 dsh 的默认值。
            let id = self.next_rpc_id();
            match self.send_effect(app_server::permission_set(id.clone(), &preset, false), None) {
                Ok(outbox_id) => {
                    self.expect_response(id, outbox_id, PendingRpc::new(RpcKind::PermissionSet));
                    self.state.view.permission.switching = Some(preset);
                    self.startup_permission = true;
                    self.state.view.notice =
                        Some("Waiting for dsh to confirm the permission preset".into());
                }
                Err(error) => self.state.error(
                    SessionPhase::Failed,
                    format!("--permission {preset} could not be requested: {error}"),
                ),
            }
            return true;
        }
        false
    }

    fn session_started(&mut self) {
        self.state.view.phase = SessionPhase::Ready;
        self.state.view.notice = None;
        self.session_ready = true;
        self.skills_generation = self.skills_generation.wrapping_add(1);
        self.queue_skills_refresh(SkillRefreshSource::Initial, false);
    }

    /// Asks dsh to switch this session's permission preset. Nothing is shown
    /// as changed until dsh confirms it.
    fn set_permission(&mut self, preset: &str, remember: bool) -> Result<(), String> {
        if preset.is_empty() || preset.len() > 64 || preset.chars().any(char::is_control) {
            return Err("name a preset.".into());
        }
        if !self.session_ready {
            return Err("dsh is not ready.".into());
        }
        if self.state.view.permission.switching.is_some() {
            return Err("another change is still waiting for dsh.".into());
        }
        let id = self.next_rpc_id();
        let outbox_id = self
            .send_effect(
                app_server::permission_set(id.clone(), preset, remember),
                None,
            )
            .map_err(|error| error.to_string())?;
        self.expect_response(id, outbox_id, PendingRpc::new(RpcKind::PermissionSet));
        self.state.view.permission.switching = Some(preset.to_owned());
        Ok(())
    }

    /// Reports a provider that was not saved; a test that was to follow fails with the same reason.
    fn provider_not_saved(&mut self, saving: Option<(String, bool)>, reason: &str) {
        let reason: String = reason.chars().take(400).collect();
        if let Some((model, true)) = saving {
            self.state.view.models.test = crate::models::ModelTest::Failed {
                model,
                message: format!("not saved: {reason}"),
            };
        }
        self.state.view.notice = Some(format!("Provider was not saved: {reason}"));
    }

    /// Switches this session to another dsh session; the ACP bridge releases
    /// the previous active session after the target has been confirmed.
    fn resume_conversation(&mut self, id: &str) -> Result<(), String> {
        if id.is_empty() || id.len() > 1024 {
            return Err("no session was named.".into());
        }
        if self.state.view.thread_id.as_deref() == Some(id) {
            return Err("it is the conversation already open here.".into());
        }
        if !self.idle_for_backend_change() || self.state.view.resuming.is_some() {
            return Err("wait for the current turn to finish first.".into());
        }
        let target = self
            .state
            .view
            .conversations
            .iter()
            .find(|conversation| conversation.id == id)
            .cloned()
            .unwrap_or_else(|| crate::conversations::ConversationSummary {
                id: id.to_owned(),
                ..Default::default()
            });
        self.state.view.resuming = Some(Self::resume_label(&target));
        self.resuming = Some(target);
        if let Err(error) = self.send_rpc(RpcKind::ThreadResume) {
            self.state.view.resuming = None;
            self.resuming = None;
            return Err(error.to_string());
        }
        self.state.view.notice = Some("Resuming the conversation...".into());
        Ok(())
    }

    /// dsh 的命令会改变会话状态（压缩历史、切换计划模式），只在会话空闲时执行。
    fn run_backend_command(&mut self, line: String) -> Result<(), String> {
        let line = line.trim().to_owned();
        if !line.starts_with('/') || line.len() > 4096 {
            return Err("it is not a command line.".into());
        }
        if !self.idle_for_backend_change() || self.scheduler.snapshot().queued_roots > 0 {
            return Err("wait for the current turn and queued tasks to finish first.".into());
        }
        if self.backend_command.is_some() {
            return Err("another command is still running.".into());
        }
        self.backend_command_count += 1;
        let notice = format!("command:{}", self.backend_command_count);
        self.backend_command = Some((notice.clone(), line.clone()));
        if let Err(error) = self.send_rpc(RpcKind::CommandRun) {
            self.backend_command = None;
            return Err(error.to_string());
        }
        self.state
            .command_notice(&notice, format!("Running {line}…"), false);
        Ok(())
    }

    /// 引导只在 root 轮次运行中有效。等待子 Agent 时不接受：那时 root 没有
    /// 可以消费引导的下一步，放行只会让它在门释放后才被处理。
    fn steer_root(&mut self, text: String) -> Result<(), String> {
        let text = text.trim().to_owned();
        if text.is_empty() || text.len() > crate::state::MESSAGE_BYTES {
            return Err("the guidance is empty or too long.".into());
        }
        match self.state.view.phase {
            SessionPhase::Running if self.state.view.turn_id.is_some() => {}
            SessionPhase::GatePending => {
                return Err("the turn is waiting for child agents.".into());
            }
            _ => return Err("no turn is running; send it as a task instead.".into()),
        }
        if self.steer.is_some() {
            return Err("the previous guidance is still being delivered.".into());
        }
        self.steer_count += 1;
        let notice_id = format!("steer:{}", self.steer_count);
        self.steer = Some(PendingSteer {
            notice_id: notice_id.clone(),
            text,
            message_id: None,
        });
        if let Err(error) = self.send_rpc(RpcKind::Steer {
            generation: self.generation,
        }) {
            self.steer = None;
            return Err(error.to_string());
        }
        self.state.steer_notice(
            &notice_id,
            "Steering queued for the next step…".into(),
            false,
        );
        Ok(())
    }

    fn start_new_conversation(&mut self) -> Result<(), String> {
        if self.state.view.creating_conversation {
            return Err("another new conversation is already starting.".into());
        }
        if !self.idle_for_backend_change() || self.scheduler.snapshot().queued_roots > 0 {
            return Err("wait for the current turn and queued tasks to finish first.".into());
        }
        self.send_rpc(RpcKind::NewThread)
            .map_err(|error| error.to_string())?;
        self.state.view.creating_conversation = true;
        self.state.view.notice = Some("Starting a new conversation...".into());
        Ok(())
    }

    fn resume_label(target: &crate::conversations::ConversationSummary) -> String {
        if target.title.is_empty() {
            "untitled session".into()
        } else {
            target.title.clone()
        }
    }

    /// Starts a model test in a task of its own; the result returns as a command.
    fn test_model(&mut self, model: String) {
        use crate::models::ModelTest;
        let model = model.trim().to_owned();
        let refusal = if model.is_empty() || model.len() > crate::models::MAX_MODEL_BYTES {
            Some("name a model to test")
        } else if self.journal.is_none() {
            Some("this session cannot start a test process")
        } else if matches!(self.state.view.models.test, ModelTest::Running { .. }) {
            Some("another test is still running")
        } else {
            None
        };
        if let Some(reason) = refusal {
            if !matches!(self.state.view.models.test, ModelTest::Running { .. }) {
                self.state.view.models.test = ModelTest::Failed {
                    model,
                    message: reason.into(),
                };
            }
            self.state.view.notice = Some(format!("Model test was not started: {reason}."));
            return;
        }
        self.state.view.models.test = ModelTest::Running {
            model: model.clone(),
        };
        let config = self.config.clone();
        let commands = self.command_tx.clone();
        tokio::spawn(async move {
            let outcome = crate::backend::acp::test_model(config, model.clone()).await;
            if let Some(commands) = commands.upgrade() {
                let _ = commands
                    .send(Command::ModelTestFinished { model, outcome })
                    .await;
            }
        });
    }

    fn queue_domain_fact(&mut self, receipt: crate::domain::receipt::FactReceipt) {
        if self
            .domain_fact_task
            .as_ref()
            .is_some_and(|pending| pending.receipt == receipt)
            || self
                .domain_fact_queue
                .iter()
                .any(|(_, queued)| queued == &receipt)
        {
            return;
        }
        if self.domain_fact_queue.len() >= 32 {
            self.domain.synchronization_failed(
                &receipt.kind,
                receipt.fact_sequence,
                "读取队列已满".into(),
            );
            return;
        }
        self.domain_fact_queue.push_back((self.generation, receipt));
        self.domain.synchronizing = true;
        self.domain.snapshot.fact_sync_notice = Some("正在同步完整领域事实".into());
        self.start_domain_fact_read();
    }

    fn start_domain_fact_read(&mut self) {
        if self.domain_fact_task.is_some() {
            return;
        }
        let Some((generation, receipt)) = self.domain_fact_queue.pop_front() else {
            self.domain.synchronization_queue_drained();
            return;
        };
        let Some(setup) = self.config.domain_setup.clone() else {
            return;
        };
        self.domain_fact_attempt = self.domain_fact_attempt.wrapping_add(1);
        let attempt_id = self.domain_fact_attempt;
        let commands = self.command_tx.clone();
        let pending = receipt.clone();
        let task = tokio::spawn(async move {
            let outcome = crate::domain::receipt::read(setup, pending)
                .await
                .map(crate::domain::DomainFactData);
            if let Some(commands) = commands.upgrade() {
                let _ = commands
                    .send(Command::DomainFactReadFinished {
                        attempt_id,
                        generation,
                        outcome,
                    })
                    .await;
            }
        });
        self.domain_fact_task = Some(PendingDomainFact {
            attempt_id,
            generation,
            receipt,
            task,
        });
    }

    fn finish_domain_fact_read(
        &mut self,
        attempt_id: u64,
        generation: u64,
        outcome: Result<crate::domain::DomainFactData, String>,
    ) {
        if !self.domain_fact_task.as_ref().is_some_and(|pending| {
            pending.attempt_id == attempt_id && pending.generation == generation
        }) {
            return;
        }
        let pending = self.domain_fact_task.take().unwrap();
        let result = if generation != self.generation {
            Err("读取结果属于已结束的执行代次".into())
        } else {
            outcome.and_then(|raw| {
                self.config
                    .domain_setup
                    .as_ref()
                    .ok_or_else(|| "建模会话已结束".to_owned())
                    .and_then(|setup| {
                        crate::domain::receipt::verify(setup, &pending.receipt, &raw.0)
                    })
            })
        };
        match result {
            Ok(value) => {
                if self.domain.apply_verified_fact(&value) == crate::domain::FactProjection::Invalid
                {
                    self.domain.synchronization_failed(
                        &pending.receipt.kind,
                        pending.receipt.fact_sequence,
                        "完整事实不能投影为当前领域状态".into(),
                    );
                } else {
                    self.domain.synchronization_finished(&pending.receipt);
                }
            }
            Err(reason) => self.domain.synchronization_failed(
                &pending.receipt.kind,
                pending.receipt.fact_sequence,
                reason,
            ),
        }
        self.start_domain_fact_read();
    }

    fn check_domain_images(&mut self) {
        if self.domain_image_check_task.is_some() {
            return;
        }
        let bindings: Vec<_> = self
            .domain
            .snapshot
            .review_images
            .iter_mut()
            .filter(|image| image.file_state == crate::domain::ImageFileState::Pending)
            .map(|image| {
                image.file_state = crate::domain::ImageFileState::Checking;
                image.binding.clone()
            })
            .collect();
        if bindings.is_empty() {
            return;
        }
        let generation = self.generation;
        let commands = self.command_tx.clone();
        self.domain_image_attempt = self.domain_image_attempt.wrapping_add(1);
        let attempt_id = self.domain_image_attempt;
        self.domain_image_check_task = Some((
            attempt_id,
            tokio::spawn(async move {
                let mut results = Vec::new();
                for binding in bindings {
                    let result = crate::domain::review::verify_image(&binding).await;
                    results.push((binding, result));
                }
                if let Some(commands) = commands.upgrade() {
                    let _ = commands
                        .send(Command::DomainImagesChecked {
                            attempt_id,
                            generation,
                            results,
                        })
                        .await;
                }
            }),
        ));
    }

    fn open_domain_image(&mut self, binding: crate::domain::ReviewImageBinding) {
        if self.domain_image_open_task.is_some() || self.domain.snapshot.authorizing {
            return;
        }
        if !self
            .domain
            .snapshot
            .proposal
            .as_ref()
            .is_some_and(|proposal| proposal.binding == binding.proposal)
            || !self.domain.snapshot.review_images.iter().any(|image| {
                image.binding == binding
                    && image.file_state == crate::domain::ImageFileState::Available
            })
        {
            self.domain.snapshot.notice =
                Some("图片不可用或已属于旧版本；请核对当前图片列表".into());
            return;
        }
        let generation = self.generation;
        let commands = self.command_tx.clone();
        self.domain_image_attempt = self.domain_image_attempt.wrapping_add(1);
        let attempt_id = self.domain_image_attempt;
        for image in &mut self.domain.snapshot.review_images {
            if image.binding == binding {
                image.externally_opened = false;
                image.notice = Some("正在外部打开".into());
            }
        }
        self.domain_image_open_task = Some((
            attempt_id,
            tokio::spawn(async move {
                let outcome = crate::domain::review::open_image(binding.clone()).await;
                if let Some(commands) = commands.upgrade() {
                    let _ = commands
                        .send(Command::DomainImageOpened {
                            attempt_id,
                            generation,
                            binding,
                            outcome,
                        })
                        .await;
                }
            }),
        ));
    }

    fn approve_domain_change(&mut self, confirmation: crate::extensions::ModelingConfirmation) {
        self.approve_domain_change_with_policy(
            confirmation,
            crate::config::DomainApprovalPolicy::Manual,
        );
    }

    fn maybe_auto_approve_domain(&mut self) {
        if self.config.domain_approval_policy
            != crate::config::DomainApprovalPolicy::ReviewedTemplate
            || !self.idle_for_backend_change()
            || self.scheduler.snapshot().queued_roots > 0
        {
            return;
        }
        if self.domain.snapshot.proposal.is_none() {
            return;
        }
        let confirmation = self.domain.snapshot.confirmation();
        if self.domain_auto_approval_attempt.as_ref() != Some(&confirmation.proposal)
            && self.domain.can_auto_authorize(&confirmation.proposal)
        {
            self.approve_domain_change_with_policy(
                confirmation,
                crate::config::DomainApprovalPolicy::ReviewedTemplate,
            );
        }
    }

    fn approve_domain_change_with_policy(
        &mut self,
        confirmation: crate::extensions::ModelingConfirmation,
        policy: crate::config::DomainApprovalPolicy,
    ) {
        let binding = confirmation.proposal;
        let review_set_sha256 = confirmation.review_set_sha256;
        if !self.idle_for_backend_change()
            || !self.domain.can_authorize(&binding)
            || self.domain.snapshot.review_set_sha256() != review_set_sha256
            || (policy == crate::config::DomainApprovalPolicy::ReviewedTemplate
                && !self.domain.can_auto_authorize(&binding))
            || self
                .state
                .view
                .requests
                .iter()
                .any(|r| self.request_is_live(r))
        {
            self.domain.snapshot.notice =
                Some("当前方案或执行状态不允许批准；请等待当前轮完成并核对版本".into());
            return;
        }
        let Some(setup) = self.config.domain_setup.clone() else {
            return;
        };
        if policy == crate::config::DomainApprovalPolicy::ReviewedTemplate {
            // An uncertain private authorization result must never trigger an automatic retry.
            self.domain_auto_approval_attempt = Some(binding.clone());
        }
        self.domain_generation = self.domain_generation.wrapping_add(1);
        let attempt_id = self.domain_generation;
        let generation = self.generation;
        let root_turn_count = self.state.view.root_turn_count;
        let images = self
            .domain
            .snapshot
            .review_images
            .iter()
            .filter(|image| image.file_state == crate::domain::ImageFileState::Available)
            .map(|image| image.binding.clone())
            .collect();
        self.domain.snapshot.authorizing = true;
        let commands = self.command_tx.clone();
        self.domain_authorization_task = Some(tokio::spawn(async move {
            let outcome = crate::domain::authorize_with_policy(
                setup,
                binding.clone(),
                review_set_sha256.clone(),
                images,
                policy,
            )
            .await;
            if let Some(commands) = commands.upgrade() {
                let _ = commands
                    .send(Command::DomainAuthorizationFinished {
                        attempt_id,
                        generation,
                        root_turn_count,
                        binding,
                        review_set_sha256,
                        policy,
                        outcome,
                    })
                    .await;
            }
        }));
    }

    /// No turn, gate or task is active, so the model or backend can change.
    fn idle_for_backend_change(&self) -> bool {
        let scheduler = self.scheduler.snapshot();
        self.session_ready
            && self.state.view.thread_id.is_some()
            && self.wait.is_none()
            && self.state.view.phase.can_submit()
            && !scheduler.tasks.iter().any(|task| task.state.active())
            && self.state.view.models.switching.is_none()
            && !self.state.view.models.reloading
            && self.state.view.resuming.is_none()
            && !self.state.view.creating_conversation
    }

    fn reload_backend(&mut self) -> Result<(), String> {
        if !self.idle_for_backend_change() {
            return Err("wait for the current turn to finish, then press Ctrl+R in /model.".into());
        }
        self.send_rpc(RpcKind::BackendReload)
            .map_err(|error| error.to_string())?;
        self.state.view.models.reloading = true;
        Ok(())
    }

    fn select_model(&mut self, model: String) {
        let model = model.trim().to_owned();
        if model.is_empty() || model.len() > crate::models::MAX_MODEL_BYTES {
            self.state.view.notice = Some(format!(
                "Enter a model name of 1-{} UTF-8 bytes.",
                crate::models::MAX_MODEL_BYTES
            ));
            return;
        }
        if self.state.view.models.switching.is_some() || self.state.view.models.reloading {
            self.state.view.notice =
                Some("A model switch or reload is already waiting for the backend.".into());
            return;
        }
        if !self.idle_for_backend_change() {
            self.state.view.notice = Some(
                "The model can be changed when no turn is running; wait for it to finish or interrupt with Ctrl+C.".into(),
            );
            return;
        }
        let id = self.next_rpc_id();
        let request = app_server::model_select(id.clone(), &model);
        match self.send_effect(request, None) {
            Ok(outbox_id) => {
                self.state.view.notice = Some(format!("Switching model to {model}..."));
                self.state.view.models.switching = Some(model);
                self.expect_response(id, outbox_id, PendingRpc::new(RpcKind::ModelSelect));
            }
            Err(error) => {
                self.state.view.notice = Some(format!("Model switch was not sent: {error}"));
            }
        }
    }

    fn request_is_live(&self, request: &RequestView) -> bool {
        self.session_ready
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
        if !self.session_ready
            && !matches!(
                self.state.view.phase,
                SessionPhase::Launching | SessionPhase::Initializing
            )
        {
            self.state.view.notice =
                Some("Task dispatch requires a session; this one did not start.".into());
            return;
        }
        match self.scheduler.enqueue(tasks) {
            Ok(ids) => {
                self.state.view.notice = Some(match ids.len() {
                    1 => "Queued; it starts after the current turn. F4 shows the queue.".into(),
                    count => format!(
                        "Queued {count} tasks; they start in order after the current turn. F4 shows the queue."
                    ),
                })
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
        if !self.session_ready
            || self.wait.is_some()
            || !self.state.view.phase.can_submit()
            || self.state.view.thread_id.is_none()
            // 模型切换或重载确认前不开始新一轮，避免这一轮落在未知的模型或进程上。
            || self.state.view.models.switching.is_some()
            || self.state.view.models.reloading
            || self.state.view.resuming.is_some()
            || self.state.view.creating_conversation
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
        let id = self.next_rpc_id();
        let request = app_server::interrupt(id.clone(), &external.thread_id, &external.turn_id);
        match self.send_effect(request, Some(effect.attempt)) {
            Ok(outbox_id) => {
                let kind = RpcKind::ChildInterrupt {
                    attempt: effect.attempt,
                };
                self.expect_response(id, outbox_id, PendingRpc::new(kind));
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
        let id = self.next_rpc_id();
        let thread = self.state.view.thread_id.clone().unwrap();
        // 新 root turn 尚未收到 usage；旧 turn 事实不能重新绑定到这一代。
        self.state.clear_usage_fact(&thread);
        let request = app_server::turn_start(id.clone(), &thread, text, None);
        let outbox_id = match self.send_effect(request, self.root_attempt) {
            Ok(outbox_id) => outbox_id,
            Err(error) => {
                self.state.error(SessionPhase::Unknown, error.to_string());
                return;
            }
        };
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
        let kind = RpcKind::StartTurn {
            generation: self.generation,
        };
        self.expect_response(id, outbox_id, PendingRpc::new(kind));
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
            self.state.begin_turn_timing(Instant::now());
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
                        if !(child_request && self.session_ready
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
                    if matches!(pending.kind, RpcKind::CommandList) {
                        let message: String = message.chars().take(200).collect();
                        self.state.view.notice =
                            Some(format!("dsh commands were not listed: {message}"));
                        return;
                    }
                    if matches!(pending.kind, RpcKind::CommandRun) {
                        if let Some((notice, line)) = self.backend_command.take() {
                            let reason: String = message.chars().take(400).collect();
                            self.state.command_notice(
                                &notice,
                                format!("{line} was not run: {reason}"),
                                true,
                            );
                        }
                        return;
                    }
                    if matches!(pending.kind, RpcKind::Steer { .. }) {
                        if let Some(steer) = self.steer.take() {
                            let reason: String = message.chars().take(200).collect();
                            self.state.steer_notice(
                                &steer.notice_id,
                                format!("Steering was not accepted: {reason}"),
                                true,
                            );
                        }
                        return;
                    }
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
                    if matches!(pending.kind, RpcKind::ModelList) {
                        let unsupported = error.get("code").and_then(Value::as_i64) == Some(-32601);
                        self.state.view.models.availability = if unsupported {
                            crate::models::ModelsAvailability::Unsupported
                        } else {
                            crate::models::ModelsAvailability::RpcError
                        };
                        return;
                    }
                    if matches!(
                        pending.kind,
                        RpcKind::ProviderList | RpcKind::PermissionDescribe
                    ) {
                        return;
                    }
                    if matches!(pending.kind, RpcKind::PermissionSet) && self.startup_permission {
                        self.state.view.permission.switching = None;
                        let message: String = message.chars().take(400).collect();
                        self.state.error(
                            SessionPhase::Failed,
                            format!("dsh refused --permission: {message}. No task was started."),
                        );
                        return;
                    }
                    if matches!(pending.kind, RpcKind::PermissionSet) {
                        // dsh 拒绝了：预设没有变。
                        self.state.view.permission.switching = None;
                        let message: String = message.chars().take(400).collect();
                        self.state.view.notice =
                            Some(format!("Permission preset was not changed: {message}"));
                        return;
                    }
                    if matches!(pending.kind, RpcKind::ProviderSave) {
                        // dsh 拒绝了（例如 schema 不接受）：什么都没有保存。
                        let saving = self.saving_provider.take();
                        self.provider_not_saved(saving, &message);
                        return;
                    }
                    if matches!(pending.kind, RpcKind::ThreadList) {
                        let message: String = message.chars().take(400).collect();
                        self.state.view.notice =
                            Some(format!("Sessions were not listed: {message}"));
                        return;
                    }
                    if matches!(pending.kind, RpcKind::ThreadHistory) {
                        // 会话已经切换成功；只是之前的消息没能显示。
                        self.state.view.resuming = None;
                        self.resuming = None;
                        let message: String = message.chars().take(400).collect();
                        self.state.view.notice = Some(format!(
                            "Resumed; its earlier messages could not be loaded: {message}"
                        ));
                        return;
                    }
                    if matches!(pending.kind, RpcKind::NewThread) {
                        self.state.view.creating_conversation = false;
                        if error
                            .pointer("/data/sessionUnavailable")
                            .and_then(Value::as_bool)
                            == Some(true)
                        {
                            self.session_ready = false;
                            self.state.error(
                                SessionPhase::Failed,
                                message.chars().take(400).collect::<String>(),
                            );
                        } else {
                            self.state.view.notice = Some(format!(
                                "New conversation was not started: {}",
                                message.chars().take(400).collect::<String>()
                            ));
                        }
                        return;
                    }
                    if matches!(pending.kind, RpcKind::ThreadResume) {
                        if error
                            .pointer("/data/sessionUnavailable")
                            .and_then(Value::as_bool)
                            == Some(true)
                        {
                            self.state.view.resuming = None;
                            self.resuming = None;
                            self.state.error(
                                SessionPhase::Failed,
                                message.chars().take(400).collect::<String>(),
                            );
                            return;
                        }
                        // 没能恢复：当前对话原样保留。
                        self.state.view.resuming = None;
                        self.resuming = None;
                        let message: String = message.chars().take(400).collect();
                        self.state.view.notice =
                            Some(format!("Conversation was not resumed: {message}"));
                        return;
                    }
                    if matches!(pending.kind, RpcKind::BackendReload) {
                        self.state.view.models.reloading = false;
                        let message: String = message.chars().take(400).collect();
                        if message.starts_with("dsh was not restarted") {
                            // 新进程没起来，原进程和会话都没动。
                            self.state.view.notice = Some(message);
                        } else {
                            self.state.error(SessionPhase::Failed, message);
                        }
                        return;
                    }
                    if matches!(pending.kind, RpcKind::ModelSelect) {
                        // 被拒绝的切换不是会话错误：当前模型不变，用户可以再选。
                        self.state.view.models.switching = None;
                        let message: String = message.chars().take(400).collect();
                        self.state.view.notice = Some(format!("Model was not changed: {message}"));
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
                if let Err(error) =
                    self.send_effect(Envelope::notification("initialized", None), None)
                {
                    self.state.error(SessionPhase::Failed, error.to_string());
                    return;
                }
                Some(RpcKind::ThreadStart)
            }
            RpcKind::ThreadStart | RpcKind::NewThread => {
                let new_session = matches!(kind, RpcKind::NewThread);
                let Some(id) = result
                    .pointer("/thread/id")
                    .and_then(Value::as_str)
                    .filter(|id| !id.trim().is_empty() && id.len() <= 1024)
                else {
                    self.state.error(
                        SessionPhase::Failed,
                        if new_session {
                            "thread/new response is missing thread.id"
                        } else {
                            "thread/start response is missing thread.id"
                        },
                    );
                    self.state.view.creating_conversation = false;
                    self.session_ready = false;
                    return;
                };
                if new_session {
                    self.reset_domain_session();
                    self.session_ready = false;
                    self.state.view.phase = SessionPhase::Initializing;
                    self.state.view.creating_conversation = false;
                    self.state.view.messages.clear();
                    self.state.view.message_replacements.clear();
                    self.state.view.todos.clear();
                    self.steer = None;
                    self.backend_command = None;
                    self.state.view.commands.clear();
                    self.state.view.commands_loaded = false;
                    self.state.view.requests.clear();
                    self.state.view.permission = Default::default();
                    self.state.view.turn_id = None;
                    self.state.view.last_error = None;
                    self.state.view.last_headless_action = None;
                    self.state.view.history_truncated = false;
                    self.state.view.usage = Default::default();
                    self.state.usage_facts.clear();
                    self.state.tool_lines.clear();
                }
                self.state.view.thread_id = Some(id.into());
                self.state.view.model = result
                    .get("model")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                if let Some(cwd) = result.get("cwd").and_then(Value::as_str) {
                    self.state.view.cwd = cwd.into();
                }
                if self.confirm_configured_permission() {
                    return;
                }
                self.session_started();
                if new_session {
                    self.state.view.notice = Some("New conversation started.".into());
                }
                new_session.then_some(RpcKind::PermissionDescribe)
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
            RpcKind::CommandList => {
                match crate::state::parse_backend_commands(&result) {
                    Some(commands) => {
                        self.state.view.commands = commands;
                        self.state.view.commands_loaded = true;
                    }
                    None => {
                        self.state.view.notice =
                            Some("dsh commands were not listed: the answer was malformed.".into())
                    }
                }
                None
            }
            RpcKind::CommandRun => {
                if let Some((notice, line)) = self.backend_command.take() {
                    let text: String = result["text"]
                        .as_str()
                        .unwrap_or_default()
                        .chars()
                        .take(2000)
                        .collect();
                    let outcome = match (result["kind"].as_str(), text.is_empty()) {
                        (Some("success"), true) => format!("{line}: done."),
                        (Some("success"), false) => format!("{line}: {text}"),
                        (_, true) => format!("{line} failed."),
                        (_, false) => format!("{line} failed: {text}"),
                    };
                    self.state.command_notice(&notice, outcome, true);
                }
                None
            }
            RpcKind::Steer { .. } => {
                // 应答只说明 dsh 收下了；送达由之后的事件确认。
                match result["messageId"].as_str().filter(|id| id.len() <= 128) {
                    Some(id) => {
                        if let Some(steer) = &mut self.steer {
                            steer.message_id = Some(id.to_owned());
                        }
                    }
                    None => {
                        if let Some(steer) = self.steer.take() {
                            self.state.steer_notice(
                                &steer.notice_id,
                                "Steering was not confirmed by dsh.".into(),
                                true,
                            );
                        }
                    }
                }
                None
            }
            RpcKind::Interrupt { .. } => None, // An RPC acknowledgement is not a terminal turn event.
            RpcKind::AgentRead => None, // The response is handled with its captured thread identity.
            RpcKind::ChildInterrupt { .. } => None,
            RpcKind::SkillsList { .. } => None,
            RpcKind::ModelList => {
                // 清单换新，进行中的切换、测试结果等状态保留。
                let listed = crate::models::parse_result(&result).unwrap_or_else(|| {
                    crate::models::ModelsSnapshot {
                        availability: crate::models::ModelsAvailability::Malformed,
                        ..Default::default()
                    }
                });
                let models = &mut self.state.view.models;
                models.availability = listed.availability;
                models.options = listed.options;
                models.truncated = listed.truncated;
                Some(RpcKind::ProviderList)
            }
            RpcKind::PermissionSet if self.startup_permission => {
                let requested = self.state.view.permission.switching.take();
                match crate::permission::parse(&result) {
                    // 只有 dsh 报告的正是所要求的预设，会话才就绪。
                    Some((current, presets)) if Some(&current) == requested.as_ref() => {
                        self.startup_permission = false;
                        self.state.view.permission.current = current;
                        self.state.view.permission.presets = presets;
                        self.session_started();
                    }
                    _ => self.state.error(
                        SessionPhase::Failed,
                        "dsh did not confirm the preset --permission asked for; no task was started",
                    ),
                }
                None
            }
            RpcKind::PermissionDescribe | RpcKind::PermissionSet => {
                let switched = matches!(kind, RpcKind::PermissionSet);
                let permission = &mut self.state.view.permission;
                if switched {
                    permission.switching = None;
                }
                match crate::permission::parse(&result) {
                    Some((current, presets)) => {
                        if switched {
                            self.state.view.notice =
                                Some(format!("Permission preset is now {current}."));
                        }
                        let permission = &mut self.state.view.permission;
                        permission.current = current;
                        permission.presets = presets;
                    }
                    None if switched => {
                        // 答复读不懂：不知道现在是哪个预设。
                        self.state.view.permission.current.clear();
                        self.state.view.notice = Some(
                            "dsh answered the permission change in an unexpected form; open /setting to read it again.".into(),
                        );
                    }
                    None => {}
                }
                None
            }
            RpcKind::ProviderList => {
                if let Some(providers) = crate::backend::dsh_config::parse_providers(&result) {
                    self.state.view.models.providers = providers;
                }
                None
            }
            RpcKind::ProviderSave => {
                let saving = self.saving_provider.take();
                let Some(added) = crate::backend::dsh_config::parse_saved(&result) else {
                    self.provider_not_saved(saving, "dsh answered in an unexpected form");
                    return;
                };
                let key = if added.key_stored {
                    String::new()
                } else {
                    format!(
                        " No key was entered; dsh uses credential {}.",
                        added.credential
                    )
                };
                self.state.view.notice = Some(format!(
                    "Saved {} to dsh; it is available now.{key}",
                    added.label
                ));
                if saving.is_some_and(|(_, test)| test) {
                    self.test_model(added.label);
                }
                Some(RpcKind::ModelList)
            }
            RpcKind::ThreadList => {
                match crate::conversations::parse_list(&result) {
                    Some(listed) => self.state.view.conversations = listed,
                    None => {
                        self.state.view.notice =
                            Some("Sessions were not listed: the answer was malformed.".into())
                    }
                }
                None
            }
            RpcKind::ThreadResume => {
                let Some(target) = self.resuming.as_ref() else {
                    self.state.view.resuming = None;
                    return;
                };
                let thread = result
                    .pointer("/thread/id")
                    .and_then(Value::as_str)
                    .unwrap_or(&target.id)
                    .to_owned();
                if let Some(model) = result.get("model").and_then(Value::as_str) {
                    self.state.view.model = Some(model.into());
                }
                // 界面换成恢复的会话；之前的会话仍在 dsh 里，可以再恢复回来。
                // 权限预设属于会话，换了会话就要重新读取。
                self.reset_domain_session();
                self.state.view.permission = Default::default();
                self.state.view.messages.clear();
                self.state.view.message_replacements.clear();
                self.state.view.todos.clear();
                self.steer = None;
                self.backend_command = None;
                self.state.view.commands.clear();
                self.state.view.commands_loaded = false;
                self.state.tool_lines.clear();
                self.state.view.history_truncated = false;
                self.state.view.thread_id = Some(thread);
                self.state.view.turn_id = None;
                self.state.view.last_error = None;
                if self.config.domain_setup.is_none() {
                    self.state.view.phase = SessionPhase::Ready;
                }
                // 消息显示出来之前仍算恢复中，新一轮排在其后。
                Some(RpcKind::ThreadHistory)
            }
            RpcKind::ThreadHistory => {
                self.state.view.resuming = None;
                let Some(target) = self.resuming.take() else {
                    return;
                };
                let label = Self::resume_label(&target);
                let thread = self.state.view.thread_id.clone().unwrap_or_default();
                match crate::conversations::parse_history(&result) {
                    Some(history) => {
                        self.state.view.history_truncated = history.omitted > 0;
                        self.state.view.messages = history
                            .messages
                            .into_iter()
                            .enumerate()
                            .map(|(index, message)| crate::state::ConversationItem {
                                id: message.id.unwrap_or_else(|| format!("history-{index}")),
                                thread_id: thread.clone(),
                                turn_id: message.turn_id.unwrap_or_default(),
                                role: match message.role.as_str() {
                                    "user" => "You",
                                    "reasoning" => "Reasoning",
                                    "tool" => "Tool",
                                    _ => "Agent",
                                }
                                .into(),
                                text: message.text,
                                complete: true,
                                truncated: message.truncated,
                                tools: message.tool.into_iter().collect(),
                            })
                            .collect();
                        self.state.view.notice = Some(format!("Resumed: {label}"));
                    }
                    None => {
                        self.state.view.notice = Some(format!(
                            "Resumed: {label}. Its earlier messages could not be read."
                        ))
                    }
                }
                if self.config.domain_setup.is_some() {
                    self.confirm_configured_permission();
                }
                Some(RpcKind::ModelList)
            }
            RpcKind::BackendReload => {
                self.reset_domain_session();
                self.state.view.models.reloading = false;
                let resumed = match result.pointer("/thread/id").and_then(Value::as_str) {
                    Some(id) if self.state.view.thread_id.as_deref() != Some(id) => {
                        // dsh 无法恢复原会话：新会话没有之前的对话上下文。
                        self.state.view.thread_id = Some(id.into());
                        false
                    }
                    _ => true,
                };
                if let Some(model) = result.get("model").and_then(Value::as_str) {
                    self.state.view.model = Some(model.into());
                }
                self.state.view.notice = Some(if resumed {
                    "dsh configuration reloaded; the conversation continues.".into()
                } else {
                    "dsh configuration reloaded in a new session; the agent no longer has the earlier conversation.".into()
                });
                if self.config.domain_setup.is_some() {
                    self.confirm_configured_permission();
                }
                Some(RpcKind::ModelList)
            }
            RpcKind::ModelSelect => {
                let requested = self.state.view.models.switching.take();
                let confirmed = result
                    .get("model")
                    .and_then(Value::as_str)
                    .filter(|model| {
                        !model.is_empty() && model.len() <= crate::models::MAX_MODEL_BYTES
                    })
                    .map(str::to_owned)
                    .or(requested);
                if let Some(model) = confirmed {
                    self.state.view.notice = Some(format!(
                        "Model switched to {model}.{}",
                        self.remember_model(&model)
                    ));
                    self.state.view.model = Some(model);
                }
                None
            }
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
        if let Some(setup) = self.config.domain_setup.as_ref() {
            match crate::domain::receipt::from_tool(setup, notice, &params["item"]) {
                Ok(Some(receipt)) => self.queue_domain_fact(receipt),
                Err(reason) => self.domain.synchronization_failed("transport", 0, reason),
                Ok(None) => self.domain.observe(notice, &params["item"]),
            }
        }
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
            self.state.tool_line(
                &notice.thread_id,
                &notice.turn_id,
                crate::tool_details::ConversationTool::from_detail(&detail),
                line,
            );
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
        let request_id = self.next_rpc_id();
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
        self.expect_response(
            request_id,
            outbox_id,
            PendingRpc {
                identity_thread: Some(id.into()),
                ..PendingRpc::new(RpcKind::AgentRead)
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

    /// 轮次终结后清理其工具详情、文件预览和未决请求。
    fn retire_turn_evidence(&mut self, thread: &str, turn: &str) {
        self.state.finish_tool_lines(thread, turn);
        self.state.finish_reasoning(thread, turn);
        let session = &self.state.view.observation.session_id;
        self.tool_details.retire(session, thread, turn);
        self.file_previews.retire(thread, turn);
        self.state
            .view
            .requests
            .retain(|request| request.thread_id != thread || request.turn_id != turn);
    }

    fn observe_output(
        &mut self,
        thread: &str,
        turn: &str,
        item: &str,
        bytes: usize,
        finalized: bool,
    ) {
        if let Err(error) =
            self.observer
                .output(thread, turn, item, bytes, finalized, Instant::now())
        {
            self.state.error(SessionPhase::Unknown, error.to_string());
        }
    }

    fn register_task_child(&mut self, thread: &str, parent: Option<TaskAttempt>, title: &str) {
        if let Err(error) = self.scheduler.register_child(thread, parent, title) {
            self.state.error(SessionPhase::Unknown, error.to_string());
        }
    }

    fn notification(&mut self, method: &str, params: Value) {
        if method == "model/listChanged" {
            // dsh 的可选模型变了；已经看过清单的界面跟着刷新。
            if self.state.view.models.availability != crate::models::ModelsAvailability::NotQueried
            {
                self.list_models();
            }
            return;
        }
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
                if method == "item/completed" {
                    if let (Some(id), Some(text)) = (
                        params["item"]["id"].as_str(),
                        protocol::decode_reasoning_text(&params["item"]),
                    ) {
                        self.state.reasoning(thread, turn, id, &text, true);
                    }
                }
                if let Some(notice) = protocol::decode_observed_tool(method, &params) {
                    match notice {
                        Ok(notice) => match self.observer.tool(&notice, Instant::now()) {
                            Ok(true) => {
                                if Some(thread) == self.state.view.thread_id.as_deref() {
                                    self.state.turn_tool(
                                        turn,
                                        &notice.item_id,
                                        notice.outcome.is_none(),
                                        Instant::now(),
                                    );
                                }
                                self.observe_tool_detail_event(&notice, &params)
                            }
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
                                let previous = params["replaces"]
                                    .as_array()
                                    .filter(|items| items.len() <= 64)
                                    .and_then(|items| {
                                        items
                                            .iter()
                                            .map(|id| id.as_str().map(str::to_owned))
                                            .collect::<Option<Vec<_>>>()
                                    })
                                    .unwrap_or_default();
                                if !output.text.is_empty()
                                    && Some(thread) == self.state.view.thread_id.as_deref()
                                {
                                    self.state.replace_preview(
                                        turn,
                                        &output.item_id,
                                        "Reasoning",
                                        &previous,
                                    );
                                }
                                if self.state.reasoning(
                                    thread,
                                    turn,
                                    &output.item_id,
                                    &output.text,
                                    false,
                                ) {
                                    if Some(thread) == self.state.view.thread_id.as_deref() {
                                        self.state.turn_stage(
                                            turn,
                                            crate::state::TurnStage::Thinking,
                                            Instant::now(),
                                        );
                                    }
                                    self.observe_output(
                                        thread,
                                        turn,
                                        &output.item_id,
                                        output.bytes,
                                        false,
                                    );
                                }
                            }
                            protocol::ObservedOutputKind::Tool(category) => {
                                if let Some(locator) =
                                    self.tool_locator(thread, turn, &output.item_id)
                                {
                                    self.tool_details.observe_output(&locator, &output.text);
                                    if let Some(detail) = self.tool_details.get(&locator) {
                                        self.state.tool_line(
                                            thread,
                                            turn,
                                            crate::tool_details::ConversationTool::from_detail(
                                                &detail,
                                            ),
                                            crate::tool_details::summary_line(&detail),
                                        );
                                    }
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
                                self.observe_output(thread, turn, id, text.len(), false);
                            }
                        }
                    } else if method == "item/completed" && params["item"]["type"] == "agentMessage"
                    {
                        if let (Some(id), Some(text)) = (
                            params["item"]["id"].as_str(),
                            params["item"]["text"].as_str(),
                        ) {
                            if self.state.child_message(thread, turn, id, text, true) {
                                self.observe_output(thread, turn, id, text.len(), true);
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
                                            self.retire_turn_evidence(thread, &old_turn);
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
                                self.retire_turn_evidence(thread, turn);
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
                if let Some(thread) = self.state.view.thread_id.clone() {
                    self.retire_turn_evidence(&thread, id.unwrap());
                }
                let completed_at = (self.state.view.phase == SessionPhase::Completed)
                    .then(|| crate::state::wall_clock_label(std::time::SystemTime::now()))
                    .flatten();
                self.state.finish_turn_timing(Instant::now(), completed_at);
                // 一轮里 Agent 可能切换了分支；轮次结束时重读一次。
                self.state.view.branch = crate::git_branch::current(&self.config.cwd);
                if let Some(steer) = self.steer.take() {
                    self.state.steer_notice(
                        &steer.notice_id,
                        "Steering was not delivered: the turn ended first.".into(),
                        true,
                    );
                }
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
            "turn/step" => {
                if let (Some(turn), Some(step)) = (
                    turn.filter(|turn| {
                        Some(*turn) == self.state.view.turn_id.as_deref()
                            && matches!(
                                self.state.view.phase,
                                SessionPhase::Running | SessionPhase::GatePending
                            )
                    }),
                    params["step"]
                        .as_u64()
                        .filter(|step| *step <= 9_007_199_254_740_991),
                ) {
                    match params["state"].as_str() {
                        Some("start") => self.state.begin_step_timing(turn, step, Instant::now()),
                        Some("end") => self.state.end_step_timing(
                            turn,
                            step,
                            Instant::now(),
                            crate::state::wall_clock_label(std::time::SystemTime::now()),
                        ),
                        _ => {}
                    }
                }
            }
            "turn/stage" => {
                let stage = match params["stage"].as_str() {
                    Some("waiting") => crate::state::TurnStage::ModelWait,
                    Some("thinking") => crate::state::TurnStage::Thinking,
                    Some("response") => crate::state::TurnStage::Response,
                    _ => return,
                };
                if let Some(turn) = turn.filter(|turn| {
                    Some(*turn) == self.state.view.turn_id.as_deref()
                        && matches!(
                            self.state.view.phase,
                            SessionPhase::Running | SessionPhase::GatePending
                        )
                }) {
                    if params.get("step").is_some_and(|step| {
                        !step
                            .as_u64()
                            .is_some_and(|step| self.state.timing_step_matches(turn, step))
                    }) {
                        return;
                    }
                    self.state.turn_stream_stage(turn, stage, Instant::now());
                }
            }
            "item/discarded" => {
                if let (Some(turn), Some(id)) = (turn, params["itemId"].as_str()) {
                    self.state.discard_item(turn, id);
                }
            }
            "turn/steer/delivered" => {
                let current = turn.is_some() && turn == self.state.view.turn_id.as_deref();
                let known = self.steer.as_ref().is_some_and(|steer| {
                    // 投递事件可能先于应答到达；同一时刻只有一个待确认的引导。
                    steer
                        .message_id
                        .as_deref()
                        .is_none_or(|id| params["id"] == id)
                });
                if current && known {
                    let steer = self.steer.take().unwrap();
                    self.state.steer_delivered(&steer.notice_id, &steer.text);
                }
            }
            "turn/notice" => match params["kind"].as_str() {
                Some("retry") => {
                    if let (Some(turn), Some(retry)) = (turn, params["retry"].as_u64()) {
                        self.state.retry_notice(
                            turn,
                            retry,
                            params["maxRetries"].as_u64(),
                            params["delayMs"].as_u64(),
                            params["code"].as_str(),
                        );
                    }
                }
                Some("compaction") => {
                    if let (Some(id), Some(state)) =
                        (params["id"].as_str(), params["state"].as_str())
                    {
                        self.state.compaction_notice(turn, id, state);
                    }
                }
                Some("goal") => {
                    if let (Some(turn), Some(round)) = (turn, params["round"].as_u64()) {
                        self.state
                            .goal_round_notice(turn, round, params["maxRounds"].as_u64());
                    }
                }
                _ => {}
            },
            "thread/todos/updated" => {
                self.state.view.todos = crate::state::parse_todos(&params["todos"]);
            }
            "item/agentMessage/delta" => {
                if let (Some(turn), Some(id), Some(delta)) =
                    (turn, params["itemId"].as_str(), params["delta"].as_str())
                {
                    let previous = params["replaces"]
                        .as_array()
                        .filter(|items| items.len() <= 64)
                        .and_then(|items| {
                            items
                                .iter()
                                .map(|id| id.as_str().map(str::to_owned))
                                .collect::<Option<Vec<_>>>()
                        })
                        .unwrap_or_default();
                    if !delta.is_empty() {
                        self.state.replace_preview(turn, id, "Agent", &previous);
                    }
                    if self.state.message(turn, id, delta, false) {
                        self.state.turn_stage(
                            turn,
                            crate::state::TurnStage::Response,
                            Instant::now(),
                        );
                        let thread = self.state.view.thread_id.clone().unwrap_or_default();
                        self.observe_output(&thread, turn, id, delta.len(), false);
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
                            let thread = self.state.view.thread_id.clone().unwrap_or_default();
                            self.observe_output(&thread, turn, id, text.len(), true);
                        }
                    }
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
    mod domain;
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
        journal_harness_with(fixture, false).await
    }

    async fn journal_harness_with(
        fixture: &JournalFixture,
        strict_outcome: bool,
    ) -> (
        ClientHandle,
        BufReader<tokio::io::DuplexStream>,
        Config,
        String,
    ) {
        let config = Config {
            journal: fixture.settings(),
            strict_outcome,
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
        // 通知没有响应，写出后即确认，恢复报告不应要求检查它的外部结果。
        let initialized = sent
            .records()
            .find(|record| record.intent.method == "initialized")
            .unwrap();
        assert_eq!(initialized.status, crate::outbox::OutboxStatus::Confirmed);

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
    async fn session_outcome_is_strict_for_workflows_and_last_turn_for_interactive_use() {
        for (strict, expected) in [
            (true, SessionPhase::Failed),
            (false, SessionPhase::Completed),
        ] {
            session_outcome_after_failed_then_completed(strict, expected).await;
        }
    }

    async fn session_outcome_after_failed_then_completed(strict: bool, expected: SessionPhase) {
        use crate::scheduler::TaskState;
        let fixture = JournalFixture::new();
        let (mut client, mut server, config, session) =
            journal_harness_with(&fixture, strict).await;
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
        assert_eq!(
            replay.info.execution_result,
            Some(expected),
            "strict={strict}"
        );
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
            dsh_executable: fixture.root.join("must-never-launch"),
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
        let runner = tokio::spawn(crate::headless::run(client, Vec::new(), Some(output)));
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
        let runner = tokio::spawn(crate::headless::run(client, Vec::new(), Some(output)));
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
    async fn dag_dispatch_waits_for_the_session_pause_and_dependencies_and_uses_new_retry_attempts()
    {
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
        initialized(&mut server).await;
        client
            .snapshots
            .wait_for(|s| s.scheduler.tasks.len() == 2)
            .await
            .unwrap();
        phase(&mut client, SessionPhase::Ready).await;
        assert_eq!(client.snapshots.borrow().root_start_requests, 0);
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

    async fn harness() -> (ClientHandle, BufReader<tokio::io::DuplexStream>) {
        harness_with_config(Config::default()).await
    }

    async fn harness_with_config(
        config: Config,
    ) -> (ClientHandle, BufReader<tokio::io::DuplexStream>) {
        let (client, server) = tokio::io::duplex(65536);
        let (read, write) = tokio::io::split(client);
        (
            ClientHandle::start(PipeTransport::new(read, write), config, None, None),
            BufReader::new(server),
        )
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
    /// Answers the first skills inventory the way a backend without one does.
    async fn skip_skills(server: &mut BufReader<tokio::io::DuplexStream>) {
        let skills = next(server).await;
        assert_eq!(skills["method"], "skills/list");
        send(
            server,
            json!({"id":skills["id"],"error":{"code":-32601,"message":"skills/list is unavailable"}}),
        )
        .await;
    }

    /// Brings a client to Ready and answers its first skills inventory.
    async fn ready(server: &mut BufReader<tokio::io::DuplexStream>) {
        initialized(server).await;
        let skills = next(server).await;
        assert_eq!(skills["method"], "skills/list");
        send(server, json!({"id":skills["id"],"result":{"data":[{"cwd":skills["params"]["cwds"][0],"errors":[],"skills":[]}]}})).await;
    }

    #[tokio::test]
    async fn new_conversation_waits_for_confirmation_then_dispatches_to_the_new_thread() {
        let (mut client, mut server) = harness().await;
        ready(&mut server).await;
        phase(&mut client, SessionPhase::Ready).await;
        client
            .commands
            .send(Command::NewConversation)
            .await
            .unwrap();
        let create = next(&mut server).await;
        assert_eq!(create["method"], "thread/new");
        client
            .snapshots
            .wait_for(|s| s.creating_conversation)
            .await
            .unwrap();
        assert_eq!(client.snapshots.borrow().thread_id.as_deref(), Some("root"));
        // A duplicate and an input while creating cannot start a second session or a turn.
        client
            .commands
            .send(Command::NewConversation)
            .await
            .unwrap();
        client
            .commands
            .send(Command::SubmitRootInput {
                text: "first message".into(),
            })
            .await
            .unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(50), next(&mut server))
                .await
                .is_err()
        );
        send(
            &mut server,
            json!({"id":create["id"],"result":{
            "thread":{"id":"fresh"},"model":"test-model"}}),
        )
        .await;
        let mut turn = None;
        for _ in 0..3 {
            let request = next(&mut server).await;
            match request["method"].as_str().unwrap() {
                "permission/describe" => {
                    send(&mut server, json!({"id":request["id"],"result":{}})).await
                }
                "skills/list" => {
                    send(
                        &mut server,
                        json!({"id":request["id"],"error":{"code":-32601,"message":"unsupported"}}),
                    )
                    .await
                }
                "turn/start" => {
                    turn = Some(request);
                    break;
                }
                other => panic!("unexpected request: {other}"),
            }
        }
        let turn = turn.unwrap();
        assert_eq!(turn["params"]["threadId"], "fresh");
        assert_eq!(turn["params"]["input"][0]["text"], "first message");
        assert!(!client.snapshots.borrow().creating_conversation);
        // Creating a new conversation during a live turn is rejected by Core.
        client
            .commands
            .send(Command::NewConversation)
            .await
            .unwrap();
        client
            .snapshots
            .wait_for(|s| {
                s.notice
                    .as_deref()
                    .is_some_and(|s| s.contains("wait for the current turn"))
            })
            .await
            .unwrap();
        client.commands.send(Command::Quit).await.unwrap();
        client.join.await.unwrap();
    }

    #[tokio::test]
    async fn a_new_conversation_with_an_unavailable_session_cannot_dispatch_into_the_old_thread() {
        let (mut client, mut server) = harness().await;
        ready(&mut server).await;
        phase(&mut client, SessionPhase::Ready).await;
        client
            .commands
            .send(Command::NewConversation)
            .await
            .unwrap();
        let create = next(&mut server).await;
        send(
            &mut server,
            json!({"id":create["id"],"error":{
            "code":-32000,"message":"model restore refused after session change",
            "data":{"sessionUnavailable":true}}}),
        )
        .await;
        phase(&mut client, SessionPhase::Failed).await;
        assert!(!client.snapshots.borrow().creating_conversation);
        assert!(client.snapshots.borrow().startup_blocked);
        client
            .commands
            .send(Command::SubmitRootInput {
                text: "do not send".into(),
            })
            .await
            .unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(50), next(&mut server))
                .await
                .is_err()
        );
        client.commands.send(Command::Quit).await.unwrap();
        client.join.await.unwrap();
    }

    #[tokio::test]
    async fn a_refused_new_conversation_keeps_the_current_thread_available() {
        let (mut client, mut server) = harness().await;
        ready(&mut server).await;
        phase(&mut client, SessionPhase::Ready).await;
        client
            .commands
            .send(Command::NewConversation)
            .await
            .unwrap();
        let create = next(&mut server).await;
        send(
            &mut server,
            json!({"id":create["id"],"error":{
            "code":-32000,"message":"cannot create session"}}),
        )
        .await;
        client
            .snapshots
            .wait_for(|s| {
                !s.creating_conversation
                    && s.notice
                        .as_deref()
                        .is_some_and(|n| n.contains("cannot create session"))
            })
            .await
            .unwrap();
        assert_eq!(client.snapshots.borrow().thread_id.as_deref(), Some("root"));
        assert_eq!(
            client.snapshots.borrow().model.as_deref(),
            Some("test-model")
        );
        assert_eq!(client.snapshots.borrow().phase, SessionPhase::Ready);
        client
            .commands
            .send(Command::SubmitRootInput {
                text: "continue old session".into(),
            })
            .await
            .unwrap();
        let turn = next(&mut server).await;
        assert_eq!(turn["method"], "turn/start");
        assert_eq!(turn["params"]["threadId"], "root");
        client.commands.send(Command::Quit).await.unwrap();
        client.join.await.unwrap();
    }

    #[tokio::test]
    async fn model_switch_is_confirmed_before_the_next_turn_starts() {
        let (mut client, mut server) = harness_with_config(Config::default()).await;
        let request = next(&mut server).await;
        assert_eq!(request["method"], "initialize");
        send(&mut server, json!({"id":request["id"],"result":{}})).await;
        assert_eq!(next(&mut server).await["method"], "initialized");
        let request = next(&mut server).await;
        assert_eq!(request["method"], "thread/start");
        send(
            &mut server,
            json!({"id":request["id"],"result":{"thread":{"id":"s"},"model":"official/flash"}}),
        )
        .await;
        phase(&mut client, SessionPhase::Ready).await;
        skip_skills(&mut server).await;

        let select = |model: &str| Command::SelectModel {
            model: model.into(),
        };
        client.commands.send(select("missing")).await.unwrap();
        let request = next(&mut server).await;
        assert_eq!(request["method"], "model/select");
        assert_eq!(request["params"]["model"], "missing");
        send(
            &mut server,
            json!({"id":request["id"],"error":{"code":-32602,"message":"not offered"}}),
        )
        .await;
        client
            .snapshots
            .wait_for(|s| {
                s.models.switching.is_none()
                    && s.notice
                        .as_deref()
                        .is_some_and(|notice| notice.contains("not offered"))
            })
            .await
            .unwrap();
        {
            let snapshot = client.snapshots.borrow();
            assert_eq!(snapshot.phase, SessionPhase::Ready);
            assert_eq!(snapshot.model.as_deref(), Some("official/flash"));
            assert!(snapshot.last_error.is_none());
        }

        client.commands.send(select("hi/gpt-6-luna")).await.unwrap();
        let request = next(&mut server).await;
        assert_eq!(request["method"], "model/select");
        client
            .commands
            .send(Command::SubmitRootInput {
                text: "hello".into(),
            })
            .await
            .unwrap();
        // 确认前不能开始新一轮。
        assert!(
            tokio::time::timeout(Duration::from_millis(50), next(&mut server))
                .await
                .is_err()
        );
        send(
            &mut server,
            json!({"id":request["id"],"result":{"thread":{"id":"s"},"model":"hi/gpt-6-luna"}}),
        )
        .await;
        let start = next(&mut server).await;
        assert_eq!(start["method"], "turn/start");
        assert!(start["params"].get("model").is_none());
        assert_eq!(
            client.snapshots.borrow().model.as_deref(),
            Some("hi/gpt-6-luna")
        );
        client.commands.send(Command::Quit).await.unwrap();
        client.join.await.unwrap();
    }

    #[tokio::test]
    async fn sessions_are_listed_from_the_backend_and_a_resume_shows_its_earlier_messages() {
        let (mut client, mut server) = harness_with_config(Config::default()).await;
        let request = next(&mut server).await;
        send(&mut server, json!({"id":request["id"],"result":{}})).await;
        assert_eq!(next(&mut server).await["method"], "initialized");
        let request = next(&mut server).await;
        send(
            &mut server,
            json!({"id":request["id"],"result":{"thread":{"id":"thread-two"},"model":"hi/luna"}}),
        )
        .await;
        phase(&mut client, SessionPhase::Ready).await;
        skip_skills(&mut server).await;
        let submit = |text: &str| Command::SubmitRootInput { text: text.into() };

        // 列表来自后端；重复的请求在答复前不再发出。
        for _ in 0..2 {
            client
                .commands
                .send(Command::ListConversations)
                .await
                .unwrap();
        }
        let list = next(&mut server).await;
        assert_eq!(list["method"], "thread/list");
        assert_eq!(
            list["params"]["cwd"],
            Config::default().cwd.display().to_string()
        );
        send(
            &mut server,
            json!({"id":list["id"],"result":{"sessions":[
                {"id":"thread-two","updatedAt":30,"live":true},
                {"id":"thread-one","title":"fix the parser","updatedAt":20},
            ]}}),
        )
        .await;
        client
            .snapshots
            .wait_for(|s| s.conversations.len() == 2)
            .await
            .unwrap();
        assert_eq!(
            client.snapshots.borrow().conversations[1].title,
            "fix the parser"
        );

        // 当前会话不需要恢复。
        client
            .commands
            .send(Command::ResumeConversation {
                id: "thread-two".into(),
            })
            .await
            .unwrap();
        client
            .snapshots
            .wait_for(|s| {
                s.notice
                    .as_deref()
                    .is_some_and(|notice| notice.contains("already open here"))
            })
            .await
            .unwrap();

        client
            .commands
            .send(Command::ResumeConversation {
                id: "thread-one".into(),
            })
            .await
            .unwrap();
        let resume = next(&mut server).await;
        assert_eq!(resume["method"], "thread/resume");
        assert_eq!(resume["params"]["threadId"], "thread-one");
        assert_eq!(
            client.snapshots.borrow().resuming.as_deref(),
            Some("fix the parser")
        );
        // 确认并显示出之前的消息之前，不开始新一轮。
        client
            .commands
            .send(submit("second question"))
            .await
            .unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(50), next(&mut server))
                .await
                .is_err()
        );
        send(
            &mut server,
            json!({"id":resume["id"],"result":{"thread":{"id":"thread-one"},"model":"hi/luna"}}),
        )
        .await;
        let history = next(&mut server).await;
        assert_eq!(history["method"], "thread/history");
        assert_eq!(history["params"]["threadId"], "thread-one");
        assert!(
            tokio::time::timeout(Duration::from_millis(50), next(&mut server))
                .await
                .is_err()
        );
        send(
            &mut server,
            json!({"id":history["id"],"result":{"omitted":0,"messages":[
                {"role":"user","text":"fix the parser\nmore detail","truncated":false},
                {"role":"reasoning","id":"shared","turnId":"saved-turn","text":"Inspect the parser","truncated":false},
                {"role":"tool","id":"call-1","turnId":"saved-turn","text":"▸ done · cargo check","truncated":false},
                {"role":"assistant","id":"shared","turnId":"saved-turn","text":"done","truncated":false},
            ]}}),
        )
        .await;
        let mut started = None;
        for _ in 0..2 {
            let request = next(&mut server).await;
            if request["method"] == "model/list" {
                send(
                    &mut server,
                    json!({"id":request["id"],"result":{"data":[]}}),
                )
                .await;
            } else {
                started = Some(request);
            }
        }
        let started = started.expect("the queued turn starts after the resume");
        assert_eq!(started["method"], "turn/start");
        assert_eq!(started["params"]["threadId"], "thread-one");
        {
            let snapshot = client.snapshots.borrow();
            assert_eq!(snapshot.thread_id.as_deref(), Some("thread-one"));
            assert!(snapshot.resuming.is_none());
            let shown: Vec<(&str, &str)> = snapshot
                .messages
                .iter()
                .map(|m| (m.role.as_str(), m.text.as_str()))
                .collect();
            assert_eq!(
                shown,
                [
                    ("You", "fix the parser\nmore detail"),
                    ("Reasoning", "Inspect the parser"),
                    ("Tool", "▸ done · cargo check"),
                    ("Agent", "done"),
                    ("You", "second question")
                ]
            );
            assert_eq!(snapshot.messages[1].id, "shared");
            assert_eq!(snapshot.messages[2].id, "call-1");
            assert_eq!(snapshot.messages[2].turn_id, "saved-turn");
            assert!(snapshot
                .messages
                .iter()
                .all(|m| m.thread_id == "thread-one"));
        }
        client.commands.send(Command::Quit).await.unwrap();
        client.join.await.unwrap();
    }

    #[tokio::test]
    async fn a_resume_whose_messages_cannot_be_loaded_still_switches_the_session() {
        let (mut client, mut server) = harness().await;
        ready(&mut server).await;
        phase(&mut client, SessionPhase::Ready).await;
        client
            .commands
            .send(Command::ResumeConversation { id: "other".into() })
            .await
            .unwrap();
        let resume = next(&mut server).await;
        assert_eq!(resume["method"], "thread/resume");
        send(
            &mut server,
            json!({"id":resume["id"],"result":{"thread":{"id":"other"}}}),
        )
        .await;
        let history = next(&mut server).await;
        assert_eq!(history["method"], "thread/history");
        send(
            &mut server,
            json!({"id":history["id"],"error":{"code":-32603,"message":"no query service"}}),
        )
        .await;
        client
            .snapshots
            .wait_for(|s| {
                s.resuming.is_none()
                    && s.notice
                        .as_deref()
                        .is_some_and(|notice| notice.contains("could not be loaded"))
            })
            .await
            .unwrap();
        {
            let snapshot = client.snapshots.borrow();
            assert_eq!(snapshot.thread_id.as_deref(), Some("other"));
            assert_eq!(snapshot.phase, SessionPhase::Ready);
        }
        // 列表失败只是提示，会话照常可用。
        client
            .commands
            .send(Command::ListConversations)
            .await
            .unwrap();
        let list = next(&mut server).await;
        assert_eq!(list["method"], "thread/list");
        send(
            &mut server,
            json!({"id":list["id"],"error":{"code":-32601,"message":"unknown method"}}),
        )
        .await;
        client
            .snapshots
            .wait_for(|s| {
                s.notice
                    .as_deref()
                    .is_some_and(|notice| notice.contains("Sessions were not listed"))
            })
            .await
            .unwrap();
        assert_eq!(client.snapshots.borrow().phase, SessionPhase::Ready);
        client.commands.send(Command::Quit).await.unwrap();
        client.join.await.unwrap();
    }

    #[tokio::test]
    async fn the_permission_preset_is_read_from_dsh_and_changes_only_when_dsh_confirms() {
        let (mut client, mut server) = harness().await;
        ready(&mut server).await;
        phase(&mut client, SessionPhase::Ready).await;
        let presets = json!([
            {"value":"read-only","name":"read-only","description":""},
            {"value":"workspace-write","name":"workspace-write","description":"Write inside the workspace."},
        ]);
        assert!(client.snapshots.borrow().permission.current.is_empty());

        for _ in 0..2 {
            client
                .commands
                .send(Command::DescribePermission)
                .await
                .unwrap();
        }
        let describe = next(&mut server).await;
        assert_eq!(describe["method"], "permission/describe");
        send(
            &mut server,
            json!({"id":describe["id"],"result":{"current":"workspace-write","default":"workspace-write","presets":presets}}),
        )
        .await;
        client
            .snapshots
            .wait_for(|s| s.permission.current == "workspace-write")
            .await
            .unwrap();
        assert_eq!(client.snapshots.borrow().permission.presets.len(), 2);

        let set = |preset: &str| Command::SetPermission {
            preset: preset.into(),
        };
        client.commands.send(set("read-only")).await.unwrap();
        let request = next(&mut server).await;
        // 重复的读取请求没有发出：下一条就是切换。
        assert_eq!(request["method"], "permission/set");
        assert_eq!(request["params"]["preset"], "read-only");
        assert_eq!(request["params"]["remember"], true);
        client
            .snapshots
            .wait_for(|s| s.permission.switching.as_deref() == Some("read-only"))
            .await
            .unwrap();
        // 确认之前显示的仍是原来的预设，第二次切换不发出。
        assert_eq!(
            client.snapshots.borrow().permission.current,
            "workspace-write"
        );
        client.commands.send(set("workspace-write")).await.unwrap();
        client
            .snapshots
            .wait_for(|s| {
                s.notice
                    .as_deref()
                    .is_some_and(|text| text.contains("another change is still waiting"))
            })
            .await
            .unwrap();
        send(
            &mut server,
            json!({"id":request["id"],"result":{"current":"read-only","default":"read-only","remembered":true,"presets":presets}}),
        )
        .await;
        client
            .snapshots
            .wait_for(|s| s.permission.current == "read-only" && s.permission.switching.is_none())
            .await
            .unwrap();
        assert_eq!(
            client.snapshots.borrow().notice.as_deref(),
            Some("Permission preset is now read-only.")
        );

        // dsh 拒绝：预设不变，会话照常可用。
        client.commands.send(set("no-such")).await.unwrap();
        let request = next(&mut server).await;
        send(
            &mut server,
            json!({"id":request["id"],"error":{"code":-32603,"message":"permission: unknown preset \"no-such\""}}),
        )
        .await;
        client
            .snapshots
            .wait_for(|s| {
                s.permission.switching.is_none()
                    && s.notice
                        .as_deref()
                        .is_some_and(|text| text.starts_with("Permission preset was not changed"))
            })
            .await
            .unwrap();
        {
            let snapshot = client.snapshots.borrow();
            assert_eq!(snapshot.permission.current, "read-only");
            assert_eq!(snapshot.phase, SessionPhase::Ready);
        }
        client.commands.send(Command::Quit).await.unwrap();
        client.join.await.unwrap();
    }

    #[tokio::test]
    async fn a_startup_permission_is_confirmed_before_any_task_and_a_refusal_starts_none() {
        // 启动到 dsh 给出会话；随后应当先要求预设。
        async fn started(
            preset: &str,
        ) -> (ClientHandle, BufReader<tokio::io::DuplexStream>, Value) {
            let (client, mut server) = harness_with_config(Config {
                permission: Some(preset.into()),
                ..Default::default()
            })
            .await;
            client
                .commands
                .send(Command::SubmitRootInput {
                    text: "queued before ready".into(),
                })
                .await
                .unwrap();
            initialized(&mut server).await;
            let set = next(&mut server).await;
            assert_eq!(set["method"], "permission/set");
            assert_eq!(set["params"]["preset"], preset);
            // 只改这个会话，不改 dsh 的默认值。
            assert_eq!(set["params"]["remember"], false);
            (client, server, set)
        }

        let (client, mut server, set) = started("read-only").await;
        // 确认之前既不就绪，也不发出任务。
        assert!(
            tokio::time::timeout(Duration::from_millis(50), next(&mut server))
                .await
                .is_err()
        );
        assert_ne!(client.snapshots.borrow().phase, SessionPhase::Ready);
        send(
            &mut server,
            json!({"id":set["id"],"result":{"current":"read-only","default":"workspace-write","presets":[{"value":"read-only"}]}}),
        )
        .await;
        let mut methods = Vec::new();
        for _ in 0..2 {
            let request = next(&mut server).await;
            methods.push(request["method"].as_str().unwrap().to_owned());
            if request["method"] == "skills/list" {
                send(
                    &mut server,
                    json!({"id":request["id"],"error":{"code":-32601,"message":"unavailable"}}),
                )
                .await;
            }
        }
        methods.sort();
        assert_eq!(methods, ["skills/list", "turn/start"]);
        assert_eq!(client.snapshots.borrow().permission.current, "read-only");
        client.commands.send(Command::Quit).await.unwrap();
        client.join.await.unwrap();

        // dsh 拒绝，或确认的不是所要求的预设：会话失败，任务一个也不发。
        for answer in [
            json!({"error":{"code":-32603,"message":"permission: unknown preset \"strict\""}}),
            json!({"result":{"current":"workspace-write","presets":[]}}),
        ] {
            let (mut client, mut server, set) = started("strict").await;
            let mut answer = answer;
            answer["id"] = set["id"].clone();
            send(&mut server, answer).await;
            phase(&mut client, SessionPhase::Failed).await;
            assert!(
                tokio::time::timeout(Duration::from_millis(50), next(&mut server))
                    .await
                    .is_err()
            );
            let error = client.snapshots.borrow().last_error.clone().unwrap();
            assert!(error.contains("--permission"), "{error}");
            client.commands.send(Command::Quit).await.unwrap();
            client.join.await.unwrap();
        }
    }

    #[tokio::test]
    async fn a_provider_is_saved_by_dsh_and_the_lists_refresh_without_a_restart() {
        use crate::backend::dsh_config::{ProviderSpec, Secret};
        let (mut client, mut server) = harness().await;
        ready(&mut server).await;
        phase(&mut client, SessionPhase::Ready).await;
        let spec = ProviderSpec {
            provider: "my-api".into(),
            base_url: "https://api.example.com/v1".into(),
            api: "openai-completions".into(),
            api_key: Secret("sk-test-1".into()),
            model: "big".into(),
            ..Default::default()
        };
        let notice = |client: &ClientHandle| client.snapshots.borrow().notice.clone();

        // 表单校验不过的内容不会发给 dsh。
        client
            .commands
            .send(Command::AddModelProvider {
                spec: ProviderSpec {
                    provider: "Bad Name".into(),
                    ..spec.clone()
                },
                test: false,
            })
            .await
            .unwrap();
        client
            .snapshots
            .wait_for(|s| {
                s.notice
                    .as_deref()
                    .is_some_and(|text| text.starts_with("Provider was not saved: Provider name"))
            })
            .await
            .unwrap();

        client
            .commands
            .send(Command::AddModelProvider {
                spec: spec.clone(),
                test: false,
            })
            .await
            .unwrap();
        let save = next(&mut server).await;
        assert_eq!(save["method"], "provider/save");
        assert_eq!(save["params"]["provider"], "my-api");
        assert_eq!(save["params"]["credential"], "MY_API_API_KEY");
        assert_eq!(save["params"]["apiKey"], "sk-test-1");
        // 答复之前的第二次保存不发出。
        client
            .commands
            .send(Command::AddModelProvider {
                spec: spec.clone(),
                test: false,
            })
            .await
            .unwrap();
        client
            .snapshots
            .wait_for(|s| {
                s.notice
                    .as_deref()
                    .is_some_and(|text| text.contains("another save is still waiting"))
            })
            .await
            .unwrap();
        send(
            &mut server,
            json!({"id":save["id"],"result":{"label":"my-api/big","credential":"MY_API_API_KEY","keyStored":true}}),
        )
        .await;
        // 不重启：直接重新读取模型清单和供应商。
        let models = next(&mut server).await;
        assert_eq!(models["method"], "model/list");
        send(
            &mut server,
            json!({"id":models["id"],"result":{"data":[{"model":"my-api/big","displayName":"big"}]}}),
        )
        .await;
        let providers = next(&mut server).await;
        assert_eq!(providers["method"], "provider/list");
        send(
            &mut server,
            json!({"id":providers["id"],"result":{"providers":[
                {"name":"my-api","baseURL":"https://api.example.com/v1","api":"openai-completions","models":[{"id":"big","contextWindow":1000}]}
            ]}}),
        )
        .await;
        client
            .snapshots
            .wait_for(|s| s.models.providers.len() == 1)
            .await
            .unwrap();
        {
            let snapshot = client.snapshots.borrow();
            assert_eq!(
                snapshot.notice.as_deref(),
                Some("Saved my-api/big to dsh; it is available now.")
            );
            assert_eq!(snapshot.models.options.len(), 1);
            assert_eq!(
                snapshot.models.providers[0].context_windows,
                [("big".to_owned(), 1000)]
            );
            assert_eq!(snapshot.phase, SessionPhase::Ready);
        }
        // 快照和提示里都没有密钥。
        assert!(!format!("{:?}", *client.snapshots.borrow()).contains("sk-test-1"));

        // dsh 推送“可选模型变了”时，看过清单的界面跟着刷新。
        send(
            &mut server,
            json!({"method":"model/listChanged","params":{}}),
        )
        .await;
        let again = next(&mut server).await;
        assert_eq!(again["method"], "model/list");
        send(&mut server, json!({"id":again["id"],"result":{"data":[]}})).await;
        let again = next(&mut server).await;
        assert_eq!(again["method"], "provider/list");
        send(
            &mut server,
            json!({"id":again["id"],"error":{"code":-32603,"message":"no settings provider"}}),
        )
        .await;

        // dsh 拒绝保存：会话不受影响；要求随后测试的，测试以同样的原因失败。
        client
            .commands
            .send(Command::AddModelProvider { spec, test: true })
            .await
            .unwrap();
        let save = next(&mut server).await;
        assert_eq!(save["method"], "provider/save");
        send(
            &mut server,
            json!({"id":save["id"],"error":{"code":-32603,"message":"dsh settings are read-only in this profile"}}),
        )
        .await;
        client
            .snapshots
            .wait_for(|s| {
                s.models.test
                    == crate::models::ModelTest::Failed {
                        model: "my-api/big".into(),
                        message: "not saved: dsh settings are read-only in this profile".into(),
                    }
            })
            .await
            .unwrap();
        assert_eq!(
            notice(&client).as_deref(),
            Some("Provider was not saved: dsh settings are read-only in this profile")
        );
        assert_eq!(client.snapshots.borrow().phase, SessionPhase::Ready);
        client.commands.send(Command::Quit).await.unwrap();
        client.join.await.unwrap();
    }

    #[tokio::test]
    async fn reload_holds_new_turns_until_confirmed_and_then_refreshes_the_model_list() {
        let (mut client, mut server) = harness_with_config(Config::default()).await;
        let request = next(&mut server).await;
        send(&mut server, json!({"id":request["id"],"result":{}})).await;
        assert_eq!(next(&mut server).await["method"], "initialized");
        let request = next(&mut server).await;
        send(
            &mut server,
            json!({"id":request["id"],"result":{"thread":{"id":"s"},"model":"official/flash"}}),
        )
        .await;
        phase(&mut client, SessionPhase::Ready).await;
        skip_skills(&mut server).await;

        client.commands.send(Command::ReloadBackend).await.unwrap();
        let reload = next(&mut server).await;
        assert_eq!(reload["method"], "backend/reload");
        client
            .snapshots
            .wait_for(|s| s.models.reloading)
            .await
            .unwrap();
        client
            .commands
            .send(Command::SubmitRootInput {
                text: "hello".into(),
            })
            .await
            .unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(50), next(&mut server))
                .await
                .is_err()
        );
        // 会话被换成新的时，Core 采用新的会话 id 并说明上下文已重置。
        send(
            &mut server,
            json!({"id":reload["id"],"result":{"thread":{"id":"s2"},"model":"hi/luna"}}),
        )
        .await;
        let mut seen = Vec::new();
        for _ in 0..2 {
            let request = next(&mut server).await;
            if request["method"] == "model/list" {
                send(
                    &mut server,
                    json!({"id":request["id"],"result":{"data":[{"model":"hi/luna","displayName":"Luna"}]}}),
                )
                .await;
            } else {
                assert_eq!(request["params"]["threadId"], "s2");
            }
            seen.push(request["method"].as_str().unwrap().to_owned());
        }
        seen.sort();
        assert_eq!(seen, ["model/list", "turn/start"]);
        let snapshot = client.snapshots.borrow().clone();
        assert!(!snapshot.models.reloading);
        assert_eq!(snapshot.thread_id.as_deref(), Some("s2"));
        assert_eq!(snapshot.model.as_deref(), Some("hi/luna"));
        client.commands.send(Command::Quit).await.unwrap();
        client.join.await.unwrap();
    }

    #[tokio::test]
    async fn a_reload_that_never_started_and_an_unlaunched_session_change_nothing() {
        let (mut client, mut server) = harness_with_config(Config::default()).await;
        let request = next(&mut server).await;
        send(&mut server, json!({"id":request["id"],"result":{}})).await;
        assert_eq!(next(&mut server).await["method"], "initialized");
        let request = next(&mut server).await;
        send(
            &mut server,
            json!({"id":request["id"],"result":{"thread":{"id":"s"},"model":"official/flash"}}),
        )
        .await;
        phase(&mut client, SessionPhase::Ready).await;
        skip_skills(&mut server).await;
        client.commands.send(Command::ReloadBackend).await.unwrap();
        let reload = next(&mut server).await;
        send(
            &mut server,
            json!({"id":reload["id"],"error":{"code":-32000,"message":"dsh was not restarted: missing"}}),
        )
        .await;
        client
            .snapshots
            .wait_for(|s| {
                !s.models.reloading
                    && s.notice
                        .as_deref()
                        .is_some_and(|notice| notice.contains("was not restarted"))
            })
            .await
            .unwrap();
        assert_eq!(client.snapshots.borrow().phase, SessionPhase::Ready);
        // 没有真正启动的会话不会为测试另起进程；迟到的结果也不会改写这个结论。
        client
            .commands
            .send(Command::TestModel {
                model: "hi/luna".into(),
            })
            .await
            .unwrap();
        let refused = crate::models::ModelTest::Failed {
            model: "hi/luna".into(),
            message: "this session cannot start a test process".into(),
        };
        client
            .snapshots
            .wait_for(|s| s.models.test == refused)
            .await
            .unwrap();
        client
            .commands
            .send(Command::ModelTestFinished {
                model: "hi/luna".into(),
                outcome: Ok(5),
            })
            .await
            .unwrap();
        client.commands.send(Command::ListModels).await.unwrap();
        assert_eq!(next(&mut server).await["method"], "model/list");
        assert_eq!(client.snapshots.borrow().models.test, refused);
        client.commands.send(Command::Quit).await.unwrap();
        client.join.await.unwrap();
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
        initialized(&mut server).await;
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
    /// Answers the handshake and the new session; the client is Ready afterwards.
    async fn initialized(server: &mut BufReader<tokio::io::DuplexStream>) {
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
    async fn a_new_thread_without_identity_cannot_start_a_model_turn() {
        let (mut client, mut server) = harness().await;
        let request = next(&mut server).await;
        let mut response: Value =
            serde_json::from_str(include_str!("../tests/fixtures/protocol/initialize.json"))
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
        send(&mut server, json!({"id":request["id"],"result":{"thread":{"id":" ","cliVersion":"PRIVATE_VERSION"}}})).await;
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
    async fn preview_replacement_publishes_authority_in_place_with_its_identity_transfer() {
        let (mut client, mut server) = harness().await;
        running_root(&mut client, &mut server).await;
        send(&mut server, json!({"method":"item/reasoning/textDelta","params":{
            "threadId":"root","turnId":"root-turn","itemId":"live:0:thinking:0","delta":"draft thought"}})).await;
        send(&mut server, json!({"method":"item/agentMessage/delta","params":{
            "threadId":"root","turnId":"root-turn","itemId":"live:0:text:1","delta":"preview answer"}})).await;
        client
            .snapshots
            .wait_for(|s| s.messages.iter().any(|m| m.id == "live:0:text:1"))
            .await
            .unwrap();
        send(
            &mut server,
            json!({"method":"item/reasoning/textDelta","params":{
            "threadId":"root","turnId":"root-turn","itemId":"thought","delta":"final thought",
            "replaces":["live:0:thinking:0"]}}),
        )
        .await;
        send(
            &mut server,
            json!({"method":"item/agentMessage/delta","params":{
            "threadId":"root","turnId":"root-turn","itemId":"answer","delta":"",
            "replaces":["live:0:text:1"]}}),
        )
        .await;
        // A malformed transfer cannot partly consume valid IDs.
        send(
            &mut server,
            json!({"method":"item/agentMessage/delta","params":{
            "threadId":"root","turnId":"root-turn","itemId":"unrelated","delta":"kept",
            "replaces":["live:0:text:1",42]}}),
        )
        .await;
        client
            .snapshots
            .wait_for(|s| s.messages.iter().any(|m| m.id == "unrelated"))
            .await
            .unwrap();
        assert!(client
            .snapshots
            .borrow()
            .messages
            .iter()
            .any(|m| m.id == "live:0:text:1"));
        send(
            &mut server,
            json!({"method":"item/agentMessage/delta","params":{
            "threadId":"root","turnId":"root-turn","itemId":"answer","delta":"final answer",
            "replaces":["live:0:text:1"]}}),
        )
        .await;
        let snapshot = client
            .snapshots
            .wait_for(|s| s.message_replacements.len() == 2)
            .await
            .unwrap();
        let messages = snapshot
            .messages
            .iter()
            .filter(|m| matches!(m.role.as_str(), "Agent" | "Reasoning"))
            .map(|m| (m.id.as_str(), m.text.as_str()))
            .collect::<Vec<_>>();
        assert_eq!(
            messages,
            [
                ("thought", "final thought"),
                ("answer", "final answer"),
                ("unrelated", "kept")
            ]
        );
        assert_eq!(snapshot.message_replacements[0].role, "Reasoning");
        assert_eq!(snapshot.message_replacements[1].previous, "live:0:text:1");
        drop(snapshot);
        client.commands.send(Command::Quit).await.unwrap();
        client.join.await.unwrap();
    }

    #[tokio::test]
    async fn a_live_preview_is_dropped_when_the_committed_message_replaces_it() {
        let (mut client, mut server) = harness().await;
        running_root(&mut client, &mut server).await;
        let texts = |snapshot: &crate::state::CoreSnapshot| -> Vec<(String, String)> {
            snapshot
                .messages
                .iter()
                .filter(|m| matches!(m.role.as_str(), "Agent" | "Reasoning"))
                .map(|m| (m.id.clone(), m.text.clone()))
                .collect()
        };
        send(&mut server, json!({"method":"item/reasoning/textDelta","params":{"threadId":"root","turnId":"root-turn","itemId":"live:thinking:0","delta":"weigh"}})).await;
        send(&mut server, json!({"method":"item/agentMessage/delta","params":{"threadId":"root","turnId":"root-turn","itemId":"live:text:1","delta":"Hel"}})).await;
        client
            .snapshots
            .wait_for(|snapshot| texts(snapshot).len() == 2)
            .await
            .unwrap();
        // 只有预览可以撤掉；其他轮次或权威消息的身份不受影响。
        send(&mut server, json!({"method":"item/agentMessage/delta","params":{"threadId":"root","turnId":"root-turn","itemId":"m0","delta":"kept"}})).await;
        send(&mut server, json!({"method":"item/discarded","params":{"threadId":"root","turnId":"root-turn","itemId":"m0"}})).await;
        send(&mut server, json!({"method":"item/discarded","params":{"threadId":"root","turnId":"old","itemId":"live:text:1"}})).await;
        send(&mut server, json!({"method":"item/discarded","params":{"threadId":"root","turnId":"root-turn","itemId":"live:thinking:0"}})).await;
        send(&mut server, json!({"method":"item/discarded","params":{"threadId":"root","turnId":"root-turn","itemId":"live:text:1"}})).await;
        send(&mut server, json!({"method":"item/agentMessage/delta","params":{"threadId":"root","turnId":"root-turn","itemId":"m1","delta":"Hello"}})).await;
        client
            .snapshots
            .wait_for(|snapshot| {
                texts(snapshot)
                    == [
                        ("m0".to_owned(), "kept".to_owned()),
                        ("m1".to_owned(), "Hello".to_owned()),
                    ]
            })
            .await
            .unwrap();
        client.commands.send(Command::Quit).await.unwrap();
        client.join.await.unwrap();
    }

    #[tokio::test]
    async fn backend_commands_run_only_when_idle_and_quote_the_backend_outcome() {
        let (mut client, mut server) = harness().await;
        ready(&mut server).await;
        phase(&mut client, SessionPhase::Ready).await;
        let notices = |snapshot: &crate::state::CoreSnapshot| -> Vec<String> {
            snapshot
                .messages
                .iter()
                .filter(|message| message.role == "Notice")
                .map(|message| message.text.clone())
                .collect()
        };
        client
            .commands
            .send(Command::ListBackendCommands)
            .await
            .unwrap();
        let request = next(&mut server).await;
        assert_eq!(request["method"], "backend/commands/list");
        send(
            &mut server,
            json!({"id":request["id"],"result":{"commands":[
            {"name":"compact","description":"Summarise earlier history"},
            {"name":"plan","description":"Plan mode","hint":"on|off"},
            {"name":"Bad Name","description":"dropped"}]}}),
        )
        .await;
        client
            .snapshots
            .wait_for(|snapshot| snapshot.commands_loaded)
            .await
            .unwrap();
        {
            let snapshot = client.snapshots.borrow();
            assert_eq!(
                snapshot
                    .commands
                    .iter()
                    .map(|command| command.name.as_str())
                    .collect::<Vec<_>>(),
                ["compact", "plan"]
            );
            assert_eq!(snapshot.commands[1].hint.as_deref(), Some("on|off"));
        }
        let run = |line: &str| Command::RunBackendCommand { line: line.into() };
        client.commands.send(run(" /compact ")).await.unwrap();
        let request = next(&mut server).await;
        assert_eq!(request["method"], "backend/command/run");
        assert_eq!(request["params"], json!({"line":"/compact"}));
        client
            .snapshots
            .wait_for(|snapshot| notices(snapshot) == ["Running /compact…"])
            .await
            .unwrap();
        // 上一条没有结果前不执行第二条。
        client.commands.send(run("/plan off")).await.unwrap();
        client
            .snapshots
            .wait_for(|snapshot| {
                snapshot
                    .notice
                    .as_deref()
                    .is_some_and(|notice| notice.contains("another command is still running"))
            })
            .await
            .unwrap();
        send(&mut server, json!({"id":request["id"],"result":{"kind":"success","text":"Compacted 3 history items."}})).await;
        client
            .snapshots
            .wait_for(|snapshot| notices(snapshot) == ["/compact: Compacted 3 history items."])
            .await
            .unwrap();
        client.commands.send(run("/plan off")).await.unwrap();
        let request = next(&mut server).await;
        send(&mut server, json!({"id":request["id"],"error":{"code":-32603,"message":"dsh has no command /plan"}})).await;
        client
            .snapshots
            .wait_for(|snapshot| {
                notices(snapshot).last().map(String::as_str)
                    == Some("/plan off was not run: dsh has no command /plan")
            })
            .await
            .unwrap();
        client.commands.send(run("not a command")).await.unwrap();
        client
            .snapshots
            .wait_for(|snapshot| {
                snapshot
                    .notice
                    .as_deref()
                    .is_some_and(|notice| notice.contains("not a command line"))
            })
            .await
            .unwrap();

        // 轮次运行中不执行，也不向 dsh 发请求。
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
        client.commands.send(run("/compact")).await.unwrap();
        client
            .snapshots
            .wait_for(|snapshot| {
                snapshot
                    .notice
                    .as_deref()
                    .is_some_and(|notice| notice.contains("wait for the current turn"))
            })
            .await
            .unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(50), next(&mut server))
                .await
                .is_err()
        );
        client.commands.send(Command::Quit).await.unwrap();
        client.join.await.unwrap();
    }

    #[tokio::test]
    async fn steering_reaches_only_a_running_root_turn_and_shows_its_real_outcome() {
        let (mut client, mut server) = harness().await;
        running_root(&mut client, &mut server).await;
        let steer = |text: &str| Command::SteerRoot { text: text.into() };
        let notices = |snapshot: &crate::state::CoreSnapshot| -> Vec<String> {
            snapshot
                .messages
                .iter()
                .filter(|message| message.role == "Notice")
                .map(|message| message.text.clone())
                .collect()
        };
        client
            .commands
            .send(steer("  use 中文 names "))
            .await
            .unwrap();
        let request = next(&mut server).await;
        assert_eq!(request["method"], "turn/steer");
        assert_eq!(
            request["params"],
            json!({"threadId":"root","turnId":"root-turn","text":"use 中文 names"})
        );
        client
            .snapshots
            .wait_for(|snapshot| notices(snapshot) == ["Steering queued for the next step…"])
            .await
            .unwrap();
        // 上一条还没确认时不接受第二条，也不向 dsh 发送。
        client.commands.send(steer("second")).await.unwrap();
        client
            .snapshots
            .wait_for(|snapshot| {
                snapshot
                    .notice
                    .as_deref()
                    .is_some_and(|notice| notice.contains("still being delivered"))
            })
            .await
            .unwrap();
        send(
            &mut server,
            json!({"id":request["id"],"result":{"messageId":"m1"}}),
        )
        .await;
        // 别的消息身份和旧轮次的投递事件不算数。
        send(&mut server, json!({"method":"turn/steer/delivered","params":{"threadId":"root","turnId":"root-turn","id":"other"}})).await;
        send(&mut server, json!({"method":"turn/steer/delivered","params":{"threadId":"root","turnId":"old","id":"m1"}})).await;
        send(&mut server, json!({"method":"turn/steer/delivered","params":{"threadId":"root","turnId":"root-turn","id":"m1"}})).await;
        client
            .snapshots
            .wait_for(|snapshot| {
                // 耗时行始终排在这一步的最末尾，引导消息在它之前。
                snapshot
                    .messages
                    .iter()
                    .any(|m| m.role == "You" && m.text == "use 中文 names")
            })
            .await
            .unwrap();
        assert!(notices(&client.snapshots.borrow()).is_empty());

        // dsh 拒绝时显示它给的原因；没有送达的引导不变成用户消息。
        client.commands.send(steer("rejected")).await.unwrap();
        let request = next(&mut server).await;
        assert_eq!(request["method"], "turn/steer");
        send(&mut server, json!({"id":request["id"],"error":{"code":-32603,"message":"no turn is running; send this as a new task"}})).await;
        client
            .snapshots
            .wait_for(|snapshot| {
                notices(snapshot)
                    == ["Steering was not accepted: no turn is running; send this as a new task"]
            })
            .await
            .unwrap();

        // 被接受但轮次先结束：明确显示没有送达。
        client.commands.send(steer("too late")).await.unwrap();
        let request = next(&mut server).await;
        send(
            &mut server,
            json!({"id":request["id"],"result":{"messageId":"m3"}}),
        )
        .await;
        send(&mut server, json!({"method":"turn/completed","params":{"threadId":"root","turn":{"id":"root-turn","status":"completed"}}})).await;
        phase(&mut client, SessionPhase::Completed).await;
        assert_eq!(
            notices(&client.snapshots.borrow()).last().unwrap(),
            "Steering was not delivered: the turn ended first."
        );
        assert!(!client
            .snapshots
            .borrow()
            .messages
            .iter()
            .any(|m| m.text == "too late" || m.text == "rejected"));
        send(&mut server, json!({"method":"turn/steer/delivered","params":{"threadId":"root","turnId":"root-turn","id":"m3"}})).await;

        // 没有运行中的轮次：Core 自己拒绝，不发请求。
        client.commands.send(steer("idle")).await.unwrap();
        client
            .snapshots
            .wait_for(|snapshot| {
                snapshot
                    .notice
                    .as_deref()
                    .is_some_and(|notice| notice.contains("no turn is running"))
            })
            .await
            .unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(50), next(&mut server))
                .await
                .is_err()
        );
        assert!(!client
            .snapshots
            .borrow()
            .messages
            .iter()
            .any(|m| m.text == "too late"));
        client.commands.send(Command::Quit).await.unwrap();
        client.join.await.unwrap();
    }

    #[tokio::test]
    async fn steering_is_refused_while_the_root_turn_waits_at_a_gate() {
        let (mut client, mut server) = harness().await;
        running_root(&mut client, &mut server).await;
        child(&mut server, "a", "a-1").await;
        wait_call(&mut server, "steer-wait", vec!["a"]).await;
        phase(&mut client, SessionPhase::GatePending).await;
        client
            .commands
            .send(Command::SteerRoot {
                text: "hurry".into(),
            })
            .await
            .unwrap();
        client
            .snapshots
            .wait_for(|snapshot| {
                snapshot
                    .notice
                    .as_deref()
                    .is_some_and(|notice| notice.contains("waiting for child agents"))
            })
            .await
            .unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(50), next(&mut server))
                .await
                .is_err()
        );
        let snapshot = client.snapshots.borrow().clone();
        assert_eq!(snapshot.phase, SessionPhase::GatePending);
        assert!(!snapshot.messages.iter().any(|m| m.role == "Notice"));
        client.commands.send(Command::Quit).await.unwrap();
        client.join.await.unwrap();
    }

    #[tokio::test]
    async fn host_steps_keep_separate_frozen_rows_and_reject_stale_step_stages() {
        let (mut client, mut server) = harness().await;
        running_root(&mut client, &mut server).await;
        for (step, item) in [(1, "alpha"), (2, "beta")] {
            observation_event(&mut client, &mut server, json!({"method":"turn/step","params":{"threadId":"root","turnId":"root-turn","step":step,"state":"start"}})).await;
            observation_event(&mut client, &mut server, json!({"method":"turn/stage","params":{"threadId":"root","turnId":"root-turn","step":step+10,"stage":"thinking"}})).await;
            observation_event(&mut client, &mut server, json!({"method":"turn/step","params":{"threadId":"root","turnId":"old","step":90,"state":"start"}})).await;
            observation_event(&mut client, &mut server, json!({"method":"item/agentMessage/delta","params":{"threadId":"root","turnId":"root-turn","itemId":item,"delta":item}})).await;
            observation_event(&mut client, &mut server, json!({"method":"turn/step","params":{"threadId":"root","turnId":"root-turn","step":step,"state":"end"}})).await;
            let snapshot = client.snapshots.borrow();
            let timers: Vec<_> = snapshot
                .messages
                .iter()
                .filter(|message| message.role == "Timing")
                .collect();
            assert_eq!(timers.len(), step as usize);
            assert!(timers.iter().all(|timer| timer.complete
                && timer.text.contains("Completed")
                && !timer.text.contains("Thinking")));
            assert!(timers
                .last()
                .unwrap()
                .text
                .starts_with(&format!("Step {step} · ")));
            assert_eq!(snapshot.messages[snapshot.messages.len() - 3].text, item);
            let summary = snapshot.messages.last().unwrap();
            assert_eq!(summary.role, "TimingSummary");
            assert!(summary.text.starts_with(&format!("{step} steps · ")));
        }
        client.commands.send(Command::Quit).await.unwrap();
        client.join.await.unwrap();
    }

    #[tokio::test]
    async fn backend_stage_and_todo_events_update_the_current_root_turn_only() {
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
            json!({"id":request["id"],"result":{"turn":{"id":"one"}}}),
        )
        .await;
        phase(&mut client, SessionPhase::Running).await;
        let timing = |snapshot: &crate::state::CoreSnapshot| {
            snapshot
                .messages
                .iter()
                .find(|message| message.role == "Timing")
                .map(|message| message.text.clone())
                .unwrap_or_default()
        };
        send(&mut server, json!({"method":"turn/stage","params":{"threadId":"root","turnId":"one","stage":"thinking"}})).await;
        client
            .snapshots
            .wait_for(|snapshot| timing(snapshot).contains("Thinking"))
            .await
            .unwrap();
        // 旧轮次、未知阶段和整块到达的正文都不改变当前阶段。
        send(&mut server, json!({"method":"turn/stage","params":{"threadId":"root","turnId":"old","stage":"response"}})).await;
        send(&mut server, json!({"method":"turn/stage","params":{"threadId":"root","turnId":"one","stage":"invented"}})).await;
        send(&mut server, json!({"method":"item/agentMessage/delta","params":{"threadId":"root","turnId":"one","itemId":"a","delta":"answer"}})).await;
        client
            .snapshots
            .wait_for(|snapshot| snapshot.messages.iter().any(|m| m.text == "answer"))
            .await
            .unwrap();
        assert!(!timing(&client.snapshots.borrow()).contains("Response"));
        send(&mut server, json!({"method":"turn/stage","params":{"threadId":"root","turnId":"one","stage":"response"}})).await;
        client
            .snapshots
            .wait_for(|snapshot| timing(snapshot).contains("Response"))
            .await
            .unwrap();

        send(
            &mut server,
            json!({"method":"turn/notice","params":{"threadId":"root","turnId":"one",
            "kind":"retry","retry":1,"maxRetries":2,"delayMs":500,"code":"rate_limit"}}),
        )
        .await;
        send(
            &mut server,
            json!({"method":"turn/notice","params":{"threadId":"root","turnId":"old",
            "kind":"retry","retry":7}}),
        )
        .await;
        send(
            &mut server,
            json!({"method":"turn/notice","params":{"threadId":"root","turnId":null,
            "kind":"compaction","id":"c1","state":"end"}}),
        )
        .await;
        client
            .snapshots
            .wait_for(|snapshot| {
                snapshot
                    .messages
                    .iter()
                    .any(|m| m.text.contains("compacted"))
            })
            .await
            .unwrap();
        assert_eq!(
            client
                .snapshots
                .borrow()
                .messages
                .iter()
                .filter(|m| m.role == "Notice")
                .map(|m| m.text.as_str())
                .collect::<Vec<_>>(),
            [
                "Retrying model request (1/2) in 500ms: rate_limit",
                "… earlier context was compacted …"
            ]
        );
        send(
            &mut server,
            json!({"method":"thread/todos/updated","params":{"threadId":"other","todos":[
            {"content":"foreign","status":"pending"}]}}),
        )
        .await;
        send(
            &mut server,
            json!({"method":"thread/todos/updated","params":{"threadId":"root","todos":[
            {"content":"scan","status":"completed"},{"content":"build","status":"in_progress"},
            {"content":"bad","status":"done"}]}}),
        )
        .await;
        client
            .snapshots
            .wait_for(|snapshot| !snapshot.todos.is_empty())
            .await
            .unwrap();
        {
            let snapshot = client.snapshots.borrow();
            assert_eq!(snapshot.todos.len(), 2);
            assert_eq!(snapshot.todos[1].content, "build");
            assert_eq!(
                snapshot.todos[1].status,
                crate::state::TodoStatus::InProgress
            );
            let stored = serde_json::to_string(&StoredSnapshot::capture(&snapshot)).unwrap();
            assert!(!stored.contains("build"));
        }
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
        {
            let snapshot = client.snapshots.borrow();
            let [.., answer, timing, summary] = snapshot.messages.as_slice() else {
                panic!("the answer and its timing row are missing");
            };
            assert_eq!(summary.role, "TimingSummary");
            assert!(summary.text.contains(" · Completed "), "{}", summary.text);
            assert_eq!(answer.text, "中文");
            // 耗时行排在这一步的最末尾，服务端确认完成后固定并带完成时刻。
            assert_eq!((timing.role.as_str(), timing.complete), ("Timing", true));
            assert!(timing.text.contains("Response "), "{}", timing.text);
            assert!(timing.text.contains(" · Completed "), "{}", timing.text);
        }
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
                // 运行中的耗时行跟在回答之后，还没有固定。
                matches!(s.messages.as_slice(), [.., answer, timing, summary]
                    if answer.text == "second" && answer.role == "Agent"
                        && timing.role == "Timing" && !timing.complete
                        && timing.turn_id == "two"
                        && summary.role == "TimingSummary" && summary.turn_id == "two")
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
    async fn disconnect_is_visible_and_never_starts_a_model_turn() {
        let (mut client, mut server) = harness().await;
        ready(&mut server).await;
        phase(&mut client, SessionPhase::Ready).await;
        assert_eq!(client.snapshots.borrow().root_turn_count, 0);
        drop(server);
        phase(&mut client, SessionPhase::Disconnected).await;
        assert!(client.join.await.unwrap().error.is_some());
    }

    #[tokio::test]
    async fn disconnect_after_a_reload_failure_preserves_the_confirmed_cause() {
        let (mut client, mut server) = harness().await;
        ready(&mut server).await;
        phase(&mut client, SessionPhase::Ready).await;
        client.commands.send(Command::ReloadBackend).await.unwrap();
        let reload = next(&mut server).await;
        assert_eq!(reload["method"], "backend/reload");
        send(
            &mut server,
            json!({"id":reload["id"],"error":{"code":-32000,"message":"dsh exited while restarting the session: reload-launch-failure"}}),
        )
        .await;
        phase(&mut client, SessionPhase::Failed).await;
        drop(server);
        phase(&mut client, SessionPhase::Disconnected).await;
        let report = client.join.await.unwrap();
        let error = report.error.unwrap();
        assert!(error.contains("reload-launch-failure"));
        assert!(error.contains("external outcome is unknown"));
        assert_eq!(client.snapshots.borrow().root_turn_count, 0);
    }

    #[tokio::test]
    async fn a_session_that_failed_to_start_cannot_be_bypassed_by_submitting_a_task() {
        let (mut client, mut server) = harness().await;
        let init = next(&mut server).await;
        send(&mut server, json!({"id":init["id"],"result":{}})).await;
        assert_eq!(next(&mut server).await["method"], "initialized");
        let thread = next(&mut server).await;
        assert_eq!(thread["method"], "thread/start");
        send(
            &mut server,
            json!({"id":thread["id"],"error":{"code":-32602,"message":"the profile has no model route"}}),
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
