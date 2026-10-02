# Rust 开发项目级 Skill 调研

调研日期：2026-10-02

## 结论

本项目适合建立一个窄范围的项目级 Rust 开发 skill：指导实现阶段如何遵守既有 Core、Transport、Scheduler 与 UI 边界，并使用 Cargo、rustfmt、Clippy 和 Tokio/Ratatui 的官方工作方式。它应补充仓库 `AGENTS.md` 中的 Rust 通用约定，具体步骤应由未来实际加入的 `Cargo.toml`、工具链配置和 CI 决定。

仓库目前仍是设计文档，没有 Cargo package 或源代码。当前设计选择一个 Cargo package，以模块划分 Core、RPC、Transport、Scheduler 和 UI；Core 独占执行事实，UI 消费只读快照。因而 skill 应覆盖“按设计落地”和“验证代码”，不预先规定尚未确定的依赖版本、lint 严格级别或 async 架构细节。

## 推荐的 skill 范围

### 1. 先确认仓库现状和设计接口

开始 Rust 任务时，检查 `Cargo.toml`、`rust-toolchain.toml`、`.cargo/config.toml`、`rustfmt.toml`、CI 配置和对应设计文档；以这些可执行配置为版本与命令的权威。设计文档与实现不一致时，说明差异并沿现有的 `ClientHandle`、`TransportAdapter`、Scheduler command/event 等公开 seam 推进，不要在 UI 中新增协议、进程管理或 Gate 逻辑。

这是针对本项目的建议：当前 Core 设计明确 Core 是执行事实唯一所有者，UI 依赖只读快照，Transport 与 Scheduler 提供可替换的测试 seam（见 `native-agent-tui-core.md`、`native-agent-tui-ui.md`、`native-agent-tui-workflow.md`）。

### 2. 保持 Cargo 结构简单且可重复

当前设计先用单 Cargo package 和模块边界；只有出现明确的独立复用、构建或所有权理由时再拆 workspace/crate。加入 manifest 后，记录并尊重项目声明的 edition 和 `rust-version`，不要根据本地已安装的最新编译器推断最低版本。依赖 feature 要有具体用途；排查 feature 组合时使用 Cargo 提供的 feature tree 能力。

Cargo 官方说明 workspace 共享 lockfile、输出目录，并可对成员统一执行命令；profile 设置由 workspace 根 manifest 管理。Cargo features 控制条件编译及可选依赖，feature unification 会影响实际构建组合。详见 [Cargo Workspaces](https://doc.rust-lang.org/cargo/reference/workspaces.html)、[Features](https://doc.rust-lang.org/cargo/reference/features.html)、[Rust Version](https://doc.rust-lang.org/cargo/reference/rust-version.html)。

### 3. 让异步任务边界和取消语义明确

使用 Tokio 时，按职责决定是在同一任务中 `select!` 多个等待，还是用 `spawn` 建立独立任务；异步任务之间通过清晰的 channel/handle 边界传递命令与事件。审查 `select!` 分支时，确认被取消分支在 drop 时的行为；如果在循环中等待一个需要跨轮保留的操作，确认是否应复用同一个 future。取消或断连不能被默认为外部副作用已撤销。

Tokio 官方文档指出 `select!` 未胜出的分支会被 drop，异步取消通过 drop future 实现；spawned task 有独立调度和 `JoinHandle`，其 panic/运行时取消会通过 join 结果体现。将这些语义应用到本项目进程与副作用管理时，应继续遵循 Core 设计中的显式停止、断连和 `Unknown` 状态，不要把本地 future 被取消等同于远端命令未执行。[Tokio `select!` 与取消](https://tokio.rs/tokio/tutorial/select)、[Tokio tasks](https://tokio.rs/tokio/tutorial/spawning)。

### 4. 把外部输入转换成已验证类型，把失败留在可观察路径

协议输入、用户配置、进程输出和终端命令属于边界数据：先解析和验证，再交给状态转换逻辑。用 `Result` 表达调用者可处理的失败，用 `Option` 表达正常缺省；只有程序不变量确实被破坏时才 panic。对无法确认的外部操作结果，保留显式未知状态和来源，不通过静默默认值、忽略错误或自动重放隐藏不确定性。

Rust Book 将错误区分为可恢复的 `Result` 与不可恢复的 panic，并强调选择恢复还是停止取决于错误语义。[Rust Book: Error Handling](https://doc.rust-lang.org/book/ch09-00-error-handling.html)。外部操作“不确定时保留 Unknown”的要求来自本项目 Core/workflow 设计，不是 Rust 语言本身的规则。

### 5. 测试行为和公开 seam

为纯状态转换编写单元测试；跨模块行为通过项目已有设计的 command/event、fake transport 或 terminal backend seam 验证。异步事件测试关注可观察的状态和顺序，包括迟到/重复事件、取消、断连及旧 generation，避免只断言内部辅助函数调用。UI 输出可用 Ratatui `TestBackend` 做缓冲区断言；它适合验证绘制结果，但不能代替真实终端的 Windows 清理、IME、resize 等检查。

Rust Book 说明测试用于检查类型系统无法证明的意图行为，并介绍单元与集成测试组织。[Rust Book: Writing Automated Tests](https://doc.rust-lang.org/book/ch11-00-testing.html)。Ratatui 将 `TestBackend` 列为可用于 UI 单元测试的后端；其事件输入则由应用选择的后端库提供。[Ratatui Backends](https://ratatui.rs/concepts/backends/)、[Ratatui Event Handling](https://ratatui.rs/concepts/event-handling/)。本项目具体要覆盖的 Gate、generation、fake terminal 和 Windows 检查来自现有设计文档。

### 6. 使用仓库配置的格式化和 lint

如果仓库已有配置，按项目固定的 Rust toolchain 与配置运行格式化和 lint；不要为减少警告而全局放宽 lint，也不要无差别启用所有 pedantic lint。新增局部 lint 例外时，限定到最小代码范围并解释原因。维护者添加项目配置时，应先由实际实现与 CI 约束决定 edition、MSRV 和阈值，再把相同配置用于本地与自动检查。

Rustfmt 的格式可能随 style edition 演进；官方 Style Guide 说明 style edition 与 Rust 语言 edition 相关但可单独配置。[Rust Style Guide: Editions](https://doc.rust-lang.org/style-guide/editions.html)。Clippy 默认运行 `clippy::all`，官方建议按需使用 lint 并允许局部抑制；`pedantic` 更严格且可能误报。Clippy 还说明 MSRV 配置会影响部分 lint 判断。[Clippy Usage](https://doc.rust-lang.org/clippy/usage.html)、[Clippy Configuration](https://doc.rust-lang.org/clippy/configuration.html)。

## 建议的按任务流程

1. 查看受影响模块的设计文档和当前 Cargo/toolchain 配置；确认 owner、公开输入输出、异步任务及失败语义。
2. 修改边界类型和状态转换，使协议/外部数据先经过校验，并维持 Core 对执行事实的单一所有权。
3. 通过对应公开 seam 覆盖用户可观察行为；涉及 Tokio 时检查 future drop、task join 和副作用取消语义；涉及 UI 时用 `TestBackend` 检查绘制输出。
4. 仅运行仓库已配置且适用于改动的命令。初始仓库尚无 Cargo 命令；Cargo 项目建成后再依据配置运行 `cargo fmt --all -- --check`、`cargo check`、`cargo test` 与 `cargo clippy --all-targets` 等检查。不要把这里的候选命令当成当前仓库已存在的脚本。
5. 汇报行为变化、验证命令与结果、仍然无法确认的外部行为。对进程或副作用结果不明的情况，保持显式未知，不声称成功或安全重试。

## 适用场景与边界

**适用：** 新增或调整 Rust 模块、Tokio task/channel/取消逻辑、协议与配置解析、Cargo 依赖/feature、Core 与 UI 接缝、Ratatui 绘制，以及实现阶段的代码检查。

**不适用：** 修改纯设计文档而不涉及 Rust 实现、替代项目架构文档、决定产品行为/外部协议兼容基线，或在依赖与工具链尚未落库时强制指定版本和 lint 策略。

建议未来 skill 的触发描述点明“本仓库 Rust 实现、Cargo/Tokio/Ratatui 代码”，并只在 Rust 任务时读取；本 Markdown 是研究依据，不是可直接调用的 skill。

## 一手来源

- [The Rust Programming Language](https://doc.rust-lang.org/book/): ownership、error handling、automated tests、Cargo 使用。
- [Cargo Book: Workspaces](https://doc.rust-lang.org/cargo/reference/workspaces.html): workspace 共享和根 manifest 配置。
- [Cargo Book: Features](https://doc.rust-lang.org/cargo/reference/features.html): 条件编译、可选依赖和 feature 解析。
- [Cargo Book: Rust Version](https://doc.rust-lang.org/cargo/reference/rust-version.html): manifest 中 Rust 版本声明。
- [Rust Style Guide](https://doc.rust-lang.org/style-guide/editions.html): 语言 edition 与格式风格 edition。
- [Clippy Usage](https://doc.rust-lang.org/clippy/usage.html) 与 [Configuration](https://doc.rust-lang.org/clippy/configuration.html): 默认 lint、pedantic 与局部配置。
- [Tokio Tutorial: Spawning](https://tokio.rs/tokio/tutorial/spawning) 与 [`select!`](https://tokio.rs/tokio/tutorial/select): task、JoinHandle、分支取消。
- [Ratatui Event Handling](https://ratatui.rs/concepts/event-handling/) 与 [Backends](https://ratatui.rs/concepts/backends/): 事件输入职责与 TestBackend。

» Sources checked on 2026-10-02. Rust documentation and library APIs evolve; implementation-time commands and API details should be checked against the versions pinned by the project.
