# 建模业务视图

2026-10-15 类别展示：Core 从完整来源分组核验并汇总 category_counts，逐组数量必须等于实例数。建模页优先展示已评估组数、未定类别候选、实际类别及来源数量，与已建数量分开。专项覆盖 8 个已标注候诊座椅、2 个未定类别、0 个已建，完整 nextest 665 项通过、4 项可选跳过，Clippy 与 release 构建通过。原八层真实 Agent 已读完 36 组／889 条 CAD 来源，保留 18 条用途或归属待确认；分类保留推断，未应用结构或构件，不能把来源数当实物数。

最终验收：全量 Rust 664 项通过、4 项可选跳过。新版 TUI 已从原运行 `project-36156-1792032656143688800` 恢复原结构提案第 1 版，Core 正确绑定八层、无事实同步失败；Agent 实际结束回合并指向建模页 Ctrl+Y，没有再弹批准问答。F4 实际展示六步业务进度，Esc 返回原建模页。此轮未代替真实用户触发批准，尚未执行应用／构建；已知旧问答的批准意见不是平台授权回执。

同次恢复验收还发现：新 TUI 会话已读 `inspect_project.recovery_index.selection`，Core 却未恢复该持久选择，使后续原楼层事实被判为未绑定。现在仅在 `catalog_current=true`、目录／来源匹配、候选属于目录且当前无选择时恢复；不覆盖会话中较新的选择，不产生批准／应用。专项红绿测试覆盖恢复原作用域、拒绝跨来源及保留新选择；75 项领域／业务界面测试、10 项工作流按键测试和 Clippy 通过。F4 打开时由工作流接管键盘，Esc 返回建模页，避免后台扩展吞掉滚动或关闭动作。

2026-10-15 审批入口修正：现场会话把“批准并继续”放进普通 `ask_user_question`，只记录意见，未产生具体版本授权；该未完成请求和运行回合又使真正授权入口不可用。模型器技能及 MCP instructions 现在要求展示具体方案后结束回合，使用建模页 Ctrl+Y；禁止用业务问答重复请求场景批准。UI 在运行回合或活跃调度任务期间不登记可批准 binding，提示等待本轮结束。回归 `running_model_turn_cannot_present_an_actionable_modeling_approval` 修正前失败、修正后通过。另修复建模页提前返回导致 F4 被遮住的问题；F4 只读展示工作流，不保留被遮挡方案的批准入口。全量 Rust 661 项通过、4 项可选跳过，Clippy 通过；模型器 MCP／领域接口 69 项通过。旧问答意见不会追溯伪造具体授权。

2026-10-15 工作流展示：F4 与建模业务页共用只读业务步骤，展示选层、结构准备、图纸审查、来源发现与分组识别、具体批准／应用／Blender 构建、最终对账复核。步骤从已核验领域快照投影，不从 Agent 回合终态推断完成；已应用结构仍显示未决来源。该列表为进度展示，不创建新的调度任务，不自行批准或推进领域操作。27 项建模／工作流界面测试通过，包括宽窄屏步骤可见性和结构应用后 323 个待处理来源的回归；Clippy 通过。

2026-10-15 主动构件发现修正：普通楼层目标由模型器技能定义完整流程，用户输入“建模八层”即可启动。Core 从来源台账提供独立构件进度，UI 以中文展示已发现／已建／待处理，并在存在未决来源时显示“整层尚未完成”；结构构建或模型回合结束不代表整层结束。项目楼层事实的 `component_discovery` 区分各图层发现、显式图层配置和需核查的旧范围，不能在普通提案中偷换。新类别沿模型器已有数据定义与具体批准机制创建和复用；修正原因、数据边界及回归命令见模型器 `docs/modeling-discovery-user-flow.md`。

日期：2026-10-12。建模器根目录为 D:/project/blender-agent-modeler，业务 Skill 位于其 .agents/skills/building-modeling/SKILL.md；权威协议、基线重评估与真实验收分别位于 docs/domain-modeling-protocol.md、docs/domain-modeling-reassessment.md、docs/dsh-domain-acceptance.md。

平台 dsh Agent 理解自然语言、准备/修订有类型的构件方案和调用 MCP。Python 管 CAD、IR、编译、验证与唯一写入/构建 seam。Rust Core 管执行生命周期、领域事实投影、具体版本批准和异步授权 ownership。UI 只消费 DomainSnapshot 并提交 typed Command，使用通用输入、请求、模型与会话页面。

项目模式的楼层定位另提供`inspect_project_floor_location`只读事实：绑定catalog_id、selection_id、source SHA、floor_key及location_id，完整逐图层实体数支持分页。未配置楼层在默认墙线不足时，Agent提出`wall_layer_choice`（exact location_id／已见wall_layers／reason），随结构准备提交；选择不是授权，进入同一具体结构摘要后经原批准通道确认。配置/定位证书和CAD依赖在prepare、授权及唯一writer处检查；配置过的楼层保持原图层，定位阈值保持不变。Core仅在当前所选楼层与定位身份/图层匹配时接纳带选择的scope；恢复/切换该scope前先恢复定位事实。F6分别显示证据、绑定选择、原结构状态，不将已应用选择继续称为待执行。真实证据与未完成边界见[完成审计](dsh-tui-completion-audit.md)。

--domain-context 在 config 中注入 stdio modeling_domain MCP。领域会话的通用工具必须 read-only，resume/reload/new 后重新确认；失败不启动 turn。普通 TUI 默认显示建模业务页，可 F6/Esc 返回会话、Enter 发反馈、Ctrl+Y 批准、PgUp/PgDn 查看依据及各阶段。小终端、平台问答和其他覆盖面板不会记录可批准的展示 binding。

Core 从已校验线程／回合身份、带 registry-derived MCP identity 的 completed 事件读取 modeling-fact-receipt/v1 小回执，再使用自己持有的 Python domain-read-fact 通道取完整封存事实。小回执含 run/context SHA、kind、ok、fact_sequence、fact_sha256、fact_bytes、具体绑定和模型摘要；模型摘要最多6,000字节，省略部分由 Agent 经 read_fact_view 分页读取。Core 逐项核对完整事实的字节数、SHA256、上下文、序号和绑定后投影；生产路径不再将模型所见完整 envelope 作为事实输入。回执接收上限16KiB，完整事实读取上限8MiB；stdout/stderr在读取过程中有界。单个读取任务及最多32条等待回执由Core持有，10秒deadline、kill_on_drop、会话重置／shutdown取消、执行generation及读取attempt拒绝迟到结果。

读取中或校验失败时，保留已有事实供对照并拒绝批准；固定页眉显示状态，成功显示序号、字节数和SHA256。读取失败不会自动重放建模写入。授权仍由独立domain-authorize CLI完成，并核对具体版本。普通展示／日志缓存继续使用自己的预算，事实全文不进入诊断。新Python服务与TUI需配套重启；旧envelope或被spill改写的文本显示回执缺失，不会静默留下可批准旧状态。

产物为受控异步 job。Python scene lock 只捕获封存快照；worker 和 artifact lock 管实际 Blender。Core 不持权威锁或执行模型调度。应用与构建分开，失败不回滚已应用场景。业务视图区分领域、场景应用、产物与验证；未执行的 AI/人工视觉复核明确显示 not_run。

平台对话缓存与重绘节流仍沿用最新基线；领域事实变化立即重绘，在真正 draw 后记录已展示版本。用户还没看到的新版不能通过旧界面批准。

建模事实区按实际终端宽度预先换行，并根据可见高度限制 PgUp/PgDn 的偏移。连续翻到底保持最后一屏可见；终端调整大小或旧偏移超出范围时，渲染和下一次键盘操作均限制到内容边界。回复仍独立滚动，审批仍使用实际渲染的具体版本。宽／窄终端的 fake-terminal 回归覆盖连续翻页、超大偏移及一次 PgUp 返回事实；6 项建模 UI 专项和全量 546 项 Rust 测试通过，2 项可选 live 默认跳过。

建模统一从普通 dsh 会话的 `--domain-project` / `--domain-context` 进入。旧 `--modeling` / `--change-context` CLI、专用 UI 和 Rust Python 工作流 owner 已移除；历史验收记录保留用于追溯。楼层、楼栋、来源、图纸修正、连接和已应用家具编辑的迁移证据见[入口迁移记录](dsh-modeling-entry-migration.md)。

已应用家具编辑使用 `prepare_component_edit` 封存完整选中组件、原来源和用户意图。模板组件提交完整替换计划；旧无模板部件使用受限的类型化几何定义。稳定部件 ID、未选中对象及旧共享定义受保护；依赖完整性、来源及具体版本在批准和 writer 处重查。整组件删除从实际授权／应用链派生来源待确认状态，后续增补必须明确绑定原删除提案重新开启。Core 校验并展示原位置、编辑意图和删除来源；实际看图由同一 dsh Agent 回复记录，人工／合规仍为独立状态。


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


## 双层楼梯／井道事实投影

2026-10-13：来源能力 floor-components/5、关系能力 relations/3 接通受控 connections 路线。平台 Agent 仍独占推进；Python 新增授权下层台阶／井道墙，引用两层已有洞口，上层和楼板只读。Rust Core 从真实 MCP 提案的 elements 读取部件轮廓、底标高、高度、服务楼层、洞口与来源位置，展示参数依据及未检查项。无效几何／缺少 typed connection 不能成为批准事实；批准仍绑定原具体版本。UI 沿现有 snapshot.objects 展示，不增加工作流主控或场景写入口。

确定性领域与真实 Blender／重复构建／GLB 已有专项证据；台阶顶面20条与井道全高内腔9条射线检查实际几何。Core 新增两项解析／具体绑定测试。真实 dsh/TUI 跨层验收、建筑净空、结构／规范、电梯与视觉复核仍未完成，不能把上述证据替代真实平台闭环。


2026-10-13跨层证明更新：Core elements解析接受vertical-connection/1及vertical-connection/2，保留具体版本／token／hash批准绑定。新版将完整typed spec与理由集中在稳定owner部件，其他部件引用，不扩大64KiB领域envelope上限。新增解析测试及两项原跨层专项通过，559项Rust全量测试通过、2项可选live默认跳过；release曾成功构建。全量verify随后重建exe时遇到运行中的真实验收占用（Windows os error 5），需验收终态后补跑该步骤；不记为全量verify成功。真实平台第2轮有效候选71,167字节被正确拒绝为批准事实，证据target/dsh-e2e-d1d7c10d保留，无授权／操作／场景写入。第3轮仍在运行。


ACP完整回执修复（2026-10-13）：第4轮target/dsh-e2e-b919c3df通过扫描和实际标高冲突，生成同链prepared修订2；候选44,404字节，完整envelope44,800字节，随后被ACP的32KiB文本展示截断，Core仍显示旧blocked事实，平台验收失败。实际Agent提案事实sequence6存在，无授权／操作，verified-failure-retention.json只证明来源与场景保留。

ACP边界现在分别返回32KiB有界展示文本与64KiB有界完整JSON回执。只有登记过MCP身份的completed事件将完整JSON交给Core；未登记工具仍是commandExecution，展示文本不再反解为领域JSON。原始回执超限、畸形或明确isError时，不能用短展示内容替代；错误标记保持。Core继续核对领域64KiB预算、run/context、fact_sequence及具体批准版本。4项新增协议专项及扩展桥接测试覆盖这些边界。scripts/verify.py完整通过：563项Rust测试通过、2项可选live默认跳过，格式／类型／Clippy／doctest／release与全部native fixtures通过，日志为模型器outputs/c08-acp-complete-result-check-20261013.log。第5轮真实平台跨层验收正在运行，不能将这些协议测试记为真实闭环。


MCP包装规范化补充（2026-10-13）：第5轮target/dsh-e2e-447307c5再次通过扫描和标高冲突；Agent持久化同链prepared修订2（实际事实sequence5），UI未显示新事实，验收失败。场景／CAD保持，无授权／操作，verified-failure-retention.json保留。边界进一步展开单个文本内容块、ACP content包装及标准MCP structuredContent；包装读取限256KiB，展开后的完整逻辑JSON仍限64KiB，Core独立64KiB检查保持。明确错误和超限内层不能退回短内容冒充完整事实。

本地纯序列化核对未调用领域工具：第4轮44,800字节逻辑envelope经当前FastMCP转成52,397字节文本、60,182字节MCP包装（无structured output schema，单个文本块）。新增回归覆盖文本块对象／数组、JSON编码数组、双份标准包装、内层超限及错误标记。最终完整scripts/verify.py通过564项Rust测试、2项可选live默认跳过及全部native fixtures，日志outputs/c08-acp-wrapper-complete-check-20261013.log。第6轮真实跨层仍在运行。

## 小回执与独立事实通道验证（2026-10-13）

真实 dsh 将大模型结果按 spill-policy 改写后才经 dsh-acp 发送，rawOutput／structuredContent 分支和扩大 Rust 上限无法恢复丢失字节。第7轮使用的二进制已包含846698e，失败不能归因于未更新二进制。第4轮旧记录的32KiB归因未独立复核，保留为当时假设。

新增回归验证：大于64KiB的事实使用小文本回执经Core私有Python进程进入可批准投影；逐字段篡改／缺失文件拒绝、同步失败可见、读取恢复、旧序号无法清除新失败、stdout/stderr资源边界，以及超时／取消后真实子进程退出。全量567项Rust测试通过、2项可选live默认跳过；release和全部native fixtures通过。第9轮独立通道扫描通过，后续提案摘要遇到大量短字段的大字典，返回fact_view_too_large，未进入预期冲突展示。摘要器已补字典省略和稳定字段分页，并只读重放该轮12,789字节blocked事实，合法proposal小回执为1,620字节。无授权或场景写入。第10轮使用新隔离输入及hi/gpt-6.1-sol，记录独立事实页眉、封存字节及14个检查点；在独立核验完成前保持未完成。


跨层第10轮进一步核对：小回执与Core独立读取已支持45,171字节同链修订、依赖完整子方案、真实预览与具体批准应用／有效构建。模型在领域成功后遇到upstream_http2_stream_error，该轮按失败保留；独立verify-partial证据核对仅一次授权／应用和真实几何，不能代替完整井道连续增补。其他provider实际调用及最新状态见领域事实独立通道文档。整体复杂计划保持实施中。


## 来源分组与实例台账的当前性（2026-10-14）

真实C04运行出现过：source_groups旧快照仍显示8个待新增，而较新source_ledger已把4个来源跳过。
这些事实的历史归属都有效，但不能同时作为当前来源视图显示。Core现按一个sources序号处理两种
核验事实；新分组取代旧实例视图，新实例台账取代旧分组视图，跨类型迟到结果保持历史归属。
不从相同总数或scan_id推导逐实例状态相同，不在UI猜测或拼合类别状态。Malformed事实不清掉
已有有效视图、不推进序号。提案及具体批准binding保持独立。

两条确定性正反回归先在旧实现上失败；修复后40项领域／业务页专项通过。全量格式、check、Clippy、
nextest 620项通过，4项可选live跳过；doctest为0项。日志在模型器outputs/source-view-currency-*-20261014.log。
release写到target/source-view-currency，不替换仍被真实医院验收持有的target/release；该新二进制的原生／回放／对话／终端／进程／Python夹具现已全量通过，日志outputs/source-view-currency-release-fixtures-20261014.log（位于模型器根目录）。

真实模型调用遵循用户当前指定的hi/gpt-6.1-sol，禁止Astra和自动回退。C04第三轮在真实候选预览验证后
因provider并发上限失败，原记录5 PASS／1 FAIL及零批准／零应用保持；当前医院29项诊断串行重试中。
历史Astra验收只保留原证据，不能冒充本次指定模型成功。


## 同一请求逐项处理多个候选（2026-10-14）

29个医院未建落地实例属于同一blocked提案。复用平台`ask_user_question`／`zf/session/ask`／`item/tool/requestUserInput`，插件、ACP与Core同步接收最多64项。每项独立回答，UI保存回答并在全部填写后一次提交；反馈ID与稳定实例ID由Agent传给现有修订和部分选择工具。没有新增领域写入通道或自动授权。

只读续接`target/dsh-e2e-30d10d78`实际2项PASS，复用`af395b49`准备的`5e7229773aca41f4ab384d2066b511a0`。模型器独立核验valid=true：29项CAD姿态及来源依据保持，29项placement/2阻塞，当前265已建场景及来源处置未改变。`af395b49`仍按provider并发限制FAIL保存；续接不冒充其成功终态，也没有重新准备或应用。

29项请求／逐项选择／自定义调整反馈／单次完整回答已由插件与Rust专项覆盖。该专项不等于用户已经批准29项；后续真实模型批量交互验收见下文，测试回答仍不充当医院人工批准。运行新版桥接时，dsh profile的`zf-tui-bridge`链接需指向本建模工作树构建的`dsh-plugin`；原平台链接不会因本工作树编译而更新。

验证记录：完整`scripts/verify.py`通过，625项Rust测试通过、4项可选live跳过，格式／check／Clippy／doctest（0项）／release／原生及Python夹具通过，日志为模型器`outputs/batch-review-tui-full-check-20261014.log`。`dsh-plugin`的19项测试及类型检查通过，日志`outputs/batch-review-plugin-check-20261014.log`；只读医院续接独立复验见`outputs/hospital-current-remaining-independent-20261014.log`。


## 真实29项提问与统一回答（2026-10-14）

独立测试profile`zf-tui-modeling-batch`已链接本工作树新版`dsh-plugin`，原`zf-tui`仍保留原平台链接。真实`target/dsh-e2e-a698f67f`使用hi/gpt-6.1-sol，5项PASS、0失败：一个请求含q01–q29，详情逐页显示全部稳定ID，29项键盘选项／自由文本分别保存，最后一项后统一返回全部原ID及精确回答，退出／进程清理通过。首轮`e0e96c31`失败保留，原因是观察脚本只滚40行、只覆盖前12项；界面当时已正确显示1/29。修正PageDown覆盖范围后重跑，未改写失败。

复跑命令：

```powershell
python tests/fixtures/dsh_tui_check.py --binary target/release/native-agent-tui.exe --model hi/gpt-6.1-sol --profile zf-tui-modeling-batch --only question --question-case batch
python tests/fixtures/question_batch_audit.py --evidence target/dsh-e2e-a698f67f --negative-tests
```

独立审计实际请求、29组填写前／填写后终端截图和最终回复，valid=true；七个证据篡改反例全拒绝，模型器日志`outputs/batch-real-independent-20261014.log`。新增分派隔离用例通过，当前`scripts/verify.py --fixtures-only`通过，日志`outputs/batch-real-tui-fixtures-check-20261014.log`；此轮仅Python验收与文档变化，没有将fixtures-only冒充新的Rust全量检查。

可用以下命令在新版profile打开当前医院隔离任务，输入自然语言由平台Agent查询事实并提出逐项问题。命令不自动批准任何对象：

```powershell
.\target\release\native-agent-tui.exe --profile zf-tui-modeling-batch --model hi/gpt-6.1-sol --cwd D:/project/blender-agent-modeler --domain-context D:/project/blender-agent-modeler/outputs/hospital-floor-live-20261013-03/context.json
```

真实验收条目是合成交互输入，Retain／Defer／adjustNN为测试回答，不是医院实际处理选择或授权。医院265／29／193状态保持；实际项目的分类、安装资料、移动选择及人工视觉核对仍等待用户答复。


## 项目楼栋派生产物与图纸审查

`--domain-project` 复用普通 dsh 会话与 F6 领域扩展。逐层结构方案使用封存 CAD 重建证明，具体授权与应用仍走 Core；目录／目标选择不授予写入批准。

项目楼栋以 `project_building` 单独发布事实：稳定 build_id 绑定完整选择、每层当前提案／授权／操作和权威哈希及定位依据，快照由独立 owned worker 构建。`assemble_project_building` 仅首次预留并启动，重用 ID 只读取；`inspect_project_building` 可等待0–20秒，`validate_project_building` 核对快照、assembly、Blender／重建／GLB 身份和哈希。取消只终止本 owner 的活跃进程树，未知任务不重启。`inspect_project.recovery_index.building_index` 报告已保存任务与省略计数。

Core 把楼栋事实同具体楼层提案区分，逐 build_id 处理序号、输入身份和验证状态。切换目标或已知后续权威变更使旧楼栋非当前；一个任务的成功不能赋给另一个任务。F6 同时显示 `source_reconciliation`、`connection_check` 和人工复核，产物 built/valid 不代表来源处理、跨层连接或人工复核完成。

`audit_drawing` 对当前权威场景及绑定提取事实运行确定性检测，返回 `drawing_audit` 分页事实，保留候选总数、场景／提取／来源哈希及下一页。它只发现候选；语义裁决由Agent读取实际候选图后记录，受控修正由独立具体提案批准后应用，详见下节。楼栋 render/read-image 工具另行提供输入及 BLEND 哈希绑定的真实图像，观察仍由实际看到图像的 Agent 回复记录。


## 图纸候选、Agent 判断与受控结构修正

`audit_drawing` 只发现候选；`render_drawing_candidate` 与 `read_drawing_candidate_image` 返回绑定当前审查的局部 CAD PNG。`record_drawing_judgement` 在已有图片读取后记录 Agent 的 error/doubt/not_error、解释及可选 typed 操作，人工状态保持 not_run。Core 显示候选总数、已判断数、当前页与判断依据，不将 recorded 当 passed。判断和图片实际准入分开验证，真实医院首项看图证据见实施记录。

`prepare_drawing_correction` 将 error 中的具体操作、候选来源、局部范围、图片、读取尝试、配置与原 DXF 哈希和决策草稿封存成一份提案。专门检查仅允许绑定楼层的受支持平面结构变更；普通 component 路线仍保持保护。准备不修改权威或决策文件，批准仍绑定 proposal/revision/token/base/candidate；实际应用由同一 patch writer 写入场景和决策，再按原产物路线构建验证。审查后权威变化须读取新证据，源或图像被改动则原提案拒绝应用。原 DXF 不写入。完整医院候选判断、受控修正真实验收和楼栋未覆盖阶段继续按迁移核对记录推进。


### 已登记楼层激活与楼栋报告

项目楼层事实中的可选`project_scope.activation_revision`为正u64。登记根持久维护代次；切换已登记楼层时只增加一次，重复当前激活幂等，scope_id、context及proposal/grant绑定保持原身份。`activate_project_floor`不进入初始化；`inspect_project_floor`始终只读，旧登记不补写代次。Core把代次纳入作用域相等性，升级后拒绝倒退、缺失和畸形代次，阻止旧A激活的迟到事实进入新A。代次参与事实语义哈希，故重新激活可以产生新的序号并恢复该楼层观察。

同一`source-view/1`的view／scan／scene哈希允许来源组和分页ledger共存。低序号等价事实仅填补缺失形态，不替换已经存在的形态或降低共享水位；不同状态及未封存旧协议仍严格处理过期页。

`check_project_building_connections`与`read_project_building_report`只派生封存事实，不重建job或写authority。连接passed要求冻结来源proof、实际已验证声明覆盖及零问题；空声明／缺相邻连接／旧job缺proof保持attention。总报告绑定input及来源／图纸／连接统计，技术状态与整体状态分别核验，human_review保持not_run。同输入后续普通检查保留报告，来源、有效性或当前性变化后显示历史。报告能力不扩展单楼层scope到多楼层写入。


### 项目连接写入（相邻两层来源体块）

新`prepare_project_connection_change`以built/valid/current且input_binding精确相等的楼栋为基线，仅接受所选下层与相邻上层的来源楼梯／竖井plan。`ProjectConnections`拥有准备和输入封存，`ProjectScopes`仍拥有单一活动作用域及激活代次，`DomainChanges`仍是具体提案／授权／patch writer的唯一owner。原楼层不嵌入别层、不改变原context；新的连接authority复制已批准完整楼栋，仅下层elements来源编译增补，原墙／柱／上层／楼板／共享定义全部保护。

`project_scope.assembly_binding`含project-connection-scope/v1、父build_id、input_binding及upper_floor_key。Core仅接受与已知当前有效父楼栋及当前selection相等的绑定；授权时再次检查父楼栋有效／当前。普通楼层与连接scope共享activation_revision，迟到事实不能跨作用域混入。连接应用不会把原楼层派生楼栋误标非当前；新楼栋`inputs.connection_authority`独立绑定连接scope、提案／授权／操作／候选SHA及父楼栋输入，后续连接权威变化只使相应连接产物历史化。

`assembly-reference.json`以project-assembly-reference/v1封存父快照、输入／授权／操作、原CAD/config、原权威及原先缺失的解释／处置状态、图纸判断集合；新生成的引用同时封存父楼栋实际GLB／BLEND／验证文件。该证明及源依赖纳入具体prepare_token，在准备、批准、写入和派生构建前重新核对。几何可通过不等于已批准：非初始连接基线必须有匹配当前SHA的已确认原授权／应用操作，不能采用注入的合法连接几何。

`assemble_project_building`可选connection_scope_id只使用该已批准连接authority的锁内副本，创建新的稳定build_id，不覆盖旧job；同ID的连接scope不等或遗漏均拒绝。新的冻结连接来源清单、统计、独立build/rebuild/GLB、连接报告和实际PNG仍单独验收。`activate_project_connection_scope`仅切换已登记scope；`inspect_project_connection_scope`只读恢复当前scope，修订恢复选择最新原提案。恢复索引报告connection_scopes与incomplete未知目录；部分准备绝不重构或自动采用。

CAD初始化新增可选`slab_opening_source_ids`，仅从当前裁切内明确选择的观察线圈生成已有楼板孔洞，要求完整单一闭合环、与楼板边界合法且互不相交，保留来源ID。缺失、开口、超界、重叠和重复选择拒绝。该项不支持已批准楼板编辑，也不推断楼梯／竖井语义；空值从序列化排除，既有context／证明身份保持兼容。


真实TUI验证：`target/project-live-708a4d47`为新的CAD结构／孔洞基线，`target/project-connection-live-01f0fb5b`对同一原连接提案独立批准并应用16级楼梯，新派生楼栋三阶段各34个精确mesh，连接passed／来源complete／技术complete，整体attention及human_review=not_run。新会话只读恢复`target/project-connection-recovery-4009bb8d`通过，原提案／授权／操作／job及产物哈希不变。之前4c77ef5a的界面夹具同屏条件失败保留，未生成批准，续验没有重做prepare。
