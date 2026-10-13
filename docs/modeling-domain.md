# 建模业务视图

日期：2026-10-12。建模器根目录为 D:/project/blender-agent-modeler，业务 Skill 位于其 .agents/skills/building-modeling/SKILL.md；权威协议、基线重评估与真实验收分别位于 docs/domain-modeling-protocol.md、docs/domain-modeling-reassessment.md、docs/dsh-domain-acceptance.md。

平台 dsh Agent 理解自然语言、准备/修订有类型的构件方案和调用 MCP。Python 管 CAD、IR、编译、验证与唯一写入/构建 seam。Rust Core 管执行生命周期、领域事实投影、具体版本批准和异步授权 ownership。UI 只消费 DomainSnapshot 并提交 typed Command，使用通用输入、请求、模型与会话页面。

--domain-context 在 config 中注入 stdio modeling_domain MCP。领域会话的通用工具必须 read-only，resume/reload/new 后重新确认；失败不启动 turn。普通 TUI 默认显示建模业务页，可 F6/Esc 返回会话、Enter 发反馈、Ctrl+Y 批准、PgUp/PgDn 查看依据及各阶段。小终端、平台问答和其他覆盖面板不会记录可批准的展示 binding。

Core 从已校验线程／回合身份、带 registry-derived MCP identity 的 completed 事件读取完整领域事实，先于通用工具详情 result 的 8 KiB 展示截断。独立领域 envelope 上限为 64 KiB，并核对 run/context SHA 和 fact_sequence；失败、超限、错身份或 shell 标题模拟结果都不能成为批准事实。事实投影不进入日志/诊断全文。授权 CLI 由 Core 独立拥有 10 秒 deadline、kill_on_drop 和 generation/attempt 检查；结果绑定具体 proposal/revision/token/hash 后交给同一 Agent。中断留 unknown，不自动补发。

产物为受控异步 job。Python scene lock 只捕获封存快照；worker 和 artifact lock 管实际 Blender。Core 不持权威锁或执行模型调度。应用与构建分开，失败不回滚已应用场景。业务视图区分领域、场景应用、产物与验证；未执行的 AI/人工视觉复核明确显示 not_run。

平台对话缓存与重绘节流仍沿用最新基线；领域事实变化立即重绘，在真正 draw 后记录已展示版本。用户还没看到的新版不能通过旧界面批准。

建模事实区按实际终端宽度预先换行，并根据可见高度限制 PgUp/PgDn 的偏移。连续翻到底保持最后一屏可见；终端调整大小或旧偏移超出范围时，渲染和下一次键盘操作均限制到内容边界。回复仍独立滚动，审批仍使用实际渲染的具体版本。宽／窄终端的 fake-terminal 回归覆盖连续翻页、超大偏移及一次 PgUp 返回事实；6 项建模 UI 专项和全量 546 项 Rust 测试通过，2 项可选 live 默认跳过。

旧 --modeling/--change-context 功能、测试及医院证据保留为兼容入口。新旧控制者在 CLI 校验中互斥；仅床/盆单层示例不能等同整栋医院或全部构件支持。


复杂来源验收入口为 `tests/fixtures/dsh_tui_check.py --only domain`，只运行领域步骤和进程清理；`--domain-case chair`（或 `MODELING_COMPLEX_FULL=1`）增加单椅方案、局部反馈、具体批准和产物验证；`--domain-case mixed` 验证椅／柜／盆的候选、两轮局部反馈、部分采纳、批准和构建。需指定 `MODELING_COMPLEX_ROOT` 与 `MODELING_COMPLEX_CONTEXT`；完整运行必须使用 `repository/outputs` 下隔离的合成场景副本，脚本会拒绝直接修改跟踪夹具。验收等待每个回合终态后再发送意见，并滚动查看实际方案和阶段事实；结果保存为 `target/dsh-e2e-*/report.json`。领域程序测试、真实模型闭环、AI／人工视觉复核分别记录，不能相互替代。


## C07 澄清交互验收

来源：模型器 `docs/modeling-complex-cases-plan.md` 的 C07 要求。真实运行表明部分 dsh/provider 工具列表没有平台问答入口；Agent 会在普通对话中询问用户。此前默认建模页只展示领域事实，这类问题需要 F6 才能看到。

建模页现在单独展示当前根线程、当前回合的 Agent 回复；默认显示回复末尾，Shift+PgUp/PgDn 浏览回复，普通 PgUp/PgDn 继续浏览方案事实，输入回复后恢复跟随末尾。回复是对话内容，不生成批准或执行事实；旧回合、子 Agent 回复不作为当前回复展示。平台问题请求仍使用原请求页面。

`--domain-case clarify` 新增真实含糊来源指代、缺高度、允许示意高度、确认尺寸与 CAD 冲突、冲突澄清、具体批准及构建验收。仅通过键盘推进同一平台 Agent；读取领域记录作独立证据，不从验收脚本直接准备、授权或应用场景。场景仍必须位于模型器 `outputs/` 的隔离副本。

验收同时保存每个阶段的场景／来源哈希、提案链、批准／应用记录与实际屏幕到 `clarification-stages.json` 及阶段文本。模型器 `scripts/verify_complex_clarification.py` 再核对未决阻塞、版本失效、来源与保护对象不变、具体授权绑定、assumed 保留、真实 Blender 重建及 GLB 回读。实际屏幕必须显示具体澄清问题，仅出现“待确认”不算问题可见。

真实 C07 证据 `target/dsh-e2e-de078e5a`：8 步通过，默认建模页可见三个阶段的具体普通对话问题，V1 blocked→V2 prepared→V3 blocked→V4 applied；仅最终版本获得具体批准与 Blender／重建／GLB 验证。模型器独立核验生成 `verified-clarification.json`（valid=true）；此前 `be33d6f7` 虽原脚本绿灯，仍因问题不可见被独立核验拒绝；`0834f997` 的纯待确认分叉失败促成领域修订修复。这些证据独立于 543 项 Rust 测试，不宣称 C08 关系验证器、C09–C11 真实恢复专项或医院完整闭环完成。AI／人工视觉复核为 not_run。


## C08 门窗宿主与真实开口验收

Core 在既有领域提案事实中投影 Opening，展示稳定对象 ID、门／窗类别、宿主、封存 CAD 来源位置、偏距／宽度／高度／窗台（米），以及宽度、高度和窗台的参数依据。非法尺寸类型或未知开口类别不能成为批准事实；具体批准仍绑定 proposal／revision／token／hash。该投影沿用 DomainSnapshot 和 typed Command，不由普通 Agent 回复生成授权。

真实入口 `--domain-case hosted` 通过键盘推进同一平台 Agent，完成扫描、宿主歧义提问、原链修订、事实参数复核及 Ctrl+Y 批准／应用／构建。必须使用模型器 `outputs/` 下新建的隔离场景副本；设置 `MODELING_COMPLEX_ROOT` 和 `MODELING_COMPLEX_CONTEXT` 后执行 `tests/fixtures/dsh_tui_check.py --binary target/release/native-agent-tui.exe --model hi/gpt-6.1-sol --only domain --domain-case hosted`。脚本仅推进交互并读取证据，不直接准备、授权或应用场景。

最终运行 `target/dsh-e2e-0990eef1`：5 步通过、0 失败，进程清理通过。输入为模型器 `outputs/complex-hosted-61498c2e/context.json`；`hosted-stages.json` 与阶段屏幕保存未批准不写入、具体提问及审批前两处参数／依据实际可见。V1 blocked→V2 applied，仅最终版本获得授权。模型器 `scripts/verify_complex_hosted.py` 独立核验为 valid=true，并核对实际 Blender 构建／重建／GLB 回读、孔洞探测、墙体体积 4.092 m³、保护对象、来源和产物哈希。

保留 `eb0e16b8`、`04d90bad`、`8f4ace6f` 三次失败：两次暴露具体提问措辞谓词过窄，一次真实 Blender 拒绝新增门扇遮挡受保护既有洞口，促成领域准备／应用阶段的反向影响检查。`04d90bad/verified-failure-retention.json` 仅证明构建失败保留已应用场景，未覆盖取消／断连恢复或旧有效产物保留。

全量 `scripts/verify.py` 通过：545 项 Rust 测试通过，2 项可选 live 默认跳过；格式、编译、Clippy、doctest、release、终端和进程专项通过。以上真实验收独立执行。关系能力 `relations/2` 仅在专用门窗路线开放宿主和真实开口，楼梯／井道跨层仍缺验证器；C09–C11 其余真实专项及医院完整闭环继续实施。AI／人工视觉复核仍为 not_run。
