# 上下文压缩观察

更新：2026-10-03。适用于锁定 Codex 0.159.2 的已确认事件。

## 查看方式

- 实时 TUI：F11 查看所选代理的保留压缩记录数、执行状态和证据来源；内容超出屏幕时可滚动。
- Ctrl+T：在工具证据中查看 `Compaction` 的 thread、turn、generation、item、状态和来源。
- F12 或 `--history SESSION_ID`：浏览持久化的压缩活动；用 F6 选择 `compaction` 类别，用 `/` 或 Ctrl+F 搜索 `Compaction`。
- `--replay SESSION_ID`：输出保留记录数、活动状态和来源；加 `--json-events` 可读取现有脱敏观察字段。

```text
cargo run --locked -- --sessions
cargo run --locked -- --history SESSION_ID
cargo run --locked -- --replay SESSION_ID --json-events
```

## 事实与限制

锁定协议的 `contextCompaction` item 只有 `id` 和 `type`。`item/started` 表示开始；有效的 `item/completed` 确认该压缩活动完成，无需普通工具使用的 `status` 字段。明确的失败、错误或格式不符的附加字段不能成为成功证据。

轮次结束或断连时，尚未收到完成事件的压缩保留为 `Unknown`，来源为 `Core`；已确认完成事件的来源为 `AppServer`。旧轮次、未知线程和重复事件不会新增压缩事实。压缩事件只更新观察证据，不释放 Gate，也不发送新的模型请求。

`Compactions retained` 仅统计当前快照保留的活动，包含运行中、完成和不确定活动，不代表完成次数或会话累计次数。活动淘汰或历史截断后，缺失记录不能按零补齐。历史筛选保留压缩开始、完成及 `ExecutionUnknown` 证据，可定位不确定终态。

原因、摘要、保留范围及压缩前后 usage 在该协议 item 中不可用；界面明确显示 `unavailable`。服务端另行报告的 token usage 不等于某次压缩前后统计，不能根据 token 下降推断压缩。

压缩观察复用 Core 活动和现有 journal 字段；不另存 prompt、原始工具输出或摘要正文。回放和历史搜索只读已提交记录，不启动模型或重发旧操作。

## 验证

```text
cargo test --lib compaction
cargo test --test journal_cli
python scripts/verify.py --live
```

协议形状由锁定 schema 指纹核对；行为回归覆盖 root/child、Gate、重复/旧轮次/未知线程、未完成活动、展示、历史检索和只读回放。真实 Codex 门禁覆盖版本、启动、审批、Gate 和 JSONL；本轮未通过真实模型任务主动触发上下文压缩，也未完成其他操作系统的原生验收。

2026-10-03 完整验证通过：230 个默认测试、8 个真实 Codex 检查，以及 Windows 进程所有权与清理、JSONL、历史回放等原生夹具；格式、编译、Clippy、文档测试和 release 构建均通过。
