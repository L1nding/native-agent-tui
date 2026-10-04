# 恢复检查命令

`native-agent-tui --recovery SESSION_ID` 读取指定会话的已提交 journal 前缀，输出恢复判断摘要。该命令只调用 `Replay::open`，不会启动 Codex、发送 RPC 或修改 journal 文件。

示例：

```text
native-agent-tui --journal-dir PATH --recovery SESSION_ID
```

输出包含：

- `needs_recovery`：会话是否仍可能包含未确认的执行结果、未完成清理或未关闭状态。
- `requires_input`：存在需要继续处理的非终态任务，但 journal 只保存脱敏任务元数据，没有任务正文，因此重新规划前必须取得用户输入。
- `can_resume=false`：历史检查不会恢复外部执行，也不会自动重放副作用。
- `uncommitted_tail`：提交高水位之后是否还有未提交的文件尾部；该尾部不会被读取或执行。
- `scheduler_restore`：对脱敏调度投影执行只读结构校验。`available` 表示快照可重建且没有待处理根任务；`requires_input` 表示结构可重建，但任务正文未持久化，继续排队或恢复执行前必须取得新输入；`invalid` 表示依赖、任务身份、活动根任务或计数元数据不一致。
- `outbox_available` 及其计数：只读显示该会话是否有 durable outbox，以及 `pending`、`sent`、`confirmed`、`unknown`、`failed` intent 数量。存在 outbox 时还会逐条显示最多 256 条脱敏 intent（method、request identity、task/attempt、状态和建议动作）；`outbox_records_truncated=true` 表示还有未展开记录。动作 `DoNotReplay` 禁止自动重发，`InspectExternalOutcome` 要求先检查外部结果，`Terminal` 表示该 intent 已有明确终态。缺少 outbox 的旧会话会显示 `outbox_available=false`，不会创建文件。
- 任务按 `active`、`unknown`、`queued`、`blocked`、`terminal` 分类，并显示数量与脱敏的任务 ID、类型、状态、attempt、父子/依赖关系、外部 thread/turn/generation 身份、待处理请求数和建议动作。动作映射为：活动或未知任务 `UnknownAfterRestart`，排队、ready 或 paused 任务 `NeedsInput`，blocked 任务 `ResolveBlock`，终态任务 `Terminal`。

完成且清理已确认的会话会报告 `needs_recovery=false`。损坏记录、撕裂尾部、缺失会话和非法身份沿用 replay 的错误码与错误文本。查询结果不包含 prompt、答案、命令、秘密或原始工具输出。
