# 执行证据、注意级别与 JSONL 契约

更新：2026-10-03。状态：S2.5（#13）目标设计；CLI 示例是待实现接口。范围见[产品计划](native-agent-tui-plan.md)，术语见[CONTEXT](../CONTEXT.md)。

## 1. 所有权与身份

Core 单独维护执行事实，并通过独立的观察计算生成 attention。UI 和 Python 消费者使用同一投影；UI 不重新计算健康状态。观察计时器只能发布提示，不能发起模型请求、轮询、重试、释放 Gate 或改变执行终态。

```text
ActivitySnapshot
  session_id, attempt_id, agent_id
  thread_id?, turn_id?, generation?, item_id?, request_id?
  kind, source, started_at, last_evidence_at
  elapsed_ms?, silence_ms?, clock_epoch, freshness
  wait_reason?, resume_condition?, wait_targets[]
  last_evidence?, recent_evidence[]
  progress_seq, output_bytes, child_terminal_count, transition_count
  attention { level, reason, quiet_after_ms?, attention_after_ms?, config_source }
```

活动集合保持固定：Starting、ModelRequest、ModelStreaming、ToolRunning、WaitingApproval、WaitingUserInput、WaitingChildren、WaitingTransport、Completed、Failed、Unknown。执行结果另有明确的 `execution_state`，包括 Interrupted；中断不能被 activity 的 Completed 冒充。平台未提供的字段为 null/unknown；ModelRequest 不能伪称已经确认 provider 正在推理。

三种序号分开：

- `event_seq`：session 中写入 journal 的事件顺序；注意变化也可能增加它。
- `snapshot_version`：投影版本；配置和注意变化也可更新。
- `progress_seq`：仅在该活动接受新的有效证据时推进。消费者不能把前两项增长当成工作进展。

有效证据包括通过身份/当前 generation 校验的状态迁移、请求创建/解决、工具开始/结束、有内容的输出增量、child 当前轮次终态。重复、旧 generation、空 delta、未知无关事件、heartbeat、重绘和重复 usage 不更新进展。原始消息计数用于诊断，单独维护。

每个 agent/活动分别保存证据。child-A 的输出不能刷新静默 child-B 的计时；父等待可汇总子状态，但保留各目标最近证据与条件。一个 turn 有多个并行工具时用活动列表和稳定 item ID 表达，不让最后一条消息覆盖其他活动。

## 2. 时钟与 Attention

使用可注入单调时钟计算当前进程内的 elapsed/silence；wall-clock 用于人类日期展示。单调时间带 `clock_epoch`，不可跨重启直接相减。回放保留历史 age/时间来源，重启或无法确认休眠时间时显示 freshness unknown，不能据墙钟倒退判断任务恢复活动。

已确认 Attention 默认阈值如下；Quiet 暂定 Attention 的一半，作为可调整的初始配置，不是执行 deadline。

| 活动 | Quiet 初始值 | Attention 默认值 |
| --- | --- | --- |
| ModelRequest / ModelStreaming | 15 秒 | 30 秒 |
| ToolRunning | 30 秒 | 60 秒 |
| WaitingChildren | 60 秒 | 120 秒 |
| WaitingTransport | 15 秒 | 30 秒 |
| WaitingApproval / WaitingUserInput | 无静默分档 | 无静默分档，立即显示 requires_action |
| Starting / Unknown | 未定义时显示 unknown | 不套用猜测阈值；启动管理 deadline 是另一契约 |
| 已终止活动 | 不继续计时 | 不继续静默升级，保留执行结果 |

新有效证据重置相应活动的静默级别；错误和审批待操作不会因其他输出被清除。`requires_action` 与静默 attention 并列，审批绝不能显示成“无需介入”。

覆盖顺序：默认 < 全局配置 < CLI < 本次 TUI 临时配置。显示生效值及来源，校验 `0 < quiet < attention`。临时配置通过 typed command 提交给 Core 后发布；“继续等待/关闭提醒”仅更改 UI 的提醒展示，不重置证据、阈值或其他消费者看到的 attention。

## 3. 等待与交付结果

等待列表默认展示等待方、目标 ID、thread/turn/generation、原因、恢复条件、最近证据及静默时长。DAG overlay 是 V2 的扩展。没有关联证据时显示 unknown，不解析自然语言补关系。

handoff 输入与最终报告分两步：agent 可经已验证的结构化工具/事件提交 reported 字段；adapter 在真实 turn terminal 后汇总统一报告。缺少结构化提交也生成报告，缺失字段为 unknown，不因此阻塞 Gate。不能假设 app-server 已提供某个 handoff API；注册/兼容性需 fixture 和真实测试证明。

报告包含结论、修改文件、验证结果、未完成项；每项都有 `reported / verified / unknown`、证据 ID、attempt 和适用范围。命令 exit 0 只证明该命令成功，不能证明全部目标完成；共享工作区的 diff 不能独自证明某个 child 修改了文件。报告与 Gate 的执行终态分别处理，报告到达不能释放等待。

## 4. JSONL CLI、游标与恢复

待实现的命令约定：

```text
native-agent-tui --run TASK --json-events
native-agent-tui --replay SESSION_ID --since EVENT_SEQ --json-events
```

`--run` 启动新执行；`--replay` 只读用户数据目录中的 journal，不启动 app-server。`--since` 只能与明确 session 的回放入口使用，拒绝和 `--run` 混用；游标指向已消费事件，返回严格大于它的事件。重新运行 `--run` 不是重连。stdout 没有 live attach/socket 保证；读取活动 journal 也只读启动回放时捕获的已提交前缀，末尾标记 `live_attached=false`。

每行公共字段：`schema_version`、`kind`、`session_id`、`attempt_id?`、`event_seq`、`snapshot_version`、`recorded_at`、`payload`；payload 包含相关身份、execution/activity/evidence/attention 和脱敏错误。不相关或不支持的字段为 null，不捏造。

- 新执行：首行完整的**脱敏状态** snapshot，再发事件；快照是摘要，不包含全部历史正文。
- 回放：首行是游标位置的基线 snapshot，补发游标之后的事件，最后发截至回放开始时已提交高水位的最新 snapshot 和 `replay_end`。这避免把最新状态再倒退播放成过去。`replay_end` 是读取结束，不能冒充任务 Completed。
- cursor 超过末尾、session 不匹配、历史淘汰或 schema 不兼容：输出明确错误和可用范围，不静默从头开始。允许用户显式选择只看最新 snapshot。
- telemetry 合并附带 `coalesced_from_seq/to_seq` 或覆盖范围；快照高水位不构成新事件。control/审批/终态不得用普通合并掩盖。
- `schema_version` 不兼容时消费者停止并报告，不默默解释未知终态；fixture 需覆盖数字/字符串 request ID 的往返。

stdout 仅 JSONL，stderr 仅脱敏人类诊断。用户输入和审批保留现有 headless 拒绝/中断的安全路径；JSONL 不是回答通道，不偷偷启用 stdin RPC。发送请求状态与明确拒绝/中断原因，进入 Core 既有终态流程，提示交互任务使用 TUI。

## 5. 背压、持久化与终态

control 状态可靠意味着持久保留或显式报告无法继续，不是无界内存、无限阻塞或绝不失效的 stdout。journal 和 stdout writer 有 owner、预算、关闭路径；Core 事件归并不等待慢 UI/输出。

正常时 control 按序写 journal，发送者从已提交序列读取；telemetry 可有界合并。临时变慢可从 journal 追赶；不能安全持久化时禁止新执行、保留 Unknown/过载错误并尝试有界清理，不静默丢 control 后仍宣称状态可靠。磁盘错误、队列饱和和断管必须有注入测试。

stdout 断管不能保证最终 snapshot 送达。停止该 headless 执行所有者，按既有中断/清理路径结束；结果能落 journal 则保留供回放，否则返回非零错误。正常有序退出在清理后输出最终结果和 cleanup 摘要再 flush；强杀/崩溃/磁盘故障下缺终态必须被消费者识别为 unknown，不能把 EOF 当完成。

初始退出码契约（实施前需验证跨平台映射）：0 完成；130 用户中断；2 CLI 参数错误；3 配置/兼容/协议失败；4 断连、执行结果未知或清理未确认；1 其他已确认执行失败。回放成功读取返回 0，但历史任务结果保留于 JSON，不把读取成功当任务成功。

journal 只保存脱敏观察投影。可重复回放要求相同已提交前缀重建相同**持久字段**；实时 age、当前进程时钟、secret、原始任务文本及已淘汰 telemetry 不在相等承诺内。默认 30 天/500 MB，快照压缩保留历史范围标记；活动 control 记录不可静默淘汰。

## 6. 验证矩阵

1. 可控时钟：边界值、覆盖配置、Active → Quiet → AttentionNeeded → Active；重复/旧事件/刷新不重置进展。
2. 父等待 child 的静默/恢复 fixture，同时检查 TUI/Python 等待对象、条件、时钟和进展序号；计时不会新增父请求或释放 Gate。
3. 多 child：一个输出、另一个静默；聚合状态不能隐藏后者。终态、handoff 和“任务已验证完成”分别验证。
4. JSON/Python：首 snapshot、schema、单调 cursor、合并范围、终态/退出码、回放无执行请求、错误 session、历史缺口和部分尾记录。
5. 背压/故障：慢读、断管、满队列、磁盘失败、强杀；控制事实仍可从持久层获取或有明确失败，绝不伪造成功。
6. 交互：等待审批立即显示 requires_action，secret 不进入流；关闭提醒不影响其他消费者。
7. TUI：宽窄终端、无颜色、中文/emoji、长 ID；顶部始终显示活动、最后证据、静默时长、来源和下一步动作。

发布时结合真实单 agent smoke test；多 child fake fixture 不构成 V2 真实多 agent 支持的证明。指标和反馈记录方式见[产品计划](native-agent-tui-plan.md)。
