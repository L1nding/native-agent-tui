# Rust Agent TUI UI 详细设计

更新：2026-10-03。依赖 [Core 设计](native-agent-tui-core.md)、[产品计划](native-agent-tui-plan.md)和[可观测性契约](native-agent-tui-observability.md)。调度扩展见[工作流设计](native-agent-tui-workflow.md)。

目标是把 TUI 做成可观察、可交互、可调度的 Agent 工作台：用户能够看到每个 thread/turn/item 的对话轨迹、工具与审批、父子关系、上下文和压缩、skill 状态、任务依赖与调度原因；用户能够在合法范围内回答请求、暂停/恢复/取消/重排任务和导出脱敏记录。UI 不解析 JSON-RPC、不猜测完成状态，也不以刷新或轮询推动模型。

## 0. Alpha 与 V2 交付边界

Alpha：状态与最近证据、时间线、审批/输入、搜索、等待列表、脱敏导出与观察恢复。已有 child/Gate 可显示，不以完整多 agent scheduler、DAG overlay、全部 context/skill 面板作为 Alpha 前置。V2 完成 1–3 个直属 child 的调度视图。文中扩展字段仅在有 Core 证据时显示。

顶部始终显示 execution、activity、最后可靠证据、静默时长、attention、来源和下一步动作；不能只用 spinner、颜色或进程存活表示正常。等待详情优先列表：等待方 → 对象/turn/generation → 恢复条件 → 最近证据。DAG 是 V2 详情。

UI 只显示 Core 计算的 Active/Quiet/AttentionNeeded。临时阈值通过 typed command 更新 Core，显示当前值及来源；关闭提醒/继续等待只更新 UI 本地状态，不重置证据或发请求。审批/输入立即显示 requires_action，虽然不按静默升级。新证据只清除对应活动的静默提示，不隐藏独立错误或待审批。

交付报告显示结论、文件、验证、未完成项和逐字段 reported/verified/unknown。没有结构化数据时显示 unknown。支持查看详情、复制 ID、继续等待和明确中断；通知默认关闭。

键盘优先，UI 英文默认；中文与 emoji 输入显示属于 Alpha，中文文案后续完善。

## 1. UI 的执行边界

UI 只做三件事：

1. 把 Core 已确认的事实投影成可浏览的视图。
2. 把键盘、鼠标和表单操作转换成类型化 `UiCommand`。
3. 维护本地选中、过滤、滚动、折叠、搜索和书签状态。

UI 不直接访问 app-server、Child、Gate、工具结果或持久化 journal。`idle`、token usage、静默时间、日志中的自然语言都不能单独改变任务状态。UI 显示“父续接被 Gate 阻塞”时，依据是 Core 的 `GatePending` 事实；显示“provider 请求为零”只有在测试 Adapter 提供计数证据时才允许。

## 2. 外部 Interface 与 Module

UI 与 Core 的 Seam：

```rust
pub trait UiBridge {
    fn snapshots(&self) -> tokio::sync::watch::Receiver<std::sync::Arc<UiSnapshot>>;
    fn submit(&self, command: UiCommand) -> Result<CommandAccepted, UiError>;
    fn query(&self, query: ViewQuery) -> Result<ViewPage, UiError>;
}
```

```rust
pub struct UiSnapshot {
    pub version: u64,
    pub session: SessionView,
    pub root: RootExecutionView,
    pub agents: std::sync::Arc<[AgentView]>,
    pub requests: std::sync::Arc<[RequestSummary]>,
    pub scheduler: SchedulerView,
    pub usage: UsageSummary,
    pub skills: SkillSummary,
    pub diagnostics: DiagnosticSummary,
    pub activities: std::sync::Arc<[ActivityView]>, // 含证据、等待和 Core attention
}
```

快照只包含小型摘要、稳定 ID、游标、计数和来源标签，不复制完整日志。事件/日志通过 Core 的 `query` 分页读取，查询不会阻塞 reader 或 Gate。

主要 Module：

| Module | Interface | Implementation |
| --- | --- | --- |
| `UiRuntime` | 终端事件、snapshot、绘制循环 | Tokio + Crossterm EventStream、dirty redraw |
| `ViewProjection` | snapshot + 本地状态 → 可见页 | 过滤、分页、关联跳转、折叠 |
| `ConversationTree` | 选择 agent、显示父子关系 | 稳定树、路径导航、未读计数 |
| `TimelineView` | thread/turn/item 轨迹 | 事件分类、搜索、书签、详情 |
| `ContextView` | usage、压缩和预算 | server confirmed / estimate / unknown |
| `SkillView` | 技能 inventory 与调用 | loaded/invoked/failed 来源标记 |
| `SchedulerView` | DAG、ready queue、槽位、阻塞原因 | 任务依赖和调度操作 |
| `RequestPanel` | 审批、输入、动态请求 | schema 对应表单和 requestId |
| `InputEditor` | compose/search/filter/form | grapheme 光标、中文、粘贴 |
| `Renderer` | `Frame` 绘制 | 可见窗口虚拟化、布局缓存 |
| `TerminalAdapter` | raw/alternate screen/鼠标 | Crossterm 生产 Adapter、TestBackend fake |
| `ExportAdapter` | 脱敏 transcript | 异步分页、临时文件、原子替换 |

UI 本地状态不改变 Core 事实：

```rust
struct UiLocalState {
    focus: FocusPane,
    selected_agent: Option<AgentId>,
    selected_task: Option<TaskId>,
    selected_request: Option<RequestId>,
    timeline_filter: TimelineFilter,
    viewport: ViewportState,
    input: InputState,
    overlay: Option<Overlay>,
    follow_tail: bool,
    show_details: bool,
    show_raw: bool,
    dirty: DirtyFlags,
}
```

## 3. 信息架构与响应式布局

默认屏幕：

```text
┌─────────────────────────────────────────────────────────────┐
│ session / cwd / policy / root / gate / elapsed / resources  │
├───────────────┬───────────────────────────────┬─────────────┤
│ Agent + Tasks  │ Conversation Timeline         │ Details     │
│ Tree           │                               │ Context     │
├───────────────┴───────────────────────────────┴─────────────┤
│ Requests / compose / search / filter / shortcuts            │
└─────────────────────────────────────────────────────────────┘
```

- `>=140` 列：Agent/Task 树、时间线、详情同时显示。
- `100–139` 列：详情变成可切换面板。
- `80–99` 列：Tab 切换“树、对话、详情、请求、调度”。
- `<80` 列或高度过低：紧凑状态、当前事件和操作提示；不绘制复杂多栏。

窗口变化只改变投影，不销毁 app-server、任务或 Gate。顶部状态始终显示最终 sandbox、approval policy、根状态、Gate 目标、队列和资源槽。

## 4. Agent 树与工作流视图（现有视图回归、完整调度归 V2）

Agent 树只使用 Core 的 `threadId`、`parentThreadId`、`agentPath` 和真实 subAgentActivity。示例：

```text
● /root                  WAITING CHILDREN  gate 00:42
  ├─ /root/worker-a      RUNNING           ctx 71%
  └─ /root/worker-b      WAITING APPROVAL   req 1
```

节点显示 path、role、状态、turn 短 ID、最近活动、未读事件、pending 请求、上下文比例和 Gate 目标标志。文字状态必须和颜色同时存在。按创建序稳定排序，不因新日志重排。

工作流模式按 `w` 打开，显示：

- `workflowId/taskId/attemptId/epoch`。
- 父任务和依赖边，未满足依赖计数。
- `Draft/Queued/Ready/Running/WaitingChildren/WaitingApproval/Blocked/Succeeded/Failed/Cancelled/Unknown`；服务端退避提示单独显示，客户端不自动重试。
- ready queue 顺序、优先级、aging 等待时间。
- 模型、工具、shell、CPU/RSS、token 资源槽使用量。
- 阻塞原因：无槽位、依赖失败、等待审批、预算耗尽、暂停、退避、重启后未知。
- 当前 thread/turn/item/request/call ID。

用户可以在 ready 任务上提升/降低优先级、暂停工作流、恢复、取消子树或安排重试；运行中的模型和工具只在 Core 声明的安全边界停止。UI 不直接挪动依赖边，也不能把 Running 任务强行塞进队列。

## 5. 对话时间线与轨迹

每行使用稳定关联键：

```rust
struct TimelineKey {
    ingress_seq: u64,
    thread_id: ThreadId,
    turn_id: Option<TurnId>,
    item_id: Option<ItemId>,
}
```

跨线程排序使用 Core 接收序列 `ingressSeq`；服务端时间只作为辅助显示。事件类别：

```text
thread/turn lifecycle
agent message delta/final
reasoning/reasoning summary
tool call/tool result
approval requested/answered/resolved
user input requested/answered
subAgent started/interacted/interrupted/completed
compaction started/completed
usage updated
task queued/started/blocked/retried/completed
system/unknown
```

规则：

- delta 按 `(threadId, turnId, itemId)` 合并，final item 到达后校正，不重复显示。
- reasoning 只显示服务端提供的内容或 summary；没有就标记 unavailable，不由 UI 编写伪摘要。
- tool call 显示名称、参数摘要、cwd、状态、持续时间；tool result 显示退出码、stdout/stderr 大小和截断。
- approval 同时保留请求、回答、resolved；resolved 只关闭同一 requestId。
- subAgentActivity 提供父 handoff、子 thread、首轮、完成或失败的关联轨迹。
- compaction 显示触发原因、压缩前后 usage、summary、保留/替代范围和是否可重建。
- usage 是诊断记录，不能改变 Gate 或任务完成状态。

轨迹有四种范围：当前 thread、当前子树、根到当前 agent 的关联路径、所有 agent。按 `t` 切换。父 handoff item、子 agent 首轮、子终态和父恢复 turn 可以点击/按 Enter 互相跳转；如果协议没有明确关联字段，则只按真实 ID 和 ingress 顺序显示，不猜测。

过滤器支持 thread、turn、类型、状态、文本、当前 Gate 目标、仅 pending、仅书签。`/` 搜索 UTF-8 文本，搜索在独立任务执行，取消旧查询；被淘汰历史显示范围不可用。

书签引用 `threadId/turnId/itemId/ingressSeq`，不引用屏幕行号。日志淘汰后仍显示“内容已淘汰”和原 ID。

## 6. 上下文、压缩和 usage

Context 面板把上下文窗口、当前 turn budget、累计 usage、本地日志量分开，并给每个值来源：`server confirmed`、`local estimate` 或 `unknown`。

```text
Context window   used 91,240 / unknown     source server confirmed
input            62,100   cached 40,000
output           18,400   tools 10,740
remaining        8,760    source local estimate
compactions      2
```

如果 schema 没有上下文上限，显示 unknown，不能从模型名字猜测。上下文详情包括 input/cached/output/reasoning/tool 参数与结果、当前 turn、线程累计、剩余预算、测量时间和误差。

压缩历史按 `c` 打开：

```text
Compaction #2  trigger=server threshold
before=118400  after=63100
summary=server provided
retained=item 48..92  replaced=item 1..47
reconstructable=unknown
```

若 Core 没有 before 快照，必须标记“服务端报告，无法重建完整 before”，不能把本地旧日志当完整压缩前上下文。压缩信息只观察，不由 UI 自动触发或决定 Gate。

## 7. Skill 面板

`F6` 打开技能面板，分为“可发现技能”和“当前 turn 调用”。每项显示名称、来源路径/URI、版本或 hash、agent/thread/turn/item、状态、开始/完成时间和错误：

```text
research
source=C:\Users\Admin\.agents\skills\research\SKILL.md
state=loaded → invoked → completed
version=provided / unknown
permission=allowed
```

状态包括 discovered、loaded、invoked、completed、failed、permission denied、unavailable、unknown。文件存在不等于已加载；如果 app-server 没有 inventory 或 invocation 事件，显示 unavailable/unknown。技能调用可按 agent、turn、状态、名称过滤，并可从时间线跳转。

## 8. 请求面板与交互

`F2` 打开请求队列。每个请求保留 requestId、threadId、turnId、kind、创建序列、允许决策和状态。command approval、file approval、user input 使用不同表单；不能全部套 Yes/No。

审批显示 agent、命令/文件、cwd、diff 摘要、风险、有效 sandbox/approval policy 和允许选项。`approval-policy never` 固定显示，UI 不自动改策略。`Esc` 只关闭弹窗，不取消请求；明确的提交动作按表单允许选项执行；提交后禁用重复回答，直到 resolved、turn 终止或断连。过期 request 由 Core 拒绝。

GatePending 时子代理请求仍可回答；处理请求不释放 Gate、不启动根 turn、不修改等待目标。根输入进入队列并显示排队原因。未知请求显示方法名和 ID，Core 按协议拒绝或停在错误状态，不猜 schema。

## 9. 输入、中文、粘贴和快捷键

输入模式：Normal、Compose、Search、Filter、ApprovalForm、UserInputForm、ExportForm、Help。编辑器按 grapheme 移动光标，按 Unicode display width 计算列宽。支持已提交的 Windows IME 文本、中英文混排、emoji、全角字符和中文搜索；预编辑由终端/IME 处理，必须在 Orca PTY 和 Windows Terminal 实测。

优先使用 Crossterm `Event::Paste`：多行一次插入、不触发快捷键、限制大小、内容不写诊断。没有 Paste event 时通过 ClipboardAdapter，不拼 shell 命令。

目标快捷键（已实现按键以 README 为准；输入焦点中的普通字符不触发全局命令）：

```text
Tab/Shift+Tab 面板     ↑↓/j/k 选择      PgUp/PgDn 翻页
Enter 详情/提交        Home/End 首尾     / 搜索
f 过滤                 n/N 搜索结果      b/B 书签
t 轨迹范围             c 上下文/压缩      F2 请求
F6 skill               w 调度             r 原始/摘要
y 脱敏复制             e 导出             Ctrl+C 停止
Ctrl+P 命令面板        Ctrl+Q 请求退出并清理
Esc 关闭 overlay       F3 当前已有的根/child 会话切换
```

输入文本时普通字符不触发全局快捷键。根输入在 Ready 或允许 follow-up 时提交，GatePending 时排队，Stopping/Disconnected 时禁止执行。UI 不提供任意 child `turn/start`；子代理交互必须通过 Core 的合法请求或明确命令。

## 10. 渲染、背压和性能

渲染链：

```text
Core facts → Arc<UiSnapshot> → local projection → visible rows → Terminal::draw
```

`watch` 快照 latest-wins；控制事实由 Core 保证无损。UI 慢时可丢中间快照，不能丢 stop、审批回答或完成事实。普通 delta 合并到约 30 FPS；状态变化、审批、resolved、turn 完成即时绘制；Gate elapsed 约 1 Hz。没有变化不 redraw。

日志采用分块存储和页索引：每代理 4 MiB、全局 32 MiB，单行和工具输出另有限额；只布局当前视口，换行/宽度变化只重建当前块。搜索、导出和上下文详情在独立任务中执行，不能阻塞 JSON-RPC reader。

初始目标：输入到命令 p99 < 1 ms，snapshot 到状态 p99 < 5 ms，160×50 绘制 p99 < 8 ms，输入到可见反馈 p99 < 16 ms，空闲无固定 FPS，8 agent UI RSS < 128 MiB（不含 app-server）。记录 snapshot version、event-to-state、state-to-draw、draw、terminal bytes、frames、dropped snapshots、queue depth、page cache、search/export duration。

## 11. 复制、导出与脱敏

`y` 复制当前可见脱敏文本；导出 `e` 支持已保留的观察摘要，提供范围选择和脱敏预览。Alpha 不将 secret 或原始工具参数写入 journal，也不承诺重建完整对话。V2 可扩展子树/关联路径选择；缺失内容显示未保留。扩展正文导出必须是用户显式选择的现存内容，并保持脱敏。

默认脱敏 API key、Bearer token、cookie、环境变量疑似凭据、路径中的敏感值，并保留 thread/turn/item、父子轨迹、压缩范围和淘汰说明。导出先固定 sequence 和页范围，写临时文件后原子重命名；失败不停止 agent、不阻塞 Gate。

## 12. 停止、错误和可访问性

错误显示类别、位置、ID、最后有效序列、是否可继续和是否可重试。临时复制/搜索错误使用 toast；协议损坏、断连和终端恢复问题使用阻塞 overlay；子代理失败显示持久诊断行。

停止时显示 `Stopping → interrupting → closing → process cleanup`。不能伪造等待完成。所有退出和 panic 路径恢复 raw mode、alternate screen、鼠标捕获、光标和颜色状态。

状态不只用颜色：同时显示文字；`--no-color` 仍可读；focus 使用边框/标题；不持续闪烁；等待使用数字耗时。可选屏幕阅读模式只播报状态变化和最终消息，避免每个 delta 重复播报。

## 13. TestBackend 与真实终端验收

用 Ratatui TestBackend 测试 80×24、100×30、120×40、160×50、60×20、30×10：所有生命周期状态、GatePending、多个请求、stale turn、delta/final 去重、上下文 source 标签、compaction unknown、skill failure、中文宽字符、长输出截断、空树、断连和窄屏不 panic。

使用 reducer 测试：`GatePending → child progress → child approval → child completed → wait reply → parent continuation`，验证普通消息不释放 Gate、等待期间根请求数不增加。

在 Orca PTY 和 Windows Terminal 验证 alternate screen、panic 恢复、中文 IME、粘贴、多行 Markdown、大量 delta、resize、鼠标、慢终端、子代理审批、断连界面和进程清理。用脚本化假 app-server 覆盖大多数 UI 测试，真实 app-server 只做兼容和最终 PTY 验收。

## 14. Core 必须提供的事实

UI 要完整实现，Core 至少提供：稳定的 thread/turn/item/ingressSeq；父子 thread 关系和 agent path；request 所属 thread/turn 与合法选项；delta/final 去重；tool call/result 关联；subAgentActivity；compaction 前后统计/摘要（如 schema 提供）；usage 与上下文 source；skill inventory/call 状态（如 schema 提供）；可分页 timeline/log reader；单调 snapshot version；历史加载状态；停止/断连后的 pending 状态；以及 command accepted/rejected 结果。

如果当前 app-server 没有某项事实，UI 显示 unknown/unavailable，并保留证据来源，不扫描日志或本地文件补齐。Alpha 还必须覆盖可控时钟的静默/恢复、审批 requires_action、关闭提醒不影响 Core、宽窄终端无颜色显示，以及与 Python 消费者相同的 attention 和 progress_seq。
