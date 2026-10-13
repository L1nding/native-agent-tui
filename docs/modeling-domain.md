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


## C09 异步版本交错验收

`tests/fixtures/dsh_tui_check.py --only domain --domain-case async` 经键盘推进同一平台 Agent，依次扫描、准备并启动 V1、修订 V2 并先完成、修订 V3 并先完成、最后验证历史 V1、浏览 V3 的实际产物及具体方案，最后 Ctrl+Y 批准和构建。门闩由实际 Blender 内的模型器测试脚本持有，只控制隔离副本的时序；验收脚本读取证据并操作键盘，准备、授权、应用和构建沿既有平台／领域工具执行。

模型器 `scripts/prepare_complex_async.py` 生成新的隔离 context，设置 `MODELING_COMPLEX_ROOT` 和 `MODELING_COMPLEX_CONTEXT` 后运行此入口。`async-stages.json` 保存九个业务阶段、实际方案链、来源／场景哈希、授权／操作、实际产物／门闩和 fact sequences；屏幕分别保存方案绑定和产物事实，避免要求两处信息同时出现在小视口。只读 `scripts/verify_complex_async.py` 核对 V2／V3／V1 完成顺序、历史结果隔离、实际展示版本与具体授权绑定，以及应用后的真实构建／重建／GLB 回读；Agent 实际验证 envelope 的 SHA 必须存在于其工具事实记录，核验脚本不发布事实。

保留失败 `target/dsh-e2e-2e9b2af8`、`target/dsh-e2e-762ca25d` 和 `target/dsh-e2e-a1851937`：分别暴露同时可见条件过严、业务滚动越界和 Agent 在修订时丢失 scan_id。滚动修复 `75899f7` 保证到边界仍显示事实、一次 PgUp 即返回内容；后者通过明确读取完整 component_plan 并保留来源绑定重新验收，领域检查保持。失败运行无具体批准、无场景应用，独立来源／基线字节核对已保存。

最终真实证据 `target/dsh-e2e-0b74cd96`：9 步通过、0 失败，进程清理通过。输入为模型器 `outputs/complex-async-c09-final-20261013/context.json`；独立 `verified-async.json` 为 valid=true，实际完成顺序 V2→V3→V1，V1/V2 保留历史归属，只有 V3 获得具体批准及唯一应用。应用构建 `applied-chair-v3-build-001` 的实际 Agent 验证事实序号为 22；三阶段对象 ID、包围盒和体积及 GLB 回读通过。四类伪造证据（历史 V1 变当前、完成顺序、缺预览验证、缺应用后验证）均被独立核验拒绝，原始阶段证据字节保持。

C09 代表真实平台路径已完成；C11 平台断连／恢复、C08 跨层关系和医院完整闭环继续推进。AI／人工视觉复核为 not_run。


本轮提交前完整 `scripts/verify.py` 通过，日志 `target/c09-final-verify-20261013.log`：格式、所有目标编译、Clippy、546 项 Rust 测试（2 项可选 live 默认跳过）、doctest、release／examples，以及终端输入、历史／重放、流式对话和 Windows 进程专项通过。真实 C09 验收独立执行。


## C11 操作发现、断连恢复与重载

用户只指定恢复楼层时，Agent 可从 `describe_capabilities.operation_index` 发现持久操作，先 `inspect_change` 再 `inspect_operation`。索引最多20条、未完成操作优先，返回 total／truncated 和状态摘要；完整原授权绑定仍从显式操作查询读取。Python 对 candidate_present 的恢复沿原授权对账，不重放场景 writer；base_present／conflict 保留不确定状态。

Core 的 `OperationFact` 校验具体提案／版本／token／base／candidate 绑定、操作 ID、状态、恢复判断和实际哈希。默认业务页展示回执及当前场景，applying 明确显示结果未确认。当前方案已有操作时不再批准；操作和应用抬高提案／操作序号，迟到的 prepared 或 applying 事实不能恢复可批准状态或覆盖已对账结果。新的候选版本清除旧操作投影，重载／新 owner 清空事实并重新确认 read-only，查询后才恢复展示。

`--domain-case recovery` 通过键盘运行真实 Agent，使用模型器 `scripts/prepare_complex_recovery.py` 的新隔离 context 和 `gated-dsh.cmd`。门闩只在此绑定 context 的实际 MCP 内调用生产 writer 后暂停，脚本核对进程祖先链和创建时间后终止本次验收自己的 dsh，使 Core 实际收到 EOF。脚本不准备、授权或应用方案；重启同一 TUI 对象的底层控制台后，用自然语言要求恢复楼层，所有对账和构建仍经真实领域工具。

最终 `target/dsh-e2e-3eaa58e3`：8 步通过、0 失败，进程清理通过。实际 Scene IR 替换后回执 applying、任务 checked；新执行进程没有旧批准绑定，Agent 发现并显示 candidate_present，只读阶段不写。继续后沿原操作 `recovery-chair-apply-v1`／原授权对账，writer 总数1、任务 applied 历史1；旧有效预览保持，实际 `recovery-applied-build` 的构建／重建／GLB 和 Agent 验证（事实序号16）通过。Ctrl+R 重载后先拒绝旧批准绑定，再查询恢复操作与当前产物，journal 证明两个平台执行进程和后续 root generation。模型器 `scripts/verify_complex_platform_recovery.py` 独立 valid=true；四项伪造证据（writer 重放、错误 dsh 身份、缺操作查询、缺实际构建验证）全部拒绝。

保留首轮 `target/dsh-e2e-38749061`（5 通过／1 失败）：实际 UI 已清除不可批准方案的 displayed binding，按 Ctrl+Y 得到 UI 拒绝提示；验收只接受 Core 的另一条拒绝提示而误判。支持两条拒绝路径后用新副本重跑，授权／操作／writer 不增加的条件保持。首轮独立核对仅证明不确定已替换场景和旧有效预览保持，不作为完整恢复成功。

本轮完整 `scripts/verify.py` 通过：549 项 Rust 测试通过，2 项可选 live 默认跳过，格式、编译、Clippy、doctest、release／examples 和终端／Windows 进程专项通过，日志 `target/c11-platform-final-verify-20261013.log`。真实 C11 独立执行。平台预览取消、后续编辑冲突和构建失败保留旧有效产物的组合路径仍待补齐；医院完整闭环、C08 跨层及视觉复核继续实施。


## C11 同方案多个构建任务与失败后保留

重新核对发现原独立 artifact／artifact_validation 字符串会混淆同一 proposal 的多个 build：A 已验证成功后，B 的 building 状态旁仍能保留 A 的 valid=true。回归先在原实现失败，修复为 Core 持有 `ArtifactBinding`／`ArtifactFact`／`ArtifactState`：完整核对 build_id、proposal、revision、token、candidate、authority/input SHA 和 output，状态／验证共享每作业事实水位。迟到 A 不替换 B；B 失败不继承 A 验证；错误字段和 ID 复用不污染序号。当前方案最多缓存32条构建事实，持久回执与旧有效产物保留；新方案和重载清空内存，再查询恢复。操作查询确认后来修改后，旧已应用产物保留 valid=true，同时标记 current=false。

业务页逐任务显示状态、验证和产物位置；状态行及验证行各自包含 build_id，任务标题滚出视口时仍能确认归属。新增宽／窄假终端、同方案晚到成功／失败、身份拒绝、水位、缓存边界和场景后续编辑专项。Python 事实仍是来源，UI 不执行工作流或推断成功。

真实 `--domain-case failure` 使用模型器 `scripts/prepare_complex_failure.py` 的新隔离 context，经键盘推进 `hi/gpt-6.1-sol`。Blender 门闩只绑定这个新运行的第二个作业，实际进入 Blender 后记录活体 PID／创建时间证据，由验收释放后抛出失败；测试编辑器在锁内验证并增加后续审阅 metadata。验收不直接 prepare／authorize／apply，也不调用 `_result` 制造事实。最终 `target/dsh-e2e-a57e53b6` 6项通过、0失败，进程清理通过，模型器独立 `verify_complex_platform_failure.py` 为 valid=true。

具体批准只产生一份授权与一次任务 applied。`failure-valid-build` 完成实际构建／重建／GLB 和 Agent 验证；同提案 `platform-failed-build` 的真实失败验证序号14，默认业务页分别展示失败 valid=false 和旧构建 valid=true，旧 GLB／BLEND／报告哈希及已应用场景保持。独立后续编辑完成后，Agent 实际查询并沿原操作 `apply-chair-46505ec85ea341fdb00a5a06f7ca4df2-r1` 获取原回执，后续场景字节保持，旧产物 valid=true／current=false，未增加批准或构建。四项证据篡改（缺失败验证、失败行改成功、缺旧产物哈希、改后续编辑回执）全被拒绝，原成功证据保持。

三次失败原样保留：`target/dsh-e2e-4fc35226`（1通过／1失败）扫描后准备超时；`target/dsh-e2e-31182daa`（4通过／1失败）失败任务标题滚出短视口，验收无法绑定；`target/dsh-e2e-c0aa7270`（3通过／1失败）带 ID 的实际 valid=true 位于末行，验收把终端边框纳入末尾匹配而拒绝。独立保留核验只记录各轮实际已完成范围，不改写为组合成功。拆分扫描／准备、每条状态绑定 ID、修正边框／相似 ID／Agent 回复匹配，并扩大 failure 场景的有界滚动范围后，使用新副本重跑。两项 `domain_failure_view_test.py` 接入默认 `scripts/verify.py`。

最终 `scripts/verify.py` 全量通过：556项 Rust 测试通过、2项可选 live 默认跳过，日志 `target/c11-artifact-binding-final-verify-20261013.log`。领域／业务页28项专项通过；新视口匹配测试接入后 `--fixtures-only` 再通过，日志 `target/c11-failure-view-final-fixtures-20261013.log`。真实6项另行执行。平台预览取消、未完成应用的后续编辑冲突、C08 跨层、医院完整闭环及 AI／人工视觉复核仍未完成；不能把该子路径当成 C11 或整个计划完成。


## C11 单个预览取消与重载后的回执发现

模型器提交 `a9b67d6` 新增 cancel_artifact 和 artifact_index：只取消当前 MCP owner 持有的运行进程树，Windows 在创建时原子加入 Job Object并确认整棵树退出；完成竞态保持完成，未拥有任务保持 unknown。关闭 MCP 与用户显式取消分别记录。范围内预览索引使新 owner 仅凭楼层发现没有应用操作的 preview；索引不推断产物有效性或授予批准。Agent 自然语言步骤已同步，Core 沿现有逐 build_id 的事实绑定展示回执，UI 发 typed command并消费快照。

新 `--domain-case cancel` 实际使用键盘和 `hi/gpt-6.1-sol`，输入为模型器 `outputs/complex-cancel-live-20261013-01/context.json`。`target/dsh-e2e-4851c281` 7项通过、0失败，模型器独立 `verify_complex_platform_cancel.py` valid=true：旧真实预览 built／rebuilt／GLB有效，同方案第二实际 Blender 在隔离门闩运行；用户要求取消后，实际 owner66748及Blender确认退出，取消验证序号11，旧有效文件／prepared方案／Scene IR保持。无授权或应用，最终构建数2。

`/model` Ctrl+R 重载先拒绝旧批准绑定，再仅凭楼层读取索引并查询／验证 built与cancelled；请求原ID重试后取消文件哈希保持，无新作业。native PID／creation time与完整实际TUI祖先链、业务页按任务展示、root generation及最后进程清理均核验。四项证据伪造（缺取消验证fact、错误owner创建时间、缺旧产物哈希、取消UI改成功）被拒绝，成功证据未改变。本次只添加验收路由／fixture／文档，`scripts/verify.py --fixtures-only` 通过（`target/c11-cancel-fixtures-20261013.log`）；此前556项Rust完整基线保持。

未完成应用遇到后续编辑冲突、C08跨层、医院完整闭环及AI／人工视觉复核仍未完成，不能把该路径作为完整C11或整个计划完成。


## C11 applying遇到后续几何编辑：实际拒绝与重载

新增 `--domain-case conflict`，绑定模型器 `prepare_complex_conflict.py` 的新隔离输入／专属MCP写入门闩。键盘驱动真实 `hi/gpt-6.1-sol`：独立回合扫描、准备、实际旧预览验证，再由具体Ctrl+Y批准。候选原子替换后终止当前TUI祖先链中的自有dsh host，Core接收EOF，原operation=applying／task=checked。独立测试编辑器将新椅子6个部件统一X平移0.25m，记录来源／理由并保留所有其他实体与原记录。

新平台owner先拒绝旧批准绑定，只读查询显示原operation_id的applying／对账conflict；沿原授权和操作真实调用apply_change，领域层拒绝并产生实际diagnostic fact sequence16。原grant／operation／proposal／task字节保持，Agent场景writer1次、独立编辑1次、恢复writer0次；旧有效预览valid=true／current=false。`/model` Ctrl+R清空旧绑定后，再读取冲突与验证事实，保持修改。新增路由纳入隔离检查；本轮复用现有Core投影，无Rust生产修改。

真实 `target/dsh-e2e-0ee78bd8` 10项通过、0失败，native清理通过；模型器独立 `verify_complex_platform_conflict.py` valid=true。四项证据副本负例（缺实际拒绝、writer重放、错误编辑回执、UI伪造candidate_present）及单部件漂移核验负例全拒绝，原证据保持。首轮 `target/dsh-e2e-9e3ad7a0` 复合准备／预览步骤超时（2通过／1失败）按实际范围保留；拆回合、明确必填request后用新输入重跑，未重写失败。`scripts/verify.py --fixtures-only`通过（`target/c11-conflict-fixtures-20261013.log`）。C08跨层、医院完整闭环和视觉复核仍未完成。
