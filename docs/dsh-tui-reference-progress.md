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
| 2.4 todo / plan | todo 已接：插件从 `todo/write` 补发整份列表，桥接再次限界后交给 Core，快照字段 `todos`；对话区底部显示紧凑面板，全部完成后隐藏，新建或切换会话时清空。plan 模式经 `/plan` 和 `/plan off` 切换（见 3.5），评审见 2.6 | 插件、桥接、Core、面板各有专项测试；真实 dsh 跑通一轮；ConPTY 夹具断言面板出现和消失。尚缺：子 Agent 的列表不显示；恢复会话后不回放列表 |
| 2.5 steering | 已接：`/steer <文字>` 发 `Command::SteerRoot`；Core 只在 root 轮次 Running 时接受，`GatePending`、空闲和上一条未确认时拒绝且不发请求。桥接只为当前绑定的轮次转发；插件只在轮次开始与结束之间接受，用宿主的 `createUserMessage` 构造消息后调用 `agent.steer`。对话里先显示占位提示，dsh 确认投递后换成用户消息；被拒绝、超时未确认、轮次先结束、执行不可用各有明确结果，不重发 | 插件、桥接、Core 各有专项测试，含 `GatePending` 拒绝；真实 dsh（`hi/gpt-6-luna`）直接走协议验证：轮次中途被接受、收到投递事件、回答里出现引导要求的标记词，空闲和轮次结束后被拒绝；ConPTY 夹具断言引导变成用户消息。尚缺：没有用真实 dsh 在界面里操作过；只支持纯文本；子 Agent 不能引导 |
| 2.6 plan 评审 | 已接。dsh 的 ACP 没有提问应答者，`exit_plan_mode` 和 `ask_user_question` 在那里直接失败；插件注册为应答者，把问题作为 `zf/session/ask` 请求转给 TUI，桥接把它变成 Core 已有的用户输入请求（复用请求面板，没有新面板）。选中的选项与输入的文字分别还原成 dsh 的选择和反馈；不回答按 dsh 的取消语义处理，dsh 撤回问题时 Core 里的请求随之结束 | 插件、桥接专项测试；真实 dsh 直接走协议验证：打开 plan 模式后模型调用 `exit_plan_mode`，问题到达、选择 Approve 后工具完成。尚缺：没有用真实 dsh 在界面里操作过；被评审的正文超过约 20 KiB 时截断 |
| 3.1 工具卡片 / 三档显示 | 三档已接：预览（每次调用一行，长组折叠）、展开、隐藏，`Ctrl+E` 和 `/tools` 循环。展开档在每次调用下列出 Core 给出的事实：工作目录、目标、diff 增删行数、输出行数；没有的事实不显示。与计划的差别：没有按终端、diff、通用画三种独立卡片，也不在对话里展开输出正文（正文仍在 `Ctrl+T` 的详情里） | `tool_display_levels_show_facts_or_hide_tools_and_join_the_steps_of_one_answer` 覆盖三档、窄宽度、搜索焦点落在被隐藏的组里，以及缓存视口与整篇排版逐行一致 |
| 3.2 内嵌思考 | 已接：思考与回答共用一个标题，用竖线 `┆` 标出并弱化颜色，其后的正文不重复标题，隔着这一步的工具行也一样 | 渲染测试与 ConPTY 夹具断言；不依赖颜色也能分辨 |
| 3.3 块顺序 / 权威替换 | 已接。块顺序：桥接在同一条消息的思考块与正文块之间每次切换时给新的一段独立身份。生成中显示：插件把流式文本按内容块分批补发（60 ms 一批），作为预览显示；dsh 提交的整条消息到达时预览被换掉，请求被放弃时立刻撤掉。无界面模式只输出提交的消息 | 插件、桥接、Core 专项测试；真实 dsh 确认预览比提交早约 0.9 秒到达，且提交标记先于整条消息。真实运行还查出并修掉一个问题：无界面模式曾把预览和正式回答各打印一遍。尚缺：预览被替换时，如果用户正停在预览那一段上阅读，位置会回到最早保留的内容 |
| 3.4 隐藏工具时合并多步标题 | 已接：隐藏档下同一轮的后续步骤不重复助手标题；另一轮照常有标题 | 与 3.1 同一个测试 |
| 3.5 输入补全 / 命令 | `@路径` 补全已接：Core 只读地列一层目录（最多 50 条，绝对路径和 `..` 拒绝），Tab 补全光标处的词。dsh 自己的斜杠命令已接：`/compact`、`/plan`、`/permission`、`/feedback` 由插件列出并执行，结果作为对话流里的一行；清单在首次输入斜杠时取一次。会自己发起轮次的命令（`/goal`、`/plan <消息>`）被拒绝，因为那些轮次不属于任何一次提交，界面无法显示。`@session` 引用没有做（计划里标为可选） | Core、插件、桥接、界面各有专项测试；真实 dsh 走协议验证了清单、`/compact`、`/plan`、`/plan off` 和两种拒绝 |
| 3.6 会话选择器 | 已接：`/` 按标题搜索，`w` 在本工作区和所有工作区之间切换；其他工作区的会话标出所在目录，选中时留在页面并说明原因（需要在那个目录启动后恢复） | 界面与解析专项测试。尚缺：没有用真实 dsh 的多工作区数据看过 |
| 3.7 retry / compaction 行内提示 | 已接：插件从 llm/retry 和 compaction/start、end 补发；重试只带次数、上限、延迟和 dsh 的失败代码，不带供应商报错文字。Core 写成对话流里的一行 Notice：重试每次一行，同一次压缩始终一行并显示结果（完成、未完成、结果未知）。只描述 dsh 已做的决定，TUI 不发起重试 | 插件、桥接、Core、渲染各有专项测试；ConPTY 夹具断言两种提示出现且压缩只留结果行。尚缺：真实 dsh 下没有触发过重试或压缩 |
| 4.1 常驻区瘦身 | 已接：去掉只显示产品名的标题行，执行状态成为第一行，顶部少占一行；工作目录旁显示 git 分支（直接读 `.git`，读不到不显示），轮次结束时重读。与计划的差别：没有把顶部压成单独一行，状态行和设置行仍是两行，执行、提醒、证据信息原样保留 | 布局与渲染测试；ConPTY 夹具通过 |
| 4.2 动态输入提示 | 已接：输入区下方的按键提示只列当前可用的操作——运行中是 `/steer` 引导、排队、中断，等待子 Agent 时是排队，空闲时是发送，启动中和会话结束后各有说明 | 渲染测试覆盖各状态与各宽度 |
| 4.3 尾部增量排版 | 已接：消息只在末尾变长时，代码块之外每个写完的行都是检查点，其上的行原样保留，只重排检查点之后的部分；文字被改写、宽度或主题变化、搜索焦点在消息里时整条重排 | 逐次追加与整篇排版逐行逐样式对比，并用字节计数断言只重排了尾部。release 下 32 KiB 流式消息每个片段的重排由 1.85 ms 降到 0.31 ms |
| 4.4 快照复制测量 | 已测量：历史写满（121 条、239 KiB）时每个片段发布一份快照约 0.07 ms（release）。结论：不需要优化 | 两项可选测量随代码加入：`cargo test --release --lib measure_ -- --ignored --nocapture` |

现有模型器跨层、医院完整闭环和视觉复核的证据还未逐项审计。已整合的建模文档不是独立入口可以删除的充分证明。

## 验证记录

结构化工具状态完成后的 `python scripts/verify.py --cargo-test` 全量通过，日志 `target/dsh-reference-verification.log`：560 项库测试、6 项集成测试通过，2 项可选测试跳过，无 Clippy 警告；release、所有 native fixtures 通过。插件新增元数据回归后 `pnpm test` 7 项通过。

滚动锚点随后新增 2 项库测试，11 项 conversation 测试及 60 项 UI 根路由测试通过；新的 ConPTY 夹具已验证原实现失败、修改后通过。最终 `python scripts/verify.py --cargo-test` 全量通过，日志 `target/dsh-reference-final-verification.log`：562 项库测试、6 项集成测试通过、2 项可选测试跳过，无 Clippy 警告；格式、check、doctest、release/examples 和全部 native fixtures 通过。最终锚点屏幕保存在 `target/conversation-anchor-70cd3243`。插件最终 `pnpm test` 7 项通过。

本轮未执行真实远程模型调用、未改用户 dsh profile、未提交或推送新增代码。后续先闭合 1.1 和 ACP 工具字段透传，再按原计划继续长任务能力与输入、布局、性能。

2026-10-13 续（Codex 会话因用量上限中断后接手）：补完事实通道的投影结果判定与测试移植，实现 2.3 分阶段耗时。cargo test --locked --all-targets 577 项库测试、6 项集成测试通过，2 项可选测试跳过；Clippy 无警告。完整验证日志见 	arget/dsh-reference-timing-verify.log。

2026-10-13 再续：调研确认 dsh 的 ACP 只转发已提交的整条消息，2.3 的思考与输出两段在真实 dsh 下会接近 0。插件新增 `zf/session/event` 通知补发模型阶段和 todo 列表；耗时在收到阶段事件后改用它们划分，整块到达的输出不再改写阶段。真实 dsh（`hi/gpt-6-luna`）跑一轮确认事件到达；ConPTY 夹具改成 dsh 的真实到达方式后通过，屏幕在 `target/conversation-stream-c658f4c1`。调研结论写入迭代计划的“dsh 能力调研”一节。

2026-10-13 三续：接通 2.5 steering 和 3.7 重试、压缩提示。`python scripts/verify.py --cargo-test` 与插件 `pnpm test` 的结果见提交说明对应的验证日志 `target/dsh-reference-steer-verify.log`。

2026-10-13 收尾：计划的迭代 2、3、4 逐项接通，状态见上表。最终 `python scripts/verify.py --cargo-test` 通过：600 项库测试和 6 项集成测试通过，4 项跳过（2 项可选 live、2 项手动测量）；插件 `pnpm test` 16 项通过。日志 `target/dsh-reference-tail-verify.log`。

没有做的三件事及原因：

- 删除独立入口 `--modeling`（1.1 的结果项）。前提是领域会话路线覆盖全部建模阶段；这份证据在建模工作树里，那边的真实跨层验收还在进行，本分支无法确认，所以没有删。
- `@session` 会话引用。计划里标为可选，价值没有确认。
- 本分支与建模工作树各有一份领域事实通道（本分支保留内联 envelope 兼容并用 `owned_process` 管理读取进程；`b20a41d` 去掉了内联路径）。合并时需要选定一边。

还没有用真实 dsh 在交互界面里操作过的功能：steering、plan 评审、dsh 斜杠命令、生成中预览。它们的真实验证都走的是协议层，界面由确定性夹具和 TestBackend 验证。
