# dsh-TUI 插件方向调查

调查日期：2026-10-23 UTC。外部仓库 `ccch1mneyyy/dsh-TUI` 的 `main` 在调查时固定为 [`3ffbf24628fc850e5dd3406798e34ca6ee16b924`](https://github.com/ccch1mneyyy/dsh-TUI/commit/3ffbf24628fc850e5dd3406798e34ca6ee16b924)，提交时间为 2026-10-22 15:44:44 UTC。外部源码和测试引用均指向这个 commit。

## 推荐

主推荐是：**优先验证“dsh-TUI 插件入口＋独立工作流/领域服务”，验证通过后再决定退出独立 TUI；不预设必须保留 Rust，也不预设必须修改宿主。** 方向判断置信度：中；生产迁移可行性尚未验证。

dsh-TUI 有公开的全屏 scene、托管 dialog 和通用 channel 操作接缝。插件组件由宿主直接渲染，可以持有插件自己的服务/client；因此领域 snapshot、来源证据、版本授权和 receipt 可以走插件自有协议连接领域服务，不必先进入宿主 channel。源码调用链支持这条方向，但本轮未运行完整闭环。DecisionEvents 是输入和会话动作的前置拦截器，不是领域执行授权；消息观察器也不能代替领域证据接口。[scene 注册](https://github.com/ccch1mneyyy/dsh-TUI/blob/3ffbf24628fc850e5dd3406798e34ca6ee16b924/src/dsh-adapter/scenes.ts#L135)、[组件挂载](https://github.com/ccch1mneyyy/dsh-TUI/blob/3ffbf24628fc850e5dd3406798e34ca6ee16b924/src/dsh-adapter/channel-scene-outlet.ts#L7)、[dialog 结果](https://github.com/ccch1mneyyy/dsh-TUI/blob/3ffbf24628fc850e5dd3406798e34ca6ee16b924/src/dsh-adapter/dialogs.ts#L362)

不建议直接把整套 Rust Core 复制到插件进程。插件负责呈现和调用，领域服务拥有业务事实、命令、授权与恢复；dsh-TUI 继续拥有通用 agent 会话、模型、工具和通用权限。是否保留 Rust 取决于现有校验和恢复代码的复用价值。公开插件路线应先验证，不能从“宿主没有领域内建契约”直接推导出需要 fork。

## 证据范围

外部仓库的插件准入指南自称是随源码修订的整合入口，公开运行时插件是 Cordis 插件，宿主提供 UI 接缝，插件补充能力。指南还明确 `trusted-in-process`，授权不是进程或 OS 安全边界。[插件准入指南](https://github.com/ccch1mneyyy/dsh-TUI/blob/3ffbf24628fc850e5dd3406798e34ca6ee16b924/tui-profile/docs/plugin-admission-and-development.md#L1-L45)、[信任与授权说明](https://github.com/ccch1mneyyy/dsh-TUI/blob/3ffbf24628fc850e5dd3406798e34ca6ee16b924/tui-profile/docs/plugin-admission-and-development.md#L277-L346)

本地仓库基线为 `a2bf83fa65e9b07b5de8ebdc1cd99346dd9bbea5`，调查时是 dirty worktree；本地引用对应读取时的工作树，不能由该 commit 单独复现。产品范围以 [native-agent-tui-plan.md](native-agent-tui-plan.md) 的目标设计、[implementation-status.md](implementation-status.md) 的当前状态说明和源码交叉理解；这些本地文件不是外部插件 API 的稳定承诺。

本轮恢复调查只补充源码核查并修订本报告，没有修改业务代码、运行主线程任务、安装插件或操作宿主。CodeGraph CLI/MCP 在当前环境不可用，尝试 CLI 后按限定文件检索。外部仓库的测试脚本下文只作为覆盖意图与断言的源码证据，本轮未执行，不能称为测试通过或真实终端验收。

## 产品差异化

产品目标是让使用者判断“现在发生什么、谁在等谁、最后可靠证据是什么、需要我做什么”，不是只提供建模面板。建模是检验版本授权与来源证据的现有场景，不作为产品范围的上限。[产品价值与版本范围](native-agent-tui-plan.md#1-产品价值与发布范围)

当前实现与目标需分开：`DomainSnapshot`、绑定版本的授权、sealed receipt 校验和 journal/只读回放已有源码；完整 V2 资源预算、调度恢复以及更广并发保证仍有目标设计部分。不能因为 workflow 文档描述了某项能力，就把它计入现有产品的迁移成本或宿主必须补齐的能力。[领域投影](../src/domain.rs#L518)、[版本授权](../src/domain.rs#L3594)、[回执校验](../src/domain/receipt.rs#L9)、[journal](../src/journal.rs#L1427)、[V2 目标声明](native-agent-tui-workflow.md#agent-工作台共享调度模型)、[实施状态](implementation-status.md#s9b-历史观察与脱敏导出)

| 能力 | 当前本地产品证据 | 判断 |
| --- | --- | --- |
| 通用 agent 客户端 | dsh 已提供会话、模型、工具、消息、通用权限、子代理和后台任务；本地 `dsh-plugin` 已把 ACP/会话桥到 Rust UI | 适合交给宿主，继续重复建设的长期收益有限 |
| 专业工作台 | `DomainSnapshot` 保存 proposal/project/selection/source ledger/floor progress/artifacts/first-use review/review images/verified fact/fact sync notice；`authorize_with_policy` 先验图像/网页、上下文 SHA，再调用专用领域授权入口 | 这是差异化，不能从 transcript 或普通 approval 推导 |
| 长流程状态 | 本地 Core 已有身份、generation、Gate、依赖和子代理投影；完整资源预算与持久调度恢复仍是后续目标；现有恢复只读且不重发副作用 | 需要明确事实所有者；宿主拥有通用 agent 执行，工作流服务只拥有额外业务约束，避免调度同一个 turn |
| 领域证据 | 本地实现保留来源、哈希、分页读取、图片/网页复核和应用回执；dsh 的 `messages.observe` 不含工具/边界事件 | 需要领域后端或上游新增结构化投影 |

独立 TUI 的主要长期收益是执行语义和领域状态完全可控，代价是继续维护通用会话、宿主适配和分发。插件入口的主要收益是复用 dsh 的通用会话与安装生态，代价是依赖宿主版本、受实验 API 约束，并要为领域状态保留第二个运行时。合理边界是让宿主只拥有通用 agent 执行事实，让领域后端拥有领域事实和领域副作用。

## 能力证据矩阵

状态含义：`公开且有证据` 表示固定 commit 中有接口和宿主消费点的静态证据，不表示本轮运行通过；`公开但实验性/降级` 表示有源码但稳定性或 live proof 受限；`缺失` 特指没有对应的宿主内建契约，不自动排除插件自带服务；`未验证` 表示本轮没有完成相应运行验证。

| 需求 | 接口/符号 | 宿主消费点与证据 | 稳定性 | 结论与缺口 |
| --- | --- | --- | --- | --- |
| 全屏工作流页面 | `ctx.tuiScenes.register/list/open/close/active/subscribe`、`TuiSceneProps` | `src/dsh-adapter/scenes.ts` 保存注册表和 owner，`channel-scene-outlet.ts` 把当前 scene component 挂入 TUI；指南说明场景打开时独占终端，组件通过注入的宿主 React 和 `channel` 刷新 | 公开，指南称稳定候选；scene driver 只自动验证 register/list，open/close/active/subscribe 仍标 degraded | 可做专业工作台主页面；scene 自己拥有键盘焦点。scene 的开关不写 session 事件，状态持久化要另走 storage/backend |
| 侧栏/面板 | `ctx.tuiPanels`、`TuiPanelRuntime`、`PanelStore` | `src/dsh-adapter/panels.ts` 有 register、owner 校验、open/close、badge、subscribe、面板预算和 panel bridge；panels driver 对 register/list 做可逆探针 | 公开源码但实验/降级；manifest v0.15 的 `panels` 仍要求为空，driver 对 open/close/badge/subscribe 不做 live proof | 不能把领域工作台的关键闭环依赖在面板上；优先用全屏 scene，面板只作附加入口 |
| 输入/确认对话框 | `ctx.tuiDialogs.select/confirm/input` | `src/dsh-adapter/dialogs.ts` 维护 FIFO 队列、按 key 决定/取消、AbortSignal、超时和 activation teardown；指南列出键盘行为和上限 | 公开且有真实宿主消费路径 | 能展示人工确认并把选择返回插件；对话框本身不等于后端授权，插件必须把结果绑定到领域命令和版本 |
| 持续进度 | `ChannelUi.version/rows/status/working/notifications/traceEvents`、`ctx.on('session/event')` | `createChannel()` 重放 durable transcript 并订阅 session events；指南把 `session/event`、`agent/status` 定为观察入口，scene 通过 `channel.subscribe` 读变化 | 公开通用能力 | 可观察 dsh 会话和已投影状态；领域进度必须由领域后端发布结构化状态，不能用模型文本猜测 |
| 工具/来源/图片证据 | `tuiRenderers`、`messages.observe`；插件自己的领域 client | renderer 输出纯文本；observer 只映射 user/assistant message，排除 streaming/tool/turn；scene 可读取自己的领域服务，而不靠 observer 重建事实 | renderer/observer 有公开接缝；领域 client 是插件自有协议 | 来源与证据分页可由领域服务提供；终端内图片、浏览器证据查看和日志分页的完整体验仍需实测，不能用纯文本 renderer 的限制否定整个 scene |
| 审批和人工输入 | `tuiDialogs`、`DecisionEvents`；插件自己的领域 command | dialog 返回选择，插件可继续调用领域服务；DecisionEvents 只拦截输入、rewind、session-switch、compact。AgentSession permission/question 是后端会话能力，不能假定一般插件可直接取得 | dialog 公开；DecisionEvents 实验性；领域授权为自有协议 | 领域服务须复核 proposal/revision/context 与请求状态。宿主没有统一领域授权接口，不妨碍插件提供自己的批准入口；需要宿主原生审批归属时再评估扩展 |
| 子 agent 与长任务 | scene 的 `ChannelUi.subagents/backgroundJobs`；后端内部的 AgentSession/capabilities | `channel-ui.ts` 暴露子代理、job、goal、trace 投影；AgentSession 定义更底层生命周期和可选控制，但不是一般插件的公开 session handle；本地桥的 host-settled 属于现有定制实现 | channel 为 scene surface；后端内部能力不可冒充插件 API | 可复用通用状态与 channel 提供的控制，完整父续行/失败/停止语义仍待验证。若是额外业务任务，交由工作流服务拥有，不再调度宿主同一个 turn |
| 暂停/停止/继续/提交 | scene 的 `ChannelUi.submit/steer/cancel/resumeTo`；Backend handler 的 `AgentSession.submit/cancel` | scene 的 `submit/steer/cancel` 返回 `void`，`resumeTo` 返回 Promise；placement 和 `CancelReceipt` 属于更底层 AgentSession 契约，不能当作 scene 已获得的确认结果 | scene 控制有公开文档；Backend 能力按实现可选 | 能发起通用操作，不等于能获得全部执行确认或拥有领域 workflow command；领域服务须自行返回明确命令结果 |
| 恢复与持久化 | `AgentSession.history/subscribe`、Backend `catalog/launch.resume/sessionPrefs`、`ctx.tuiPluginStorage` | Backend 启动恢复绑定所选 backend；assembly/registry 测试验证装配、lazy import、resume 绑定和失败不回落；plugin-storage 写入 `~/.dsh-tui/plugin-storage/<namespace>.json`，按插件隔离、有配额、损坏文件不覆盖 | Backend `alpha`；storage 公开且有边界；scene 状态不随 `/new`/`/resume` 自动持久化 | 宿主 session transcript 和插件私有配置可恢复；领域事实/执行恢复必须在独立后端，且要区分只读回放和重新执行 |
| 外部工作流服务/MCP/进程 | scene 组件闭包中的插件自有 service/client；可选 Backend contribution | 注册表保存组件函数，outlet 直接 React 渲染；没有把组件序列化或限制为只读 channel。Backend contribution 用于增加宿主 agent backend，是另一种需求 | scene 公开；直接 I/O 符合 trusted-in-process 结构，但具体进程/网络行为未实测；Backend alpha | 插件连接自己的领域服务不以新增 Backend 或 channel contract 为前提；避免私有 host service，且自己负责连接、退出、断连与身份映射 |
| 发布、安装、升级 | `dsh-plugin.json`、`dsh.profile.bundles`、`dsh plugin --profile ... add`、Backend install recipe | admission/assembly 读取包根 manifest；验证脚本覆盖 profile bundle、声明投影、handler 名称 `backend`、重启后进入 picker、只在选中时 import | 插件安装链公开；Backend 仍 alpha | 安装体验可复用；必须跟宿主版本和 profile 重启节奏，未知 executor 只是无安装面；本轮未验证真实外部包安装 |
| Windows/平台维护 | Node ESM package、宿主 loader、trusted-in-process | 指南要求 Node 22.19 起的 22.x 或 24 及以上、纯 ESM；源码没有给出本轮所需领域插件在 Windows 的真实安装/终端验收 | 未验证 | 不能把宿主跨平台声明当作本产品 Windows 闭环证据；需单独跑真实 TTY、重启、恢复和进程清理 |

### 两个容易混淆的边界

1. **DecisionEvents 不是执行授权。** `tui/input` 的返回值只能改写、处理或取消用户输入；`tui/rewind-prompt`、`tui/session-switch` 和 `tui/compact` 只能否决相应宿主动作。它没有 proposal revision、来源哈希、领域上下文或具体 backend attempt 的参数。授权 grant 只决定插件能否进入拦截链，不把插件变成执行所有者。
2. **Backend 是适配器接缝，不是自动拥有领域模型。** `validateBackendSpec` 和 `backendAdmission` 只验证声明形状、能力/权限词表和授权；宿主随后才按 profile bundle 发现、注册、lazy import `backend` handler。即使 handler 成功打开 `AgentSession`，领域事实、领域证据和应用幂等性仍须由该 session 的明确 capability 或独立领域后端定义。

### 已追通的调用链与证据等级

- **插件注册→用户界面**：指南的 `ctx.tuiScenes.register` 和打开命令示例 → 注册表保留 `scene.component` → outlet 调用 `React.createElement(scene.component, { React, ui, channel, close })`。组件可以闭包引用插件自有 client，不要求先把领域状态注入宿主 channel。[指南示例](https://github.com/ccch1mneyyy/dsh-TUI/blob/3ffbf24628fc850e5dd3406798e34ca6ee16b924/tui-profile/docs/plugin-admission-and-development.md#L839)、[注册表](https://github.com/ccch1mneyyy/dsh-TUI/blob/3ffbf24628fc850e5dd3406798e34ca6ee16b924/src/dsh-adapter/scenes.ts#L135)、[outlet](https://github.com/ccch1mneyyy/dsh-TUI/blob/3ffbf24628fc850e5dd3406798e34ca6ee16b924/src/dsh-adapter/channel-scene-outlet.ts#L7)
- **用户确认→插件续行**：`tuiDialogs.confirm` → store 排队并由 UI 回答 → Promise 返回 boolean → 插件代码决定是否发送自己的领域 command。到 Promise 返回为源码事实；“插件 command→领域服务→拒绝过期版本→receipt”是待验证方案，不能写成宿主已提供的执行授权。[dialog store](https://github.com/ccch1mneyyy/dsh-TUI/blob/3ffbf24628fc850e5dd3406798e34ca6ee16b924/src/dsh-adapter/dialogs.ts#L129)、[confirm](https://github.com/ccch1mneyyy/dsh-TUI/blob/3ffbf24628fc850e5dd3406798e34ca6ee16b924/src/dsh-adapter/dialogs.ts#L362)
- **测试覆盖边界**：scene boundary 脚本检查渲染与异常隔离，lifecycle 脚本覆盖卸载时 dialog 取消；它们不证明插件调用领域服务的闭环。本轮没有执行这些脚本。[scene 回归源码](https://github.com/ccch1mneyyy/dsh-TUI/blob/3ffbf24628fc850e5dd3406798e34ca6ee16b924/scripts/verify-plugin-scene-boundary.tsx)、[dialog 生命周期断言](https://github.com/ccch1mneyyy/dsh-TUI/blob/3ffbf24628fc850e5dd3406798e34ca6ee16b924/scripts/verify-plugin-lifecycle.ts#L259)

类型还需区分：`ChannelUi` 经 `TuiSceneProps` 暴露给场景；`AgentSession`/capabilities 是后端实现抽象，不能仅凭仓内类型存在，就算作任意插件可调用的 API。尤其 channel 的 `submit/steer/cancel` 返回 void，不能提供表格中更底层 `CancelReceipt` 的保证。[scene props](https://github.com/ccch1mneyyy/dsh-TUI/blob/3ffbf24628fc850e5dd3406798e34ca6ee16b924/src/dsh-adapter/scenes.ts#L32)、[channel 方法](https://github.com/ccch1mneyyy/dsh-TUI/blob/3ffbf24628fc850e5dd3406798e34ca6ee16b924/src/adapter/ports/channel-ui.ts#L492)、[包导出](https://github.com/ccch1mneyyy/dsh-TUI/blob/3ffbf24628fc850e5dd3406798e34ca6ee16b924/package.json#L48)

## 所有权边界

| 所有者 | 应拥有 | 不应重复拥有 |
| --- | --- | --- |
| dsh-TUI 宿主 | 通用 session transcript、agent 状态、模型/provider、工具、通用 permission/question、通用 subagent/task 控制、插件生命周期和宿主 UI | 领域 proposal、领域 source ledger、领域授权规则、领域应用幂等性 |
| dsh-TUI 插件 | scene/panel/dialog/shortcut/renderer、插件命令、宿主 channel 的只读投影、调用通用 submit/steer/cancel、插件私有配置 | 不复制 ACP/session transcript，不从文本推导领域完成，不直接拥有宿主 session 的事实 |
| 独立领域后端 | domain snapshot、proposal/revision、来源与证据、上下文哈希、人工复核状态、authorize/apply receipt、领域恢复和副作用不确定性 | 不再维护第二套通用 agent transcript 或第二套宿主 model/tool scheduler |
| native-agent-tui Rust Core（过渡期） | 若保留，集中做领域后端适配、结构化快照和安全命令边界 | 不应与 dsh 同时成为同一个 agent session 的执行 owner |

这里的“领域服务”是一项职责，不要求新增一个进程：当前 `authorize_with_policy` 已调用 Python 的 `domain-authorize` / project authorizer，Rust 校验返回的 run/context/proposal/revision/prepare token 后才提交续行信息。迁移应先明确哪些校验由现有领域服务拥有、哪些投影仍在 Rust，不能把整套 Rust Core 直接搬成第二个 agent runtime。[实际授权调用与结果核对](../src/domain.rs#L3594)、[Core 的 attempt/generation 回传](../src/client.rs#L3075)

需要避免的坏状态是：dsh 认为 turn 已结束，Rust Core 认为领域任务仍在运行；或插件把对话文本当作授权结果，独立后端又按自己的 revision 执行。所有领域命令必须带稳定 proposal/revision/attempt 身份，后端返回明确 receipt/unknown，插件只展示结果。

## 迁移取舍

### 保留

- 保留领域 snapshot、receipt、来源 ledger、复核和授权策略；它们是产品差异化。
- 保留结构化命令、generation/revision 校验、未知外部结果和恢复语义。
- 保留一个领域服务边界。现有 Rust 的绑定、回执、观察和恢复代码可以复用；若既有 Python/领域服务已可靠拥有同样事实，插件可直接调用它，不必再加一层 Rust。语言和是否独立进程根据生命周期、调用协议与复用价值选择，不能把 Rust 本身当作授权隔离保证。

### 改造

- 把 dsh 通用对话、模型、工具、子代理观察接到插件 scene/channel；现有 `dsh-plugin` 已证明 ACP 桥可复用，但当前桥接依赖实际 DSH 安装中的 ACP/context repair 版本检查，不能作为“纯公开插件 API 已覆盖领域闭环”的证据。
- 由插件自有 client 连接领域服务，提供结构化 snapshot、证据分页和 typed commands；不要求新增宿主 Backend contribution，不通过 transcript 解析恢复领域事实。
- 先把领域批准做成后端命令，再用 `tuiDialogs` 做呈现。若需要在宿主审批面板中原生承载领域 request，再提出小的上游 contract，而不是把 DecisionEvents 误当成批准接口。

### 退出或延后

- 通过纵向闭环后，逐步退出独立 TUI 中的通用聊天、模型选择和宿主工具展示。
- 在 Backend 仍标为 `alpha`、面板/决策仍有 degraded 面之前，不把整个产品发布承诺绑定在 dsh-TUI 插件上。
- 当前没有证据支持长期维护 dsh-TUI fork；只有当最小上游扩展无法接受、而领域闭环又必须使用宿主私有注入时，才重新评估 fork 或继续独立 TUI。

一次性成本是 scene/channel adapter、领域后端边界和安装/版本矩阵；长期成本是 dsh-TUI/profile/API 跟随、Node/ESM 生态和真实 TTY 回归。这个取舍不能按“插件代码更少”判断。

## 是否需要修改 dsh-TUI 源码

### 公开插件即可验证的部分

- 一个全屏 scene 读取当前 `ChannelUi` 状态，使用托管 dialog 做人工输入，并调用当前 session 的 `submit/steer/cancel`。
- 插件私有配置用 `storage.local`，普通 session 事件用 renderer 或受限 observer 展示。
- 一个符合 `dsh-plugin.json`、profile bundle、`backend` export 约定的第三方 Backend fixture，等待重启后进入 picker。

### 不需要预先修改宿主的领域路径

scene 使用插件自己的 client 订阅领域 snapshot，dialog 回答后发送版本绑定命令，领域服务校验并返回 receipt；恢复从领域服务读取。插件自己定义这份协议，不要求 dsh 先定义领域 channel 或 authorization contract。当前源码支持此结构，实际外部服务连接、身份绑定、终端交互和重启恢复尚未验证。

### 仅在额外宿主集成要求下考虑上游扩展

- 若要求领域状态成为宿主拥有、跨插件共享且随宿主日志统一回放的 channel 投影，现有 `src/adapter/ports/channel-ui.ts` / `src/dsh-adapter/channel.ts` 没有相应领域字段，可考虑可选结构化 feature；插件私有页面不以此为前提。
- 若要求领域请求进入宿主原生审批/执行链，由宿主统一撤销、过期和答复，则需另定义版本与 attempt 绑定契约。`src/dsh-adapter/dialogs.ts` 只返回 UI 选择，DecisionEvents 的事件集合也不足以替代该契约；插件与领域服务自己的批准链不受此限制。

这些是条件性增补候选，不是本轮已证实的阻塞，也不能在未验证范围时称为“小改动即可”。先用公开插件与自有服务闭环验证；只有明确不可外包、必须归宿主所有的能力被私有接缝阻塞时，才比较上游扩展、fork 与独立 TUI。现有 ACP 补丁是当前桥接方案的成本，不能自动算作新插件路线的必需成本。

## 最小验证闭环

下一步只建议一个隔离纵向闭环，本轮不实施：**插件自有 client 读取 snapshot → scene 查看一条来源证据 → dialog 确认具体 proposal/revision → 领域服务执行 → receipt/unknown → 重启后只读恢复。** 先去掉对 AgentSession 私有访问、新领域 channel feature 和上游 authorization API 的依赖，验证它们是否确实不需要。

通过标准：

- scene 不解析 transcript，直接读取结构化领域 snapshot；
- 确认请求显示 proposal/revision/context identity，回答只提交给领域后端；
- 后端是唯一执行 owner，执行前再次校验 revision，结果有稳定 receipt 或明确 unknown；
- 故意提交过期 revision 必须被服务拒绝，不产生领域副作用；
- 重启/`/resume` 后能恢复 snapshot、证据和 receipt，不自动重发副作用；
- dsh session、插件场景和领域后端的 session/workflow identity 能明确对应。

失败标准：闭环只能依赖未公开 host service、修改 ACP 内部协议、把工具/助手文本当证据，或无法区分只读回放与重新执行。先查明是插件自有服务实现问题还是确属宿主缺口；只有后者且需求必须由宿主拥有时，才支持新增上游接口。通过前保留现有独立 TUI，不进入大规模迁移。

## 未决事项

### 已核查的静态证据

- 固定 commit 的公开 scene、dialog、renderer、storage、message observer、session/backend/capability 类型和 backend assembly/registry 测试存在。
- Backend 安装声明、准入、lazy import、handler 身份、恢复绑定和池清理有仓内验证脚本；仅阅读源码，不声称本轮已跑过。
- DecisionEvents 的事件集合、授权门和串行/过期输入语义在指南和实现中有明确描述。

### 文档声明或实验性

- 指南称 scenes/dialogs/status/shortcuts/renderers 为稳定候选；DecisionEvents、panels 和 Backend 仍带实验、degraded 或 alpha 语义。
- `trusted-in-process`、插件安装和权限 grant 是行为/兼容约束，不是安全隔离。

### 推断

- 插件适合做工作流入口，独立后端适合保留领域事实和副作用 owner。
- 领域闭环需要明确结构化协议，但该协议可以属于插件与领域服务，不必属于宿主 channel；长期依赖 transcript 或私有宿主 service 会增加维护风险。

### 尚未验证且会改变推荐的事项

- 在固定 commit 对应的真实 dsh profile 中安装外部 scene/dialog 插件，完成 Windows TTY 的打开、焦点、卸载、重启和会话切换验证；只有产品需要新增 agent executor 时，才另外验证 Backend bundle 的 picker/resume。
- 用公开 scene/dialog API＋插件自有领域 client 完成 snapshot→证据→确认→执行→receipt→恢复闭环；尤其验证过期版本拒绝、断连 unknown、session/workflow 身份绑定，不使用私有 ACP/host patch。
- 产品是否确实要求领域状态进入宿主共享 channel，或授权进入宿主原生执行链；只有答案为是，上游接口与维护方接受意愿才影响主推荐。
- 迁移后产品是否必须支持宿主之外的独立 headless JSONL/Python 消费者；若必须，领域后端不能只存在于 TUI 插件进程内。

## 主要一手来源

- [插件准入与开发指南，固定 commit](https://github.com/ccch1mneyyy/dsh-TUI/blob/3ffbf24628fc850e5dd3406798e34ca6ee16b924/tui-profile/docs/plugin-admission-and-development.md)
- [Backend contract，固定 commit](https://github.com/ccch1mneyyy/dsh-TUI/blob/3ffbf24628fc850e5dd3406798e34ca6ee16b924/tui-profile/registry/contracts/backend-v1alpha1.json)
- [DecisionEvents contract，固定 commit](https://github.com/ccch1mneyyy/dsh-TUI/blob/3ffbf24628fc850e5dd3406798e34ca6ee16b924/tui-profile/registry/contracts/decision-events-v1alpha1.json)
- [Channel contract，固定 commit](https://github.com/ccch1mneyyy/dsh-TUI/blob/3ffbf24628fc850e5dd3406798e34ca6ee16b924/tui-profile/registry/contracts/tui-channel-v1alpha1.json)
- [Scene runtime](https://github.com/ccch1mneyyy/dsh-TUI/blob/3ffbf24628fc850e5dd3406798e34ca6ee16b924/src/dsh-adapter/scenes.ts)
- [Panel runtime](https://github.com/ccch1mneyyy/dsh-TUI/blob/3ffbf24628fc850e5dd3406798e34ca6ee16b924/src/dsh-adapter/panels.ts)
- [DecisionEvents driver](https://github.com/ccch1mneyyy/dsh-TUI/blob/3ffbf24628fc850e5dd3406798e34ca6ee16b924/src/adapter/upstream/decisions-driver.ts)
- [AgentSession interface](https://github.com/ccch1mneyyy/dsh-TUI/blob/3ffbf24628fc850e5dd3406798e34ca6ee16b924/src/agent/session.ts)
- [Session capabilities](https://github.com/ccch1mneyyy/dsh-TUI/blob/3ffbf24628fc850e5dd3406798e34ca6ee16b924/src/agent/capabilities.ts)
- [Channel UI port](https://github.com/ccch1mneyyy/dsh-TUI/blob/3ffbf24628fc850e5dd3406798e34ca6ee16b924/src/adapter/ports/channel-ui.ts)
- [Backend registry](https://github.com/ccch1mneyyy/dsh-TUI/blob/3ffbf24628fc850e5dd3406798e34ca6ee16b924/src/dsh-adapter/backend-registry.ts)
- [Backend assembly](https://github.com/ccch1mneyyy/dsh-TUI/blob/3ffbf24628fc850e5dd3406798e34ca6ee16b924/src/dsh-adapter/backend-assembly.ts)
- [Backend contribution verification](https://github.com/ccch1mneyyy/dsh-TUI/blob/3ffbf24628fc850e5dd3406798e34ca6ee16b924/scripts/verify-backend-contribution.ts)
- [Backend assembly verification](https://github.com/ccch1mneyyy/dsh-TUI/blob/3ffbf24628fc850e5dd3406798e34ca6ee16b924/scripts/verify-backend-assembly.ts)
- [Message observer](https://github.com/ccch1mneyyy/dsh-TUI/blob/3ffbf24628fc850e5dd3406798e34ca6ee16b924/src/dsh-adapter/message-observer.ts)
- [Plugin storage](https://github.com/ccch1mneyyy/dsh-TUI/blob/3ffbf24628fc850e5dd3406798e34ca6ee16b924/src/dsh-adapter/plugin-storage.ts)
- [Dialogs](https://github.com/ccch1mneyyy/dsh-TUI/blob/3ffbf24628fc850e5dd3406798e34ca6ee16b924/src/dsh-adapter/dialogs.ts)
- [本地 dsh-plugin bridge](../dsh-plugin/README.md)
- [本地产品计划](native-agent-tui-plan.md)
- [本地 Core 边界](native-agent-tui-core.md)
- [本地实施状态](implementation-status.md)
