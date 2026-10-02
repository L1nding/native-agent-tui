# 实时 JSONL 与非交互运行

## 使用方式

```text
cargo run --locked -- --run "Inspect this repository" --json-events
cargo run --locked -- --workflow docs/workflow-example.json --headless --json-events
cargo run --locked -- --run "Inspect this repository" --json-events | python tests/fixtures/live_event_consumer.py
```

stdout 只输出脱敏 JSONL，stderr 显示 session ID 和固定诊断。流包含状态、身份、证据计数、等待关系与 attention；任务正文、命令、完整模型输出和问题/答案均不进入流。普通 `--run` 继续显示模型文本。

`--json-events` 可与 `--run`、`--workflow --headless` 或 `--replay` 使用。`--since` 只支持指定 session 的回放；它不附着当前进程的 stdout。回放说明见[日志与回放](journal-replay.md)。

## 记录顺序和 schema

新会话使用 **schema 2**；schema 1 日志仍能只读回放，同一 session 不能混用两个版本。schema 2 新增 `last_headless_action` 和输出不可用的错误分类。消费者遇到未知版本应停止读取。

| 顺序 | 内容 |
| --- | --- |
| 首条 `snapshot`，`event_seq=0` | app-server 启动前已提交的脱敏基线 |
| `state`，严格递增的事件序号 | 日志已确认提交的完整状态投影 |
| 最后 `snapshot`，重复末尾事件序号 | 执行所有者清理后确认的结果和 cleanup 摘要 |

实时流的 `historical` 字段省略；回放将它设为 true，并将历史 Current freshness 改为 Unknown。`snapshot_version` 可因确认/观察变化跳跃；只有各活动的 `progress_seq` 表示有效证据推进。EOF、输出条数和事件序号增长都不能证明任务完成。

成功有序退出必须看到最后的快照，并检查 `session_closed`、`execution_result`、`cleanup_confirmed`。Unknown 或缺少最终关闭记录时需要检查 journal。上面的 Python 消费者验证序列与最终快照，输出一个脱敏摘要；管道末端的退出码不自动代表前一个进程的退出码。

`execution_result` 汇总所有根任务：全部成功才是 Completed；早先确认失败、取消或依赖阻塞会保留 Failed，即使最后一轮成功。仍有未确认或未执行的根任务时保留 Unknown。各任务终态与最后一轮的 `phase` 分别保存，不因汇总改写。

## 审批与输入

JSONL 不是回答通道。Headless 通过 Core 类型化命令执行固定策略，并保留 request ID、thread ID 和 turn ID：

- 提供 decline 的审批：发送 decline，记录 `declineApproval`。
- 不提供 decline 的审批：请求中断，记录 `interruptForApproval`。
- 用户输入：请求中断，记录 `interruptForInput`，提示使用 TUI。
- 输出不可用：停止执行所有者，记录 `stopForOutput`。

`last_headless_action` 记录最近一次动作尝试；Responding 不代表服务端已接受，Resolved 才是对应的协议事实。中断确认仍依赖真实终态。交互无法处理且五秒内未确认中断时，关闭执行所有者并保留 Unknown；不批准、不编造答案，也不重复发起任务。旧 turn 的动作命令不会作用于新请求。

## 背压与退出

一个独立线程顺序读取 journal 已提交前缀，按行输出和 flush。没有另一份随时间增长的输出队列，也不在 Core 中等待 stdout。暂时慢读时从日志追赶；执行所有者退出后重新捕获最终高水位，再发送最终快照。

单次底层写入最多 8 KiB。连续阻塞五秒触发退出；清理后输出排空最多等待五秒，取消后的线程退出另有一秒期限。Windows 使用私有 stdout handle 和同步 I/O 取消，避免持有全局 stdout 锁阻碍进程结束。普通磁盘 I/O 故障仍可能无法立刻取消，错误退出不承诺最终快照送达。

断管或永久慢读返回 4。活动外部结果进入 Unknown，尝试清理并持久化关闭摘要；此前已确认的终态保留。输出失败与执行结果分别判断，历史 Completed 不等于本次流交付成功。stdout 未收到的已提交状态可通过回放读取。

执行退出码：0 所有根任务成功；130 真实中断；2 参数错误；3 启动/配置或模型轮次前失败；4 未知结果、断连、输出或清理未确认；1 已确认执行失败。回放读取成功返回 0，历史执行结果保留在记录中。

验证方法见[实时 JSONL 验证](jsonl-validation.md)。当前实测支持 Windows；其他平台、强杀、设备故障和完整 Alpha 发布门禁仍待验收。
