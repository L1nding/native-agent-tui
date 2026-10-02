# 实施状态与验证

更新日期：2026-10-02。

## 当前可用范围

- `--tui` 和 `--run` 共用真实 app-server/Core 路径；初始化、创建线程和 shell 预检完成后才发送任务。
- Core 独占执行状态。独立 reader 与单 writer 通过有界队列传递 JSONL；UI 只提交类型化命令和读取快照。
- 对话支持多轮输入、增量输出、final 校正、中文与 emoji 编辑、滚动、审批和多问题回答。
- 预检失败禁止提交任务；旧轮响应和终态不能覆盖新轮；中断确认不会伪造轮次完成。
- 未知外部结果会关闭执行通道，避免迟到事件恢复运行或自动重放副作用。
- 秘密回答不会进入对话记录；请求结束会清除回答缓冲，恢复原任务草稿。
- 启动前读取有效模型目录，生成仅将 `tool_mode` 改为 `direct` 的临时副本；app-server 持有副本并在退出时清理，不修改用户配置。
- Core 登记真实子代理身份、直属关系和当前 turn/generation。原生 `subAgentActivity` 可先提供临时身份，首次 `turn/started` 后用一次 `thread/read` 补齐元数据；补齐期间不提前释放等待。
- 根线程注册 `wait_for_subagent_completion`：目标集合在接受时固定，无等待超时；全部当前轮次完成或任一失败/中断才回复，重复和旧轮事件不能释放新等待。
- 等待期间子代理审批/问题继续处理；根输入最多排队 8 项，当前根轮成功结束后按依赖提交，失败/中断保留为阻塞任务供检查、取消或显式重试。断连不伪造子代理完成。
- F3 切换根/子代理对话；宽屏显示代理列表，状态区显示等待进度、排队数和等待期间根 `turn/start` 增量。输入框明确标示根任务。
- Windows Job Object 清理 app-server 的已纳入进程树；终端退出恢复 raw mode、alternate screen 和 bracketed paste。

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

1. 在已接入的 DAG、根槽位、Gate 和任务控制之上，实现完整资源预算、原生并发/深度限制及持久调度恢复。当前能力和验收见[调度接入验证](scheduler-validation.md)。
2. 在已接入的脱敏 journal、实时 JSONL 与只读回放上，完成手动导出、观察恢复入口、上下文压缩及 skill 的服务端事实记录。
3. 扩展协议兼容快照，验证一层子代理限制、整棵代理树停止，以及正在运行的子代理收到普通消息时的轮次语义。
4. 提供代理/任务树、搜索、详细工具轨迹、usage/context 面板和诊断指标。

`scheduler.rs` 已接入真实 Core 和 UI，具体证据见[调度接入验证](scheduler-validation.md)。`rpc.rs` 和 `diagnostics.rs` 的独立接口仍不能作为完整诊断已接入的证据。本地 provider 验证同一直属子代理的两轮；8 子代理场景通过内存 transport 验证，尚未扩展到真实 provider 并行计数。

Job 在进程创建后附加。现有清理测试覆盖附加后的后代；创建与附加之间的竞态仍需进一步消除或验证。

## S2.5a 活动观察接入

Core 已提供按 agent/tool/request 隔离的活动证据、进展序号和注意级别；CLI 支持显式全局配置和分类阈值覆盖，TUI 通过 F10 临时设置、F11 查看证据。观察计时器不会发起执行或释放 Gate。使用说明见[活动观察](activity-observation.md)。

本次 85 项默认测试、3 项可选真实 app-server 测试及 Python 消费 fixture 通过；fmt、check、clippy、release build 通过。真实 Gate fixture 验证静默升级时 provider 请求和根进展序号不变。完整记录和验证范围见[观察验证](observation-validation.md)。后续 journal 接入结果见下节。

## S9a 持久日志与只读回放

Core 默认写入脱敏状态，首个快照先于 app-server 启动提交；写入确认与 UI 投影版本分开，持久化故障进入 Unknown 并停止执行。CLI 新增 `--sessions`、`--replay`、`--since` 和 `--journal-dir`，`--json-events` 当前只支持回放。读取保留历史证据和不确定结果，不启动模型或回答旧审批。

本轮 102 项默认测试、3 项可选真实 app-server 测试、Python 持久消费者及 fmt/check/clippy/doctest/release build 通过。完整记录见[日志回放验证](journal-validation.md)。实时 stdout 背压/断管、脱敏导出、观察恢复交互和 Alpha 完整发布门禁仍未完成。

## S2.5b 实时 JSONL 与输出所有权

`--run --json-events` 和 `--workflow --headless --json-events` 已接入独立写线程，只读已提交日志，支持慢消费者追赶和最终关闭快照。持续堵塞与断管进入有界清理，活动外部结果保留 Unknown，已确认终态保留；非交互请求通过 Core 保存动作理由。新日志使用 schema 2，schema 1 历史继续支持。

本轮 115 项默认测试、4 项可选真实 Codex 测试、11 个 release CLI 管道场景和 Python 消费者通过；fmt/check/clippy/doctest/release build 通过。多根工作流的 CLI、日志与消费者使用一致的最终汇总，保留各任务已确认终态。完整记录见[实时 JSONL 验证](jsonl-validation.md)。手动导出、观察恢复入口、协议兼容门禁、完整进程创建竞态验证及 Alpha 发布验收仍未完成。
