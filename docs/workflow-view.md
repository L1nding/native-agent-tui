# 工作流关系视图

F4 打开只读工作流视图。Core 的 `SchedulerSnapshot` 是执行事实的唯一来源；UI 投影仅决定显示顺序和导航，不修改调度状态。

## 关系语义

- `parent` 只表示快照明确记录的任务父级。Native child 的注册事件和 Agent 启动事件不会补造 task parent；多个 root task 可以共享 root thread。
- 详情中的 RootTurn `Parent: none` 表示已确认没有 parent；NativeChild 缺少 parent 时显示 `unconfirmed / unavailable`，不把未知所有权显示成已确认无父。
- `dependencies` 和 `policy` 表示 DAG 调度条件；`wait_targets` 表示 Gate 捕获的 child attempt。它们互不替代。只有 wait target 包含 attempt，不能从 parent 或 DAG 关系推断 attempt。
- `children: thread_id → TaskId` 是 Scheduler 内部 thread 身份的唯一映射。只读快照和 journal 从该映射派生 `child_thread_id`，让尚未收到首轮 started 的 child 也可识别。
- 新程序可读取旧 journal：缺失的 `child_thread_id` 只可由已确认的 `external.thread_id` 恢复；首轮前没有确认身份时保持未关联。schema 保持 2。旧二进制不保证能读含新增字段的新记录；按未知字段拒绝是 fail closed。导出时，派生身份和 `external.thread_id` 由同一 Redactor 别名处理。
- `RecoveryTaskSummary` 是恢复提示摘要，不含 live 工作流导航所需的完整身份关系；F4 仅使用 `SchedulerSnapshot`，不会从摘要反推关系。
- Child 对话关联按精确 thread ID 匹配 Agent。已绑定时，`external.thread_id` 必须等于派生身份，turn ID/generation 必须与 Agent 一致，child generation 必须等于 task attempt。首轮前只接受 attempt 0、Agent generation 0、无 turn 且 `awaiting_turn` 的快照。
- Root 对话跳转要求 Core 当前 thread/turn 与 `external` 相同，且 Turn 活动的 task、attempt、thread、turn、generation 全匹配。Root generation 是 Core 全局值，不等于 task attempt；正在绑定新 turn 时不跳旧对话。活动任务还必须等于 Scheduler 的 `active_root`。

## 导航与命令

- Up/Down 在显式 parent 的稳定投影顺序中选择任务；详情始终展示依赖策略、failure policy、blocked reason、Gate 目标及身份事实。
- `d` 循环浏览依赖，`g` 循环浏览 Gate wait target；详情显示关系目标标题和状态，并把 Gate 捕获 attempt 与目标当前 attempt 分开标注。Enter 打开精确匹配的 root 或 child 对话；不匹配时显示原因。
- F4 是模态视图：编辑、删除、粘贴、Ctrl+S、Ctrl+Enter 和 Ctrl+Y/N/B 不会改动草稿或发命令；Esc 关闭并保留草稿。F2 显式切回请求面板后，审批快捷键才会生效；Ctrl+C、Ctrl+Q/D 及工作流控制键保留原路径。
- `g` 选中的 Gate 目标若已换 attempt，Enter、F6–F8、`+` 和 `-` 会拒绝对新 attempt 操作；Up/Down 清除关系光标并显式重选。所有有效 task command 带上观察到的 attempt。PageUp/PageDown/Home/End 可独立滚动换行后的关系详情。
- 导航只更改本地视图，不发送执行 RPC，也不清空输入草稿。

## 验证

运行 `cargo test --all-targets` 验证 Rust 行为；UI 回归覆盖 stale Gate attempt 的 Enter/任务控制拦截、显式重选、F4 草稿与审批隔离，以及 Root/NativeChild parent 缺失语义。Windows ConPTY fixture 覆盖实际终端键盘路径、额外执行 RPC 计数、cursor 关闭记录和只读 replay 的清理/恢复结论。

### 最终验收记录（2026-10-04）

- `python scripts/verify.py --cargo-test --live`：exit 0，输出 `Repository verification passed`。
- Rust 测试：288 个库测试和 6 个集成测试通过；另有 8 个库级 live 测试和 1 个 JSONL live 测试通过，均锁定 Codex 0.159.2。Windows 与 ConPTY fixtures（含 workflow fixture）全部通过。
- Workflow ConPTY 使用 fake app-server，验证真实终端按键、导航不发额外执行 RPC、会话关闭、进程清理确认及未完成任务保留恢复状态。真实 provider 覆盖来自现有锁定版本 live 回归，不来自该 fake fixture。
- 此验证记录不表示完整 multi-child V2 已实现或验收。
