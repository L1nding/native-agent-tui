# UI 模块边界

实时 TUI 的根模块 `src/ui.rs` 只保留终端生命周期、全局按键路由、Core 快照读取、跨面板命令和布局组合。面板内部的规则放在对应子模块，避免把执行语义复制到渲染层。

## 当前子模块

- `ui/input`：VT/Windows 输入记录、粘贴和有界编辑事件。
- `ui/requests`：审批和用户输入请求的详情、草稿和身份校验。
- `ui/search`：统一搜索面板、异步查询任务和消息/工具结果导航。
- `ui/tool_search`：工具详情字段扫描、类别/生命周期过滤、UTF-8 范围和 revision 校验。
- `ui/skills`：技能清单面板的布局、状态摘要和滚动内容；只接收快照与滚动位置。
- `ui/activity`：活动摘要、证据面板、用量/预算格式化和 compaction 证据行；F11 按选中 agent 显示 root 或 child 的完整 usage，并始终把 token budget 标成会话级；不持有 UI 本地状态。
- `ui/context`：Context 面板本地可见/滚动状态与只读渲染；按当前 root/child 展示 usage 来源、该 agent 的 compaction 事实，以及独立的会话级 token budget。面板不发送 Core 命令。
- `ui/timeline`：证据时间线选择与工具详情定位。
- `ui/tool_detail`：工具摘要和搜索命中详情的只读渲染；`ui/tool_trace` 从快照保留窗口按完整 locator 投影生命周期元数据，不读取正文、参数或命令，也不维护额外缓存。
- `ui/workflow`：任务选择顺序、依赖/Gate 链接导航、过期链接检查和 root/child 精确会话定位。
- `ui/history`：持久会话列表、历史搜索、只读详情和导出预览。

工作流模块只接收 `CoreSnapshot`、选中的 `TaskId` 和链接游标，返回导航结果；它不依赖根 UI 的 `LocalState`，也不发送 Core 命令。根模块负责把结果应用到本地滚动/选择状态，并决定是否提交调度或打开对话。

Context 模块只消费 Core 快照和当前选中的 agent。根模块处理全局 `Ctrl+G` 路由并打开或关闭面板；面板拥有自己的 `Esc` 关闭和 PgUp/PgDn、Home/End 滚动行为。普通编辑文本中的 `c` 仍由输入编辑器处理。

## 维护规则

新增 UI 功能先确定所属面板和 seam，再在根路由接入。只有跨面板的终端生命周期、全局快捷键和命令发送留在 `ui.rs`。模块测试应通过公开的父模块接口或模块自己的窄接口覆盖身份、过期、预算和无副作用行为。
