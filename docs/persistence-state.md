# 快照持久化状态

CoreSnapshot 的 `persistence` 字段只描述当前 Core 快照与 journal 提交水位之间的关系：

- `Submitted`：快照已交给 journal writer，`submitted_seq` 高于 `committed_seq`。这表示已排队，不能当作 durable commit。
- `Committed`：journal view 的提交水位已追平最新提交序号，且没有错误。
- `Uncertain`：journal 不可用、写入线程报告错误，或没有可用的 journal view。此时外部执行结果和快照持久化都需要人工检查。

`Client::publish` 在 `append` 返回后重新读取 `JournalView`，`publish_journal_status` 在 writer 更新提交水位后再次投影状态。两条路径都保留 `submitted_seq` 和 `committed_seq`，所以 UI 可以区分排队与已落盘。

UI 的 F11 证据面板显示该状态。JSONL 输出在写出已提交记录时增加顶层 `persistence: "committed"` 字段；这个字段是输出时的只读投影，不写回 journal，也不改变历史记录的兼容格式。旧 journal 记录仍可读取，缺失该输出字段时按旧格式处理。

该状态不会触发重试、自动恢复或 outbox 发送。`Uncertain` 只提供事实边界，调用方必须等待明确的终态或让用户决定下一步。
