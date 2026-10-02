# 会话日志与只读回放

S9a 已接入 Core：普通 TUI、`--run`、`--workflow` 和 `--check-shell` 默认持久化脱敏状态。日志的首个快照在 app-server 启动前提交；失败时禁止启动。F11 显示 session、已提交序号和已排队序号，headless 将 session ID 写到 stderr。

## 查看历史

```text
cargo run --locked -- --sessions
cargo run --locked -- --replay SESSION_ID
cargo run --locked -- --replay SESSION_ID --since 10 --json-events
cargo run --locked -- --sessions --cwd PATH --journal-dir PATH
```

`--cwd` 选择原工作区。工作区键由规范化路径的原生字节确定，用于目录隔离，不是安全认证或加密摘要。`--journal-dir` 可指定专用存储目录；相对路径从应用启动目录解析。

回放不启动 app-server、不附着旧进程、不回答旧请求，也不重试历史任务。文本模式显示最新已提交摘要；JSONL 模式按下面的顺序输出：

1. `--since` 所指事件位置的基线 `snapshot`，默认序号为 0。
2. 严格晚于游标的 `state` 记录。
3. 回放打开时捕获的高水位 `snapshot`。
4. `replay_end`，固定 `live_attached=false`。

每条 `state` 保存完整的脱敏投影。`event_seq` 表示日志顺序，`snapshot_version` 表示投影版本，活动 `progress_seq` 才表示有效证据进展。基线、最新快照和结束记录可以重复同一事件序号。回放期间新增记录留待下一次读取。

新日志使用 `schema_version=2`；旧 schema 1 日志仍能回放，输出保留原版本。回放设置 `historical=true`，历史 elapsed/silence 数值与 attention 保留；原来为 Current 的活动 freshness 改为 Unknown，不能继续用旧时钟计算当前静默。成功读取返回 0；历史执行结果另见 `execution_result`，`replay_end` 不代表任务成功。

## 存储与故障语义

默认目录：Windows `%LOCALAPPDATA%/native-agent-tui/journal`；macOS `~/Library/Application Support/native-agent-tui/journal`；Linux `$XDG_DATA_HOME/native-agent-tui/journal`，未设置时使用 `~/.local/share/native-agent-tui/journal`。

日志为 `<workspace>_<session>.jsonl`，配套 `.cursor` 保存提交高水位，`.lease` 保护活动写入者。专用线程先写入并同步日志，再同步并替换游标；回放只读游标确认的字节前缀。完整但未提交或撕裂的尾部用 `uncommitted_tail=true` 报告。日志与游标不匹配、未知 schema、错误 session、无效游标均显式失败。

默认预算为 **500 MiB、30 天**，在后续写入时检查。只淘汰已关闭、清理已确认且无需恢复的会话；`.removed` 保留历史已移除的标记。活动或不确定会话不被淘汰，无法腾挪时停止执行并报告存储错误。尚未提供快照压缩、旧工作区迁移或自动执行恢复。

队列最多 128 条、64 MiB 编码数据，单条最多 4 MiB；元数据及序列化临时对象另有少量开销。Core 入队不等待磁盘。目录锁冲突在启动时立即报告，写入线程最多等待两秒；退出排空与最终提交最多等待五秒。底层磁盘调用自身无法由此期限强制取消。满队列或写入错误进入可见 Unknown 并清理执行所有者，不能继续宣称可靠完成。

`session_closed`、`execution_result` 和 `cleanup_confirmed` 分开保存。缺少最终关闭记录、Unknown 结果或未确认清理时，`needs_recovery=true` 表示需要检查历史观察证据。它不是自动重试授权。

## 隐私与当前范围

存储 typed 身份、执行状态、任务关系、请求 ID、证据计数和 attention。排除任务正文/标题、对话正文、命令、问题/答案、模型配置、工作区原始路径和原始错误；保留的协议身份字段仍属于本地数据。错误仅保存固定分类。这里不提供完整 transcript 或凭据存储。

`--run TASK --json-events` 与 `--workflow FILE --headless --json-events` 已接入实时脱敏流、日志追赶和断管清理；使用方式见[实时 JSONL](jsonl-events.md)。日志验证方法见[回放验证](journal-validation.md)。
