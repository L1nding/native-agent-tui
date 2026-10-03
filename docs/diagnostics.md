# 运行诊断指标

F11 证据视图顶部显示当前会话的有界诊断计数。指标来自 Core 和 stdio transport 的只读快照，不会触发新的请求，也不包含任务正文、提示词、答案、密钥或原始工具输出。

当前字段：

- `transport_bytes_in` / `transport_bytes_out`：已完成读取或写入的 JSONL 载荷字节数。换行符不计入字节数；排队但尚未写出的数据不会提前计入。
- `control_events`：响应、服务端请求和生命周期通知等需要保持顺序的事件数。
- `telemetry_events`：`*/delta` 输出以及 token usage 更新等可合并观察事件数。

指标只描述当前运行实例。历史 journal 和脱敏导出不会保存这些计数，因此回放界面不会把旧指标误认为当前进程的实时数据。计数使用饱和加法；达到 `u64` 上限后保持上限。

相关实现位于 `src/diagnostics.rs`、`src/transport.rs` 和 `src/state.rs`，生产路径由 `ClientHandle` 的 Core 快照发布。测试通过 duplex transport、Core 生命周期和 Ratatui `TestBackend` 验证收发计数、事件分类、窄宽终端显示及隐私边界。
