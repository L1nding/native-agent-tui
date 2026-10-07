# Rust Agent TUI Core 详细设计

更新：2026-10-03。本文与 [UI 设计](native-agent-tui-ui.md)、[工作流调度设计](native-agent-tui-workflow.md) 共享身份和状态边界。发布范围以[产品计划](native-agent-tui-plan.md)为准，活动、Attention、JSONL 和恢复以[可观测性契约](native-agent-tui-observability.md)为准。

Core 是执行事实的唯一所有者。它启动并持有 Codex app-server，解析 stdio JSONL，维护线程与轮次，处理审批和用户输入，决定等待 Gate 是否释放，并向 UI 发布只读快照。UI 不直接碰 JSON-RPC、子进程或 Gate。

## 0. 发布范围与观察所有权

V1/Alpha 优先交付可靠单 agent、活动证据、审批/输入、journal 观察恢复和 JSONL 状态流。Gate/child 已有实现继续回归；完整调度与 1–3 直属 child 的产品支持归入 V2。本文类型示例为目标设计，当前事实见[实施状态](implementation-status.md)。

Core 保存 execution state、ActivitySnapshot 和独立 attention 投影；UI、Python 均消费此投影。计时可以更新 attention 与 snapshot_version，不能更新 progress_seq、启动模型或触发 Gate。不同 agent 的证据分别计时；未知 provider 阶段显示 unknown。

状态流 S2.5a 在状态 seam 后实现；S2.5b 的持久 JSONL 依赖 S9a journal 基础。恢复只读脱敏投影，重新执行必须由用户创建新 attempt。

## 1. 外部 Interface 与 Module

整个工程先保持一个 Cargo package。拆成 Module，不提前拆成多个 crate：

```text
src/
  main.rs          # 参数解析、启动、终端恢复
  config.rs        # CLI、配置合并、兼容性
  protocol.rs      # JSONL Envelope 与已验证 schema 类型
  transport.rs     # app-server 子进程、stdout/stderr/stdin
  rpc.rs           # 出站请求和管理 deadline
  state.rs         # 执行事实、代理、轮次、日志、usage
  gate.rs          # 无超时子代理等待
  interactions.rs  # approval/user-input 请求
  scheduler.rs     # 任务 DAG 和资源槽（详见 workflow 文档）
  client.rs        # 唯一执行所有者
  diagnostics.rs   # 有界指标和诊断
```

核心外部 Interface 保持小而深：

```rust
pub struct ClientHandle {
    pub commands: tokio::sync::mpsc::Sender<Command>,
    pub snapshots: tokio::sync::watch::Receiver<std::sync::Arc<CoreSnapshot>>,
    pub join: tokio::task::JoinHandle<ExitReport>,
}

pub enum Command {
    Start,
    SubmitRootInput { text: String },
    AnswerApproval { request_id: RequestId, decision: ApprovalDecision },
    AnswerUserInput { request_id: RequestId, answer: UserInputAnswer },
    RejectRequest { request_id: RequestId, reason: String },
    Stop { mode: StopMode },
    Quit,
}
```

`ClientHandle` 是 UI、脚本测试和未来其他前端共同使用的 Seam。命令不能携带任意 JSON；Core 必须再次检查生命周期、requestId、viewVersion 和策略。

### Module 职责

| Module | Interface | Implementation 隐藏的复杂性 |
| --- | --- | --- |
| `protocol` | `decode_line`、`encode`、已验证消息类型 | string/number ID、缺失 jsonrpc、未知字段、schema 兼容 |
| `transport` | 有序收发 Envelope、关闭进程 | stdin 单写者、stdout reader、stderr、EOF、子进程清理 |
| `rpc` | 发管理请求、关联响应 | pending 表、管理 deadline、不可重试副作用 |
| `state` | 应用事件、只读摘要、分页日志 | thread/turn/item 关联、去重、generation、字节预算 |
| `gate` | 接收等待、应用事件、取出一次性结果 | 直属子代理解析、当前轮绑定、旧事件过滤、幂等回复 |
| `interactions` | 创建/回答/解决请求 | 不同 schema、过期 request、resolved 竞态 |
| `client` | `Command`、snapshot、退出报告 | 单一执行所有者、顺序、目标续接、错误分类 |

`transport` 是真实的外部 Seam。生产使用 `StdioAdapter`，测试使用 `ScriptedTransport`；二者都必须提供相同的消息顺序和断连语义。

## 2. 协议与进程

当前 app-server 通过 `--listen stdio://` 使用按行 JSON，线上省略标准 JSON-RPC 的 `jsonrpc` 字段。初始化为 `initialize` 请求、响应后发送 `initialized` 通知。dynamicTools 需要 `experimentalApi` 能力。该行为以当前 Codex 二进制导出的 schema 和兼容快照为准，不把 crates.io 的同名协议包当作权威。

```rust
pub enum RpcId { Number(i64), String(String) }

pub enum Envelope {
    Request { id: RpcId, method: String, params: Option<Box<serde_json::value::RawValue>> },
    Notification { method: String, params: Option<Box<serde_json::value::RawValue>> },
    Response { id: RpcId, result: Option<Box<serde_json::value::RawValue>>, error: Option<RpcError> },
}
```

客户端 RPC ID、服务端 request ID、dynamic tool `callId` 是三种身份，必须分开保存。单行上限初始 16 MiB，入站控制与 telemetry 队列总预算初始 64 MiB。超过预算应进入可见过载错误，不能丢掉 `turn/completed`。

启动顺序：

1. 解析 Codex 可执行文件、cwd、prompt 文件、sandbox、approval policy、模型和 schema 版本。
2. 如需 direct tool catalog，复制到临时目录，只把副本的 `tool_mode` 改成 `direct`；不写回用户配置。
3. 启动 `codex ... app-server --strict-config --listen stdio://`，stderr 独立排空。
4. 发送 `initialize`，声明 `experimentalApi: true`，收到响应后发送 `initialized`。
5. `thread/start` 注册 dynamic tool、developer instructions、cwd、策略和已验证的 multi-agent 配置。
6. 用 `command/exec` 做 shell 与目标文件可读性预检。失败就停在启动错误，不调用模型。
7. `--run` 时设置 goal 并发起根 turn；`--check-shell` 只输出检查结果后关闭。

Codex 0.159.2 的本地兼容基线为：

```text
features.code_mode = false
features.code_mode_only = false
features.multi_agent_v2.enabled = true
features.multi_agent_v2.wait_agent_enabled = false
features.multi_agent_v2.expose_spawn_agent_model_overrides = true
allowProviderModelFallback = false
```

首个受支持版本固定并写入兼容表；本段 0.159.2 是历史验证基线，不代表最新版本。能力探测仅允许已验证版本缺少可选能力时降级，必需能力缺失则禁止执行。升级 Codex 必须重新生成 schema、运行兼容测试和真实事件回放。模型请求配置与服务端返回的实际模型/effort 分开存储。

## 3. reader、writer 与事件归并

stdout reader 只负责读取 LF、检查大小、解码 Envelope、分派队列。它绝不能等待子代理、UI、审批或另一个消息，也不能直接回复 dynamic tool。stdin 只有一个 writer，所有出站消息按队列顺序序列化为 UTF-8 JSON 加 LF。

逻辑上分两条队列：

```text
control: response, server request, thread/started, turn/started/completed,
         status changed, resolved, transport closed
telemetry: agentMessage/delta, 普通 item 增量和诊断
```

Core 优先处理 control，同时保留 ingressSeq 因果关系；不能让终态越过必要前序 delta 后丢掉最终输出。telemetry 按 `(threadId, turnId, itemId)` 合并并记录覆盖范围；生命周期事实必须持久保留或显式报告过载失败，不承诺无限缓存。writer 失败后，所有 pending 操作进入失败/断连路径，不自动重放 `turn/start`、文件或 shell 操作。

统一处理顺序：

```text
Envelope → 协议校验 → CoreEvent → SessionState → CompletionGate
          → 出站响应（如有）→ CoreSnapshot → UI
```

先更新事实，再发布快照，避免 UI 已显示完成而 Gate 仍使用旧状态。

## 4. 状态模型

```rust
pub struct SessionState {
    pub phase: SessionPhase,
    pub root_thread_id: Option<ThreadId>,
    pub root_turn: Option<TurnRef>,
    pub agents: HashMap<ThreadId, AgentState>,
    pub pending_rpcs: HashMap<RpcId, PendingRpc>,
    pub pending_requests: HashMap<RpcId, PendingServerRequest>,
    pub gate: GateState,
    pub goal: GoalState,
    pub scheduler: SchedulerState,
    pub usage: UsageState,
    pub skills: SkillState,
    pub diagnostics: CoreMetrics,
}

pub struct TurnRef {
    pub thread_id: ThreadId,
    pub turn_id: TurnId,
    pub generation: u64,
}

pub struct AgentState {
    pub thread_id: ThreadId,
    pub parent_thread_id: Option<ThreadId>,
    pub aliases: BTreeSet<String>,
    pub agent_path: Option<String>,
    pub role: Option<String>,
    pub requested_model: Option<String>,
    pub actual_model: Option<String>,
    pub requested_effort: Option<String>,
    pub actual_effort: Option<String>,
    pub current_turn: Option<TurnRef>,
    pub turns: VecDeque<TurnSummary>,
    pub rearm: RearmState,
    pub log_store: LogStore,
}
```

日志按字节限制：每代理初始 4 MiB、全局 32 MiB；淘汰时保留 `truncated_before` 和 sequence 范围。final item 到达时校正 delta 聚合，防止重复显示。UI 获取摘要与分页日志，CoreSnapshot 不复制全量文本。

事件身份必须包含 `threadId`、`turnId`、`itemId` 和本地 `ingressSeq`。只带 thread 的 idle/status 不能覆盖有 turn 的终态；自然语言不能创建代理或判定完成。`thread/read` 只做一次性历史/身份补齐，不能当作实时订阅。

Core 应记录上下文和 skill 的事实，但明确来源：服务端确认、客户端估算、未知。上下文压缩至少保存触发原因、前后 usage、摘要/保留范围（如果 schema 提供）；skill 至少保存名称、来源、版本/hash（如提供）、loaded/invoked/completed/failed 状态。扫描本地 skill 文件不能证明服务端已经加载。

## 5. CompletionGate（现有能力回归与 V2 支持）

Gate 是深 Module，外部接口只暴露接受等待、应用事件、取消、断连和取结果：

```rust
impl CompletionGate {
    pub fn accept_wait(&mut self, request: WaitRequest) -> Result<WaitToken, GateError>;
    pub fn apply(&mut self, event: &GateEvent) -> GateChange;
    pub fn cancel(&mut self) -> GateChange;
    pub fn disconnect(&mut self) -> GateChange;
    pub fn take_result(&mut self) -> Option<(WaitToken, Vec<WaitTarget>)>;
}
```

动态工具定义：

```json
{
  "type": "function",
  "name": "wait_for_subagent_completion",
  "deferLoading": false,
  "inputSchema": {
    "type": "object",
    "properties": { "targets": { "type": "array", "items": { "type": "string" } } },
    "required": ["targets"],
    "additionalProperties": false
  }
}
```

收到根代理的 `item/tool/call` 后，Core 校验方法、tool、父 thread、参数集合和目标直属关系，快照目标集合与每个目标的当前 turn，然后保存 `PendingWait`。这个请求没有 deadline，不使用 sleep、polling、模型 heartbeat 或 UI 计时器。

```rust
struct PendingWait {
    server_request_id: RpcId,
    call_id: String,
    parent_turn: TurnRef,
    targets: Vec<WaitTarget>,
    captured_turns: HashMap<ThreadId, CapturedTurn>,
    accepted_at: Instant,
    phase: WaitPhase,
}
```

空数组表示接受时已知的全部直属子代理；之后新出现的代理不自动加入。UUID、agent path 和已登记别名可作为目标；未知、非直属或歧义目标直接返回参数错误。普通消息、工具结束、idle、status、UI 计时都不能释放 Gate。

唯一释放条件：所有绑定轮次 completed，或任一绑定轮次 failed/interrupted，或 transport 断连。回复一次后先把 wait 标为 `Responding`，重复事件不能第二次回复。子代理失败应作为结构化 outcome 返回，工具本身仍可 `success: true`。

### 零主请求保证

GatePending 期间禁止 `turn/start`、`turn/steer`、goal continuation、status 轮询、shell polling 和任何根模型调用；允许接收 stdout、处理子代理审批/输入、日志合并、一次身份补齐、停止和断连。

记录：

```text
parent_turn_start_count_at_gate_enter
parent_turn_start_count_at_gate_release
parent_turn_start_count_while_gate_pending
```

验收要求 Gate 内新增根 `turn/start` 为 0；provider 级请求数必须通过计数 mock app-server/provider 验证，不能从 UI 图标或 token usage 推断。

### 轮次复用竞态

同一 child 的旧轮完成后可能立即 follow-up。每个 agent 保存：

```rust
enum RearmState {
    Stable,
    AwaitingNewTurn { after_generation: u64, evidence_seq: u64 },
    BoundToNewTurn { turn: TurnRef },
    Ambiguous,
}
```

旧 turn 的迟到完成、重复完成和 idle 都不能释放新等待。只有明确的新 `turn/started` 绑定后，Gate 才观察新 generation。没有新轮证据时保持 pending 或报告兼容错误，不能猜测。

## 6. 审批、输入、目标和调度

所有服务端请求统一进入 `PendingServerRequest`，但每个方法使用自己的 schema。V1 必须支持 command approval、file change approval、tool user input；已有 wait tool 保持回归，完整多 agent 等待/调度归入 V2。Core 拥有 request，UI 只显示并提交带 requestId 的回答。

根 GatePending 时，子代理审批/输入仍可即时处理；根普通输入只进入队列，不启动根新轮。`serverRequest/resolved` 会关闭对应请求，过期回答返回 `AlreadyResolved`。未知请求、未知 dynamic tool 或未实现的权限请求要拒绝并停止不安全的 goal continuation，不无限重试。

调度器只负责 task DAG、ready queue、资源槽和手动控制，不能越过 Core 的 thread/turn 事实；详细状态见工作流文档。任务进入 `WaitingChildren` 时释放模型推理槽但保留 Gate/RPC 所需状态，Gate 结果只能通过当前 attempt 的幂等 join 恢复父任务。

## 7. 停止、断连和 Windows 清理

状态路径：

```text
Created → Launching → Initializing → Ready → Running
        → GatePending → GoalChecking → Completed
任何状态 → Stopping → ClosingTransport → Stopped
连接断开 → Disconnected；未确认的外部执行结果 → Unknown
明确 turn failed → Failed（断连本身不证明任务失败）
```

Stop 是幂等的：禁止新根轮，发送已验证的 interrupt，继续消费终态和 resolved，关闭 stdin，等待 app-server，必要时显式 terminate/kill，再清理临时目录和终端。不能伪造 completed 唤醒父模型，也不能重放未知副作用。

Tokio `Child` drop 不保证 app-server、shell 和孙进程退出。Windows 必须实测工作目录占用、shell 残留和 Job Object/进程树策略；没有证据就显示清理未确认。

## 8. 性能与诊断

初始目标：协议解码 p99 < 2 ms、事件到状态 p99 < 5 ms、child 终态到 wait 响应入队 p99 < 10 ms；无固定 FPS，等待时钟约 1 Hz。控制事件不可丢，telemetry 可合并，UI 只读有界快照。

必须记录：`transport_bytes_in/out`、消息数、解析与 apply 耗时、control/telemetry 队列高水位、日志字节、late event、重复终态、pending wait、Gate 时长、根 turn 计数、unsupported request、writer error、上下文压缩次数和 skill 状态变化。诊断默认脱敏，不写 token、密钥和完整任务正文。

先用 serde_json；只有 profile 证明解析/分配是主要成本后再测试 RawValue、缓冲复用、LTO、allocator 或 PGO。所有性能结论都要保留 workload、终端、构建模式和 p50/p95/p99/max。

## 9. 测试和放行条件

测试必须穿过 `ClientHandle` 和 `TransportAdapter` Seam：

- 数字/字符串 RPC ID、缺失 jsonrpc、未知字段、非法 JSON、超限行。
- stdout EOF、stderr 隔离、writer 失败、控制优先、telemetry 洪泛。
- thread/turn/item 关联、delta/final 去重、按字节截断、requested/actual model 分离。
- 单子、多子、失败、中断、普通消息、idle、空 targets、未知/非直属/歧义目标。
- follow-up 后旧 generation 不释放新等待；重复终态只回复一次。
- GatePending 期间根 turn 增量为 0；子代理审批可处理；根输入排队。
- stop、断连、未知请求、管理超时不能伪造完成或重复副作用。
- shell preflight 不产生模型 turn；策略和 catalog 覆盖正确。
- 真实锁定 app-server + 本地计数 mock provider 验证 dynamic tool 与请求计数。
- Windows app-server 与 shell 进程树清理有证据。

Alpha 按产品计划验收单 agent、S2.5、journal、Python 消费者和真实 smoke test；UI 可先通过 typed snapshot 和 fake replay 开发，不等待完整 scheduler。已有 Gate 路径仍须证明 reader 不阻塞、无等待 deadline、旧轮不能释放新等待、审批可路由、断连/停止不伪造完成。V2 增加 1–3 child 的真实并发与调度验收。

## 10. 必须验证的兼容假设

实现前固定 schema 快照并验证：stdio framing、experimentalApi、dynamicTools 注册位置、item/tool/call 参数、turn ID、父子 thread 字段、thread/read 语义、interrupt 方法、审批/输入 result schema、direct catalog 启动时机、Gate 内不会自动根续接、active session 不可被第二客户端安全接管、Windows 子进程清理和一层子代理限制。
