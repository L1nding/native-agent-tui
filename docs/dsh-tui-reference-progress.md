# dsh-tui 参考迭代实施记录

2026-10-13。目标保持原计划的迭代 0–4；以下是逐项核对记录，不是缩小后的完成定义。

## 已完成的收口

- 建模分支的合并 `ad6b27e` 已解决旧 ACP 冲突；`43df037` 已纳入指定的 `f98f682`、`2a7c20a`、`85acc3d`、`4785e50`。当前 `feat/dsh-first` 从 `4785e50` 快进到 `eeb1bf9`，两分支共享这一已提交基线。
- 原 `4785e50` 上完整执行 `python scripts/verify.py --cargo-test`：格式、check、Clippy、506 项库测试、6 项集成测试、doctest、release/examples、对话 ConPTY 和 Windows 进程夹具全部通过。原插件 `pnpm test` 的 6 项测试通过。
- 建模工作树实际执行 `cargo test --locked --all-targets`：558 项库测试及 6 项集成测试通过、2 项可选测试跳过。该工作树已有的 `src/backend/acp_protocol.rs` 未提交修改保持原样；没有复制或覆盖它。
- `.claude/`、`.qoder/` 加入忽略列表；`refactor-goal.md` 标记为被当前路线取代的历史任务说明。

## 当前实现与证据

| 计划项 | 当前事实 | 验证 / 尚缺证据 |
| --- | --- | --- |
| 1.1 领域扩展接口 | 已接入 `ExtensionSnapshot`、`ExtensionConfirmation`、视图注册接口和 `ConfirmExtension`；建模是第一个注册视图。根 UI 无建模专用滚动或批准分支；实际绘制后才记录确认，覆盖、窄窗口或事实消失清除绑定 | 31 项领域原有回归与注册／绘制／确认专项通过。新增 Core 拒绝运行中扩展确认的验证；旧版本、read-only 重确认和异步授权 generation 仍由原 owner 检查。删除独立入口仍待全流程覆盖证据 |
| 领域事实通道（计划外，真实验收暴露） | dsh 0.2.0-rc.2 的 ACP 只转发给模型看的工具结果；超过 `maxInlineTokens: 12500` 时中间被替换为 `[...]`，完整 envelope 到不了 Core。现在领域工具返回小回执 `modeling-fact-receipt/v1`，Core 用自有的 Job Object 子进程执行 `domain-read-fact`，核对大小、SHA256、上下文、序号和提案绑定后才投影；内联 envelope 仍兼容。同步中或同步失败时不能批准；哈希通过但无法投影的事实显示为同步失败，不再静默丢弃 | 根因证据：`blender-agent-modeler/outputs/complex-vertical-live-20261013-07/transport-metrics.jsonl`（51,156 字节、非 JSON、无 `rawOutput`）与 dsh 会话记录中的 spill 提示。移植自建模工作树 `b20a41d`，并改用 `owned_process`；Core 级、领域级和读取进程（超时、取消、子孙进程清理、输出超限）专项通过。本树未跑真实 dsh 跨层验收 |
| 1.2 结构化工具状态 | `ConversationItem.tools` 携带有界 `ConversationTool`，Core 提供类型、生命周期、命令、目录、目标、退出码、保留输出行数与 diff 统计；ACP 已透传这些事实，并分别映射终端、文件和通用工具；未知字段保持 `None` | 34 项 ACP 专项通过，覆盖已登记 MCP、未登记 JSON、字段与 diff 透传、Unicode／大小边界。安装的 dsh ACP 把工具统一标 `other`，通过真实工具名／结构化输入提取类型；标准 diff 内容块也支持。退出码只接受显式数字，未给出时不解析日志补值。结构化工具目标同时接入详情和搜索 |
| 1.2 UI 状态判断 | 样式、汇总和折叠使用工具事实；旧历史缺事实时保持中性；插件历史新增 typed 工具元数据 | `tool_styles_and_folding_use_core_facts_even_when_summary_words_disagree` 验证摘要写 done、Core 事实为 failed 时按 failed 显示；插件历史新增专项测试 |
| 2.1 滚动锚点 | UI 缓存保存 thread/turn/item/role + 消息内行号；新片段和后续消息到达后固定阅读起点；回到底部恢复跟随；被淘汰的锚点回到最早保留内容 | 原 release 在真实 ConPTY 中失败，`target/conversation-anchor-163b8cac` 与 `2ae045c6` 保存漂移屏幕；修改后 `target/conversation-anchor-fa1e0e24` 通过。Unicode、新消息、淘汰/同 ID 不同 turn 有专项回归。夹具加入默认 `scripts/verify.py` |
| 2.2 活动指示 | 普通对话和领域视图输入区显示活动标记；提示同时包含 Working／等待确认／等待子 Agent 等文字，动画是本地状态，50 ms 限流 | mono 的 TestBackend 验证文字与帧变化；runtime 验证空闲 tick 不请求绘制。ConPTY 的 `waiting-screen.txt` 证明等待首字时可见，默认完整验证含该夹具 |
| 2.3 分阶段耗时 | Core 按事件到达时间把 root 轮次分成等首字、思考、输出、工具四段，写成该步最末尾的 `Timing` 对话项；运行中随事件和 1 秒观察时钟更新，轮次结束后固定。服务端确认完成时附完成时刻（无时区数据，标注 UTC）；断连、停止等没有服务端终态的结束只固定数值。工具运行期间穿插的输出不结束工具阶段。UI 只显示：无标题、弱化颜色 | `turn_timing_accumulates_each_stage_and_freezes_when_the_turn_ends` 用确定性时钟覆盖累计、并行工具、旧轮次事件、排位和冻结；`timing_row_is_a_muted_line_without_a_title_after_the_step` 覆盖渲染与窄窗口；ConPTY 夹具断言四段与完成时刻都出现且结束 1.2 秒后不变，屏幕在 `target/conversation-stream-579de06b`。尚缺：子 Agent 轮次不计时；恢复的历史没有耗时行 |
| 2.4 todo / plan | todo 已接：插件从 `todo/write` 补发整份列表，桥接再次限界后交给 Core，快照字段 `todos`；对话区底部显示紧凑面板（进度、当前项、ASCII 状态标记），全部完成后隐藏，新建或切换会话时清空。plan 模式未做 | 插件、桥接、Core、面板各有专项测试；真实 dsh 跑通一轮（两次 `todo_write` 都推到 TUI 一侧）；ConPTY 夹具断言面板出现和消失。尚缺：子 Agent 的列表不显示；恢复会话后不回放列表 |
| 2.5 steering | 已接：`/steer <文字>` 发 `Command::SteerRoot`；Core 只在 root 轮次 Running 时接受，`GatePending`、空闲和上一条未确认时拒绝且不发请求。桥接只为当前绑定的轮次转发；插件只在轮次开始与结束之间接受，用宿主的 `createUserMessage` 构造消息后调用 `agent.steer`。对话里先显示占位提示，dsh 确认投递后换成用户消息；被拒绝、超时未确认、轮次先结束、执行不可用各有明确结果，不重发 | 插件、桥接、Core 各有专项测试，含 `GatePending` 拒绝；真实 dsh（`hi/gpt-6-luna`）直接走协议验证：轮次中途被接受、收到投递事件、回答里出现引导要求的标记词，空闲和轮次结束后被拒绝；ConPTY 夹具断言引导变成用户消息。尚缺：没有用真实 dsh 在界面里操作过；只支持纯文本；子 Agent 不能引导 |
| 2.6 plan 评审 | 未实现 | 需复用审批面板并保留绑定与请求生命周期 |
| 3.1 工具卡片 / 三档显示 | 未实现 | 需终端、diff、通用三种卡片与预览/展开/隐藏的 TestBackend 断言 |
| 3.2 内嵌思考 | 未实现 | 需助手回答内小节，保留来源事实 |
| 3.3 块顺序 / 权威替换 | 未实现 | 需核对服务端提供的块身份与顺序，覆盖思考/回答/再思考及 final 校正 |
| 3.4 隐藏工具时合并多步标题 | 未实现 | 依赖三档显示，不能打乱原事件顺序 |
| 3.5 输入补全 / 命令 | 未实现 | 需文件路径补全、compact、插件命令；session 引用仍为原计划的可选项 |
| 3.6 会话选择器 | 未实现 | 需标题搜索、工作区切换、不可恢复原因 |
| 3.7 retry / compaction 行内提示 | 已接：插件从 llm/retry 和 compaction/start、end 补发；重试只带次数、上限、延迟和 dsh 的失败代码，不带供应商报错文字。Core 写成对话流里的一行 Notice：重试每次一行，同一次压缩始终一行并显示结果（完成、未完成、结果未知）。只描述 dsh 已做的决定，TUI 不发起重试 | 插件、桥接、Core、渲染各有专项测试；ConPTY 夹具断言两种提示出现且压缩只留结果行。尚缺：真实 dsh 下没有触发过重试或压缩 |
| 4.1 常驻区瘦身 | 未实现 | 需单行目录/分支/模型/token/上下文，保留顶部所有执行与证据事实 |
| 4.2 动态输入提示 | 未实现 | 依赖真实 steering；提示必须对应当前可用命令 |
| 4.3 尾部增量排版 | 未实现 | 需只重排未闭合尾部的计数断言，沿用长历史缓存验证 |
| 4.4 快照复制测量 | 未测量 | 需记录长历史 publish/draw clone 的实际开销，测量后才决定优化 |

现有模型器跨层、医院完整闭环和视觉复核的证据还未逐项审计。已整合的建模文档不是独立入口可以删除的充分证明。

## 验证记录

结构化工具状态完成后的 `python scripts/verify.py --cargo-test` 全量通过，日志 `target/dsh-reference-verification.log`：560 项库测试、6 项集成测试通过，2 项可选测试跳过，无 Clippy 警告；release、所有 native fixtures 通过。插件新增元数据回归后 `pnpm test` 7 项通过。

滚动锚点随后新增 2 项库测试，11 项 conversation 测试及 60 项 UI 根路由测试通过；新的 ConPTY 夹具已验证原实现失败、修改后通过。最终 `python scripts/verify.py --cargo-test` 全量通过，日志 `target/dsh-reference-final-verification.log`：562 项库测试、6 项集成测试通过、2 项可选测试跳过，无 Clippy 警告；格式、check、doctest、release/examples 和全部 native fixtures 通过。最终锚点屏幕保存在 `target/conversation-anchor-70cd3243`。插件最终 `pnpm test` 7 项通过。

本轮未执行真实远程模型调用、未改用户 dsh profile、未提交或推送新增代码。后续先闭合 1.1 和 ACP 工具字段透传，再按原计划继续长任务能力与输入、布局、性能。

2026-10-13 续（Codex 会话因用量上限中断后接手）：补完事实通道的投影结果判定与测试移植，实现 2.3 分阶段耗时。cargo test --locked --all-targets 577 项库测试、6 项集成测试通过，2 项可选测试跳过；Clippy 无警告。完整验证日志见 	arget/dsh-reference-timing-verify.log。

2026-10-13 再续：调研确认 dsh 的 ACP 只转发已提交的整条消息，2.3 的思考与输出两段在真实 dsh 下会接近 0。插件新增 `zf/session/event` 通知补发模型阶段和 todo 列表；耗时在收到阶段事件后改用它们划分，整块到达的输出不再改写阶段。真实 dsh（`hi/gpt-6-luna`）跑一轮确认事件到达；ConPTY 夹具改成 dsh 的真实到达方式后通过，屏幕在 `target/conversation-stream-c658f4c1`。调研结论写入迭代计划的“dsh 能力调研”一节。

2026-10-13 三续：接通 2.5 steering 和 3.7 重试、压缩提示。`python scripts/verify.py --cargo-test` 与插件 `pnpm test` 的结果见提交说明对应的验证日志 `target/dsh-reference-steer-verify.log`。
