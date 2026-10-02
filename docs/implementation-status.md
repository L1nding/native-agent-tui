# 实施状态与验证

更新日期：2026-10-02。

## 当前可用范围

- `--tui` 和 `--run` 共用真实 app-server/Core 路径；初始化、创建线程和 shell 预检完成后才发送任务。
- Core 独占执行状态。独立 reader 与单 writer 通过有界队列传递 JSONL；UI 只提交类型化命令和读取快照。
- 对话支持多轮输入、增量输出、final 校正、中文与 emoji 编辑、滚动、审批和多问题回答。
- 预检失败禁止提交任务；旧轮响应和终态不能覆盖新轮；中断确认不会伪造轮次完成。
- 未知外部结果会关闭执行通道，避免迟到事件恢复运行或自动重放副作用。
- 秘密回答不会进入对话记录；请求结束会清除回答缓冲，恢复原任务草稿。
- Windows Job Object 清理 app-server 的已纳入进程树；终端退出恢复 raw mode、alternate screen 和 bracketed paste。

## 验证证据

自动测试穿过 `ClientHandle`、内存 duplex transport 和 Ratatui TestBackend，覆盖：

- 初始化和零模型轮次预检；通知先于 RPC 响应；旧轮 started/response/completed 的过滤。
- 预检失败禁止绕过；启动期间中断一次；审批 resolved；EOF 和 writer 错误。
- 数字/字符串 ID、空 result、非法响应结构、帧上限、分片接收和取消安全。
- 消息校正、UTF-8 字节截断、秘密输入失效，以及 40×12、80×24、160×45 终端布局。
- Windows 实际启动隐藏的 PowerShell 父子进程，持有子进程句柄，确认父进程退出后 Job 关闭终止子进程。

真实 Codex 0.159.2 验证：

```text
cargo run --locked -- --check-shell --windows-sandbox unelevated
# shell preflight: passed (zero model turns)
cargo run --locked -- --run "只回复 READY" --sandbox read-only --windows-sandbox unelevated
# READY
```

本机 elevated 沙箱预检超时；相同命令在显式 unelevated 沙箱下通过。首次 read-only 会话预检也曾超时，随后独立预检与重复会话通过。保留失败状态和配置建议，不自动扩大权限或重试。

两个需安装 Codex 的可选测试已单独运行：

```text
cargo nextest run --locked --run-ignored only live_windows_launch
cargo nextest run --locked --run-ignored only live_windows_core_reaches_ready
```

前者通过 `config/read` 确认 Windows sandbox 覆盖生效；后者验证真实 Core 启动到 Ready、根轮次为零，连续三次通过。这两个测试默认忽略，避免普通单元测试依赖本机认证和 Codex 安装。

Windows ConPTY 交互验证已收到 `TUI_READY`，同一线程第二轮输入中文任务后收到 `SECOND_READY`，两轮均由真实 `turn/completed` 进入 Completed。

生成期间 Ctrl+C 已收到真实 Interrupted 终态；Ctrl+Q 退出后恢复终端。快速中断曾返回 `no active turn to interrupt`：现在保留会话、显示拒绝原因并继续接收终态，允许用户再次明确请求中断。相应竞态有自动回归测试。

## 尚未完成的设计要求

1. 将子线程身份、直属关系、当前 turn 和 generation 接入 Core。
2. 注册和处理 `wait_for_subagent_completion`，验证等待期间新增根模型请求为零。
3. 将 scheduler 的任务 DAG、资源槽、等待和手动命令连接到真实执行。
4. 提供持久化日志、恢复语义、上下文压缩及 skill 的服务端事实记录。
5. 扩展协议兼容快照和本地计数 provider 验证。

`gate.rs`、`scheduler.rs`、`rpc.rs` 和 `diagnostics.rs` 的独立接口不能作为上述功能已在真实运行中工作的证据。

Job 在进程创建后附加。现有清理测试覆盖附加后的后代；创建与附加之间的竞态仍需进一步消除或验证。
