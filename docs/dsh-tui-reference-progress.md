# dsh-tui 参考迭代实施记录

更新：2026-10-15。原计划迭代0–4已完成；最终逐项证据见[完成审计](dsh-tui-completion-audit.md)。下文保留各阶段的实现与当时缺口，不用历史失败报告替换最终核验。

## 最终收口（2026-10-15）

补齐已应用家具的移动、形状／参数替换和整组件删除，保留完整组件身份、来源绑定、旧共享定义和未选中对象；复用原writer及Core具体版本批准。删除来源保持待确认，重新增补需明确绑定原删除提案。实际TUI批准／应用／Blender、只读恢复、删除来源拒绝和两图进入后续模型回复的证据已独立核对；原观察器失败报告保留，细节见[入口迁移记录](dsh-modeling-entry-migration.md#2026-10-15-已应用编辑与入口收口)。

已删除旧CLI四个参数、专用UI、Rust Python工作流owner与仅服务该owner的fixtures。普通dsh的领域入口通过当前终端夹具；release实际拒绝全部旧参数。`scripts/verify.py --modeler-repo D:/project/blender-agent-modeler`通过，655项Rust测试、4可选跳过、全部native及release/examples；模型器729项和Ruff／格式检查、插件20项通过。日志为`target/dsh-entry-retirement-verify.log`、`target/dsh-entry-retirement-modeler-tests.log`、`target/dsh-entry-retirement-plugin-check.log`。

医院279未决来源、14doubt及人工／规范检查继续如实记录。`@session`原为可选，未实施。代码未提交或推送。

## 阶段记录：定位回退（2026-10-14）

未配置楼层的非默认墙体图层证据、来源绑定选择及同结构具体批准已迁移。实际TUI原提案一次应用并真实构建；回复遇到上游HTTP/2失败的原夹具保留FAIL，新TUI只读恢复同原业务身份3项PASS。当前Rust664／模型器691／插件20项及全部native、release/examples验证通过。完整证据及边界以[完成审计](dsh-tui-completion-audit.md)为准；原0–4目标未缩小，已应用家具编辑尚缺，`--modeling`未删除，goal未完成。

## 已完成的收口

- 建模分支的合并 `ad6b27e` 已解决旧 ACP 冲突；`43df037` 已纳入指定的 `f98f682`、`2a7c20a`、`85acc3d`、`4785e50`。当前 `feat/dsh-first` 从 `4785e50` 快进到 `eeb1bf9`，两分支共享这一已提交基线。
- 原 `4785e50` 上完整执行 `python scripts/verify.py --cargo-test`：格式、check、Clippy、506 项库测试、6 项集成测试、doctest、release/examples、对话 ConPTY 和 Windows 进程夹具全部通过。原插件 `pnpm test` 的 6 项测试通过。
- 建模工作树实际执行 `cargo test --locked --all-targets`：558 项库测试及 6 项集成测试通过、2 项可选测试跳过。该工作树已有的 `src/backend/acp_protocol.rs` 未提交修改保持原样；没有复制或覆盖它。
- `.claude/`、`.qoder/` 加入忽略列表；`refactor-goal.md` 标记为被当前路线取代的历史任务说明。
- 2026-10-14 从 `617e70a` 快进整合 `feat/modeling-workflow` 的 `f72f14c`。两分支已共享该提交；保留主分支的 `owned_process` 事实读取 owner 和内联兼容路径，已无两份待选实现。建模工作树的未跟踪图表未改动。

## 2026-10-13 实现与当时证据（后续更新见下文及最终审计）

| 计划项 | 当前事实 | 验证 / 尚缺证据 |
| --- | --- | --- |
| 1.1 领域扩展接口 | 已接入 `ExtensionSnapshot`、`ExtensionConfirmation`、视图注册接口和 `ConfirmExtension`；建模是第一个注册视图。根 UI 无建模专用滚动或批准分支；实际绘制后才记录确认，覆盖、窄窗口或事实消失清除绑定 | 31 项领域原有回归与注册／绘制／确认专项通过。新增 Core 拒绝运行中扩展确认的验证；旧版本、read-only 重确认和异步授权 generation 仍由原 owner 检查。删除独立入口仍待全流程覆盖证据 |
| 领域事实通道（计划外，真实验收暴露） | dsh 0.2.0-rc.2 的 ACP 只转发给模型看的工具结果；超过 `maxInlineTokens: 12500` 时中间被替换为 `[...]`，完整 envelope 到不了 Core。现在领域工具返回小回执 `modeling-fact-receipt/v1`，Core 用自有的 Job Object 子进程执行 `domain-read-fact`，核对大小、SHA256、上下文、序号和提案绑定后才投影；内联 envelope 仍兼容。同步中或同步失败时不能批准；哈希通过但无法投影的事实显示为同步失败，不再静默丢弃 | 根因证据：`blender-agent-modeler/outputs/complex-vertical-live-20261013-07/transport-metrics.jsonl`（51,156 字节、非 JSON、无 `rawOutput`）与 dsh 会话记录中的 spill 提示。移植自建模工作树 `b20a41d`，并改用 `owned_process`；Core 级、领域级和读取进程（超时、取消、子孙进程清理、输出超限）专项通过。本树未跑真实 dsh 跨层验收 |
| 1.2 结构化工具状态 | `ConversationItem.tools` 携带有界 `ConversationTool`，Core 提供类型、生命周期、命令、目录、目标、退出码、保留输出行数与 diff 统计；ACP 已透传这些事实，并分别映射终端、文件和通用工具；未知字段保持 `None` | 34 项 ACP 专项通过，覆盖已登记 MCP、未登记 JSON、字段与 diff 透传、Unicode／大小边界。安装的 dsh ACP 把工具统一标 `other`，通过真实工具名／结构化输入提取类型；标准 diff 内容块也支持。退出码只接受显式数字，未给出时不解析日志补值。结构化工具目标同时接入详情和搜索 |
| 1.2 UI 状态判断 | 样式、汇总和折叠使用工具事实；旧历史缺事实时保持中性；插件历史新增 typed 工具元数据 | `tool_styles_and_folding_use_core_facts_even_when_summary_words_disagree` 验证摘要写 done、Core 事实为 failed 时按 failed 显示；插件历史新增专项测试 |
| 2.1 滚动锚点 | UI 缓存保存 thread/turn/item/role、消息内行号及正文位置；新片段和后续消息到达后固定阅读起点。Core 原位替换预览，并提供有界的身份衔接；合并块按正文位置映射，分帧正式内容尚未到达阅读点时保留锚点。回到底部恢复跟随，被淘汰锚点回到最早保留内容 | 原 release 在真实 ConPTY 中失败，`target/conversation-anchor-163b8cac` 与 `2ae045c6` 保存漂移屏幕；修改后 `target/conversation-anchor-fa1e0e24` 通过。Unicode、新消息、淘汰/同 ID 不同 turn 有专项回归。夹具加入默认 `scripts/verify.py` |
| 2.2 活动指示 | 普通对话和领域视图输入区显示活动标记；提示同时包含 Working／等待确认／等待子 Agent 等文字，动画是本地状态，50 ms 限流 | mono 的 TestBackend 验证文字与帧变化；runtime 验证空闲 tick 不请求绘制。ConPTY 的 `waiting-screen.txt` 证明等待首字时可见，默认完整验证含该夹具 |
| 2.3 分阶段耗时 | 已从整轮累计补齐为 root 每步计时。插件转发当前宿主 turn 的 `step/start`、`step/end`，阶段携带当前 step；Core 单独累计等首字、思考、输出、工具，在工具之后显示 `Step N` 耗时行，确认步骤结束后固定 UTC 完成时刻，下一步新建独立计时器。重试仍属于原 step；重复／旧步骤不能重置，迟到阶段不能进入下一步。旧提供方没有 step 事件时保留整轮兼容统计。断连、停止等无确认完成事件时只固定数值 | 确定性时钟覆盖两步分别累计、工具排位、冻结和旧身份拒绝；ClientHandle 专项覆盖两步独立行和迟到阶段拒绝；桥接及插件验证身份与边界。ConPTY 的宿主步骤事件、步骤身份及结束后冻结检查通过；新全量验证已通过。尚缺：子 Agent 轮次不计时；恢复的历史没有耗时行 |
| 2.4 todo / plan | todo 已接：插件从 `todo/write` 补发整份列表，桥接再次限界后交给 Core，快照字段 `todos`；对话区底部显示紧凑面板，全部完成后隐藏，新建或切换会话时清空。plan 模式经 `/plan` 和 `/plan off` 切换（见 3.5），评审见 2.6 | 插件、桥接、Core、面板各有专项测试；真实 dsh 跑通一轮；ConPTY 夹具断言面板出现和消失。尚缺：子 Agent 的列表不显示；恢复会话后不回放列表 |
| 2.5 steering | 已接：`/steer <文字>` 发 `Command::SteerRoot`；Core 只在 root 轮次 Running 时接受，`GatePending`、空闲和上一条未确认时拒绝且不发请求。桥接只为当前绑定的轮次转发；插件只在轮次开始与结束之间接受，用宿主的 `createUserMessage` 构造消息后调用 `agent.steer`。对话里先显示占位提示，dsh 确认投递后换成用户消息；被拒绝、超时未确认、轮次先结束、执行不可用各有明确结果，不重发 | 插件、桥接、Core 各有专项测试，含 `GatePending` 拒绝；真实 dsh（`hi/gpt-6-luna`）直接走协议验证：轮次中途被接受、收到投递事件、回答里出现引导要求的标记词，空闲和轮次结束后被拒绝；ConPTY 夹具断言引导变成用户消息。尚缺：没有用真实 dsh 在界面里操作过；只支持纯文本；子 Agent 不能引导 |
| 2.6 plan 评审 | 已接。dsh 的 ACP 没有提问应答者，`exit_plan_mode` 和 `ask_user_question` 在那里直接失败；插件注册为应答者，把问题作为 `zf/session/ask` 请求转给 TUI，桥接把它变成 Core 已有的用户输入请求（复用请求面板，没有新面板）。选中的选项与输入的文字分别还原成 dsh 的选择和反馈；不回答按 dsh 的取消语义处理，dsh 撤回问题时 Core 里的请求随之结束 | 插件、桥接专项测试；真实 dsh 直接走协议验证：打开 plan 模式后模型调用 `exit_plan_mode`，问题到达、选择 Approve 后工具完成。尚缺：没有用真实 dsh 在界面里操作过；被评审的正文超过约 20 KiB 时截断 |
| 3.1 工具卡片 / 三档显示 | 已补齐终端、diff、通用三种投影，正文直接进入对话卡片。终端显示命令、目录及已知退出码；diff 显示目标、增删正文及统计；通用显示工具名与结果。预览显示前三个换行后的可见行及剩余行数，展开显示保留正文，隐藏不画卡片；长组继续折叠。Core 正文单项沿用 64 KiB 上限，全部历史正文另限 128 KiB，旧正文超限保留摘要和截断提示；Arc 共享、序列化跳过、Debug 不输出正文 | 每种类型与每档都有 TestBackend 断言，中文／emoji／组合字符及窄屏、单个超长行的预览上限、权威输出替换后的缓存一致性、Core 流式与终态替换、退役后正文保留和有界内存均通过；ConPTY 实际翻读卡片、正文与早先思考后返回底部 |
| 3.2 内嵌思考 | 已接：思考与回答共用一个标题，用竖线 `┆` 标出并弱化颜色，其后的正文不重复标题，隔着这一步的工具行也一样 | 渲染测试与 ConPTY 夹具断言；不依赖颜色也能分辨 |
| 3.3 块顺序 / 权威替换 | 已接。块顺序：桥接在同一条消息的思考块与正文块之间每次切换时给新的一段独立身份。生成中显示：插件把流式文本按内容块分批补发（60 ms 一批），作为预览显示；dsh 提交的整条消息到达时预览被换掉，请求被放弃时立刻撤掉。无界面模式只输出提交的消息 | 插件、桥接、Core 专项测试；真实 dsh 确认预览比提交早约 0.9 秒到达，且提交标记先于整条消息。真实运行还查出并修掉一个问题：无界面模式曾把预览和正式回答各打印一遍。已补齐预览／正式内容的原位替换和阅读身份衔接，合并块、交替角色及分帧到达均有回归；ConPTY 验证预览段阅读位置在正式替换后保持 |
| 3.4 隐藏工具时合并多步标题 | 已接：隐藏档下同一轮的后续步骤不重复助手标题；另一轮照常有标题 | 与 3.1 同一个测试 |
| 3.5 输入补全 / 命令 | `@路径` 补全已接：Core 只读地列一层目录（最多 50 条，绝对路径和 `..` 拒绝），Tab 补全光标处的词。dsh 自己的斜杠命令已接：`/compact`、`/plan`、`/permission`、`/feedback` 由插件列出并执行，结果作为对话流里的一行；清单在首次输入斜杠时取一次。会自己发起轮次的命令（`/goal`、`/plan <消息>`）被拒绝，因为那些轮次不属于任何一次提交，界面无法显示。`@session` 引用没有做（计划里标为可选） | Core、插件、桥接、界面各有专项测试；真实 dsh 走协议验证了清单、`/compact`、`/plan`、`/plan off` 和两种拒绝 |
| 3.6 会话选择器 | 已接：`/` 按标题搜索，`w` 在本工作区和所有工作区之间切换；其他工作区的会话标出所在目录，选中时留在页面并说明原因（需要在那个目录启动后恢复） | 界面与解析专项测试。真实界面已在dsh-conversation-live-47348cf8切换all/current工作区、搜索标题并恢复本工作区旧会话；跨工作区直接恢复仍按界面边界拒绝，不由本验收扩大 |
| 3.7 retry / compaction 行内提示 | 已接：插件从 llm/retry 和 compaction/start、end 补发；重试只带次数、上限、延迟和 dsh 的失败代码，不带供应商报错文字。Core 写成对话流里的一行 Notice：重试每次一行，同一次压缩始终一行并显示结果（完成、未完成、结果未知）。只描述 dsh 已做的决定，TUI 不发起重试 | 插件、桥接、Core、渲染各有专项测试；ConPTY 夹具断言两种提示出现且压缩只留结果行。真实dsh重试已在building-interrupt-live-c2c26f21/failed-01-conversation.txt显示Retrying model request (1/5): TRANSPORT；source-assessment-live-84e48cf2的持久会话实际出现compaction/start／summary／end，去正文元数据见target/dsh-reference-host-events-20261014.json。压缩时的界面截图仍未单独核对 |
| 4.1 常驻区瘦身 | 已补齐：目录、分支、模型、token 和上下文占用集中在输入框上方的一行。上下文用当前身份匹配的已报告 input/context_window 显示，缺事实显示 unavailable；顶部保留执行、提醒、证据和下一步操作。长字段按显示宽度分配并按完整字素截短，避免目录吞掉后续字段。布局继续共用编辑器与按键几何 | TestBackend 断言元数据紧贴输入框且不在顶部；宽／窄屏及原生输入、滚动、卡片 ConPTY 夹具通过 |
| 4.2 动态输入提示 | 已接：输入区下方的按键提示只列当前可用的操作——运行中是 `/steer` 引导、排队、中断，等待子 Agent 时是排队，空闲时是发送，启动中和会话结束后各有说明 | 渲染测试覆盖各状态与各宽度 |
| 4.3 尾部增量排版 | 已接：消息只在末尾变长时，代码块之外每个写完的行都是检查点，其上的行原样保留，只重排检查点之后的部分；文字被改写、宽度或主题变化、搜索焦点在消息里时整条重排 | 逐次追加与整篇排版逐行逐样式对比，并用字节计数断言只重排了尾部。release 下 32 KiB 流式消息每个片段的重排由 1.85 ms 降到 0.31 ms |
| 4.4 快照复制测量 | 已测量：历史写满（121 条、239 KiB）时每个片段发布一份快照约 0.07 ms（release）。结论：不需要优化 | 两项可选测量随代码加入：`cargo test --release --lib measure_ -- --ignored --nocapture` |

现有模型器跨层专项及部分医院闭环已有真实证据；其当前审计仍明确医院整层未完成，16组语义、9项安装关系及人工视觉待确认。旧入口还支持楼栋目标选择／整段 Python 工作流，不能用局部成功证明全部旧入口能力可删除；原计划的删除前提保持。

## 验证记录

结构化工具状态完成后的 `python scripts/verify.py --cargo-test` 全量通过，日志 `target/dsh-reference-verification.log`：560 项库测试、6 项集成测试通过，2 项可选测试跳过，无 Clippy 警告；release、所有 native fixtures 通过。插件新增元数据回归后 `pnpm test` 7 项通过。

滚动锚点随后新增 2 项库测试，11 项 conversation 测试及 60 项 UI 根路由测试通过；新的 ConPTY 夹具已验证原实现失败、修改后通过。最终 `python scripts/verify.py --cargo-test` 全量通过，日志 `target/dsh-reference-final-verification.log`：562 项库测试、6 项集成测试通过、2 项可选测试跳过，无 Clippy 警告；格式、check、doctest、release/examples 和全部 native fixtures 通过。最终锚点屏幕保存在 `target/conversation-anchor-70cd3243`。插件最终 `pnpm test` 7 项通过。

本轮未执行真实远程模型调用、未改用户 dsh profile、未提交或推送新增代码。后续先闭合 1.1 和 ACP 工具字段透传，再按原计划继续长任务能力与输入、布局、性能。

2026-10-13 续（Codex 会话因用量上限中断后接手）：补完事实通道的投影结果判定与测试移植，实现 2.3 分阶段耗时。cargo test --locked --all-targets 577 项库测试、6 项集成测试通过，2 项可选测试跳过；Clippy 无警告。完整验证日志见 	arget/dsh-reference-timing-verify.log。

2026-10-13 再续：调研确认 dsh 的 ACP 只转发已提交的整条消息，2.3 的思考与输出两段在真实 dsh 下会接近 0。插件新增 `zf/session/event` 通知补发模型阶段和 todo 列表；耗时在收到阶段事件后改用它们划分，整块到达的输出不再改写阶段。真实 dsh（`hi/gpt-6-luna`）跑一轮确认事件到达；ConPTY 夹具改成 dsh 的真实到达方式后通过，屏幕在 `target/conversation-stream-c658f4c1`。调研结论写入迭代计划的“dsh 能力调研”一节。

2026-10-13 三续：接通 2.5 steering 和 3.7 重试、压缩提示。`python scripts/verify.py --cargo-test` 与插件 `pnpm test` 的结果见提交说明对应的验证日志 `target/dsh-reference-steer-verify.log`。

2026-10-13 收尾：计划的迭代 2、3、4 逐项接通，状态见上表。最终 `python scripts/verify.py --cargo-test` 通过：600 项库测试和 6 项集成测试通过，4 项跳过（2 项可选 live、2 项手动测量）；插件 `pnpm test` 16 项通过。日志 `target/dsh-reference-tail-verify.log`。

当时尚未完成的结果项和可选项（最终状态见本文开头）：

- 删除独立入口 `--modeling`（1.1 的结果项）。已核对模型器 `docs/modeling-complex-current-audit.md`：跨层专项完成，但旧入口的楼栋目标选择／医院整层路线尚不能以新领域入口完全替代。保留原计划的全流程前提，继续迁移与验收，不把已有局部闭环当作该结果完成。
- `@session` 会话引用。计划里标为可选，价值没有确认。
- 领域事实通道的分支差异已在 2026-10-14 快进整合中收口，见上面的共同基线记录。

最初尚未用真实dsh界面操作steering、plan评审、斜杠命令和生成预览；这些缺口现已由`target/dsh-conversation-live-47348cf8`补验，详见文末。协议／TestBackend证据继续作为独立回归。

## 2026-10-14 卡片、布局与逐步耗时验证

已整合 `f72f14c`，补齐三类工具卡片正文和输入框上方元数据行，随后将整轮耗时拆成宿主每步计时。全部正文保留于有界内存，流式更新、权威替换与工具退役均保持行为。全量测试还暴露 JSONL 输出完成检查的两次状态读取竞态，已改为同一状态快照内判断错误和完成，避免缺终态日志误报成功。

`python scripts/verify.py --cargo-test` 全量通过：624 项库测试、6 项集成测试，4 项可选测试跳过；fmt、全目标 check／Clippy（无警告）、doctest、release/examples、历史／日志／JSONL、Windows 原生输入、对话流／滚动锚点和进程树夹具均通过。日志为 `target/dsh-reference-step-final-verify.log`；工具卡片与步骤屏幕为 `target/conversation-stream-7b3f2872`，滚动屏幕为 `target/conversation-anchor-9accb5e1`。插件 `pnpm test` 20 项通过，包含宿主 step 身份、重试沿用原 step、旧步骤和错 turn 拒绝。

原计划仍未整体完成：1.1 的独立入口删除前提需要完整迁移／覆盖证据；2.1／3.3 的生成预览滚动身份衔接随后补齐，见下一节。`@session` 是原计划可选项。以上验证不把这两个必需结果改写为完成，也不把模型器的局部医院成功代替整层目标。


## 2026-10-14 预览替换与阅读位置

ACP 为每次生成尝试分配独立 live 身份；正式块携带其替代的预览 ID。Core 在原顺序位置放入正式内容，记录最多128条 thread／turn／role／item 身份及前缀字节数，不保存额外正文。过期轮次、错角色、非预览身份、跨越其他角色的合并、已有正式内容及畸形替代数组均不参与替换；会话切换清除映射。正文和首帧替换在同一次状态处理内发布，空 delta 不撤掉预览。

UI 从共用排版记录正文行的位置，处理同角色多个预览合并、前缀停在行中间、思考与回答交替、窄屏、Unicode 和分帧正式内容，不通过独立换行算法猜测偏移。重试、别的 thread／turn 与弃用预览不能重绑当前阅读锚点。增量尾部排版继续复用已定稿行，同时复用对应正文位置。

建模独立入口仍待迁移；当前代码核对及验收项见 [入口迁移核对](dsh-modeling-entry-migration.md)。缺口包括项目目标准备、从图纸生成结构、整层图纸审查、楼栋汇总以及新入口最终复核恢复。既有部分医院和跨层成功不能代替这些入口路径。

最终 `CARGO_BUILD_JOBS=2 python scripts/verify.py --cargo-test` 全量通过，日志 `target/dsh-reference-preview-final-verify.log`：629项库测试、6项集成测试通过，4项可选测试跳过；fmt、check、Clippy（无警告）、doctest、release/examples 与全部原生夹具通过。插件 `pnpm test` 20项通过，日志 `target/dsh-reference-preview-plugin.log`。最终对话屏幕在 `target/conversation-stream-20bac9f6`，预览替换滚动屏幕在 `target/conversation-anchor-744ec749`。

验证过程中曾遇到 Windows 页面文件不足（OS1455），降低 Cargo 构建并行度后恢复。ConPTY 夹具还捕获一次页眉已更新、正文 diff 尚未写完的中间帧；修改为一次读取已稳定屏幕，保持逐行锚点断言，连续四次专项和后续两次完整原生夹具通过。增量正文位置与整篇排版对比、分帧等待、空 delta、角色交替、非法合并及公共 ClientHandle 原位发布均有回归。本轮代码未提交或推送；原计划的独立入口迁移仍进行中。


## 2026-10-14 项目入口接续与实际图像通道

接续会话 `01a133e5-1fb8-7743-8412-50ee790611a1` 的目标选择后执行迁移。模型器提供 `prepare_project_floor`，逐层建立隔离 CAD 结构候选和具体提案；当前 TUI 的 Core 只接收绑定原目标选择和作用域的完整事实，通过 Ctrl+Y 授权具体版本。重新选择目标后拒绝复用或授权旧作用域，恢复完全相同的原选择则只读取原提案。`inspect_project.recovery_index` 可发现原选择和已准备作用域，不激活或重放执行。技能说明已按这些实际能力更新。

`target/project-live-aeb56d9d` 已由真实 `hi/gpt-6.1-sol` TUI Agent 完成两个选定楼层的结构准备、两次具体批准、应用、Blender 构建、重复构建和 GLB 回读。每层恰好一份授权，权威哈希与具体候选相等，原项目输入字节不变。但该轮的图像阶段原判定只检查 MCP 的 `attached_png`，随后核对真实 dsh 记录发现图片没有进入模型上下文。因此该轮只作为结构／执行证据，不能支持工作流 Agent 视觉复核完成。

实际图像失败有两处：当前自定义模型配置没有声明 `input: [text, image]`，dsh 把图片变成 unavailable 诊断；桥接层在混合图片／诊断内容旁未提取小回执，Core 显示同步失败。桥接现只接受一个完整 `modeling-fact-receipt/v1`，仍拒绝多个 JSON 候选、显式错误和超限主结果。回归先复现 structured_output=None，再验证修复。验收新增 `domain_project_images.py`，按 journal 的唯一 thread_id 读取本次 dsh 持久会话，仅导出回执哈希、附件元数据和步骤序号；必须有非 offloaded 图片及同轮后续模型回复。5 项验收器测试覆盖 unavailable 文字、offloaded、错 turn 和未发生后续模型步骤。

创建隔离测试 profile `zf-tui-project-images-7002f57d`，只在该副本声明模型图片输入，没有改写默认 profile 或读取凭据。`target/project-recovery-e5607402` 新会话只读恢复通过：原选择、方案、授权、操作、build_id、权威场景和来源哈希保持不变，无新业务身份；原产物 top 图片进入实际图像附件，随后模型继续回复。图像 metadata 在 `model-image-admission.json`，真实项目人工复核仍 `not_run`。F6 把 Agent 观察指向实际回复，避免用工具的 visual_analysis=not_run 断言 Agent 没看图。

验证：`target/dsh-project-image-verify.log` 全量通过（636 项库测试、6 项集成测试，4 项可选测试跳过；fmt、check、Clippy、doctest、release/examples 和原生夹具含项目入口）；插件20项通过，日志 `target/dsh-project-resume-plugin.log`。模型器最终 `scripts/check.py` 596项通过，日志 `target/dsh-project-final-modeler-verify.log`。F6 文案调整有11项领域视图专项回归通过。最终 `target/dsh-project-entry-final-verify.log` 全量再次通过，插件20项最终日志 `target/dsh-project-entry-final-plugin.log`，模型器项目域21项专项再次通过。

原计划仍未完成：医院整层图纸问题与受控修正、完整来源处理、楼栋总产物及连接检查、异常阶段覆盖和独立入口删除仍需证据。两层示意结构与新会话恢复不能替代这些结果；当前代码未提交或推送。


最终实际两层＋看图闭环在 `target/project-live-8b76908c` 全部通过：同一工作流 Agent 对 f08 和 f09.b 分别准备、经 TUI 具体版本批准、应用、构建、独立重建和 GLB 回读；原输入字节未变，每层仅一份授权。top 与 perspective 两份回执均在 dsh 原会话中关联到非 offloaded 图像附件及同轮后续步骤，证据 `model-image-admission.json`。`agent-observations.txt` 保存工作流 Agent 最后回复，实际描述矩形围合、墙板、明暗分界及隐藏面／精确尺寸等未覆盖细节；只记录有限看图观察，不写整体复核通过。新会话恢复证据在 `target/project-recovery-e5607402`，同样保存实际看图回复和图像准入元数据。

中途的旧 release、方案滚动、错误回执文件名和过宽图像判定均已修正，失败目录保留以说明验收边界；另一次上游 `upstream_http2_stream_error` 发生在应用／构建已完成后。夹具遇到这个明确错误时仅发起新的只读事实查询并比较业务文件哈希与身份集合，不能重放应用或构建。成功证据仍以最终完整运行记录为准。


## 2026-10-14 楼栋任务、CAD 结构证明与图纸审查

接通 `project_building` 与 `drawing_audit` 专用事实，Core/F6 按真实身份显示，独立于楼层提案批准。每个楼栋 build_id 绑定完整选定目标、逐层当前提案／授权／已完成操作、源／配置／权威哈希与同名轴网／绝对 CAD 标高；快照在楼层锁内复制，owned worker 异步构建。稳定 ID 只启动一次，重复调用、failed/cancelled 和丢失 owner 的 unknown 均不重放。提供 inspect、validate、cancel 与楼栋 render/read-image，恢复索引报告 total/truncated。输出完整性 valid 与输入当前性 current 独立，未决来源为 attention、连接和人工复核仍 not_run。

`audit_drawing` 调用旧流程五类确定性检测器，核对当前权威 Scene IR、提取配置和原 DXF，返回完整总数和分页身份；候选不是 AI 结论。尚未迁移的语义裁决、候选局部图及 decision-backed 结构修正继续明确保留，不能把零候选当无问题。

真实 CAD 结构含柱和参考元素时，旧验证路径把所有 elements 当作模型生成的跨层构件。现以 `cad-structure/v1` 封存完整重建场景并纳入 source dependencies：初始化必须是一份空的绑定楼层，每个操作与源重建实体、作用域、assumed 依据及来源 ID 完全相等。准备和应用重复核对；没有证明、篡改柱高或改变封存源均拒绝。只有该确定性完整初始化使用结构路径，后续模型构件继续执行原 hosted/vertical/placement 检查。未下调通用 derive_config 的墙线阈值。

真实当前闭环 `target/project-live-e00b3a0f` 全部通过：原 DXF 包含四根柱、四面墙与楼板，f08/f09.b 各一份具体授权，分别应用、构建／重建／GLB 后生成两层楼栋；楼栋同样核对实际导出网格与重建／GLB 身份。`building-core-screen.txt` 保存 F6 的 project_building、valid/current 与未检查项，`building-image-admission.json` 关联真实楼栋图片和模型后续回复，`drawing-audit-facts.json` 保存当前楼层审查事实。原项目输入和已批准候选哈希核对不变。较早的 `target/project-live-c774aa2a` 证明异步楼栋和 F6，但没有覆盖新带柱证明，保留各自范围。

新会话楼栋只读恢复 `target/project-recovery-936f56ea` 通过，读取原楼层与楼栋图像，业务身份集合和原输入／提案／授权／操作／楼栋快照及所有已构建文件哈希不变。`model-image-admission.json` 独立关联两份实际图像。前一次恢复 `215d6c3c` 上游响应流中断，保持失败，不改写报告；后续成功是新的只读恢复运行，不称为第一次成功。

当前验证：TUI `target/dsh-project-building-verify.log` 全量通过（639 库测试、6 集成，4 可选跳过；fmt/check/Clippy/doctest/release/examples 与全部原生夹具）；模型器 `target/dsh-building-modeler-final-check.log` 625 项及 lint/format 全通过。索引当前性的最后专项32项通过。模型器专项覆盖构建ID幂等、未知 owner、源／上下文／权威变化、实际 artifact hash 验证、失败／取消终态及结构证据反例。楼栋取消状态采用可控进程替身测试，其通用 owned-process 进程树另有专项；未冒称完成楼栋真实中途取消验收。

仍未完成原计划：医院新项目入口的完整来源业务处理、图纸语义裁决和受控结构修正、楼栋跨层连接及总报告、初始化／楼栋异常的完整真实覆盖，以及满足前提后的独立入口删除。下一项正在用医院原 DXF 的隔离副本验证新入口整层结构，不能用小型楼栋替代医院目标。


## 2026-10-14 医院项目入口恢复与图纸判断

医院原 DXF 的隔离副本在普通 TUI 项目入口完成 2,220 项确定性结构候选的具体批准、应用和实际 Blender 构建／重建／GLB 独立验证，成功恢复记录为 `target/hospital-project-entry-43e8a086`。保持原 proposal `106dde911bc84b4c9eda42a36f86bd0e`、revision 1、prepare token 和候选 SHA256；全程只有一份原 Core 授权，原 DXF 与复制配置哈希不变。测试代理批准仅覆盖结构试验，不代表医院构件业务、净空或人工复核完成。

此前两次失败分别保留：`3b628d9b` 的 MCP 准备请求超时，后台候选已生成但 Core 缺少 project_scope；只读 `inspect_project_floor` 现可恢复已激活楼层的原作用域，然后 inspect_change 清除对应同步失败。`4e78662c` 恢复了候选并写入授权，但完整保护对象列表让批准回传超过 Core 64 KiB 上限，应用未开始。控制回传现只含六个具体版本身份字段，完整范围仍保存在原授权中并在应用时核对。最终恢复核对唯一原授权、原测试决策及基线后，通过 TUI 将其交给同一工作流 Agent 续接，没有重复批准。不能将两次失败改写为一次连续通过。

Core 回归覆盖丢失作用域时拒绝提案、只读恢复作用域后仍需重新检查提案；大范围授权回传测试先失败、修复后通过，同时核对完整持久授权没有丢失保护对象。F6 控制错误移到明细之前，避免错误藏在两千多条实体后；底部滚动测试仍验证最后一条业务明细可达。

图纸审查新增 render_drawing_candidate、read_drawing_candidate_image、record_drawing_judgement 和 prepare_drawing_correction。候选、当前权威、提取配置、原 DXF、实际 PNG、读取尝试和 Agent 判断分别保留身份；判断是 error/doubt/not_error，显示 judged_count 与未覆盖页，人工复核仍 not_run。结构修正通过专门证据检查绑定原判断中的完整操作、局部范围、原始来源和决策草稿，再进入同一具体批准／应用通道；普通构件保护规则没有开放通用结构修改。未批准时不写权威或决策文件；源／图／判断变动会使旧提案失效。

真实医院局部判断 `target/drawing-review-live-f756bd60` 通过：原图共有16个候选，本轮只读第一项 orphan_symbol。模型实际收到640×640 PNG，并描述红色框格与灰色周边线条；现有证据不能区分引用遗漏、宿主提取遗漏或图纸问题，记录 doubt、无修正操作，另外15项保持未复核。`image-admission.json` 将该PNG与同轮后续模型步骤关联，`agent-judgement.json` 保存具体解释；原上下文、权威、提案、授权、操作和构建产物哈希／身份不变。不能把这一项判断当16项审查或医院语义完成。

模型器当前全量 `target/dsh-drawing-review-modeler-check.log`：634项测试及 Ruff／format 通过。图纸专项覆盖真实确定性厚度检测、PNG读取、不可变判断、无批准无写入、单一提案幂等、决策随应用写入、无授权拒绝、越界／错误来源／缺证据／图片篡改反例。受控修正的真实TUI闭环正在另一个作者测试图上验收，尚不计入完成证据。


图纸真实验收继续暴露并修复了协议边界：中文判断说明原先误用512字节短标签限制，模型器允许4000字符，导致正常长说明被Core拒绝。现仅对判断解释及对应修正摘要按4000字符核验，其他身份短标签限制不变；决策正文按独立大小界限读取。回归同时覆盖中文多字节边界、超限拒绝，以及200项候选页每一项都核验并展示，避免原先只处理前100项。

`target/drawing-correction-live-265f0d8b` 保留第一次真实修正失败：结构已批准构建，模型实际看图、记录error并生成具体修正，但Core拒绝长说明，第二份授权尚未生成。`aee12a70` 只读恢复同一提案时，模型对read_fact_view错误传入limit=1000/200，工具拒绝并阻止批准；没有重新prepare或生成授权。现MCP schema与技能明确limit=1..40、文本固定最多800字符／页，省略内容沿next_offset读取。新的恢复沿同一原候选继续，结果待独立记录，不改写失败报告。

医院原判断的增强只读验收 `target/drawing-review-live-40ea1f41` 通过：除实际PNG准入及原业务哈希／身份不变外，还要求F6没有事实同步失败，并实际滚动到“已记录1/16”的Core投影。原doubt观察未重写。较早的f756bd60证明实际图片与持久判断，但没有断言Core同步，不能单独作为F6判断显示证据。

已批准决策记录也纳入执行事实完整性检查。决策正文缺失或改变时，来源当前性为false，操作对账及新的构建拒绝；只有候选与完整原决策都恢复后，才将applying操作对账为applied，并证明没有再次调用patch writer。楼栋快照同时封存其输入提案的已批准决策文件，避免只核验场景而漏掉偏离DXF的批准依据。


真实受控修正恢复 `target/drawing-correction-live-3b931f4e` 三步通过：只读恢复原初始化与未批准修正，F6展示同一原提案后单独Ctrl+Y批准，Agent应用及实际独立构建验证，TUI正常退出。原初始化与修正合计两份具体授权。`independent-geometry-check.json` 独立比较前后Scene IR：只有wall.f08.A8.C8.A8.C9的厚度从0.25m变为0.2m及相应来源／决策元数据，所有身份和其余几何字段不变；Blender、重建、GLB三阶段各10个网格与当前build plan身份精确相等，权威SHA与批准候选相等。原DXF及配置哈希不变，`image-admission.json`绑定实际图片及后续模型步骤。之前两次失败各自保留。

医院整层16个图纸候选已在 `target/drawing-review-live-c5a73dc5` 全量完成实际PNG准入和判断记录：error 6、doubt 9、not_error 1，既有首项判断未重写。F6显示recorded、16/16，原上下文／权威／提案／授权／操作／产物哈希与身份不变。这是Agent判断记录完整，不代表16项通过：6项error尚未准备医院修正，9项doubt继续待定，人工复核仍not_run。

本轮TUI全量 `target/dsh-drawing-review-final-verify.log` 通过：650项测试（4项可选跳过）、fmt/check/Clippy/doctest/release/examples及全部原生夹具。模型器 `target/dsh-drawing-review-modeler-final-check.log` 634项与lint/format通过；随后初始化与来源范围的新专项另行继续验证，不能把旧全量结果作为新增代码的最终基线。

初始化中断核对发现已存在但未登记的楼层目录原先会先重建、后检查authority存在，可能在报错前覆盖提取文件。新增两项回归分别覆盖有／无authority的部分目录，先红后绿：现在分配新stage之前拒绝已有目录，不调用重建也不写任何原文件；inspect_project的incomplete_floor_scopes索引以unknown显示，含total/truncated，不自动采用或重放。真实中途进程取消仍不由这两个可控目录测试替代。

来源完整性发现另一差异：当前医院新入口默认扫描的是323个观察对象、11组；旧医院构件范围是明确四个图层中的487实例、16组。两者不能混用。现新增通用component_sources项目配置，严格核对非空／唯一图层及可选已有分类规则，封存至目录、楼层上下文和来源依赖；配置变化会使旧选择／作用域失效，必须新建上下文。未配置的旧目录身份不因软件新增展示字段而变化，F6明确显示默认范围。专项以自定义USER-FIXTURES图层证明完整实例保留，非法配置在写入领域状态之前拒绝。正在新的医院隔离上下文使用原有四图层声明核对487项，原医院结构／16项图纸判断上下文保持不变。


## 2026-10-14 完整来源范围、评估恢复与楼栋总报告

新配置医院上下文 `target/hospital-project-entry-8df28336` 已通过真实 TUI 结构准备、单次具体批准、应用、实际 Blender 重建和 GLB 回读。prepare 曾返回 Request timed out，同一会话通过 inspect_project_floor 恢复既有作用域，不能描述为零工具失败。它绑定四个明确构件来源图层，共487实例、16组；此前323实例／11组仍只是默认观察范围。`independent-component-inventory.json` 从原始DXF直接提取并逐页核对487条实际初始ledger，missing、extra、pose／身份／镜像差异均为0。夹具已包含该独立核对。

`target/source-assessment-live-84e48cf2` 的16组评估均已写入，但原验收因Core显示缺失而失败。source_groups与source_ledger共用来源序号水位，实例分页清掉组投影后，复读语义不变的旧组事实无法恢复。现Core以source-view/1的view／scan／scene哈希识别同一封存来源状态，保留两种投影；较旧等价事实只补缺失形态，不替换已存在的新形态，水位始终取max。无封存标记的旧协议保持严格顺序，不同状态仍拒绝迟到页。两项回归和既有过期页测试通过。`target/source-assessment-live-c587d0a2` 真实只读恢复通过，F6同时显示已评估16组及487实例，评估文件、场景及业务身份哈希不变。

487实例当前分类：pending_addition 230、pending_confirmation 195、capability_missing 48、suspected_duplicate 14。评估均为assumed，尚未建成这些来源构件，也没有人工确认。旧医院图纸16项判断绑定另一默认来源上下文，不转移为这个新上下文的审查结果。

模型器新增check_project_building_connections与read_project_building_report，封存连接来源、来源/图纸未决统计和总报告。实际冻结的vertical_connections证明参与源几何、服务楼层、楼板孔、支承／标高／碰撞检查；空声明、缺相邻连接或旧job缺冻结证明均不能算passed。来源解释／处置、图纸判断及其原先缺失后新建均纳入当前性；冻结连接清单篡改会使产物验证失败。总报告技术状态与整体状态分开，人工复核始终not_run；Core严格核验数量、输入绑定、连接passed所需正向覆盖与proof哈希，同绑定的后续普通检查保留报告，失效时显示历史。

真实报告首次 `target/building-report-live-00db1f88` 因upstream_http2_stream_error失败，记录保留。第二次 `target/building-report-live-d29b8f02` 通过：读取原两层build_id，F6展示attention总报告，明确旧job缺连接来源及相邻楼层没有已验证连接；全部原输入、authority、授权和产物哈希不变。该报告不证明项目单层作用域已经支持新的跨层提案／写入。

楼层重新激活专项已先红后绿：旧A→B→A复用A的序号9，低于B的10；Core还接受旧A激活的迟到提案。现登记根维护正整数activation_revision，切换已登记楼层时增加一次，同一当前楼层幂等；scope、context、提案／批准身份不变。新增activate_project_floor仅切换已登记且仍在当前选择中的楼层；inspect_project_floor保持只读，旧登记只在显式激活时安装代次。所有楼层事实携带代次，Core拒绝旧代次、倒退、缺失或畸形代次（兼容尚未升级的旧事实）。新代次参与语义回执，避免跨楼层复用旧序号。真实TUI验收仍单独记录，不能由这些单测替代。

最新全量：模型器 `target/dsh-activation-modeler-final-check.log` lint／format及646项通过；TUI `target/dsh-activation-rust-final-check.log` fmt／all-targets check／Clippy／doctest及657项通过（4项可选跳过），release构建见`target/dsh-activation-release.log`。此前654项结果仅覆盖报告阶段，不作为新激活代码的最终验证。


已登记楼层真实TUI切换`target/project-activation-live-e6c7e424`通过：同一会话A=f08→B=f09.b→A，F6逐次显示原作用域和原已应用提案；activation_revision为1／2／3，scope事实序号为30／33／35，原场景、提案、授权、操作、job和构建文件哈希完全不变。此前`dd2bca84`的首轮失败来自夹具只看F6顶部目录区；当时Core页首已显示floor.f08.trial、Agent回传activation_revision=1，夹具尚未滚至作用域行即报错。现夹具滚动核对实际scope行，失败记录保留，未修改业务事实迎合验收。


## 2026-10-14 医院来源首批实际建成与楼栋实际断连

`target/hospital-component-batch-e027ed5c`真实TUI完整通过：TUI Agent按两个床组准备95实例，首提案`6381725952154ce48e8990a7690b728b`被19条实际collision阻塞；保留CAD姿态和阻塞身份后，以76实例子集准备`bd45fbac120246e583df482d496d4295`，Core展示具体版本并经单独Ctrl+Y批准。随后应用、真实Blender构建／重建／GLB与实际top/perspective PNG准入均通过，未移动或缩小CAD足迹避碰。原碰撞19实例未建成。床面0.55m、床垫0.10m及床头0.90m等为编码Agent代理验收允许的assumed示意参数，不代表真实医院确认。

`independent-batch-verification.json`独立核对：当前权威恰好为原场景加批准的877个solids、1个primitive、2个templates，全部既有结构和身份不变；76精确来源身份属于原pending_addition床组，原评估未改写，新增恰好一份具体授权及一次应用。实际built／rebuilt／glb_roundtrip各3096个mesh身份与当前公开build plan精确相等且无重复。完整487实例仍为16组：built76、pending_addition154、pending_confirmation195、capability_missing48、suspected_duplicate14；人工not_run。旧结构产物与原DXF／配置／提案／授权／操作哈希保留。新增独立几何观察器3项反例测试，覆盖原墙被改、额外添加、候选形态改变、错误来源与丢失CAD姿态，加入verify.py。

楼栋实际取消`target/building-cancel-live-8fefb9a1`遇到完成竞态：确实观察到属于该TUI的实际Blender进程及创建身份，但模型调用cancel前已built，严格保持built，验收失败保留。不能写为取消已验收。首次断连`969f06ce`确实到达Core Disconnected／unknown，但夹具只等Completed／Failed而超时；该失败仍保留。随后用更早两层样本`c2c26f21`时因缺轴网／绝对标高而拒绝装配，没有真实Blender，中断未发生，亦保留失败。

`target/building-interrupt-live-2d37e25e`实际断连验收通过：在原两层项目新派生任务的真实Blender运行时，按进程祖先链和创建时间只中断本次TUI拥有的dsh，确认实际Blender退出；Core显示Disconnected及执行结果未知。新TUI只调用原build_id的inspect/validate，F6恢复unknown、valid=false；原job及快照文件哈希不变，没有assemble重放、批准或authority改动，旧产物不变，恢复TUI正常退出。该证据证明实际中途断连和未知结果恢复，不冒充主动cancelled或初始化中断验收。

`target/dsh-activation-native-final-check.log`release examples及全套原生夹具通过，包含项目入口、实际Core来源显示、滚动锚点和Windows进程树；新增独立几何观察器专项另行3项通过。


主动取消补验`target/building-cancel-live-55ae7d75`通过：仅本次TUI子进程环境将BLENDER_EXECUTABLE指向测试wrapper，实际Blender先执行绑定精确context SHA与build_id/output路径的暂停脚本，再接原未改动生产构建脚本。暂停发生在真实Blender进程内、生产导出之前，不能描述为完整构建中途取消。TUI Agent通过cancel_project_building终止同owner进程树，持久state=cancelled、owner_tree_confirmed=true；实际Blender进程句柄确认退出，F6显示取消，原业务文件／旧产物不变，正常退出。默认模型配置及用户环境未持久修改。此前不加暂停的完成竞态失败不改写。


医院真实初始化中断`target/project-init-interrupt-live-858140c1`通过：新隔离副本在prepare实际写入config／enclosure／assemblies后、重建尚未完成且scope未登记时，按精确context参数和进程祖先链中断本TUI的dsh。Windows venv会出现相同模块/参数的父子Python进程，夹具选该链唯一叶节点，再校验归属；前次812cf35d因误要求只一个Python而中止，失败保留。实际断连后MCP与dsh创建身份均确认退出。新TUI的incomplete_floor_scopes报告unknown，明确一次prepare重试由project_floor_incomplete拒绝；既有三个部分文件和所有输入哈希完全不变，没有scope自动采用、authority应用或授权，恢复正常退出。该证明是实际CAD准备中途断连；不扩大为Blender构建取消证明。


医院椅组首批`target/hospital-component-batch-15290c96`真实TUI三步与独立验证通过：原pending_addition中的100精确椅实例，经Core展示`e16a46d6679b4c15a26289951e61307d`、独立Ctrl+Y、应用及实际构建。新增688solids、9primitives、3templates，原结构及床实例全部不变；只有一份新增授权／应用，原输入和业务哈希不变。三阶段各3784mesh与当前build plan精确相等，top／perspective实际图片准入通过。合计built176、pending_addition54、pending_confirmation195、capability_missing48、suspected_duplicate14，仍487实例／16组。先前76床的验证绑定其当时权威，不把它的旧产物当作当前100椅批次产物；每批各有自己的精确SHA和build_id。


## 2026-10-14 真实对话交互补验

`target/dsh-conversation-live-47348cf8`独立隔离工作目录的14项真实dsh界面检查全部通过：空闲／运行提示、宿主斜杠命令清单、@路径及Tab、工具调用与每步冻结计时、todo出现／清空、Ctrl+E三档、运行中steering实际改变最终回复、/plan及请求面板Approve评审、/plan off、生成预览、/compact、会话选择与正常退出。流式回答在运行中观察到46种增长长度；/compact报告压缩42条历史／约5238tokens，回复在conversation-08-compact.txt。会话页实际切换all/current、搜索及恢复本目录原会话。计划任务明确不写文件，最终独立核对README不存在。未观察到思考增量，不虚构Thinking阶段。

首轮`target/dsh-e2e-623ce81a`在ping请求更宽权限时，夹具误把审批等待当作可编辑运行窗口，要求/steer提示而失败；随后/plan检查也被未处理审批阻塞。正确界面当时显示Ctrl+Y／Ctrl+N。现夹具只在没有审批请求的运行工具窗口发送steering，3项回归核对审批等待、工具运行及完成状态，加入verify.py；首次失败和精确夹具树终止记录保留。新夹具遇到首个失败停止继续发送无关交互。该验收证明正常工具运行时steering；审批等待仍以批准按键为当前操作，不当作隐含steering支持。

`target/dsh-reference-plugin-final-check.log`桥接插件当前20项测试通过。真实重试TRANSPORT1/5的界面证据与宿主持久compaction事件元数据亦已记录；不从无思考增量的模型推断推理行为。


## 2026-10-14 医院可新增来源批次收尾与最终原始DXF核对

`target/hospital-component-batch-28f9dbb0`的32实例批次通过真实TUI及独立验证：原35候选中的3个柱式盆与围护墙collision保留未建，27椅＋5柱式盆以具体提案`65868993f28b4722929876504ec63a6a`第2版独立Ctrl+Y批准后应用。原35项提案`7ed6fc2036164f47b37c3c8b12eb9c8e`及其三条诊断保持superseded历史，不将它的碰撞计为新候选通过。新增177solids、3primitives、1template；复用已有椅模板，原结构／床／上一批椅全部不变。新授权／应用各一份；actual built／rebuilt／glb_roundtrip各3961mesh与公开build plan精确相等，两张top／perspective PNG真实准入。

当前权威SHA为`cc77f1dea257a9759891b98d1fc0b0b9c046ab173f8e87febbeb3a63e8a4f8ad`。全部487来源／16组／16组评估继续保留：**built208、pending_addition22、pending_confirmation195、capability_missing48、suspected_duplicate14**。22待新增是19床和3柱式盆的实际碰撞，未移动、缩小足迹或改标签绕过；其余语义、宿主、竖向尺寸和能力缺口继续未决。三批built分别76床、100椅、27椅＋5柱式盆；不能把示意assumed写为真实医院人工确认。

最终raw-DXF独立核对最初误用了仅接受“全部未分类pending_confirmation”的初始化观察器，`target/dsh-hospital-remaining-independent.log`保留批次PASS后该额外核对失败；实际存在同一ledger的15页0→487连续证据。观察器保留原初始化约束，另接受显式expected_state_counts以核对当前混合状态，并从完整页逐行Counter复算其状态。新增8项专项覆盖初始化正例／分类拒绝、当前正例、缺页、伪造总数、逐行状态不符、镜像和错场景。实际重验`target/dsh-hospital-final-inventory-retry.log`通过：独立487实例、1份完整当前ledger、2492条历史及当前实际页行全部身份／姿态核对，missing／extra／pose_or_identity均0，当前状态与208/22/195/48/14精确相等。新结果在批次目录的`independent-component-inventory.json`，原失败日志不改写。

基于这个新487上下文和最新权威的图纸审查继续真实TUI验收；旧323观察对象上下文的16个判断始终仅作为其原上下文历史，不移植。


新增当前台账观察器后，模型器最终`target/dsh-final-inventory-modeler-final-check.log`lint／format及654项全量通过（含8项原始来源观察器专项）。Rust仍为最新657项通过／4项可选跳过（无后续Rust改动）；插件当前20项通过。最新原生夹具在加入独立几何／steering窗口反例后重新运行，`target/dsh-final-native-check.log`全套通过。


当前487来源上下文、权威`cc77f1...a4f8ad`的全量图纸复核`target/drawing-review-live-2206ec5e`通过：16张实际候选PNG准入后逐项记录判断，F6显示recorded／16/16，原业务哈希和身份不变，正常退出。`agent-judgements.json`为14doubt、2not_error，fix_operations全部空；首项原观察不重写。第一项实际复核证据`target/drawing-review-live-d6f6ac7a`先通过。旧默认观察上下文`c5a73dc5`的6error／9doubt／1not_error始终保留为其独立来源／权威绑定，不与新结果合并、不挑选有利结论覆盖历史。模型观察差异不能代替人工裁决，当前14doubt仍待资料／核对，没有医院修正批准或应用。

本轮新增收口并不完成原计划：`--modeling`仍保留。后续必须完成项目跨层提案／执行owner迁移及相应真实审批／构建／恢复证据，继续279未决来源（22实际碰撞待新增、195待确认、48缺能力、14疑似重复）与图纸问题处理，再完整审计旧入口删除条件。本轮未提交，也未改写既有失败证据。


## 2026-10-14 项目跨层连接提案／writer迁移续验

新增模型器`integrations/project_connections.py`与`orchestrator/assembly_sources.py`，将已批准、当前且验证通过的完整楼栋复制到独立连接authority。普通单层contexts不修改。仅下层来源编译的stair/shaft elements新增；原楼板、上层和所有原几何只读，复用原DomainChanges具体提案／授权／单一patch writer及artifact jobs。原scope/context/授权不继承为新连接批准。`project_scope.assembly_binding`和新楼栋`connection_authority`进入Core严格类型检查，连接authority应用不误使原楼层楼栋历史化；新连接派生产物跟踪自己的权威SHA。

CAD初始化可选slab_opening_source_ids明确绑定完整闭合观察线圈，缺失／未闭合／重叠／超界／重复都拒绝，空字段不改变旧序列化。它只在新结构具体批准前引入有来源的孔洞，不提供已批准楼板切孔。跨层专项先在缺字段与缺连接seam处变红，Core也先复现无已验证父楼栋仍接受连接scope的失败，再修复。专项覆盖来源状态缺失→出现、原权威／父产物／上层孔洞／引用证明变更、无授权合法几何注入、混合计划、错相邻层、原scope与artifact串用、修订恢复、部分目录未知与ID重放边界。

新真实两层图纸基线`target/project-live-708a4d47`通过两次具体结构批准、实际逐层构建、楼栋合并和实际PNG准入；包含明确CAD楼梯来源及上层闭合孔洞。首次连接运行`target/project-connection-live-4c77ef5a`已prepared16部件、未生成授权，但界面夹具错误要求连接绑定说明与方案标题同屏，分页跳过了组合位置而失败。失败保留；新TUI只读恢复同一原提案，分别记录作用域绑定与具体候选实际展示后才Ctrl+Y，未重复prepare。恢复运行的最终产物、报告与新会话验收待单独记录，不能由单测替代。

Rust当前全量`target/dsh-project-connections-rust-full-check.log`fmt/check/Clippy/doctest与659项通过、4项可选跳过。模型器前次675项通过，但追加父楼栋产物篡改覆盖后正在以dsh-project-connections-modeler-verified.log再次收口；不以旧结果作为新最终基线。


连接续验`target/project-connection-live-01f0fb5b`三步通过：新TUI只读恢复原prepared16部件提案`7c20216b7af9422b92b24a40ab7a6c77`，分别实际显示assembly binding与具体版本后Ctrl+Y。唯一连接授权／应用，新增16级楼梯，仅连接authority改变；原上下层／楼板／旧楼栋／输入及业务身份哈希不变。新楼栋built/valid/current，三阶段各34mesh与完整连接build plan精确相等，真实top／perspective PNG准入。连接passed／来源complete／技术complete，整体attention、净高／结构／合规／人工not_run保留。`inputs.connection_authority`封存原scope、proposal/token、grant、operation、候选SHA与父楼栋输入，没有继承结构批准。

新会话恢复`target/project-connection-recovery-4009bb8d`两步通过：只读恢复同一连接scope、原提案／应用／构建／连接报告及两张已有PNG，所有业务hash及身份集合不变。没有prepare/render重做、授权、apply或build。当前模型器`target/dsh-project-connections-modeler-verified.log`676项及lint/format通过；Rust全量659项／4可选跳过；`target/dsh-project-connections-native-check.log`release examples与全套原生夹具通过。

删除前审计又识别两处旧入口实际差异：旧start_change支持现有家具solids patch编辑，新ComponentPlan仅增补／未应用修订且声明不支持edit-existing-component；旧locate的TooFewWalls分图层wall_layers回退尚无新项目等价seam。下一步先迁移已应用组件编辑及实际批准／构建／来源保护，再核对定位回退，最终做原计划逐项完成审计；不删除--modeling、不标记目标完成。


性能要求4.4在当前源码再次测量，`target/dsh-project-connections-measurements.log`两项release测量通过：121条消息／239KiB快照发布约50.061μs/片段；满历史下2／8／32KiB流式消息重排约57.931／71.507／226.966μs/片段。只作同机观察，无耗时阈值断言；没有据此修改快照owner或引入第二份执行状态。
