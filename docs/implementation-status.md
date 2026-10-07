# 实施状态与验证

实现验证记录日期：2026-10-02；计划索引更新：2026-10-03。本次文档同步没有重新运行这些测试，不将历史通过数作为当前复验结论。

## 当前可用范围

- `--tui` 和 `--run` 共用真实 app-server/Core 路径；初始化、创建线程和 shell 预检完成后才发送任务。
- Core 独占执行状态。独立 reader 与单 writer 通过有界队列传递 JSONL；UI 只提交类型化命令和读取快照。
- 对话支持多轮输入、增量输出、final 校正、中文与 emoji 编辑、滚动、审批和多问题回答。
- 预检失败禁止提交任务；旧轮响应和终态不能覆盖新轮；中断确认不会伪造轮次完成。
- 未知外部结果会关闭执行通道，避免迟到事件恢复运行或自动重放副作用。
- 秘密回答不会进入对话记录；请求结束会清除回答缓冲，恢复原任务草稿。
- 启动前读取有效模型目录，生成仅将 `tool_mode` 改为 `direct` 的临时副本；app-server 持有副本并在退出时清理，不修改用户配置。
- 执行前准确核对 Codex CLI 0.159.2，验证初始化字段及新线程的 `cliVersion`；版本不符、查询失败或握手不完整时禁止模型轮次。历史入口继续离线可用。
- Core 登记真实子代理身份、直属关系和当前 turn/generation。原生 `subAgentActivity` 可先提供临时身份，首次 `turn/started` 后用一次 `thread/read` 补齐元数据；补齐期间不提前释放等待。
- 根线程注册 `wait_for_subagent_completion`：目标集合在接受时固定，无等待超时；全部当前轮次完成或任一失败/中断才回复，重复和旧轮事件不能释放新等待。
- 等待期间子代理审批/问题继续处理；根输入最多排队 8 项，当前根轮成功结束后按依赖提交，失败/中断保留为阻塞任务供检查、取消或显式重试。断连不伪造子代理完成。
- F3 切换根/子代理对话；宽屏显示代理列表，状态区显示等待进度、排队数和等待期间根 `turn/start` 增量。输入框明确标示根任务。
- Windows 在创建时将 app-server 和目录查询纳入 Job，关闭时确认已纳入进程树退出；终端退出恢复 raw mode、alternate screen 和 bracketed paste。

## 验证证据

自动测试穿过 `ClientHandle`、内存 duplex transport 和 Ratatui TestBackend，覆盖：

- 初始化和零模型轮次预检；通知先于 RPC 响应；旧轮 started/response/completed 的过滤。
- 预检失败禁止绕过；启动期间中断一次；审批 resolved；EOF 和 writer 错误。
- 数字/字符串 ID、空 result、非法响应结构、帧上限、分片接收和取消安全。
- 消息校正、UTF-8 字节截断、秘密输入失效，以及 40×12、80×24、160×45 终端布局。
- Windows 实际启动隐藏的 PowerShell 父子进程，持有子进程句柄，确认父进程退出后 Job 关闭终止子进程。
- 原生活动先到达、元数据后到达、后续轮次 started 的两种顺序、重复活动、非直属目标，以及身份 RPC 失败。
- 8 个子代理中的任一失败可提前释放；根结束后其余活动子代理的审批仍可回答。
- 虚拟时间推进一小时仍保持等待；取消/断连保留未完成 outcome；等待 ID 在不同根 turn 中复用不会误判为重复。
- 40×12、80×24、160×45 的子代理对话与 Gate 渲染、F3 只修改本地视图，以及队列满时保留输入草稿。

本轮通过 55 项默认测试，以及 fmt、check、clippy 和 release build。3 项可选真实 app-server 测试曾全部通过；最新复验中的本地 Gate、Windows 配置覆盖测试通过，本机 Ready 启动测试再次超时。后续验证结果保留在下方记录中。

真实 Codex 0.159.2 验证：

```text
cargo run --locked -- --check-shell --windows-sandbox unelevated
# shell preflight: passed (zero model turns)
cargo run --locked -- --run "只回复 READY" --sandbox read-only --windows-sandbox unelevated
# READY
```

本机 elevated 沙箱预检超时；相同命令在显式 unelevated 沙箱下通过。首次 read-only 会话预检也曾超时，随后独立预检与重复会话通过。保留失败状态和配置建议，不自动扩大权限或重试。

本轮 release 版 `--run "只回复 READY" --sandbox read-only --windows-sandbox unelevated` 再次在预检阶段超时，未发起模型任务；随后相同配置的独立 `--check-shell` 通过。退出后未发现应用进程或本轮临时模型目录残留。该本机预检不稳定性仍未消除，不能将本轮计数 provider 验证视为外部 provider 实跑成功。

预检时显示 `CheckingShell`，初始化期间提交的任务继续留在初始队列。Ready 集成测试的观察窗口为 35 秒，覆盖 Core 的 30 秒 RPC deadline；失败时报告实际阶段和错误，不通过扩大 sandbox 或重试任务取得成功。

扩大测试观察窗口后，实际收到 Core 的 `Unknown` 与 `Shell preflight timed out`，不是测试框架提前退出。独立零模型诊断分别运行了无 dynamic tool、仅 dynamic tool、dynamic tool 加 feature 配置三种启动请求，三者的 shell 检查均返回 exit code 0。该对照未找到可归因于工具注册或 feature 参数的失败条件；仍需继续定位本机 app-server/sandbox 与执行时序。

两个需安装 Codex 的可选测试已单独运行：

```text
cargo nextest run --locked --run-ignored only live_windows_launch
cargo nextest run --locked --run-ignored only live_windows_core_reaches_ready
```

前者通过 `config/read` 确认 Windows sandbox 覆盖生效；后者验证真实 Core 启动到 Ready、根轮次为零，连续三次通过。这两个测试默认忽略，避免普通单元测试依赖本机认证和 Codex 安装。

### 本地 provider 的真实 Gate 验收

新增的可选测试运行真实 Codex 0.159.2 app-server 和 `tests/fixtures/gate_provider.py`：

```powershell
$env:NATIVE_AGENT_TUI_PYTHON = (python -c 'import sys; print(sys.executable)')
cargo nextest run --locked --run-ignored only live_app_server_gate --no-capture
```

独立 `CODEX_HOME` 不含认证数据；所有模型请求送到 localhost。测试复制安装版本的 bundled catalog，设置 direct，并仅为普通 Responses SSE fixture 关闭 `use_responses_lite`。该测试配置与正常启动时保留用户目录其他字段的行为分开验证。

第一次等待前根 provider 收到 2 次请求、子 provider 收到 1 次；保持子响应未返回，300 ms 后重新采样，根仍为 2 次。释放子响应后发送原生 `followup_task`，第二次等待时根为 4 次、子为 2 次；再次保持响应并采样，根仍为 4 次。最终释放后根为 5 次并输出 `GATE_DONE`。两个等待窗口内额外根 provider 请求均为零，且第二次 Gate 绑定新 generation。这个证据不依赖 UI 图标或本地 turn 计数。

Windows ConPTY 交互验证已收到 `TUI_READY`，同一线程第二轮输入中文任务后收到 `SECOND_READY`，两轮均由真实 `turn/completed` 进入 Completed。

生成期间 Ctrl+C 已收到真实 Interrupted 终态；Ctrl+Q 退出后恢复终端。快速中断曾返回 `no active turn to interrupt`：现在保留会话、显示拒绝原因并继续接收终态，允许用户再次明确请求中断。相应竞态有自动回归测试。

## 尚未完成的设计要求

1. 在已接入的 DAG、根槽位、Gate 和任务控制之上，实现细粒度资源预算及持久调度恢复；直属 child 数量、观察深度和活动 turn 数已支持启动配置。当前能力和验收见[调度接入验证](scheduler-validation.md)。
2. 在已接入的脱敏 journal、实时 JSONL、只读历史和手动导出上，继续补齐上下文压缩及 skill 的服务端事实记录；执行恢复与完整 transcript 重建没有实现。
3. 扩展协议兼容快照，验证一层子代理限制、整棵代理树停止，以及正在运行的子代理收到普通消息时的轮次语义。
4. 提供完整代理/任务树、详细工具轨迹、compaction/skill 事实和 usage/context 面板；当前只显示服务端确认的 token 汇总，缺失字段明确为 unavailable。

`scheduler.rs` 已接入真实 Core 和 UI，具体证据见[调度接入验证](scheduler-validation.md)。`rpc.rs` 和 `diagnostics.rs` 的独立接口仍不能作为完整诊断已接入的证据。本地 provider 验证同一直属子代理的两轮；8 子代理场景通过内存 transport 验证，尚未扩展到真实 provider 并行计数。

Windows 已通过创建时的 Job 属性建立进程所有权，消除先创建、后附加之间的窗口；原生 suspended/硬终止与关闭确认见[进程所有权](windows-process-ownership.md)。Shell 预检仍存在间歇超时，启动可靠性门禁尚未通过。

## 2026-10-03 计划同步（尚未实现的契约）

产品支持范围见[产品计划](native-agent-tui-plan.md)，活动证据、Attention、JSONL/Python、journal 只读回放见[可观测性契约](native-agent-tui-observability.md)。现有 Gate/child 能力继续保留；V1/Alpha 优先可靠单 agent 与观察恢复，V2 验收 1–3 个直属 child 的完整调度。

新增契约不代表 `--json-events`、`--replay`、`--since`、持久 journal 或 Attention 已可使用。S0 必须对当前实现重新核对，Issue 关闭需对应测试证据。

## S2.5a 活动观察接入

Core 已提供按 agent/tool/request 隔离的活动证据、进展序号和注意级别；CLI 支持显式全局配置和分类阈值覆盖，TUI 通过 F10 临时设置、F11 查看证据。观察计时器不会发起执行或释放 Gate。使用说明见[活动观察](activity-observation.md)。

本次 85 项默认测试、3 项可选真实 app-server 测试及 Python 消费 fixture 通过；fmt、check、clippy、release build 通过。真实 Gate fixture 验证静默升级时 provider 请求和根进展序号不变。完整记录和验证范围见[观察验证](observation-validation.md)。后续 journal 接入结果见下节。

## S9a 持久日志与只读回放

Core 默认写入脱敏状态，首个快照先于 app-server 启动提交；写入确认与 UI 投影版本分开，持久化故障进入 Unknown 并停止执行。CLI 新增 `--sessions`、`--replay`、`--since` 和 `--journal-dir`，`--json-events` 当前只支持回放。读取保留历史证据和不确定结果，不启动模型或回答旧审批。

本轮 102 项默认测试、3 项可选真实 app-server 测试、Python 持久消费者及 fmt/check/clippy/doctest/release build 通过。完整记录见[日志回放验证](journal-validation.md)。实时 stdout 背压/断管、脱敏导出、观察恢复交互和 Alpha 完整发布门禁仍未完成。

## S2.5b 实时 JSONL 与输出所有权

`--run --json-events` 和 `--workflow --headless --json-events` 已接入独立写线程，只读已提交日志，支持慢消费者追赶和最终关闭快照。持续堵塞与断管进入有界清理，活动外部结果保留 Unknown，已确认终态保留；非交互请求通过 Core 保存动作理由。新日志使用 schema 2，schema 1 历史继续支持。

本轮 115 项默认测试、4 项可选真实 Codex 测试、11 个 release CLI 管道场景和 Python 消费者通过；fmt/check/clippy/doctest/release build 通过。多根工作流的 CLI、日志与消费者使用一致的最终汇总，保留各任务已确认终态。完整记录见[实时 JSONL 验证](jsonl-validation.md)。手动导出、观察恢复入口、协议兼容门禁、完整进程创建竞态验证及 Alpha 发布验收仍未完成。

## S9b 历史观察与脱敏导出

实时 TUI 的 F12 和离线 `--history [SESSION_ID]` 已接入独立只读服务，支持会话列表、逐事件导航和固定前缀刷新。历史时间冻结，当前执行阶段与请求数仍可见；历史审批、取消和重试不能进入 Core。

`--export SESSION_ID [--since SEQ] [--output NEW_FILE]` 和历史页的 `e` 支持预览后保存。导出保留结果、关系及证据，将自由字符串身份替换为稳定别名；预览范围固定，保存不加入后续记录。已有文件与托管 journal 受保护；写入失败不发布部分目标。使用和文件系统限制见[历史与导出](history-export.md)，最终复验结果见[观察恢复验证](history-validation.md)。

更广协议兼容快照与必需能力校验、Shell 启动可靠性、搜索/提醒关闭/完整请求详情，以及 Alpha 试用与发布验收仍待完成。历史导入和自动执行恢复没有实现，V2 完整调度继续保留独立验收范围。

## S10a 持续验证入口

新增 `scripts/verify.py`，统一 Rust 基线、release 构建与历史/回放/观察/实时 JSONL 原生 fixture；每个命令失败立即停止。nextest 不可用时使用 Cargo 原生测试，可选 `--live` 验证锁定 Codex。`.github/workflows/verify.yml` 已配置 Windows、Rust 1.96.0 和 Python 3.12，使用固定 action SHA 与只读权限。

本地 Cargo 路径完成 128 项默认测试和全部原生 fixture；失败工具链用例在首个检查停止。workflow 通过 actionlint；远端 Actions 尚未运行。最低 Rust 1.89、Linux/macOS、终端交互和 Alpha 发布门禁保持单独验收，详见[持续验证](ci-validation.md)。

## S3 Windows 创建时的进程所有权

app-server 和目录查询共用私有 `owned_process` 接口。Windows 通过 `PROC_THREAD_ATTRIBUTE_JOB_LIST` 在创建时建立 Job 所有权，主线程随后才恢复；继承列表限于三条 stdio。关闭时确认 Job 活动进程归零，超时或查询失败返回清理错误；取消 wait 后可重新等待，保留真实退出码。命令参数及批处理长度在创建边缘校验。

清理本包编译缓存后，`python scripts/verify.py --live` 全部通过：128 项默认测试、4 项真实 Codex 测试、11 个 Windows 原生所有权场景及已有原生 fixture；fmt/check/clippy/doctest/release 通过。ConPTY 验证历史动作只读、秘密草稿保持掩码、退出恢复终端，RPC 与 journal 均没有提交测试回答。

此前本轮完整验证及重复 Ready 测试曾出现预检超时。固定旧版 `f6e7a70` 的独立对照也捕获同类 Unknown，零模型轮次；共享编译缓存的三路径探针统计已排除，不能作为验收证据。最终单次通过不证明启动稳定，也未确定超时根因。创建边界、样本与限制见[Windows 进程所有权](windows-process-ownership.md)，持续验证范围见[CI 验证](ci-validation.md)。Alpha/V2 尚未完成。

## S1a 后端版本与启动协议门禁

生产启动只接受准确的 `codex-cli 0.159.2`；有界版本查询通过后才读取目录和启动 app-server。Core 校验初始化必需字符串，以及新线程报告的固定 `cliVersion` 和非空身份。拒绝时不能发起模型轮次，banner 和私有初始化 metadata 不进入错误或 journal；查询清理未获确认时保留 CleanupUncertain。

新增八份导出 schema 指纹、两个保留源 schema、脱敏初始化与合成启动回放夹具。默认 CI 检查本地夹具及九个原生 CLI 场景；可选真实 schema 测试在独立目录重新导出并比较，字段类型漂移注入会返回失败。范围与升级步骤见[Codex 兼容门禁](codex-compatibility.md)。完整 typed event 与更多协议快照尚未全部实现。

最终 `python scripts/verify.py` 通过 134 项默认测试、九个兼容场景、11 个 Windows 进程所有权场景和原有原生 fixture；fmt/check/clippy/doctest/release 通过。Cargo 原生测试路径同样通过 134 项。最新完整 `--live` 的五项真实检查四项通过，Ready 因约 31 秒的 shell 预检超时进入 Unknown，零模型轮次，整次命令失败。最终源码的 schema 专项复验通过，不能据此替代 Ready 或完整真实验证。启动可靠性及 Alpha/V2 仍未完成。

## Windows 隔离启动预检

正常 Windows 启动在主线程版本校验后创建辅助 app-server，只做有界版本查询、初始化和 shell 检查；辅助端不创建线程或模型轮次。主进程的 MCP 保留，两端共享 catalog 生命周期、使用独立 RPC ID。成功且辅助进程树清理确认后才进入 Ready；超时、无效响应和清理不确定性保留 Unknown，取消会 join 所持有的检查，不自动重试。

独立源码/target 的 20 组对照为原路径 18/20、隔离路径 20/20。初次生产接入十次启动通过；补齐响应不确定性后，最终源码另跑五次，全部确认 Ready、零根请求/轮次、进程清理及 journal 关闭。最终 `python scripts/verify.py --live` 全部通过：143 项默认 Rust 测试、五项真实 Codex 测试、十二个兼容场景、八项启动 runner 测试、三项进程身份检查、11 个 Windows 所有权场景及原有原生夹具；fmt/check/clippy/doctest/release 通过。清理 fixture 核对 PID 与创建时间并有界确认退出，覆盖主/辅助父子进程。

详细边界、历史失败和验证限制见[启动可靠性](startup-reliability.md)与[Codex 兼容门禁](codex-compatibility.md)。结果限于本机锁定版本；搜索、提醒关闭、完整请求详情、交互和试用验收，以及 Alpha/V2 仍待完成。

## UI 本地静默提醒

Ctrl+W 关闭/恢复所选代理当前的静默提醒；记录仅在 UI 内绑定活动身份、证据序号和注意级别。新证据只恢复对应活动，新轮次/尝试/时钟来源及注意级别变化重新提示。审批、requires_action 和 Unknown 不能关闭，Core attention、journal、JSONL 及 Gate 保持原事实；任务与秘密草稿保留。

15 项 UI 测试通过，其中四项新增回归覆盖活动隔离、身份/证据更新、无命令副作用、草稿和宽窄终端呈现。完整 `python scripts/verify.py` 通过 147 项默认 Rust 测试与全部原生夹具，fmt/check/clippy/doctest/release 通过。Windows 终端使用持续运行的 Python fixture 验证中文/emoji 草稿、F11、提醒关闭/恢复；切换前后只有一个根请求、零中断请求，静默进展序号保持 3。随后明确中断、退出，确认进程清理、journal 关闭和只读回放的 Interrupted。最终 CLI 帮助也已同步并完成 fmt/check/clippy/release 验证。

使用与范围见[活动观察](activity-observation.md)和[本地提醒验证](attention-reminders-validation.md)。本次终端检查没有调用真实模型，前一项启动修复的五项真实 Codex 验证保持独立记录。搜索、完整请求详情、更广终端和试用验收，以及 Alpha/V2 仍待完成。

## Usage 身份事实（2026-10-06）

Core 快照新增 typed `UsageFact`，携带 server/local/unknown 来源和 thread/turn/generation 身份。新 root 或 child generation 先发布 unavailable 占位；只有精确匹配当前 thread/turn 的更新才能填充事实，累计 total 仍保持单调。Context/F11 只消费该事实，缺少身份或当前 turn usage 时显示 unavailable，不从 agent id 推断归属。回归覆盖 root/child 精确接受、旧 turn 忽略、新 generation 未确认、旧快照身份默认值和宽窄 UI 渲染。

## S4 请求身份与 UI 详情

审批、输入和 headless 动作改为携带 RPC ID、线程、轮次与接收序号；Core 校验完整身份、当前轮次和 responding。同轮次复用 ID 产生新身份，重复投递仍保留原身份。UI 固定选择，过期后需明确选择当前请求；每个提交立即锁定，输入草稿分别绑定完整身份。

F2 已接入可滚动请求详情，显示命令/目录、允许决策、策略及权限提案、输入问题和选项。文件 diff 只关联同一线程/轮次/item 的服务端事件，缓存与预览有界，缺失或裁剪明确可见；详情不进入 journal、JSONL 或历史。Esc 关闭详情并保留草稿；答案超限可修正，秘密仍保持掩码。目前支持 accept/decline，其他决策表单、风险事实与子代理独立策略保持 unavailable。

最终 `python scripts/verify.py` 通过 155 项默认 Rust 测试和全部原生夹具，fmt/check/clippy/doctest/release 通过。新增原生 JSONL 场景验证同 ID 两次独立审批、文件请求、秘密输入中断及脱敏持久结果。Windows ConPTY 使用假 app-server 验证详情导航、重复提交锁定、过期选择、中文/emoji 秘密回答、Esc 保留和任务草稿恢复；一个根请求、一个 shell 检查、一个明确中断，退出后确认进程清理、journal 关闭和只读 Interrupted 回放。使用、内存边界与验证范围见[请求详情](request-details.md)。真实审批兼容性、更广终端/IME、搜索、试用与 Alpha/V2 发布验收仍待完成。

## S4a 真实请求往返与取消审批

真实 Codex 普通命令审批提供 cancel 而通常没有 decline。新增 Ctrl+B 按服务端允许列表取消所选审批，拒绝操作并中断所属轮次；Ctrl+N 保持 decline。完整请求身份和立即锁定继续约束所有回答，Core 等待服务端 resolved 与终态确认；输入详情新增有类型的 blocking 与废弃时间提示，不触发自动回答或状态改变。窄屏独立显示三个允许决策。

最终 `python scripts/verify.py --live` 全部通过：160 项默认 Rust 测试、八项真实 Codex 检查和全部原生夹具，fmt/check/clippy/doctest/release 通过。三项新增真实请求测试使用 localhost provider 和独立 Codex home，验证取消命令无副作用、文件 decline/accept、接受命令仍受 read-only 约束、中文/emoji 输入与明确 RequestResolved 证据；持久结果和清理均确认。输入工具仅在夹具中显式开启普通模式的可选功能。

Windows ConPTY 另确认不允许的 Ctrl+N 不发送回答、连续 Ctrl+B 只发一次 cancel、零额外根中断、Interrupted 与草稿保留；退出后进程身份检查和脱敏只读回放通过。边界与复现见[请求兼容验证](request-compatibility.md)。搜索、更广终端/IME、子代理真实审批、试用任务集及 Alpha/V2 发布验收仍待完成。

## S8a 实时对话搜索

Ctrl+F 已接入独立查询编辑区与可取消的后台任务。搜索 Core 有界对话快照，支持中文/emoji、线程/子树/关联路径/全部代理范围和 turn/角色/消息状态过滤。结果固定来源，按线程/轮次/item/角色与 UTF-8 位置定位；刷新、淘汰或文本变化明确禁用旧命中。查询字段最多 1024 字节，保留最多 512 个命中并显示准确总数。原输入和秘密草稿保留，搜索无 Core 执行命令，查询与结果不进入 journal/JSONL。

九项新增回归验证 Unicode、身份与范围、取消/释放、字段/命中上限、未投递查询立即禁用旧结果、秘密草稿、窄屏帮助、定位/resize/翻页及 Core/Gate 无副作用。最终 `python scripts/verify.py --live` 全部通过：169 项默认 Rust 测试、八项真实 Codex 检查及全部原生夹具，fmt/check/clippy/doctest/release 通过。

Windows ConPTY 用假 app-server 确认中文查询与过滤、命中定位、草稿恢复和返回审批；搜索前后 RPC、原始消息/接受证据计数与各活动 progress_seq 不变。明确取消后退出，确认进程清理、journal 关闭、脱敏 Interrupted 回放。多行转义注入本次只显示首行，不能作为原生粘贴验收；完整事件时间线、书签、工具/历史搜索、Windows Terminal/Orca IME 与粘贴、试用任务集及 Alpha/V2 发布验收仍待完成。使用与限制见[实时对话搜索](conversation-search.md)。

## Windows 多行粘贴与兼容提交键

Windows 终端输入改为有界 VT/Win32 记录适配，完整标记的多行中文/emoji 粘贴不再把换行当成 Enter。一次最多 32 KiB，超限整段丢弃并提示；Ctrl+O 换行、Ctrl+S 明确排队或提交当前答案。搜索和秘密/任务草稿保持独立，粘贴内控制字符不触发命令。

本轮 `python scripts/verify.py --live` 全部通过：182 项默认 Rust 测试、八项真实 Codex 检查和全部原生夹具，含 12 项 ConPTY 输入检查；fmt/check/clippy/doctest/release 通过。真实 TUI 夹具另行验证三行秘密答案精确提交一次、粘贴和搜索零额外 RPC、进程清理及脱敏回放。

本机 ConPTY 跨写入拆分粘贴标记会丢失前缀，物理 Shift/Ctrl+Enter 可能变为普通 Enter；这两项宿主能力探针当前失败并保留复现命令。Windows Terminal、Orca 剪贴板及 IME 仍待验收，Alpha/V2 未完成。详见[终端输入验证与限制](windows-terminal-input.md)。

## S8b 有界实时证据时间线

`Ctrl+T` 已接入 Core 拥有的有界证据归档。时间线记录活动身份、状态、工具 item、请求完整投递引用和等待目标；按 Core 接受证据的顺序展示，并明确不等于原始 ingress 或 journal 游标。归档最多 512 条、256 KiB 元数据，整条淘汰并显示高水位和缺口；快照共享不可变条目，tick、配置或 UI 浏览不会生成新证据。

时间线支持线程/已确认子树/关联路径/全部范围、事件类别、执行状态、当前待请求、元数据和精确 turn 过滤；Enter 只定位仍保留的消息或完整身份匹配的当前请求。b/B 书签最多 64 条/64 KiB，仅保存在本次 TUI。淘汰、旧 session、复用 RPC ID、陈旧请求或缺少正文都会明确提示，不会自动操作其他请求。原始输出、reasoning、compaction 和历史跨 session 搜索仍显示 unavailable；F11 证据视图现在显示服务端确认的 input/cached/output/reasoning/total token 字段及 context window，缺失值保持 unavailable。

新增回归覆盖证据预算、冻结等待目标、重试身份、过滤/定位、淘汰书签、同批按键、秘密草稿、窄宽屏帮助和持久化故障 Unknown。Windows ConPTY 假 app-server 夹具验证过滤、书签、resize、两次同 ID 审批、历史切换后的秘密答案和零额外观察 RPC，并已接入 `scripts/verify.py`。

当前 `python scripts/verify.py --live` 通过：196 项默认 Rust 测试、8 项真实 Codex 检查、全部原生夹具及时间线 TUI 夹具；fmt/check/clippy/doctest/release 通过。Windows Terminal、Orca 剪贴板/IME、完整事件轨迹、原始工具结果搜索、试用任务集和 Alpha/V2 发布门禁仍待完成。详见[实时证据时间线](evidence-timeline.md)。

## S9c 历史证据搜索

历史会话列表和详情都支持 `/`、`Ctrl+F` 输入脱敏证据元数据，F6 在生命周期、输出、工具、请求和等待类别间切换；Up/Down 选择命中，Enter 重新打开对应 session/event。列表搜索分别固定所有当前保留 session 的已提交前缀，详情搜索固定当前 session；搜索在线程中取消旧查询，排除未提交尾部，并限制查询、命中和元数据内存。结果可以用内部身份字段匹配，但界面只显示脱敏摘要；提示、答案、秘密、命令、路径和原始工具输出不会进入结果。

新增回归覆盖真实历史搜索、取消旧查询、脱敏结果、事件定位和 Ratatui 渲染。2026-10-03 的 `python scripts/verify.py --live` 通过 200 项默认 Rust 测试、8 项真实 Codex 检查、全部原生 fixture、fmt/check/clippy/doctest/release；历史搜索使用与实时执行隔离的只读线程。跨 session 全局搜索、原始工具结果搜索和 Alpha/V2 发布验收仍未完成。详见[历史证据搜索](history-evidence-search.md)。

## 易用性迭代（2026-10-06）

用 ConPTY 驱动 release 版 TUI（真实 Codex 仅用于预检复现，其余场景使用 `jsonl_app_server.py` 假服务端，不发起模型请求）逐项试用并修复：

- 启动失败可见：预检超时后原先只显示 `Unknown`，进行中提示遮住错误；现在 Core 出错时清除过期提示，状态区始终先显示错误原因。Windows 预检超时提示 `--windows-sandbox unelevated` 或在 Codex `config.toml` 持久设置 `[windows] sandbox = "unelevated"`。
- 不可用会话：Core 快照新增 `startup_blocked`；预检失败、Unknown、断连后按 Enter 不再提交，说明原因并保留草稿。运行中按 Enter 提示 Ctrl+S 排队；轮次失败显示 `Turn failed` 及继续方式，Core 原始错误不变。
- 交互：Esc 不再清空草稿；F1 改为分组按键浮层（80 列两栏完整显示）；底栏把 F1 放在最前；顶部隐藏为零的计数、缺失 token 汇总和已结束活动行；状态区无内容时收起。
- 请求详情首屏先显示命令、文件预览或问题，元数据移到分隔线后；仅允许取消时说明取消语义；修复提示行残留文字。
- 对话：You/Agent 标题加粗着色；向上滚动时标题显示剩余行数和 Ctrl+End。每轮工具调用以灰色摘要行保留在对话中（命令首行、状态、退出码、耗时，最多 40 行），轮次结束后仍可见；`ConversationItem` 的 Debug 只输出正文长度。
- 夹具：Python 夹具按 UTF-8 读取标准输入，修复中文 Windows 下秘密回答被 GBK 解码导致的误报。

验证：`python scripts/verify.py` 通过（376 项默认 Rust 测试、全部原生夹具含 ConPTY TUI 场景，fmt/check/clippy/doctest/release）。本轮未运行 `--live` 真实模型测试；本机 elevated 沙箱预检超时仍存在，需按提示使用 unelevated。

## 真实后端验证（2026-10-07）

- `python scripts/verify.py --live` 通过：默认检查加 9 项真实 Codex 0.159.2 测试（真实问题 ID、Gate 零父请求、skills、零模型轮次 Ready、CLI JSONL 回放等）。
- 真实 Codex TUI（ConPTY，`--sandbox read-only`）：问答两轮、token 用量、工具摘要行、命令审批面板与详情首屏均正常；审批后退出无残留，文件未写入。据此修复：审批面板显示 root 而非线程 UUID，摘要剥离 `pwsh -Command` 包装，未设预算时省略 `/unavailable`。
- elevated 沙箱预检超时根因：Codex elevated 沙箱需要一次性管理员配置（UAC），从 app-server 启动时无法完成；配置后又发现 elevated 运行器不能解析用户目录下的裸名 `pwsh`。预检改用 `powershell.exe`，默认 elevated 预检约 2.6 秒通过；超时提示给出 `codex sandbox -- cmd /c echo ok` 配置步骤。
- ACP（dsh 0.2.0-rc.2）：启动、`session/new`、`session/prompt` 到达真实 agent，账户余额不足导致未完成模型轮次；prompt 错误改为映射 failed 终态，会话可继续。流式输出、工具和权限映射仍缺真实后端证据。
- 远端 CI 首次运行通过（run 37587593093）。未完成：Windows Terminal/Orca 的 IME 与剪贴板验收、固定任务集试用、ACP 完整模型轮次。

## 删除式精简（2026-10-07）

按 `refactor-goal.md` 的删除测试完成 12 处精简，净删 130 行（11 个文件，+181/−311）：删除 `display_text_for_cli` 转发、`CoreSnapshot` 手写 Default、重复的 `queued_inputs` 快照字段、单实现的 `CompletionGate` trait（具体类型改名为 `CompletionGate`，释放与旧 generation 过滤逻辑不变）、未使用的 ACP 配置构造函数和 `Outbox::mark_failed`；合并 journal 投影、UI 会话定位与请求面板打开、TUI 启动、输出观测错误分支、轮次终结清理和 RPC 等待登记中的重复代码。rebase 到当前 main 后 `python scripts/verify.py` 通过（380 项默认测试与全部原生夹具）。跳过的候选（行为或优先级有差异）见本次提交记录。

