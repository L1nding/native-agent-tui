# 独立建模入口迁移核对

更新：2026-10-15。对应 [参考迭代计划](dsh-tui-reference-iteration-plan.md) 1.1 的结果项。楼层定位回退与已应用家具编辑已补齐，旧 CLI、专用 UI 和 Rust Python 工作流 owner 已移除；当前全量验证见[完成审计](dsh-tui-completion-audit.md)。下文按日期保留能力调查及历史缺口，最新状态以下面的收口记录为准。

## 要替代的行为

用户目前可以直接输入楼层、楼层范围或整栋目标，也可以修改一个具体构件，或恢复最终复核。新入口需要让普通 dsh 会话推进这些工作，领域工具提供事实与受控动作，Core 继续拥有批准和执行状态。

“流程跑通”的证据需要涵盖相应目标从选择到产物及复核的路径。对于缺资料、未批准或检查失败，流程必须保留明确的未完成、attention 或失败结果；不能把这些结果写成通过。某次局部构件成功不证明楼层或楼栋路径已迁移。医院业务判断与人工视觉确认也不能由 TUI 的测试结果替代。

## 当前代码证据

| 能力 | 旧入口的实际路径 | 领域会话的实际路径 | 删除入口前尚需完成 |
| --- | --- | --- | --- |
| 楼层、范围、整栋目标 | `src/modeling.rs::Operation::Model` 发 `workflow-stdio start`；模型器 `orchestrator/run.py::run` 读取项目、建目录、调用 `cad/floor_catalog.py::resolve_target`，重复候选由 `_pick_duplicates` 询问 | `--domain-project` 在普通 dsh 入口创建 `modeling-project/v1`；`project_mcp` 提供目录、目标解析与重复候选证据，`prepare_project_floor` 进入已选楼层的隔离领域上下文，Core 核验选择与作用域身份 | 目标入口和结构候选已经接通；继续完成完整楼层／楼栋产物、复核恢复和异常路径验收 |
| 图纸到楼层结构 | 模型器 `run_floor` 先提取、重建结构，再运行建筑语义分析；检查点含 CAD 哈希、楼层配置与场景 | `prepare_project_floor` 调用确定性 CAD 重建，保存隔离空权威场景、结构候选、来源依赖和具体提案；仅 TUI/Core 的当前版本授权允许应用。每层保持独立 run/context/proposal/operation/build 身份 | 医院2,220项结构候选的原授权恢复、应用及真实独立构建已通过；继续整层来源业务、后续修订和未决项。Python 不执行模型循环 |
| 构件识别、方案、修订、批准、应用 | `Operation::Component` 发 `start_change`；旧整层流程调用 `furnish_each` 与 `_ask_task` | `scan_source_instances/groups`、interpret、prepare、inspect、preview、Core 独立授权、apply；真实演示床及医院部分批次已有闭环 | 项目来源配置已接通；默认323观察对象不等于医院四图层487构件；新上下文487条已独立核对且16组均已评估，F6只读恢复通过。三批已建208实例，22碰撞待新增／195待确认／48缺能力／14疑似重复仍未决，详见下方逐批证据，保留具体版本、来源、授权、操作绑定 |
| 图纸问题与受控修正 | `run_floor` 调 drawing audit，修正经 `_ask_task`，然后继续构建与复核 | `audit_drawing` 调用五类确定性检测器；候选局部图、实际 PNG 读取和 error/doubt/not_error 判断独立绑定当前权威与来源。prepare_drawing_correction 封存具体操作与决策草稿，经 Core 具体批准才应用；F6 显示已判断数与未覆盖页 | 医院16项候选实际看图及判断记录通过（6 error、9 doubt、1 not_error）；作者图上的独立批准／决策写入／实际构建修正闭环通过。旧默认范围的error/doubt保留历史；新487来源上下文16项实际PNG判断已另行验收（14 doubt／2 not_error）；医院问题收尾和279未决来源仍未完成 |
| 楼栋合并 | 多楼层完成后 `assemble_run` 合并，报告未决项 | `assemble_project_building` 封存逐层具体提案／授权／操作、Scene 哈希、同名轴网对齐和绝对 CAD 标高；独立 worker 构建楼栋，Core 按 build_id 投影验证／当前性，楼栋图片绑定输入与 BLEND 哈希 | 已有真实两层派生楼栋及看图证据；连接检查／封存总报告和真实TUI只读验收已接通。旧job缺连接证明继续attention；相邻两层来源楼梯／竖井独立连接作用域的提案／写入／新楼栋及恢复已验收；已应用家具编辑等旧入口差异仍须核对，医院未决业务不冒充完成 |
| 构建与独立验证 | Blender 构建、重复构建、GLB 回读，报告产物 | `preview_candidate`、`build_scene`、`inspect_artifact`、`validate_artifact` 已有 | 使用对应楼层／楼栋完整目标的真实产物验证，不能复用别的提案或旧预览作为证据 |
| 最终复核及重试 | `Operation::RetryReview` 发 `retry_review`，旧界面支持 F5 和“复核上次” | `render_artifact_review`、`read_artifact_review_image` 可供同一 dsh Agent 复核，持久索引可对账 | 普通会话验证目标产物复核、失败后重试和恢复；报告 AI 实际看见的内容及人工 not_run，保持图像与 build/hash 绑定 |
| 取消、断连与恢复 | Rust owner 管 Python 进程树，旧报告及 source run 支持复核恢复 | 楼层 domain jobs 和楼栋 jobs 都有 cancel／inspect；楼栋保留稳定 ID、独立 worker 及恢复索引，缺 owner 的非终态为 unknown；Core generation 与事实读取 owner 沿用原边界 | 楼栋实际中途断连＋新TUI unknown恢复、受控真实Blender暂停下cancelled、医院实际CAD初始化中断＋部分目录保护均已通过，详见下方证据；继续医院剩余业务及新跨层写入路径 |

模型器路径均指本机 `D:/project/blender-agent-modeler/src/blender_agent/`，本表起点是 2026-10-14 的只读核对，当前已包含随后在两个仓库的项目入口及结构候选实现；模型器其他未提交工作保持原有范围。既有真实验收见模型器 `docs/dsh-domain-acceptance.md` 和 `docs/modeling-complex-current-audit.md`；它们证明局部与跨层能力，也明确记录医院未决项目。

## 接续实现与验收顺序

1. 先补领域侧的项目目录及目标准备 seam。目标解析返回候选及来源证据；重复目录和未知目标必须显式处理。上下文选择不授予场景写入批准。
2. 让 dsh 以纯领域能力推进结构、来源解释、修订、具体授权、应用、构建及复核。Core 不新增业务模型循环，UI 仍通过扩展快照展示。
3. 为新入口增加楼层、范围／整栋、复核恢复的公共 seam 测试；覆盖重复候选、旧 generation、拒绝批准、断连、取消与未确认操作。
4. 用隔离项目完成真实单层和多层目标的全过程。逐项核对来源、保护对象、提案版本、具体授权、操作、产物及复核证据；医院目标保留真实未决项。
5. 证据满足原计划后，删除 CLI、独立 UI、Rust Python workflow owner 和相应旧入口文档，保留新路径的回归，运行仓库全量验证。


## 2026-10-14 会话接续与选择绑定

接续会话 `01a133e5-1fb8-7743-8412-50ee790611a1`，保留用户授权的测试分工：编码 Agent 通过实际 TUI 代做隔离项目的选择与具体版本批准，工作流 Agent 执行所有领域工具。观察脚本只读取持久事实，不替代它准备、授权、应用或构建。

项目作用域绑定原 selection_id、source_sha256 与 floor_key。重新选择目标后，旧作用域不能被当成新选择复用或授权；模型器返回 `project_scope_selection_changed`。恢复完全相同的原选择可以读取原提案，不能隐式重做初始化。回归先复现了旧实现未拒绝目标变化，再验证准备和授权均拒绝、原记录不变和原选择可恢复。

真实两层及新会话复核恢复的验收夹具分别为 `tests/fixtures/domain_project_live_check.py` 和 `tests/fixtures/domain_project_recovery_check.py`。前者核对当前 release、在 F6 滚动到具体方案后批准、读取正确的持久 `build.json`；工具错误后要求工作流 Agent 重新核对方案，事实同步恢复前不批准。后者绑定既有隔离运行，仅允许恢复与图像读取，比较权威场景、提案、授权、操作、构建和来源哈希及身份集合，排除写入重放。最终运行结果记录在实施记录中。


## 图像复核验收边界

`target/project-live-aeb56d9d` 证明两层具体批准、应用、Blender 重建／GLB 回读与来源不变，但其原夹具的图像阶段判定过宽，不能作为工作流 Agent 完成视觉复核的证据。读取该 dsh 会话的真实 `tool/result` 后确认，`hi/gpt-6.1-sol` 未声明 image input，图片被替换为 `image unavailable`。模型器的 `attached_png` 仅证明 MCP 返回 PNG；不能证明模型已看到它。

桥接层原先只接受单一内容块，混合图片／诊断旁的小回执不能作为 structured_output 传给 Core。现在仅提取一个完整 `modeling-fact-receipt/v1`；多个 JSON 候选、显式错误、超限主结果仍拒绝。回归已先在实际失败模式上变红，再通过修复。

新夹具还调用 `domain_project_images.py`，按本次 journal 的唯一 thread_id 读取对应 dsh 持久会话，只导出回执哈希、图像附件元数据与序号，验证该回执对应的非 offloaded 图片之后同轮出现下一步模型回复。旧的图片未准入记录会失败。隔离 profile 的图像声明和运行结果需要同时验收；声明本身不证明图像已处理。AI 结论、实际图像准入与真实项目人工复核继续分别记录。


当前验收结果：真实两层＋实际看图闭环 `target/project-live-8b76908c`；新会话只读恢复 `target/project-recovery-e5607402`；最终全量 TUI 检查 `target/dsh-project-entry-final-verify.log`。工作流 Agent 的实际图像观察和能力边界分别保存在两次运行的 `agent-observations.txt`，不把有限示意结构看图扩大为医院整层或楼栋全流程。下一步仍是图纸审查／修正、医院来源收尾和楼栋合并产物迁移，再审计独立入口删除条件。


## 2026-10-14 楼栋任务与结构证明

楼栋工具拆为 `assemble_project_building`、`inspect_project_building`、`validate_project_building`、`cancel_project_building` 和楼栋 render/read-image 工具。预留时持有项目及各楼层锁，核对当前场景与已完成具体操作、批准绑定以及 source dependencies 后封存快照。使用绑定楼层配置内的轴网和标高读取定位，未降低通用目录推导的墙线阈值，也不从整张图的相邻平面借标高。

稳定 build_id 只分配一次。重复调用只查询，包括 failed/cancelled 和丢失 owner 的 unknown；重启后索引报告已保存的 job，最多32项，标记 total/truncated。独立 worker 在快照上构建；楼栋检查实际 mesh 身份、重建和 GLB 回读及持久哈希，原楼层和源 DXF 不变。`valid` 是产物完整性，`current` 是选择／来源／权威当前性；来源未全量解决或跨层连接未检查仍单独记录 `attention` / `not_run`。

确定性结构初始化新增 `cad-structure/v1` 证明：完整重建场景封存为 source dependency，每个操作必须与原场景的对应实体、作用域、assumed 依据和 source IDs 完全相等，基线必须是单一空楼层。普通柱、墙体参考及原重建门窗不再冒充模型提出的跨层元素；只在完整结构证明通过时使用这一路线，准备和应用均重新核对。没有证明、篡改操作或来源改变均阻止执行；后续模型构件／楼梯仍执行原来源和关系检查。旧提案没有新字段时保持原 token 序列化。

真实同步原型证据 `target/project-live-48ca9366` / `7809643a` 只证明早期合并，不能代替现在的任务恢复边界。异步楼栋＋Core＋图像＋审查验收见 `target/project-live-c774aa2a`：两个具体批准的楼层独立构建后，产生楼栋 Scene、assembly、GLB 并完成实际楼栋 PNG 读取；F6 读取独立 `project_building`，显示连接检查 `not_run`。新结构证明下带柱的闭环与新会话楼栋恢复结果继续记录于实施记录。


## 2026-10-14 来源恢复、报告及楼层激活续验

配置医院487实例／16组的结构闭环和独立DXF核对见`target/hospital-project-entry-8df28336`；prepare超时经同会话inspect_project_floor恢复。原16组评估已经持久化但UI验收失败（`84e48cf2`），同一封存来源状态的组／实例投影修复后，只读恢复`target/source-assessment-live-c587d0a2`通过。230待新增、195待确认、48缺能力、14疑似重复均非建成；旧323观察对象上下文的图纸判断不能移植。

连接／总报告真实TUI读取`target/building-report-live-d29b8f02`通过，前次`00db1f88`的上游HTTP/2失败保留。冻结来源／连接证据、未决来源／图纸数量和当前性已核验；总报告attention不冒充人工验收。项目单层作用域的新跨层proposal／writer路径尚缺，不能据此删除旧入口。

activate_project_floor新增为已登记楼层切换入口，持久activation_revision区别A→B→A的三次激活。旧scope／提案／授权稳定，新的事实序号不复用旧激活；Core拒绝迟到的旧激活事实。inspect_project_floor保持只读，prepare只在新楼层进入初始化。专项先红后绿，真实TUI A→B→A验收`target/project-activation-live-e6c7e424`通过：代次1／2／3、scope事实序号30／33／35，各次显示原身份且全部业务哈希不变。前次夹具未滚至scope行的失败`dd2bca84`保留。


医院新487来源上下文的首批76床实例闭环`target/hospital-component-batch-e027ed5c`通过，95实例原方案的19碰撞保留未决；精确批准后新增877solids/1primitive/2templates，原几何不变。实际三阶段各3096mesh与公开build plan精确相等，两视图PNG真实准入。剩余154待新增／195待确认／48缺能力／14疑似重复；该局部批次不代表医院整层完成。

楼栋真实Blender中途断连＋新TUI只读恢复`target/building-interrupt-live-2d37e25e`通过，unknown/valid=false及无重放已验收。主动取消样本`8fefb9a1`实际抢先built，不能计取消通过；初始化实际中断继续验收。失败样本969f06ce（夹具漏Disconnected等待）及c2c26f21（早期项目缺定位证明）分别保留。


主动楼栋取消在`target/building-cancel-live-55ae7d75`受控真实Blender暂停条件下通过，持久cancelled、owner_tree_confirmed=true与实际进程退出均核对，原业务哈希不变。暂停绑定本测试context/build/output，位于原未修改生产导出脚本之前；无暂停小样本抢先完成的8fefb9a1继续保留为完成竞态失败。


医院实际初始化中断`target/project-init-interrupt-live-858140c1`通过：在config／enclosure／assemblies已写、scope尚未登记时中断实际dsh及MCP；新TUI恢复unknown，prepare明确拒绝重建，部分文件与原输入哈希不变，无自动采用或应用。前次812cf35d夹具未处理venv双Python的失败保留；Windows进程创建身份退出已独立核验。


随后100椅实例批次`target/hospital-component-batch-15290c96`与独立观察器通过，实际三阶段各3784mesh、两个视图PNG准入及原几何不变已核对。合计built176，尚余54待新增／195待确认／48缺能力／14疑似重复。每批分别取得当前具体授权，旧批次产物不充当新候选证据。


再以`target/hospital-component-batch-28f9dbb0`完成27椅＋5柱式盆，具体第2版批准、实际三阶段各3961mesh、原几何保留和两PNG准入均独立核对。医院累计built208，待新增22（19床＋3柱式盆碰撞）、待确认195、缺能力48、疑似重复14。最终raw-DXF核对在`target/dsh-hospital-final-inventory-retry.log`通过，487完整来源／15页当前ledger身份与姿态无遗漏或差异，状态复算相等。首次把初始化观察器用于混合状态的失败单独保留，未放松原初始化检查。新上下文当前图纸判断继续实际验收，不转用旧上下文观察。


当前487来源权威的16项图纸复核`target/drawing-review-live-2206ec5e`通过全部实际图片准入、F6 recorded16/16及业务哈希不变。14doubt／2not_error，未提出修正；旧上下文的6error／9doubt／1not_error保持独立历史绑定，不作跨上下文“已解决”。项目跨层写入和未决医院业务尚缺，旧入口仍不能删除。


## 2026-10-14 项目连接写入闭环与新发现的删除前差异

相邻两层连接写入已由`ProjectConnections`独立owner迁移，复用DomainChanges而非新增patch writer或模型循环。`project-live-708a4d47`真实结构及CAD孔洞基线通过；`project-connection-live-01f0fb5b`恢复首轮未批准的原16级楼梯提案后独立Ctrl+Y，新增连接authority、单份授权／应用、新楼栋三阶段各34mesh及两个PNG实际准入通过。连接passed／来源complete／技术complete，但总报告attention与人工not_run不提升。原楼层、上层、楼板、旧楼栋与旧授权文件全部哈希不变。新TUI`project-connection-recovery-4009bb8d`只读恢复同scope／提案／grant／operation／build/report及原两张PNG通过，没有prepare、授权、应用、构建或新业务身份。首轮4c77ef5a因夹具错误同屏要求而失败，原记录保留。

当前仍不能删除独立入口，不能把“跨层已补齐”替代全量能力审计：

- **已应用家具编辑**：旧`integrations/change_stdio.py::serve_changes`→`orchestrator/change_planner.py::propose_change`让模型返回PatchDraft，允许绑定楼层的现有家具solids set/remove及新增数据定义，并经DomainChanges具体批准。新`ComponentPlan`只编译增补和未应用候选修订，`describe_capabilities.unsupported`明确edit-existing-component。需要新增可追溯的已应用组件编辑seam及保护／来源／具体版本／实际TUI验收，不能简单开放任意patch或改标签绕过。
- **未配置图纸的定位回退已验收**：旧`run.py::locate`的逐图层证据/Agent选择/用户确认已迁移到`inspect_project_floor_location`与`prepare_project_floor.wall_layer_choice`。来源和配置依赖封存，选择进入同原结构具体批准；已配置图层不能覆盖、阈值不降低。实际TUI完成一次批准/应用/Blender，回复上游失败后新TUI恢复同原记录，不重放。最新证据与失败边界见[完成审计](dsh-tui-completion-audit.md#2026-10-14定位回退验收)；这一项不再是删除入口的能力缺口。
- 新连接作用域明确只支持一个指定下层与其相邻上层的来源体块，不提供任意跨层编辑／切孔／电梯／合规。多楼层任务保留未覆盖连接attention，不默认为连接完成。
- 医院当前279未决来源与14doubt仍保留真实问题；流程能如实报告未完成不等于几何／人工完成。全流程替换证据与实际业务完成分别审计。

前一连接阶段Rust659项（4可选跳过）、模型器676项、native全套通过，日志分别dsh-project-connections-rust-full-check.log、dsh-project-connections-modeler-verified.log、dsh-project-connections-native-check.log。最新定位回退阶段的Rust664、模型器691、插件20及native/release证据见完成审计。本记录不是入口删除批准；仍保留--modeling。


### 2026-10-14 已应用编辑的调查起点

已重新读取`reconcile_sources`、`compile_plan`与`DomainTools._require_active_sources`：现有组件具有多个稳定part ID及共享template/primitive；单一additive编译不能覆盖已有组件。删除全部parts后若无有效disposition，来源会回到`pending_addition`，所以编辑seam须封存完整选中组件、精确来源版本和用户移动/参数/删除意图；删除不能暗中标built/skipped，必须保留显式待确认/重新开启约束，避免被后续增补自动重建。来源disposition已是prepare/apply输入约束，不能为编辑绕过。新路线仍应复用现有writer及具体版本批准；本记录仅是代码证据和下轮起点，不是编辑能力已完成。

## 2026-10-15 已应用编辑与入口收口

`prepare_component_edit` 现在支持完整选中家具的移动、形状／参数替换及整组件删除，复用原 `DomainChanges` writer 和 Core 具体版本授权。完整模板替换与旧无模板部件的类型化几何编辑均覆盖；保留稳定 part IDs、作者依赖与非编译器元数据，旧共享 primitive/template 和未选中对象保持原值。来源变化、未授权、旧版本、保护范围、失去支撑的未选中夹具和非法混合编辑均由专项反例拒绝。已批准删除从授权／操作链派生来源 `pending_confirmation`；明确引用原删除提案重新开启后才可增补，不能被扫描自动重建。

实际 TUI `target/component-edit-live-711a5c95` 已对移动带孔床、参数替换和第二张床删除取得一次具体批准，完成应用及实际 Blender 构建／独立重建／GLB 回读，各阶段 11 个 mesh IDs 一致，带孔床体积 1.35m³。第三张床、原几何、共享定义及来源输入保持不变，来源为 2 built／1 pending_confirmation。该夹具随后因旧诊断观察方式失败，报告保留 FAIL。

同路径的首个已执行上下文 `target/component-edit-live-6c0a0f69` 由于 Core 面板观察器裁剪错误留下 FAIL；未重做该业务。新 TUI `target/component-edit-recovery-cc8bcd12` 已恢复其原提案／授权／操作／产物，并实际观察删除来源增补被拒绝。两张绑定原构建的 PNG 在该恢复的 dsh 会话 `058bf794-ca1e-449d-bd6b-3ef7cbfb7ae5` 中进入后续模型步骤，Agent 实际观察到两张床、矩形孔及透视遮挡范围，人工／合规仍 not_run。原夹具最终误计历史图像回执而 FAIL；2026-10-15 独立重读同一持久会话与本次 request IDs，核对两图哈希及全部 52 个既有文件哈希不变，结果写入 `independent-image-verification.json`。未改写原 report，也未重新批准／应用／构建。恢复夹具已按当前 request IDs 过滤回执并增加历史／其他 build／reference-only 反例。

后续 `93667a9b` 的 hi 403 余额不足及 `57aefb5e` 的 openai 402 配额耗尽均保留；它们不取代上述已发生的图像准入和观察证据。旧 `--modeling`、`--modeling-project`、`--target`、`--change-context` 现在明确报未知参数，专用 `src/modeling*`、`src/ui/modeling.rs` 及只服务旧 owner 的两个 child fixtures 已删除。建模通过普通 dsh 模型、会话、权限与领域工具运行。

医院 279 未决来源、当前 14 doubt 及人工／规范未检查仍是业务结果；新路线能如实报告这些结果。软件迁移完成不将其改写为几何或人工通过。
