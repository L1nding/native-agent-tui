# Blender 预览会话接入

来源：2026-10-12 Blender 实时预览 MVP 授权及模型器领域协议 v2。
基线：`4f932c749cde12507da7797471c6cffd8c88f50c`；在独立 Orca 工作树
`L1nding/blender-preview-tui` 实施，原 TUI 和建模工作树保持只读。

启动时使用 `--domain-context FILE --domain-preview`。Core 持有外部 Python
`blender_agent.integrations.preview_service` 子进程，其连接文件路径显示于 F6 业务页。
配套模型器必须包含预览 publisher/service，并在独立 `.venv` 安装 `.[mcp]`。
用户在 Blender 安装本地 Extension ZIP、选择连接文件并连接；完整安装/协议见模型器
`docs/blender-live-preview.md`，实际闭环见 `docs/blender-live-preview-acceptance.md`。

`preview` module 隐藏子进程、JSONL 和反馈解码；它复用 owned_process 与 PipeTransport。
最多 64 KiB control frame、32 个 receipt 队列、256 个 request IDs。
Core 的 typed PreviewEvent 绑定独立的会话 epoch；resume/reload/new 与退出会回收旧 Bridge，
旧 ready/feedback 不进入新会话。反馈结构的 Debug 不含意见原文。

收到反馈后，Core 要求平台 idle、无交互/授权进行中，检查完整 ProposalBinding、未应用状态和大小限制。
服务负责映射 part→instance→patch targets；Core 通过原 SubmitRootInput/scheduler/outbox 提交同一 dsh 会话。
回执在正常 scheduler 确认出站提交后才为 submitted；排队不当成已提交。
此路径不创建 authorization_id；Ctrl+Y 继续绑定实际渲染的具体版本并使用原 domain-authorize seam。

来源内容、网格显示、selected feedback 都不产生隐式业务批准。失败保持应用场景与旧产物；
GUI 是显示副本，手动拖动/保存 BLEND 不能应用 IR。床/洗手盆仍是领域工具现有能力，
预览身份和映射协议没有固定类别枚举。

验证：`scripts/verify.py` 包含 fmt、locked check/Clippy、nextest、doctest、release/examples，
journal/replay、终端输入/会话流、Windows 进程身份与 ownership；专项覆盖旧 epoch 不投影、
版本/上下文/已应用/超长反馈拒绝及 instance/part 映射消息。真实验收使用 dsh 的
hi/gpt-6.1-sol、实际 F6/Ctrl+Y 和 Blender 5.1.2，不用假模型替代。
