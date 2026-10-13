use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use tokio::time::Instant;

use crate::agents::{AgentRegistry, AgentSnapshot};
use crate::gate::WaitTarget;
use crate::interactions::RequestView;

pub const MESSAGE_BYTES: usize = 32 * 1024;
pub const HISTORY_BYTES: usize = 256 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SessionPhase {
    #[default]
    Created,
    Launching,
    Initializing,
    CheckingShell,
    Ready,
    StartingTurn,
    Running,
    GatePending,
    Completed,
    Interrupted,
    Stopping,
    ClosingTransport,
    Stopped,
    Disconnected,
    Failed,
    Unknown,
}

/// Core 对最新快照的持久化确认状态。
///
/// Submitted 只表示快照已交给 journal writer；只有提交序号追平后才是 Committed。
/// writer 出错或 journal 不可用时结果为 Uncertain。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PersistenceState {
    Submitted,
    Committed,
    #[default]
    Uncertain,
}

impl SessionPhase {
    pub fn can_submit(self) -> bool {
        matches!(
            self,
            Self::Ready | Self::Completed | Self::Interrupted | Self::Failed
        )
    }
}

/// A completed turn is successful only when every root task succeeded.
pub(crate) fn workflow_outcome(
    phase: SessionPhase,
    roots: impl Iterator<Item = crate::scheduler::TaskState>,
) -> SessionPhase {
    use crate::scheduler::TaskState;
    if phase != SessionPhase::Completed {
        return phase;
    }
    let mut failed = false;
    for state in roots {
        match state {
            TaskState::Succeeded => {}
            TaskState::Failed | TaskState::Cancelled | TaskState::Blocked => failed = true,
            _ => return SessionPhase::Unknown,
        }
    }
    if failed {
        SessionPhase::Failed
    } else {
        SessionPhase::Completed
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct ConversationItem {
    pub id: String,
    pub thread_id: String,
    pub turn_id: String,
    pub role: String,
    pub text: String,
    pub complete: bool,
    pub truncated: bool,
    /// Core 提供工具事实；UI 不从摘要文本推断生命周期。
    pub tools: Vec<crate::tool_details::ConversationTool>,
}

impl std::fmt::Debug for ConversationItem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // 快照可能被调试打印；提示、回答和工具命令只输出长度，不输出正文。
        f.debug_struct("ConversationItem")
            .field("id", &self.id)
            .field("thread_id", &self.thread_id)
            .field("turn_id", &self.turn_id)
            .field("role", &self.role)
            .field("text_bytes", &self.text.len())
            .field("complete", &self.complete)
            .field("truncated", &self.truncated)
            .finish()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FactSource {
    ServerConfirmed,
    LocalEstimate,
    #[default]
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct UsageSummary {
    pub input_tokens: Option<u64>,
    pub cached_input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub reasoning_tokens: Option<u64>,
    pub total_tokens: Option<u64>,
    pub context_window: Option<u64>,
    pub source: FactSource,
}

/// 服务端 usage 的归属身份；UI 不得从 agent id 或当前选中项推断这些字段。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct UsageIdentity {
    pub thread_id: Option<String>,
    pub turn_id: Option<String>,
    pub generation: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct UsageFact {
    pub summary: UsageSummary,
    pub identity: UsageIdentity,
}

/// 会话级预算投影；数字只来自服务端确认的 usage。
///
/// `per_agent_*` 是当前会话配置的单 agent/turn 预算及其触发摘要，
/// 不携带 thread、turn 或 prompt 等私有执行内容。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TokenBudgetSnapshot {
    pub confirmed_total_tokens: Option<u64>,
    pub confirmed_complete: bool,
    pub limit: Option<u64>,
    pub stop_triggered: bool,
    pub per_agent_limit: Option<u64>,
    pub per_agent_stop_triggered: bool,
}

impl UsageSummary {
    pub fn has_value(self) -> bool {
        self.input_tokens.is_some()
            || self.cached_input_tokens.is_some()
            || self.output_tokens.is_some()
            || self.reasoning_tokens.is_some()
            || self.total_tokens.is_some()
            || self.context_window.is_some()
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CoreSnapshot {
    pub version: u64,
    pub phase: SessionPhase,
    pub root_turn_count: u64,
    pub gate: Option<GateSnapshot>,
    pub root_start_requests: u64,
    pub scheduler: crate::scheduler::SchedulerSnapshot,
    pub observation: crate::observation::ObservationSnapshot,
    pub timeline: crate::timeline::TimelineSnapshot,
    pub tool_details: crate::tool_details::ToolDetailsSnapshot,
    pub extensions: Vec<crate::extensions::ExtensionSnapshot>,
    pub diagnostics: crate::diagnostics::CoreMetrics,
    pub journal: Option<crate::journal::JournalView>,
    pub persistence: PersistenceState,
    pub agents: Vec<AgentSnapshot>,
    pub thread_id: Option<String>,
    pub turn_id: Option<String>,
    pub model: Option<String>,
    pub cwd: String,
    /// dsh's permission preset of this session and the ones that can be chosen.
    pub permission: crate::permission::PermissionSnapshot,
    pub messages: Vec<ConversationItem>,
    pub requests: Vec<RequestView>,
    pub last_headless_action: Option<crate::interactions::HeadlessAction>,
    pub notice: Option<String>,
    pub last_error: Option<String>,
    pub usage: UsageSummary,
    pub usage_facts: Vec<UsageFact>,
    pub token_budget: TokenBudgetSnapshot,
    pub skills: crate::skills::SkillsSnapshot,
    /// dsh 自己的斜杠命令；`commands_loaded` 为假时还没有向 dsh 问过。
    pub commands: Vec<BackendCommand>,
    pub commands_loaded: bool,
    /// 工作区当前的 git 分支；不是仓库或读不到时为空，不猜。
    pub branch: Option<String>,
    /// 输入 `@路径` 时最近一次查询到的工作区路径。
    pub path_hints: crate::path_hints::PathHints,
    pub models: crate::models::ModelsSnapshot,
    /// dsh's sessions of this workspace, as last listed for the sessions page.
    pub conversations: Vec<crate::conversations::ConversationSummary>,
    /// Title of the dsh session this one is switching to.
    pub resuming: Option<String>,
    /// A fresh dsh conversation is awaiting backend confirmation.
    pub creating_conversation: bool,
    /// Choices stored for the next start; empty for a setting never saved.
    pub saved_settings: BTreeMap<crate::preferences::Setting, String>,
    pub history_truncated: bool,
    /// 当前 root 会话里 Agent 自己维护的 todo 列表；由 dsh 推送，整份替换。
    pub todos: Vec<TodoItem>,
    /// 启动预检未通过且已不在启动阶段：本会话不能再提交任务。
    pub startup_blocked: bool,
}

/// dsh 的一条斜杠命令，例如 `compact`；名称不带斜杠。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackendCommand {
    pub name: String,
    pub description: String,
    pub hint: Option<String>,
}

/// 解析 dsh 的命令清单；名称必须是可以直接跟在斜杠后面的小写标识。
pub(crate) fn parse_backend_commands(value: &serde_json::Value) -> Option<Vec<BackendCommand>> {
    Some(
        value["commands"]
            .as_array()?
            .iter()
            .take(64)
            .filter_map(|command| {
                let name = command["name"].as_str().filter(|name| {
                    !name.is_empty()
                        && name.len() <= 64
                        && name
                            .bytes()
                            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
                })?;
                Some(BackendCommand {
                    name: name.to_owned(),
                    description: command["description"]
                        .as_str()
                        .unwrap_or_default()
                        .chars()
                        .take(200)
                        .collect(),
                    hint: command["hint"]
                        .as_str()
                        .map(|hint| hint.chars().take(80).collect()),
                })
            })
            .collect(),
    )
}

pub const MAX_TODOS: usize = 100;
pub const MAX_TODO_CHARS: usize = 512;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TodoStatus {
    Pending,
    InProgress,
    Completed,
}

#[derive(Clone, PartialEq, Eq)]
pub struct TodoItem {
    pub content: String,
    pub status: TodoStatus,
}

impl std::fmt::Debug for TodoItem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // 内容是 Agent 写给用户看的文字；调试输出只给长度。
        f.debug_struct("TodoItem")
            .field("content_bytes", &self.content.len())
            .field("status", &self.status)
            .finish()
    }
}

/// 解析一份推送来的 todo 列表；无法识别的条目丢弃，不猜测状态。
pub(crate) fn parse_todos(value: &serde_json::Value) -> Vec<TodoItem> {
    value
        .as_array()
        .into_iter()
        .flatten()
        .take(MAX_TODOS)
        .filter_map(|todo| {
            let status = match todo["status"].as_str()? {
                "pending" => TodoStatus::Pending,
                "in_progress" => TodoStatus::InProgress,
                "completed" => TodoStatus::Completed,
                _ => return None,
            };
            Some(TodoItem {
                content: todo["content"]
                    .as_str()?
                    .chars()
                    .take(MAX_TODO_CHARS)
                    .collect(),
                status,
            })
        })
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GateSnapshot {
    pub targets: Vec<WaitTarget>,
    pub pending: bool,
    pub root_starts_at_enter: u64,
    pub root_starts_at_release: Option<u64>,
}

/// Modified only by the Core command/event owner.
#[derive(Debug, Default)]
pub(crate) struct SessionState {
    pub view: CoreSnapshot,
    pub agents: AgentRegistry,
    pub(crate) usage_facts: BTreeMap<String, UsageFact>,
    /// 工具摘要行，按 item 原位更新；一个轮次里被思考或正文隔开的调用各成一组。只存在内存中。
    pub(crate) tool_lines: VecDeque<ToolLines>,
    /// 当前 root 轮次的分阶段耗时；轮次结束后固定进对话并清空。
    pub(crate) turn_timing: Option<TurnTiming>,
}

/// 一个 root 轮次的耗时阶段；由 Core 按事件到达时间切换。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TurnStage {
    ModelWait,
    Thinking,
    Response,
    Tools,
}

const TURN_STAGES: [(TurnStage, &str); 4] = [
    (TurnStage::ModelWait, "Model wait"),
    (TurnStage::Thinking, "Thinking"),
    (TurnStage::Response, "Response"),
    (TurnStage::Tools, "Tools"),
];

#[derive(Debug)]
pub(crate) struct TurnTiming {
    turn_id: String,
    stage: TurnStage,
    since: Instant,
    totals: [Duration; 4],
    /// 仍在运行的工具；非空时保持工具阶段，穿插的输出不打断它。
    running_tools: BTreeSet<String>,
    /// 后端报告过模型所处阶段：之后整块到达的输出不再用来划分阶段。
    stream_staged: bool,
}

impl TurnTiming {
    fn switch(&mut self, stage: TurnStage, now: Instant) {
        self.totals[self.stage as usize] += now.saturating_duration_since(self.since);
        self.stage = stage;
        self.since = now;
    }

    fn text(&self, now: Instant, active: bool) -> String {
        TURN_STAGES
            .iter()
            .filter_map(|(stage, label)| {
                let live = active && *stage == self.stage;
                let mut total = self.totals[*stage as usize];
                if live {
                    total += now.saturating_duration_since(self.since);
                }
                (live || !total.is_zero())
                    .then(|| format!("{label} {:.1}s", total.as_millis() as f64 / 1000.0))
            })
            .collect::<Vec<_>>()
            .join(" · ")
    }
}

/// 轮次结束时刻的墙上时间；没有时区数据，明确标注 UTC。
pub(crate) fn wall_clock_label(now: SystemTime) -> Option<String> {
    let seconds = now.duration_since(UNIX_EPOCH).ok()?.as_secs() % 86_400;
    Some(format!(
        "{:02}:{:02}:{:02} UTC",
        seconds / 3600,
        seconds % 3600 / 60,
        seconds % 60
    ))
}

#[derive(Debug, Default)]
pub(crate) struct ToolLines {
    thread_id: String,
    turn_id: String,
    /// 这一组在轮次里的序号；第一组是 0。
    segment: usize,
    lines: VecDeque<(crate::tool_details::ConversationTool, String)>,
    dropped: usize,
}

impl ToolLines {
    fn message_id(&self) -> String {
        match self.segment {
            0 => format!("tools:{}", self.turn_id),
            segment => format!("tools:{}:{segment}", self.turn_id),
        }
    }
}

const TOOL_LINES_PER_TURN: usize = 40;
const TOOL_LINE_TURNS: usize = 64;

impl SessionState {
    pub fn snapshot(&mut self) -> Arc<CoreSnapshot> {
        self.view.agents = self.agents.snapshots();
        self.view.usage_facts = self.usage_facts.values().cloned().collect();
        self.view.version += 1;
        Arc::new(self.view.clone())
    }

    pub fn clear_usage_fact(&mut self, thread_id: &str) {
        // 用明确的 unavailable 占位遮住旧 turn，直到服务端确认新事实。
        self.usage_facts.insert(
            thread_id.to_owned(),
            UsageFact {
                summary: UsageSummary::default(),
                identity: UsageIdentity {
                    thread_id: Some(thread_id.to_owned()),
                    ..Default::default()
                },
            },
        );
    }

    pub fn set_usage_fact(&mut self, fact: UsageFact) {
        if let Some(thread_id) = fact.identity.thread_id.clone() {
            self.usage_facts.insert(thread_id, fact);
        }
    }

    /// 用一条 "Tool" 消息汇总连续的工具调用，避免每次调用占用一条历史。
    /// 思考或正文在上一组之后到达过，新的调用就另起一组，保持到达顺序。
    pub fn tool_line(
        &mut self,
        thread: &str,
        turn: &str,
        tool: crate::tool_details::ConversationTool,
        line: String,
    ) {
        let of_turn = |group: &ToolLines| group.thread_id == thread && group.turn_id == turn;
        let known = self.tool_lines.iter().position(|group| {
            of_turn(group)
                && group
                    .lines
                    .iter()
                    .any(|(existing, _)| existing.item_id == tool.item_id)
        });
        let latest = self.tool_lines.iter().rposition(of_turn);
        let open = latest.filter(|&index| {
            let id = self.tool_lines[index].message_id();
            let messages = &self.view.messages;
            messages
                .iter()
                .rposition(|m| m.thread_id == thread && m.turn_id == turn && m.id == id)
                .is_some_and(|at| {
                    !messages.iter().skip(at + 1).any(|m| {
                        m.thread_id == thread
                            && matches!(m.role.as_str(), "Agent" | "Reasoning" | "You")
                    })
                })
        });
        let index = match known.or(open) {
            Some(index) => index,
            None => {
                let segment = latest.map_or(0, |index| self.tool_lines[index].segment + 1);
                if self.tool_lines.len() >= TOOL_LINE_TURNS {
                    self.tool_lines.pop_front();
                }
                self.tool_lines.push_back(ToolLines {
                    thread_id: thread.into(),
                    turn_id: turn.into(),
                    segment,
                    ..Default::default()
                });
                self.tool_lines.len() - 1
            }
        };
        let group = &mut self.tool_lines[index];
        match group
            .lines
            .iter_mut()
            .find(|(existing, _)| existing.item_id == tool.item_id)
        {
            Some(existing) => *existing = (tool, line),
            None => {
                group.lines.push_back((tool, line));
                if group.lines.len() > TOOL_LINES_PER_TURN {
                    group.lines.pop_front();
                    group.dropped += 1;
                }
            }
        }
        self.render_tool_lines(index);
    }

    /// 轮次结束时仍未报告终态的工具，结果不确定；不能继续显示为 running。
    pub fn finish_tool_lines(&mut self, thread: &str, turn: &str) {
        for index in 0..self.tool_lines.len() {
            let group = &self.tool_lines[index];
            if group.thread_id == thread && group.turn_id == turn {
                self.finish_tool_group(index);
            }
        }
    }

    /// 断连或执行不可用时，所有仍在运行的工具结果都不确定。
    pub fn finish_all_tool_lines(&mut self) {
        for index in 0..self.tool_lines.len() {
            self.finish_tool_group(index);
        }
    }

    fn finish_tool_group(&mut self, index: usize) {
        let mut changed = false;
        for (tool, line) in &mut self.tool_lines[index].lines {
            if tool.lifecycle == crate::tool_details::ToolLifecycle::Running {
                tool.lifecycle = crate::tool_details::ToolLifecycle::EndedUnknown;
                *line = line.replacen("▸ running", "▸ ended, outcome unknown", 1);
                changed = true;
            }
        }
        if changed {
            self.render_tool_lines(index);
        }
    }

    fn render_tool_lines(&mut self, index: usize) {
        let group = &self.tool_lines[index];
        let (thread, turn) = (group.thread_id.clone(), group.turn_id.clone());
        let mut text = if group.dropped > 0 {
            format!(
                "… {} earlier tool calls (Ctrl+T)
",
                group.dropped
            )
        } else {
            String::new()
        };
        text.push_str(
            &group
                .lines
                .iter()
                .map(|(_, line)| line.as_str())
                .collect::<Vec<_>>()
                .join(
                    "
",
                ),
        );
        let id = group.message_id();
        let tools = group.lines.iter().map(|(tool, _)| tool.clone()).collect();
        match self
            .view
            .messages
            .iter_mut()
            .find(|m| m.thread_id == thread && m.turn_id == turn && m.id == id)
        {
            Some(message) => {
                message.text = text;
                message.tools = tools;
            }
            None => self.view.messages.push(ConversationItem {
                id,
                thread_id: thread,
                turn_id: turn,
                role: "Tool".into(),
                text,
                complete: true,
                truncated: false,
                tools,
            }),
        }
        self.bound_history();
    }

    pub fn submission(&mut self, text: &str) {
        self.view.phase = SessionPhase::StartingTurn;
        self.view.last_error = None;
        self.view.notice = None;
        self.view.messages.push(ConversationItem {
            id: format!("user-{}", self.view.root_turn_count + 1),
            thread_id: self.view.thread_id.clone().unwrap_or_default(),
            turn_id: String::new(),
            role: "You".into(),
            text: text.to_owned(),
            complete: true,
            truncated: false,
            tools: Vec::new(),
        });
        self.bound_history();
    }

    pub fn turn_started(&mut self, id: String) {
        if self.view.turn_id.as_ref() != Some(&id) {
            self.view.root_turn_count += 1;
        }
        self.view.turn_id = Some(id);
        self.view.phase = SessionPhase::Running;
    }

    /// 开始为当前 root 轮次计时；同一轮次重复绑定不重置。
    pub fn begin_turn_timing(&mut self, now: Instant) {
        let Some(turn) = self.view.turn_id.clone() else {
            return;
        };
        if self
            .turn_timing
            .as_ref()
            .is_some_and(|timing| timing.turn_id == turn)
        {
            return;
        }
        self.turn_timing = Some(TurnTiming {
            turn_id: turn,
            stage: TurnStage::ModelWait,
            since: now,
            totals: [Duration::ZERO; 4],
            running_tools: BTreeSet::new(),
            stream_staged: false,
        });
        self.render_turn_timing(now, None);
    }

    /// 已被接受的 root 输出把计时切到对应阶段。
    pub fn turn_stage(&mut self, turn: &str, stage: TurnStage, now: Instant) {
        let Some(timing) = self
            .turn_timing
            .as_mut()
            .filter(|timing| timing.turn_id == turn)
        else {
            return;
        };
        if !timing.stream_staged && timing.running_tools.is_empty() && timing.stage != stage {
            timing.switch(stage, now);
        }
        self.render_turn_timing(now, None);
    }

    /// 后端报告的模型阶段。dsh 的输出在提交时整块到达，只有这些事件能区分
    /// 等首字、思考和输出；收到第一条后，输出到达的时刻不再划分阶段。
    pub fn turn_stream_stage(&mut self, turn: &str, stage: TurnStage, now: Instant) {
        let Some(timing) = self
            .turn_timing
            .as_mut()
            .filter(|timing| timing.turn_id == turn)
        else {
            return;
        };
        timing.stream_staged = true;
        if timing.running_tools.is_empty() && timing.stage != stage {
            timing.switch(stage, now);
        }
        self.render_turn_timing(now, None);
    }

    /// 工具开始进入工具阶段；全部工具结束后回到等待模型。
    pub fn turn_tool(&mut self, turn: &str, item: &str, running: bool, now: Instant) {
        let Some(timing) = self
            .turn_timing
            .as_mut()
            .filter(|timing| timing.turn_id == turn)
        else {
            return;
        };
        if running {
            if timing.running_tools.len() < 64 {
                timing.running_tools.insert(item.to_owned());
            }
            if timing.stage != TurnStage::Tools {
                timing.switch(TurnStage::Tools, now);
            }
        } else {
            timing.running_tools.remove(item);
            if timing.running_tools.is_empty() && timing.stage == TurnStage::Tools {
                timing.switch(TurnStage::ModelWait, now);
            }
        }
        self.render_turn_timing(now, None);
    }

    /// 没有新事件时，只推进正在进行的阶段的显示值。
    pub fn refresh_turn_timing(&mut self, now: Instant) {
        if self.turn_timing.is_some() {
            self.render_turn_timing(now, None);
        }
    }

    /// 轮次终结：固定各阶段数值。`completed_at` 只在服务端确认完成时给出。
    pub fn finish_turn_timing(&mut self, now: Instant, completed_at: Option<String>) {
        if let Some(timing) = self.turn_timing.as_mut() {
            let stage = timing.stage;
            timing.switch(stage, now);
            self.render_turn_timing(now, Some(completed_at));
            self.turn_timing = None;
        }
    }

    fn render_turn_timing(&mut self, now: Instant, finished: Option<Option<String>>) {
        let Some(timing) = self.turn_timing.as_ref() else {
            return;
        };
        let thread = self.view.thread_id.clone().unwrap_or_default();
        let turn = timing.turn_id.clone();
        let mut text = timing.text(now, finished.is_none());
        if let Some(Some(completed_at)) = &finished {
            if !text.is_empty() {
                text.push_str(" · ");
            }
            text.push_str("Completed ");
            text.push_str(completed_at);
        }
        let id = format!("timing:{turn}");
        let same_turn = |m: &ConversationItem| m.thread_id == thread && m.turn_id == turn;
        let existing = self
            .view
            .messages
            .iter()
            .position(|m| same_turn(m) && m.id == id);
        let mut item = match existing {
            // 耗时行排在这一步的最末尾；后面来了新消息就挪到它们之后。
            Some(index) if self.view.messages[index + 1..].iter().any(same_turn) => {
                self.view.messages.remove(index)
            }
            Some(index) => {
                let item = &mut self.view.messages[index];
                item.text = text;
                item.complete = finished.is_some();
                return;
            }
            None => ConversationItem {
                id,
                thread_id: thread.clone(),
                turn_id: turn.clone(),
                role: "Timing".into(),
                text: String::new(),
                complete: false,
                truncated: false,
                tools: Vec::new(),
            },
        };
        item.text = text;
        item.complete = finished.is_some();
        let at = self
            .view
            .messages
            .iter()
            .rposition(same_turn)
            .map_or(self.view.messages.len(), |index| index + 1);
        self.view.messages.insert(at, item);
        self.bound_history();
    }

    pub fn message(&mut self, turn: &str, id: &str, text: &str, complete: bool) -> bool {
        if self.view.turn_id.as_deref() != Some(turn)
            || (!complete
                && !matches!(
                    self.view.phase,
                    SessionPhase::Running | SessionPhase::GatePending
                ))
        {
            return false;
        }
        let thread = self.view.thread_id.clone().unwrap_or_default();
        self.message_for(&thread, turn, id, "Agent", text, complete)
    }

    pub fn child_message(
        &mut self,
        thread: &str,
        turn: &str,
        id: &str,
        text: &str,
        complete: bool,
    ) -> bool {
        if !self.agents.current_turn(thread, turn)
            || (!complete && !self.agents.active_turn(thread, turn))
        {
            return false;
        }
        self.message_for(thread, turn, id, "Agent", text, complete)
    }

    /// Only server-provided reasoning from the current root or child turn is shown.
    pub fn reasoning(
        &mut self,
        thread: &str,
        turn: &str,
        id: &str,
        text: &str,
        complete: bool,
    ) -> bool {
        let current = if self.view.thread_id.as_deref() == Some(thread) {
            self.view.turn_id.as_deref() == Some(turn)
                && (complete
                    || matches!(
                        self.view.phase,
                        SessionPhase::Running | SessionPhase::GatePending
                    ))
        } else {
            self.agents.current_turn(thread, turn)
                && (complete || self.agents.active_turn(thread, turn))
        };
        current && self.message_for(thread, turn, id, "Reasoning", text, complete)
    }

    pub fn finish_reasoning(&mut self, thread: &str, turn: &str) {
        for message in &mut self.view.messages {
            if message.role == "Reasoning" && message.thread_id == thread && message.turn_id == turn
            {
                message.complete = true;
            }
        }
    }

    /// 模型请求的一次重试：对话流里的一行，不弹面板。只描述 dsh 已经做出的决定。
    pub fn retry_notice(
        &mut self,
        turn: &str,
        retry: u64,
        max_retries: Option<u64>,
        delay_ms: Option<u64>,
        code: Option<&str>,
    ) {
        if self.view.turn_id.as_deref() != Some(turn) {
            return;
        }
        let mut text = match max_retries {
            Some(max) => format!("Retrying model request ({retry}/{max})"),
            None => format!("Retrying model request ({retry})"),
        };
        if let Some(delay) = delay_ms {
            text.push_str(&format!(" in {delay}ms"));
        }
        if let Some(code) = code {
            text.push_str(&format!(": {code}"));
        }
        self.notice_item(format!("retry:{turn}:{retry}"), turn, text, true);
    }

    /// 上下文压缩的开始与结果；同一次压缩始终是同一行。
    pub fn compaction_notice(&mut self, turn: Option<&str>, id: &str, state: &str) {
        let (text, complete) = match state {
            "start" => ("Compacting earlier context…", false),
            "end" => ("… earlier context was compacted …", true),
            "failed" => ("Context compaction did not complete.", true),
            _ => return,
        };
        self.notice_item(
            format!("compaction:{id}"),
            turn.unwrap_or_default(),
            text.into(),
            complete,
        );
    }

    /// 执行不可用时，没有等到结果的压缩和引导不能继续显示为进行中。
    pub fn finish_all_notices(&mut self) {
        for message in &mut self.view.messages {
            if message.role == "Notice" && !message.complete {
                message.text = if message.id.starts_with("steer:") {
                    "Steering outcome unknown."
                } else if message.id.starts_with("command:") {
                    "Command outcome unknown."
                } else {
                    "Context compaction ended, outcome unknown."
                }
                .into();
                message.complete = true;
            }
        }
    }

    /// dsh 命令的结果：对话流里的一行，原样引用 dsh 给出的说明。
    pub fn command_notice(&mut self, id: &str, text: String, complete: bool) {
        let turn = self.view.turn_id.clone().unwrap_or_default();
        self.notice_item(id.into(), &turn, text, complete);
    }

    /// 一次引导的占位提示：已提交、被拒绝或没有送达。
    pub fn steer_notice(&mut self, id: &str, text: String, complete: bool) {
        let turn = self.view.turn_id.clone().unwrap_or_default();
        self.notice_item(id.into(), &turn, text, complete);
    }

    /// dsh 确认引导已交给模型：占位提示换成用户消息，排在它实际生效的位置。
    pub fn steer_delivered(&mut self, id: &str, text: &str) {
        let thread = self.view.thread_id.clone().unwrap_or_default();
        self.view
            .messages
            .retain(|m| !(m.role == "Notice" && m.thread_id == thread && m.id == id));
        self.view.messages.push(ConversationItem {
            id: format!("user-{id}"),
            thread_id: thread,
            turn_id: self.view.turn_id.clone().unwrap_or_default(),
            role: "You".into(),
            text: text.to_owned(),
            complete: true,
            truncated: false,
            tools: Vec::new(),
        });
        self.bound_history();
    }

    fn notice_item(&mut self, id: String, turn: &str, text: String, complete: bool) {
        let thread = self.view.thread_id.clone().unwrap_or_default();
        match self
            .view
            .messages
            .iter_mut()
            .find(|m| m.role == "Notice" && m.thread_id == thread && m.id == id)
        {
            // 已有结果的提示不被迟到的“开始”改回进行中。
            Some(item) if item.complete && !complete => {}
            Some(item) => {
                item.text = text;
                item.complete = complete;
            }
            None => {
                self.view.messages.push(ConversationItem {
                    id,
                    thread_id: thread,
                    turn_id: turn.into(),
                    role: "Notice".into(),
                    text,
                    complete,
                    truncated: false,
                    tools: Vec::new(),
                });
                self.bound_history();
            }
        }
    }

    /// 撤掉当前 root 轮次里的一段预览；权威内容随后以自己的身份到达。
    pub fn discard_item(&mut self, turn: &str, id: &str) {
        if self.view.turn_id.as_deref() != Some(turn) || !id.starts_with("live:") {
            return;
        }
        let thread = self.view.thread_id.clone().unwrap_or_default();
        self.view
            .messages
            .retain(|m| !(m.thread_id == thread && m.turn_id == turn && m.id == id));
    }

    pub fn finish_all_reasoning(&mut self) {
        for message in &mut self.view.messages {
            if message.role == "Reasoning" {
                message.complete = true;
            }
        }
    }

    fn message_for(
        &mut self,
        thread: &str,
        turn: &str,
        id: &str,
        role: &str,
        text: &str,
        complete: bool,
    ) -> bool {
        if !complete && text.is_empty() {
            return false;
        }
        if id.is_empty() || id.len() > 1024 {
            return false;
        }
        let item =
            match self.view.messages.iter_mut().find(|m| {
                m.thread_id == thread && m.turn_id == turn && m.id == id && m.role == role
            }) {
                Some(item) => item,
                None => {
                    self.view.messages.push(ConversationItem {
                        id: id.into(),
                        thread_id: thread.into(),
                        turn_id: turn.into(),
                        role: role.into(),
                        text: String::new(),
                        complete: false,
                        truncated: false,
                        tools: Vec::new(),
                    });
                    self.view.messages.last_mut().unwrap()
                }
            };
        if item.complete && !complete {
            return false;
        }
        if item.complete
            && complete
            && (item.text == text || item.truncated && text.ends_with(&item.text))
        {
            return false;
        }
        if complete {
            item.text = text.to_owned();
            item.complete = true;
        } else {
            item.text.push_str(text);
        }
        if item.text.len() > MESSAGE_BYTES {
            trim_front(&mut item.text, MESSAGE_BYTES);
            item.truncated = true;
        }
        self.bound_history();
        true
    }

    fn bound_history(&mut self) {
        for item in &mut self.view.messages {
            if item.text.len() > MESSAGE_BYTES {
                trim_front(&mut item.text, MESSAGE_BYTES);
                item.truncated = true;
            }
        }
        while self.view.messages.len() > 128
            || self
                .view
                .messages
                .iter()
                .map(|m| {
                    m.text.len()
                        + m.tools
                            .iter()
                            .map(crate::tool_details::ConversationTool::bytes)
                            .sum::<usize>()
                })
                .sum::<usize>()
                > HISTORY_BYTES
        {
            self.view.messages.remove(0);
            self.view.history_truncated = true;
        }
    }

    pub fn error(&mut self, phase: SessionPhase, error: impl Into<String>) {
        self.view.phase = phase;
        self.view.last_error = Some(error.into());
        // 进行中的提示（如“正在检查 shell”）在出错后已过期，留着会遮住错误原因。
        self.view.notice = None;
    }
}

pub(crate) fn trim_front(text: &mut String, max_bytes: usize) {
    let mut cut = text.len().saturating_sub(max_bytes);
    while !text.is_char_boundary(cut) {
        cut += 1;
    }
    text.drain(..cut);
}

pub fn display_text(text: &str) -> String {
    text.chars()
        .filter(|c| !c.is_control() || *c == '\n' || *c == '\t')
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tool(
        item: &str,
        lifecycle: crate::tool_details::ToolLifecycle,
    ) -> crate::tool_details::ConversationTool {
        crate::tool_details::ConversationTool {
            item_id: item.into(),
            category: crate::protocol::ToolCategory::Shell,
            lifecycle,
            name: None,
            command: None,
            cwd: None,
            target: None,
            exit_code: None,
            output_lines: None,
            added_lines: None,
            removed_lines: None,
        }
    }

    #[test]
    fn final_item_corrects_deltas_without_duplicates_and_old_turn_is_ignored() {
        let mut state = SessionState::default();
        state.turn_started("turn-2".into());
        assert!(!state.message("turn-1", "item", "late", false));
        state.message("turn-2", "item", "hel", false);
        state.message("turn-2", "item", "lo", false);
        state.message("turn-2", "item", "hello!", true);
        state.message("turn-2", "item", "late", false);
        assert_eq!(state.view.messages.len(), 1);
        assert_eq!(state.view.messages[0].text, "hello!");
    }

    #[test]
    fn tool_lines_share_one_message_per_turn_and_stay_bounded() {
        let mut state = SessionState::default();
        for index in 0..45 {
            state.tool_line(
                "root",
                "turn",
                tool(
                    &format!("item-{index}"),
                    crate::tool_details::ToolLifecycle::Running,
                ),
                format!("▸ step {index}"),
            );
        }
        state.tool_line(
            "root",
            "turn",
            tool("item-44", crate::tool_details::ToolLifecycle::Completed),
            "▸ step 44 · done".into(),
        );
        let tools: Vec<_> = state
            .view
            .messages
            .iter()
            .filter(|message| message.role == "Tool")
            .collect();
        assert_eq!(tools.len(), 1);
        let lines: Vec<_> = tools[0].text.lines().collect();
        assert_eq!(lines[0], "… 5 earlier tool calls (Ctrl+T)");
        assert_eq!(lines.len(), 41);
        assert_eq!(lines[40], "▸ step 44 · done");
        state.tool_line(
            "root",
            "next",
            tool("item-0", crate::tool_details::ToolLifecycle::Unknown),
            "▸ next".into(),
        );
        assert_eq!(
            state
                .view
                .messages
                .iter()
                .filter(|m| m.role == "Tool")
                .count(),
            2
        );
    }

    #[test]
    fn tools_after_later_thinking_or_text_start_a_new_group_in_arrival_order() {
        use crate::tool_details::ToolLifecycle::{Completed, Running};
        let mut state = SessionState::default();
        state.view.thread_id = Some("root".into());
        state.turn_started("turn".into());
        state.reasoning("root", "turn", "r1", "first thought", true);
        state.tool_line("root", "turn", tool("a", Running), "▸ running · a".into());
        state.tool_line("root", "turn", tool("b", Running), "▸ running · b".into());
        state.reasoning("root", "turn", "r2", "second thought", true);
        state.tool_line("root", "turn", tool("c", Running), "▸ running · c".into());
        // 迟到的结果更新它原来所在的那一组，不搬到新的一组里。
        state.tool_line("root", "turn", tool("a", Completed), "▸ done · a".into());
        // 提示和耗时行不算回答的进展，不拆组。
        state.retry_notice("turn", 1, Some(2), Some(300), Some("rate_limit"));
        state.tool_line("root", "turn", tool("d", Running), "▸ running · d".into());
        state.message("turn", "answer", "done", true);
        let shown: Vec<_> = state
            .view
            .messages
            .iter()
            .filter(|m| m.role != "Notice")
            .map(|m| (m.id.as_str(), m.text.as_str()))
            .collect();
        assert_eq!(
            shown,
            [
                ("r1", "first thought"),
                ("tools:turn", "▸ done · a\n▸ running · b"),
                ("r2", "second thought"),
                ("tools:turn:1", "▸ running · c\n▸ running · d"),
                ("answer", "done"),
            ]
        );
        // 轮次结束时，每一组里没有终态的调用都变成结果未知。
        state.finish_tool_lines("root", "turn");
        let unknown = |id: &str| {
            let message = state.view.messages.iter().find(|m| m.id == id).unwrap();
            message.text.matches("ended, outcome unknown").count()
        };
        assert_eq!((unknown("tools:turn"), unknown("tools:turn:1")), (1, 2));
    }

    #[test]
    fn backend_stages_replace_block_arrival_for_timing_and_todos_parse_strictly() {
        let mut state = SessionState::default();
        state.view.thread_id = Some("root".into());
        state.turn_started("turn".into());
        let start = Instant::now();
        let at = |ms: u64| start + Duration::from_millis(ms);
        state.begin_turn_timing(at(0));
        state.turn_stream_stage("turn", TurnStage::Thinking, at(1000));
        state.turn_stream_stage("turn", TurnStage::Response, at(3000));
        state.turn_stream_stage("old", TurnStage::ModelWait, at(3500));
        // dsh 在提交时才把整块思考和正文发来：这些到达时刻不能改写阶段。
        state.reasoning("root", "turn", "r", "think", false);
        state.turn_stage("turn", TurnStage::Thinking, at(5000));
        state.message("turn", "a", "answer", false);
        state.turn_stage("turn", TurnStage::Response, at(5000));
        state.turn_tool("turn", "tool", true, at(5000));
        // 下一次请求在工具结果转发完之前就开始了；工具结束后回到等首字。
        state.turn_stream_stage("turn", TurnStage::ModelWait, at(5900));
        state.turn_tool("turn", "tool", false, at(6000));
        state.turn_stream_stage("turn", TurnStage::Response, at(6500));
        state.finish_turn_timing(at(7000), None);
        assert_eq!(
            state.view.messages.last().unwrap().text,
            "Model wait 1.5s · Thinking 2.0s · Response 2.5s · Tools 1.0s"
        );

        let todos = parse_todos(&serde_json::json!([
            {"content":"scan","status":"completed"},
            {"content":"x".repeat(2000),"status":"in_progress"},
            {"content":"bad","status":"done"},
            {"status":"pending"},
        ]));
        assert_eq!(todos.len(), 2);
        assert_eq!(todos[1].status, TodoStatus::InProgress);
        assert_eq!(todos[1].content.len(), MAX_TODO_CHARS);
        assert!(!format!("{todos:?}").contains("scan"));
        assert!(parse_todos(&serde_json::json!("not a list")).is_empty());
    }

    /// 手动测量：`cargo test --release --lib measure_ -- --ignored --nocapture`。
    /// 每个流式片段 Core 都发布一份完整快照；这里量历史写满时这一份有多贵。
    #[test]
    #[ignore = "a measurement, not a check"]
    fn measure_snapshot_publication_with_a_full_history() {
        let mut state = SessionState::default();
        state.view.thread_id = Some("root".into());
        state.turn_started("turn".into());
        for index in 0..120 {
            state.message(
                "turn",
                &format!("m{index}"),
                &"中文 text ".repeat(170),
                true,
            );
        }
        let bytes: usize = state.view.messages.iter().map(|m| m.text.len()).sum();
        let rounds = 2000;
        let start = std::time::Instant::now();
        for _ in 0..rounds {
            state.message("turn", "live", "delta ", false);
            std::hint::black_box(state.snapshot());
        }
        let each = start.elapsed() / rounds;
        println!(
            "snapshot publication: {} messages, {} KiB of text -> {each:?} per delta",
            state.view.messages.len(),
            bytes / 1024
        );
    }

    #[test]
    fn retry_and_compaction_are_single_inline_notices_with_explicit_outcomes() {
        let mut state = SessionState::default();
        state.view.thread_id = Some("root".into());
        state.turn_started("turn".into());
        let notices = |state: &SessionState| -> Vec<(String, bool)> {
            state
                .view
                .messages
                .iter()
                .filter(|m| m.role == "Notice")
                .map(|m| (m.text.clone(), m.complete))
                .collect()
        };
        state.retry_notice("turn", 1, Some(2), Some(500), Some("rate_limit"));
        state.retry_notice("turn", 1, Some(2), Some(500), Some("rate_limit"));
        state.retry_notice("turn", 2, None, None, None);
        state.retry_notice("old", 3, None, None, None);
        state.compaction_notice(Some("turn"), "c1", "start");
        state.compaction_notice(Some("turn"), "c2", "start");
        state.compaction_notice(Some("turn"), "c3", "start");
        state.compaction_notice(Some("turn"), "c1", "end");
        state.compaction_notice(Some("turn"), "c1", "start");
        state.compaction_notice(Some("turn"), "c2", "failed");
        state.compaction_notice(None, "c4", "invented");
        state.finish_all_notices();
        assert_eq!(
            notices(&state),
            [
                (
                    "Retrying model request (1/2) in 500ms: rate_limit".to_owned(),
                    true
                ),
                ("Retrying model request (2)".to_owned(), true),
                ("… earlier context was compacted …".to_owned(), true),
                ("Context compaction did not complete.".to_owned(), true),
                (
                    "Context compaction ended, outcome unknown.".to_owned(),
                    true
                ),
            ]
        );
    }

    #[test]
    fn turn_timing_accumulates_each_stage_and_freezes_when_the_turn_ends() {
        let mut state = SessionState::default();
        state.view.thread_id = Some("root".into());
        state.turn_started("turn".into());
        let start = Instant::now();
        let at = |ms: u64| start + Duration::from_millis(ms);
        let row = |state: &SessionState| {
            let rows: Vec<_> = state
                .view
                .messages
                .iter()
                .filter(|m| m.role == "Timing")
                .collect();
            assert_eq!(rows.len(), 1);
            (rows[0].text.clone(), rows[0].complete)
        };
        state.begin_turn_timing(at(0));
        state.refresh_turn_timing(at(400));
        assert_eq!(row(&state), ("Model wait 0.4s".into(), false));
        state.reasoning("root", "turn", "r", "think", false);
        state.turn_stage("turn", TurnStage::Thinking, at(1000));
        state.message("turn", "a", "answer", false);
        state.turn_stage("turn", TurnStage::Response, at(3000));
        state.turn_tool("turn", "tool-1", true, at(6000));
        state.turn_tool("turn", "tool-2", true, at(6100));
        // 工具运行期间穿插的输出不结束工具阶段；另一轮次的事件不计入。
        state.turn_stage("turn", TurnStage::Response, at(6200));
        state.turn_stage("old", TurnStage::Thinking, at(6250));
        state.turn_tool("turn", "tool-1", false, at(6300));
        assert!(row(&state).0.ends_with("Tools 0.3s"));
        state.turn_tool("turn", "tool-2", false, at(6500));
        state.message("turn", "b", "more", false);
        state.turn_stage("turn", TurnStage::Response, at(7000));
        // 这一步后来的消息到达后，耗时行仍在最末尾。
        assert_eq!(state.view.messages.last().unwrap().role, "Timing");
        state.begin_turn_timing(at(7200));
        state.finish_turn_timing(at(7500), Some("14:32:12 UTC".into()));
        let frozen = (
            "Model wait 1.5s · Thinking 2.0s · Response 3.5s · Tools 0.5s · Completed 14:32:12 UTC"
                .to_owned(),
            true,
        );
        assert_eq!(row(&state), frozen);
        state.refresh_turn_timing(at(60_000));
        state.turn_stage("turn", TurnStage::Thinking, at(60_000));
        assert_eq!(row(&state), frozen);
        assert_eq!(
            wall_clock_label(UNIX_EPOCH + Duration::from_secs(86_400 + 52_332)).as_deref(),
            Some("14:32:12 UTC")
        );

        // 没有服务端终态的结束只固定数值，不写完成时刻。
        state.view.turn_id = Some("lost".into());
        state.begin_turn_timing(at(0));
        state.finish_turn_timing(at(2000), None);
        let lost = state.view.messages.last().unwrap();
        assert_eq!(
            (lost.text.as_str(), lost.complete),
            ("Model wait 2.0s", true)
        );
    }

    #[test]
    fn unfinished_tool_lines_become_unknown_when_the_turn_ends() {
        let mut state = SessionState::default();
        state.tool_line(
            "root",
            "turn",
            tool("a", crate::tool_details::ToolLifecycle::Completed),
            "▸ done · exit 0 · ls".into(),
        );
        state.tool_line(
            "root",
            "turn",
            tool("b", crate::tool_details::ToolLifecycle::Running),
            "▸ running · Start-Sleep 90".into(),
        );
        state.finish_tool_lines("root", "turn");
        let tools = state
            .view
            .messages
            .iter()
            .find(|m| m.role == "Tool")
            .unwrap();
        assert_eq!(
            tools.text,
            "▸ done · exit 0 · ls
▸ ended, outcome unknown · Start-Sleep 90"
        );
    }

    #[test]
    fn error_replaces_stale_progress_notice() {
        let mut state = SessionState::default();
        state.view.notice = Some("Starting the session".into());
        state.error(SessionPhase::Unknown, "Session start timed out");
        assert_eq!(state.view.notice, None);
        assert_eq!(
            state.view.last_error.as_deref(),
            Some("Session start timed out")
        );
    }

    #[test]
    fn truncates_chinese_on_character_boundaries_and_bounds_total_history() {
        let mut state = SessionState::default();
        state.turn_started("turn".into());
        for i in 0..30 {
            state.message("turn", &i.to_string(), &"中文".repeat(12000), true);
        }
        assert!(state.view.history_truncated);
        assert!(
            state
                .view
                .messages
                .iter()
                .map(|m| m.text.len())
                .sum::<usize>()
                <= HISTORY_BYTES
        );
        assert!(state
            .view
            .messages
            .iter()
            .all(|m| m.text.len() <= MESSAGE_BYTES && m.truncated));
    }

    #[test]
    fn terminal_turn_ignores_late_deltas_but_accepts_authoritative_final_text() {
        let mut state = SessionState::default();
        state.turn_started("one".into());
        state.message("one", "a", "partial", false);
        state.view.phase = SessionPhase::Completed;
        assert!(!state.message("one", "a", "late", false));
        assert!(!state.message("one", "new", "late", false));
        assert!(state.message("one", "a", "final", true));
        assert_eq!(state.view.messages.len(), 1);
        assert_eq!(state.view.messages[0].text, "final");
    }
}
