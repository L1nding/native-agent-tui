# 文件补丁的活动证据

文件工具持续发送 `item/fileChange/patchUpdated` 时，Core 用实际变化刷新该工具和所属轮次的最近证据。工具因此从 Quiet 或 AttentionNeeded 恢复为 Active；其他工具、交互请求和等待中的父代理继续保留各自状态。

## 模块职责

- `src/protocol/file_changes.rs` 校验线程、轮次、item、路径、变更类别和 diff，返回借用原事件的有类型快照。完整快照指纹也在这里计算。
- `src/observation.rs` 为已确认运行中的文件工具保存私有指纹，判断是否接受新活动证据。去重不依赖审批预览缓存。
- `src/interactions/files.rs` 将有效快照转成有界审批预览，沿用 32 KiB 单项、64 项和 256 KiB 总预算。
- `src/client.rs` 在当前 root/child 轮次通过归属检查后连接这些模块；UI 继续读取快照。

协议校验只有一个实现。文件预览不再解析原始 JSON，也不承担活动判断；Core 不需要比较正文。

## 进展规则

`patchUpdated.changes` 是累计完整快照，按替换处理。`item/started` 中的有效快照只建立基线；开始事件缺少 `changes` 时，首次非空补丁更新算新进展。首次空快照只建立基线，已有非空快照变为空也算变化。

相同快照不推进 `progress_seq`。缺失或非法补丁、其他线程、旧轮次、未知 item、其他工具类别、已结束工具及失去执行所有权后的事件不产生进展。重复开始事件不会回退已有补丁的去重基线。

Observer 同时过滤预览的重复开始事件；即使预览缓存已淘汰，旧快照也不能覆盖挂起审批里的较新内容。输出先于开始事件时，迟到的首次开始仍可确认工具开始时间，但不能替换已接受的补丁。重复补丁可以恢复被淘汰的预览，进展序号保持不变。

新进展沿用 `Output` 证据类型，`output_bytes` 为零：累计补丁不能反复加进输出长度。它不确认成功、不回答审批、不释放 Gate，也不产生模型、轮询或中断请求。

## 内存与隐私

指纹覆盖完整路径、类别、移动路径和 diff，包括审批预览被裁剪后的部分。每个保留活动只增加固定大小的指纹，受既有 1024 项活动身份上限约束；换轮清理随活动所有者进行。

指纹使用标准库进程内随机哈希，仅用于本次会话去重，不能用于安全校验或跨会话比较。正文和指纹不进入 ObservationSnapshot、时间线元数据、journal、JSONL、历史、导出或 diagnostics；审批预览仍只保存在实时内存。

## 验证范围

专项回归通过协议、Observer 和 ClientHandle 的生产事件入口验证活动恢复、重复事件、裁剪后的变化、身份过滤、父子隔离、审批预览和脱敏投影。Client 回归独立放在 `src/client/tests/file_progress.rs`。

2026-10-05 的 `python scripts/verify.py --cargo-test --live` 全部通过：315 项库测试、6 项集成测试、8 项真实 Codex 库测试和 1 项真实 CLI JSONL 测试；fmt、check、Clippy、doctest、release 构建和全部原生夹具通过。专项 nextest 检查另通过 12 项文件相关测试。

可控时钟覆盖 Quiet、AttentionNeeded 和更新后的 Active；生产 Core 回归覆盖 gated child 的完整身份、父进展与请求计数保持不变、65 项预览淘汰、32 KiB 裁剪后的后缀变化、重复和迟到开始事件、旧轮次、非法输入及脱敏投影。独立只读复审未发现剩余生产反例。

真实 Codex 检查验证锁定 0.159.2 的兼容性、审批、Gate 和清理；本次 patch 活动回归使用内存 transport，不代表已实测真实文件工具的全部流式事件时序。工具正文搜索、其他终端宿主及 Alpha/V2 发布验收继续保留独立范围。
