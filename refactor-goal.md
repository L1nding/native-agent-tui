# native-agent-tui 删除式精简目标

> 状态：历史任务说明，已被 dsh 优先路线及 `docs/dsh-tui-reference-iteration-plan.md` 取代。以下保留用于追溯，不是当前执行指令；其中 Codex 后端与代理分工要求不适用于本轮。

请在当前工作树 D:/project/native-agent-tui 中直接实施代码优化。目标是让已有 Rust TUI 基础实现变得更小、更清楚、更容易维护，采用删除优先，而不是继续叠加抽象。

## 必须遵守

- 你是主 agent，必须使用 Codex 原生子代理完成调查、实施和审查；保持父模型在等待子代理期间不产生新的模型请求。
- 只能修改当前工作树 D:/project/native-agent-tui，不修改其他仓库。
- 遵循当前 AGENTS.md、.agents/skills/rust-development/SKILL.md、codebase-design 和 improve-codebase-architecture 的要求。
- 先读取状态记录、git status、当前 HEAD 和完整调用关系；工作区已有改动属于本轮脚本之前的基线，先核对归属，不要覆盖或 reset。
- 先读并实际应用 deletion test：删除一个 Module、类型、方法或包装层后，复杂度是否消失，还是回流到调用方。没有生产调用、测试调用或协议依据的冗余导出可以删除；不能只因为名字长就删除语义。
- 主 agent 不直接改源文件；由唯一 architecture_worker 修改代码。探索和 reviewer 只读。不要自行提交或推送，除非当前 AGENTS.md 明确要求；本任务重点是产生可审查代码修改。

## 优先范围

逐个完整审查 src/ 下每个 Rust 文件，至少覆盖：

- src/client.rs
- src/app_server.rs
- src/state.rs
- src/protocol.rs
- src/gate.rs
- src/scheduler.rs
- src/interactions.rs
- src/transport.rs
- src/config.rs
- src/ui.rs
- src/lib.rs
- src/main.rs

优先删除：

- 未被真实调用的 public 导出、测试专用转发和死代码；
- 只包装一次调用且没有隔离价值的函数、类型和模块；
- 重复维护的状态、快照字段、错误分支和命令转发；
- 为未来假设的 adapter、协议或功能提前保留的抽象；
- 已经由更深 Module 负责、调用方仍重复判断的逻辑。

然后简化命名：

- 文件名、模块名、类型名、方法名、变量名按上下文缩短；
- 保留跨模块和协议字段的明确语义，不能把 thread/turn/item/generation/request 等稳定身份改成含义不清的缩写；
- Rust 命名统一 snake_case / PascalCase；
- 每次命名修改必须同步所有调用点和测试，不能留下兼容别名或旧入口。

关键注释只加在：

- CompletionGate 的等待释放条件和旧 generation 过滤；
- app-server stdio reader/writer 单所有者和退出清理；
- Core/UI 状态所有权 seam；
- Windows 进程和审批/断连等容易被误改的生命周期处。

注释使用简洁中文，说明为什么必须这样做，不重复代码表面含义。

## 必须保持的不变量

- `ClientHandle`、`TransportAdapter`、scheduler command/event seam 和 UI 只读快照边界保持可测试。
- JSONL/RPC 数字和字符串 ID、unknown/disconnected/stale generation 语义不能丢。
- Gate 只由绑定的当前 child turn 完成/失败/中断或断连释放；idle、普通消息、UI 刷新不能释放。
- GatePending 期间不能发起新的 root turn、steer、轮询或 heartbeat。
- approval/user-input 的 requestId 不能混淆；停止和 writer/transport 失败不能伪造完成。
- UI 不能拥有执行事实；日志、上下文、skill 和调度信息仍通过快照/查询读取。
- 不为“通过测试”把实现缩成假的 demo；真实 app-server 路径和现有测试行为要保留。

## 交付与验收

1. 主 agent 完整阅读修改后的文件及调用点，检查是否把复杂度推给调用方。
2. 删除和命名修改必须有实际 diff；只扫描和改注释不算完成。
3. 运行 cargo fmt --all -- --check、cargo check --all-targets、cargo clippy --all-targets、cargo test --all-targets、cargo build --release；如果失败，区分本轮引入和基线问题。
4. 完成后报告：修改文件、删除了什么、命名变化、保留的关键注释、测试结果、未验证范围、剩余候选和当前 git status。
5. 不要把“没有更多建议”当作完成；至少完成一个有代码证据的删除/精简目标，并继续到同一根因闭合。
