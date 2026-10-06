# UI 模块边界

实时 TUI 的根模块 `src/ui.rs` 只保留终端生命周期、全局按键路由、Core 快照读取、跨面板命令、`LocalState` 和布局组合。面板内部的规则放在对应子模块，避免把执行语义复制到渲染层。

## 当前子模块

- `ui/input`：VT/Windows 输入记录、粘贴和有界编辑事件。
- `ui/layout`：纯终端几何、会话布局约束和按 grapheme 的文本换行；不读取或修改 Core/UI 状态。
- `ui/terminal`：交互终端进入、退出和资源恢复；只封装 `Terminal`/`TerminalInput` 生命周期，不参与路由或渲染。
- `ui/runtime`：交互执行与只读历史的终端事件循环；协调快照、History/Search handle 和根模块 typed helpers，不持有执行事实。
- `ui/render`：从只读 `CoreSnapshot` 与 `LocalState` 组合终端视图；不处理按键、发送命令或修改状态。
- `ui/overlays`：命令面板触发的互斥面板切换与关闭；接收快照和本地 UI 状态，调用父模块的请求同步 seam，不发送 Core 命令。
- `ui/editor`：有界、按 Unicode grapheme 操作的文本编辑器状态与光标操作；不发送 Core 命令。
- `ui/attention`：注意力阈值编辑器状态与弹窗渲染；只通过根模块路由提交类型化命令。
- `ui/requests`：审批和用户输入请求的详情、草稿和身份校验。
- `ui/request_state`：按稳定 `RequestRef` 选择当前请求、同步各请求输入草稿，并依据 Core 阶段与提交状态锁定回答；只更新 UI 本地状态，不构造或发送命令。
- `ui/search`：统一搜索面板、异步查询任务和消息/工具结果导航。
- `ui/tool_search`：工具详情字段扫描、类别/生命周期过滤、UTF-8 范围和 revision 校验。
- `ui/skills`：技能清单面板的布局、状态摘要和滚动内容；只接收快照与滚动位置。
- `ui/activity`：活动摘要、证据面板、用量/预算格式化和 compaction 证据行；F11 按选中 agent 显示 Core 提供的 `UsageFact`（含 thread/turn/generation/source），缺少精确身份时显示 unavailable，并始终把 token budget 标成会话级；不持有 UI 本地状态。
- `ui/context`：Context 面板本地可见/滚动状态与只读渲染；按 Core usage fact 的精确 root/child 身份展示 usage 来源、累计值和最近确认语义，以及该 agent 的 compaction 事实和独立的会话级 token budget。面板不从 agent id 推断 usage 归属，也不发送 Core 命令。
- `ui/timeline`：证据时间线选择与工具详情定位。
- `ui/tool_detail`：工具摘要和搜索命中详情的只读渲染；`ui/tool_trace` 从快照保留窗口按完整 locator 投影生命周期元数据，不读取正文、参数或命令，也不维护额外缓存。
- `ui/workflow`：任务选择顺序、依赖/Gate 链接导航、过期链接检查和 root/child 精确会话定位。
- `ui/workflow_view`：Agent 树投影与渲染、工作流/Gate 视图和状态文案；紧凑布局由终端高度决定，视图只读快照。
- `ui/history`：持久会话列表、历史搜索、只读详情和导出预览。
- `ui/commands`：本地命令面板的静态导航项、过滤/选择/滚动、粘贴编辑和无副作用 `PaletteAction`。

工作流模块只接收 `CoreSnapshot`、选中的 `TaskId` 和链接游标，返回导航结果；它不依赖根 UI 的 `LocalState`，也不发送 Core 命令。根模块负责把结果应用到本地滚动/选择状态，并决定是否提交调度或打开对话。

Context 模块只消费 Core 快照和当前选中的 agent。根模块处理全局 `Ctrl+G` 路由并打开或关闭面板；面板拥有自己的 `Esc` 关闭和 PgUp/PgDn、Home/End 滚动行为。普通编辑文本中的 `c` 仍由输入编辑器处理。

## 维护规则

新增 UI 功能先确定所属面板和 seam，再在根路由接入。只有跨面板的终端生命周期、全局快捷键和命令发送留在 `ui.rs`。请求选择、生命周期锁定和草稿切换归 `ui/request_state`，根模块只在交互入口调用同步逻辑；请求身份仍由完整 `RequestRef` 匹配。模块测试应通过公开的父模块接口或模块自己的窄接口覆盖身份、过期、预算和无副作用行为。
