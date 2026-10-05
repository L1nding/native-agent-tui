# 实时工具详情

`Ctrl+T` 打开证据时间线。选中工具开始或结束事件后按 `Enter` 查看仍保留的实时详情；`PageUp` / `PageDown` 滚动，`Home` / `End` 跳到首尾，`Esc` 返回时间线。

## 模块与所有权

- `protocol` 提取已识别工具的字段，并校验完成状态。它不保存详情。
- `tool_details` 管理独立的实时缓存，包括字段合并、UTF-8 截断、预算和生命周期。
- Core 在确认当前线程与轮次归属后绑定 session、thread、turn、item、task attempt 和 generation。
- `ui/timeline` 只负责选择和定位；`ui/tool_detail` 渲染只读详情，不发送执行请求。
- 工具正文搜索由独立的 `ui/tool_search` 扫描，统一搜索面板只保存字段定位和 revision；使用与过期命中规则见[工具详情搜索](tool-search.md)。

条目事件先由 Observer 接受，再更新详情。已有终态事实的条目即使正文被淘汰，重复事件也不能重新插入详情。断连或执行关闭将运行中的详情标为 Unknown，保留已接收的部分输出；子代理换轮时，为旧轮运行中的工具记录 Unknown，并清理旧详情、文件预览和审批请求。

时间线和书签继续只保存事件元数据。缺失、淘汰或身份不匹配的详情显示 unavailable，不按 item 名称寻找其他轮次的内容。

## 输出与预算

命令详情包含服务端提供的命令、目录、退出码和耗时；MCP 与 dynamic 工具可显示已保留的参数和结果。缺失字段明确显示 unavailable。

同一 item 的输出 delta 按接收顺序合并。最终 `aggregatedOutput` 替换已有输出，避免重复拼接。Codex 0.159.2 的输出 delta 没有 stream 字段，因此显示合并输出，不能区分 stdout 与 stderr。

缓存最多保留 128 项、512 KiB；单项最多 64 KiB，包含身份和字段。命令、目录、参数和结果分别最多 8 KiB。裁剪遵守 UTF-8 边界，并显示裁剪标记。item 结束后可继续查看；turn 退休后清理详情。退休 turn 的墓碑最多保留 128 条，Core 入口仍检查当前轮次归属。

## 协议完成语义

固定版本 schema 中，`webSearch`、`imageView`、`sleep` 和 review item 没有 status 字段。合法 `item/completed` 确认生命周期结束，结果显示未报告，不据此推断工具成功。`imageGeneration` 要求 status，缺失时保持 Unknown。

`contextCompaction` 沿用压缩完成事实。矛盾状态、错误类型的结果字段和明确失败不能被视为成功。文件 `patchUpdated` 刷新同一线程、轮次和 item 的有效审批预览；运行中文件工具的完整快照发生变化时，也刷新对应活动证据。去重、静默恢复与父子隔离见[文件补丁活动证据](file-patch-progress.md)。

### 文件预览更新的版本依据

Codex `rust-v0.159.2` 的 `patchUpdated.changes` 是截至当前已解析内容的完整集合，因此用新数组替换旧预览。格式错误的通知保留旧预览；流中尚未解析的文件不会提前出现。

- [`StreamingPatchParser::push_delta`](https://github.com/openai/codex/blob/rust-v0.159.2/codex-rs/apply-patch/src/streaming_parser.rs#L132) 返回累积 hunks 的副本；同文件的多文件测试逐字符输入后得到全部七个文件。
- [生产 emitter](https://github.com/openai/codex/blob/rust-v0.159.2/codex-rs/core/src/tools/handlers/apply_patch.rs#L115) 转换完整 hunks 列表生成 `changes`；缓冲期间保留最新完整事件。

仓库保留三份完整通知 schema 并校验指纹：`ItemCompletedNotification`、`FileChangePatchUpdatedNotification` 和 `CommandExecutionOutputDeltaNotification`。schema 校验形状，以上固定版本实现证明数组替换语义。

## 隐私与验证范围

正文只进入实时内存和详情面板，不进入 `ObservationSnapshot`、timeline metadata、journal、JSONL、history、export 或 diagnostics。详情中的控制字符经过终端显示过滤；关闭应用后不能从历史恢复原始工具内容。

## 验证记录

2026-10-04 执行 `python scripts/verify.py --cargo-test --live` 通过：307 项库测试、6 项集成测试、8 项真实 Codex 库测试和 1 项真实 CLI JSONL 测试；fmt、check、Clippy、doctest、release 构建和全部原生夹具通过。

本批回归覆盖 UTF-8 单项与总预算、条目淘汰后的重复终态、退休轮次边界、root/child 完整身份、换轮清理、断连保留部分输出、最终 aggregate 替换、压缩数值补录和非法/陈旧文件预览更新。UI 测试覆盖精确定位、身份错配、淘汰、返回、滚动和尺寸变化；Windows ConPTY 夹具确认详情正文展示、最终输出替换和浏览零额外执行请求。

其他终端宿主、Alpha 试用任务集和 V2 发布验收仍有独立范围。
