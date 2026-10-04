# 调度接入验收

## 当前行为

`Scheduler` 由 Core 独占。`QueueRootTasks`、带 attempt 的任务操作和工作流命令通过 `ClientHandle` 进入 Core；真实 `turn/started`、`turn/completed`、审批和原生 child 事件更新任务快照。UI 只读取快照和提交命令。

初始输入和 Gate 期间的输入统一存入根任务队列；不再保留另一份可独立派发的 prompt 队列。工作流文件入口、F4 任务面板、暂停/恢复、取消、显式重试和优先级调整均已连接到执行路径。用法及限制见[任务操作](scheduler-usage.md)。

## 自动检查

本次执行通过 67 项默认测试、fmt、check、无警告的 clippy、release build 和 doctest 检查。另行运行的 3 项可选真实 app-server 测试全部通过。

```text
cargo fmt --all -- --check
cargo check --locked --all-targets
cargo clippy --locked --all-targets
cargo nextest run --locked --no-fail-fast
cargo test --locked --doc
cargo build --locked --release
```

新增回归覆盖：

- 批次原子校验、前向依赖、环、非法策略和不可能的 quorum；阻塞逐级传播。
- 优先级 aging/FIFO、不越过依赖、审批/Gate 释放根槽位但保留单根执行约束。
- shell 预检与暂停阻止派发；失败重试产生新 attempt，旧终态和旧任务操作不能影响新 attempt。
- 暂停时审批仍可回答；子代理终态仍可见，但已就绪的 Gate 仅在恢复后回复。
- 无待处理 RPC/中断的静默 Gate 不发布纯计时快照；虚拟时钟推进一小时不会触发 UI 重绘积压。恢复计时检查时跳过漏掉的 tick。
- 身份已知但第一轮未开始时延迟取消；中断 RPC 确认不能完成任务或释放 Gate。
- 停止根及 3 个已知 child，分批发送中断；停止后才到达的 child 也等待真实身份后中断。
- 40×12、80×24、160×45 的任务渲染、选择和命令投递；UI 操作不修改 Core 快照。

## 真实 app-server 与本地 provider

安装版本基线为 Codex 0.159.2。`live_app_server_gate_has_zero_provider_requests_while_a_child_is_pending` 使用隔离 `CODEX_HOME`，请求仅送 localhost，不需要 API key。

```powershell
$env:NATIVE_AGENT_TUI_PYTHON = (python -c 'import sys; print(sys.executable)')
cargo nextest run --locked --run-ignored only live_app_server_gate --no-capture
```

| 验收阶段 | provider 根请求数 | 系统事实 |
| --- | --- | --- |
| 第一轮 child 响应保持未返回 | 2 | 父 Gate pending，无额外父请求 |
| 暂停派发并完成 child | 2 | child Completed；父 Gate 仍 pending |
| 恢复后原生 follow-up，第二轮 child 保持未返回 | 4 | 等待新 generation，无额外父请求 |
| 再次暂停，发送 child `turn/interrupt` | 4 | 未释放 provider 响应前收到真实 Interrupted；父仍 pending |
| 释放 fixture 并恢复 | 5 | 父输出 `GATE_DONE`，收到 Completed |
| 追加两项根任务，第二项依赖第一项 | 7 | 两项均 Succeeded，根 `turn/start` 累计 3 次 |

每个保持窗口及暂停后的就绪窗口均等待 300 ms 再采样。这个计数只证明 fixture 观察窗口中的请求行为，不代表任意 provider 的永久统计。

另外两个可选测试验证本机零模型启动到 Ready，以及 Windows sandbox 覆盖生效。

## 发布二进制 CLI smoke test

实际 release 二进制以 `--workflow FILE --headless --sandbox read-only --windows-sandbox unelevated` 运行两项依赖任务，正文分别为“只回复 WORKFLOW_ONE”和“只回复 WORKFLOW_TWO”。它使用本机认证 provider，依次输出两项标记，退出码为 0。

负向 CLI 检查使用不存在的 `--codex` 路径：循环计划在启动前退出 2；非交互终端未指定 `--headless` 时也在启动前退出 2。`--help` 显示工作流模式和任务操作。

## 尚未保证

脱敏 journal/outbox 和 `--recovery` 的只读调度结构校验已经接入；它们不会接管旧进程、自动派发任务或重放副作用。细粒度工具/模型资源预算、自动执行恢复、完整诊断与 Alpha 活动证据仍待实现。直属 child 数量、观察深度和活动 turn 数已有启动参数硬限制，但根任务共用一个线程，不支持多个根工作流并发。

本次新任务面板通过 TestBackend 验证，尚未新增实际 ConPTY 的 F4 操作录制。整组停止的多 child 行为通过内存 transport 验证；真实 provider 验证的是同一 child 的两轮和一次中断。

本机 shell 预检此前出现过超时。本次启动和 release 工作流通过，但没有修改预检代码或找到可复现根因，不能据此宣称启动不稳定性已修复。Windows Job 创建后附加的竞态仍未消除。
