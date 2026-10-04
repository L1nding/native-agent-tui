# Token Budget

## 两种预算

`--max-total-tokens N` 是会话级预算。Core 将当前 root turn 与已知 child turn 的服务端确认 `total_tokens` 汇总；汇总达到 `N` 后，停止后续调度，并为每个仍在运行的 turn 发送一次中断请求。

`--max-agent-tokens N` 是单个 agent 单个 turn 的预算。root 和每个 child 分别按自己的 `threadId`、`turnId` 和 generation 计数；某一 turn 达到 `N` 只中断该 turn，不影响其他 agent 的独立预算计数。

两个选项都要求正整数。未设置时不启用对应预算，保留原有调度行为。

## Usage 来源与边界

预算只使用 app-server 发来的 `thread/tokenUsage/updated` 中可解析的服务端 confirmed usage。缺少 `total_tokens`、来源未知或仅为本地估算的值不会触发停止，也不会写入预算证据。

Core 会校验当前 thread、turn 和 generation。旧 turn、迟到事件以及 turn 已进入终态后的 usage 不会再次触发中断。每个当前 agent/turn 最多触发一次中断；最终状态仍以服务端 `turn/completed` 事件为准。

运行时 notice 只包含预算上限和脱敏的 thread/turn 标识，不保存 prompt、回答或原始工具输出。

## 示例

```text
native-agent-tui --run "检查项目" --max-total-tokens 20000
native-agent-tui --run "拆分任务" --max-agent-tokens 4000
```

达到单 agent 预算时，notice 会说明 `limit`、`thread` 和 `turn`，并提示等待服务端终态事件。
