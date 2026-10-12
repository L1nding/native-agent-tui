# 建模业务视图

日期：2026-10-12。建模器根目录为 D:/project/blender-agent-modeler，业务 Skill 位于其 .agents/skills/building-modeling/SKILL.md；权威协议、基线重评估与真实验收分别位于 docs/domain-modeling-protocol.md、docs/domain-modeling-reassessment.md、docs/dsh-domain-acceptance.md。

平台 dsh Agent 理解自然语言、准备/修订有类型的构件方案和调用 MCP。Python 管 CAD、IR、编译、验证与唯一写入/构建 seam。Rust Core 管执行生命周期、领域事实投影、具体版本批准和异步授权 ownership。UI 只消费 DomainSnapshot 并提交 typed Command，使用通用输入、请求、模型与会话页面。

--domain-context 在 config 中注入 stdio modeling_domain MCP。领域会话的通用工具必须 read-only，resume/reload/new 后重新确认；失败不启动 turn。普通 TUI 默认显示建模业务页，可 F6/Esc 返回会话、Enter 发反馈、Ctrl+Y 批准、PgUp/PgDn 查看依据及各阶段。小终端、平台问答和其他覆盖面板不会记录可批准的展示 binding。

只接受带 registry-derived MCP identity 的 authoritative completed results，且 run/context SHA 匹配；不能通过 shell 标题模拟领域结果。事实投影有界，不进入日志/诊断全文。授权 CLI 由 Core 独立拥有 10 秒 deadline、kill_on_drop 和 generation/attempt 检查；结果绑定具体 proposal/revision/token/hash 后交给同一 Agent。中断留 unknown，不自动补发。

产物为受控异步 job。Python scene lock 只捕获封存快照；worker 和 artifact lock 管实际 Blender。Core 不持权威锁或执行模型调度。应用与构建分开，失败不回滚已应用场景。业务视图区分领域、场景应用、产物与验证；未执行的 AI/人工视觉复核明确显示 not_run。

平台对话缓存与重绘节流仍沿用最新基线；领域事实变化立即重绘，在真正 draw 后记录已展示版本。用户还没看到的新版不能通过旧界面批准。

旧 --modeling/--change-context 功能、测试及医院证据保留为兼容入口。新旧控制者在 CLI 校验中互斥；仅床/盆单层示例不能等同整栋医院或全部构件支持。
