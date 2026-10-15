# dsh-tui参考计划完成审计

日期：2026-10-15。对象为`dsh-tui-reference-iteration-plan.md`原迭代0–4。
当前结论：**计划已完成**。旧独立入口、专用 UI 和 Rust Python 工作流 owner 已删除，当前源码的完整验证通过。下面逐项列出实现与证据；医院未决业务、人工复核及可选 `@session` 不被提升为已完成。

## 完成条件与证据

| 原要求 | 需要的证明 | 当前证据／结论 |
| --- | --- | --- |
| 0.1–0.2 合并两分支、消除ACP冲突 | 提交祖先及共享基线；无未解决冲突 | 实施记录的ad6b27e／43df037／f72f14c整合及两分支历史验证；当前保留并发未提交工作，不重置 |
| 0.3 fmt/check/Clippy/tests/release/examples/native/plugin | 实际命令终态，范围匹配当前变更 | 当前 `scripts/verify.py --modeler-repo D:/project/blender-agent-modeler` 完整通过：fmt/check/Clippy、655项Rust测试（另4可选跳过）、doctest、release/examples及全部native；模型器729项及lint/format、插件20项通过；日志见最新索引 |
| 0.4 忽略元配置与历史refactor目标 | .gitignore及refactor-goal状态 | 实施记录确认.claude／.qoder忽略、历史目标被当前路线取代；不得删除其他未提交资料 |
| 1.1 通用扩展接口、可见具体版本确认 | 视图注册、Core typed命令、TestBackend及真实TUI；领域路径覆盖旧入口后删除 | 注册／绘制／旧版本拒绝、项目目标／医院／图纸／楼栋／连接／定位／家具编辑的实际批准、构建、恢复及看图证据齐全；旧CLI/UI/owner和专用fixtures已删除，release实际拒绝旧参数；普通项目入口当前终端夹具通过 |
| 1.2 工具结构化状态 | Core事实优先，不从摘要推断状态；未知值不猜 | ACP／client/tool_details及样式反例测试通过；实际工具三档、执行状态／计时可见 |
| 2.1 固定滚动锚点 | 原失败复现，身份锚点及流式权威替换验收 | 历史红绿ConPTY及最新native锚点／Unicode／跨turn回归通过 |
| 2.2 活动指示 | 有文字、mono/NO_COLOR可读、空闲不绘制 | runtime／TestBackend／真实工具及领域长任务证据；未观察思考增量时保持未知 |
| 2.3 每步分阶段计时 | 宿主step/turn身份、阶段、冻结、重试不新造step | Core时钟／桥接20项／真实对话步骤；不把无思考的提供方说成Thinking已观察 |
| 2.4 todo/plan | 有界列表、完成隐藏、会话清除 | 真实对话47348cf8及确定性回归通过；子Agent列表和旧历史恢复不在本轮已证明范围 |
| 2.5 steering | 当前轮次绑定、投递确认、拒绝/未知不重发 | 真实47348cf8在工具运行中改变最终标记词；审批等待显示批准操作；纯文本范围明确 |
| 2.6 plan评审 | 宿主exit_plan_mode→现有请求面板→Approve/反馈 | 47348cf8真实请求面板批准及协议／取消回归；未引入第二模型循环 |
| 3.1 终端/diff/通用卡片和三档 | 结构化内容、大小界限、中文/emoji/组合字符/窄屏 | TestBackend＋最新native卡片＋47348cf8真实Ctrl+E三档通过 |
| 3.2–3.4 思考内嵌、块顺序、隐藏多步标题 | 类型／身份驱动排序、预览原位权威替换 | Core/ACP/conversation反例与native通过；真实流式回答46种增长长度，不冒称模型发过思考 |
| 3.5 路径／插件命令／compact | 宿主命令列表执行，路径安全，拒绝自发未跟踪轮次 | 47348cf8真实@Tab、/plan、/compact；可选@session未实施，非必需 |
| 3.6 搜索/工作区/不可恢复原因 | 当前与all列表、标题过滤、恢复边界 | 47348cf8真实切换／搜索／恢复本目录；其他目录必须相应启动，不能猜测目录或越权恢复 |
| 3.7 retry/compaction行内 | 宿主事实、失败代码有界，不自发重试 | c2c26f21真实Retrying1/5 TRANSPORT；47348cf8实际compact42项；宿主压缩事件脱正文元数据另存 |
| 4.1–4.2 常驻区与状态按键 | 元数据贴输入上方；执行/attention等不隐藏 | 最新native/TestBackend及真实工具/审批/断连界面通过 |
| 4.3 尾部增量排版 | 已定稿行复用及只重排尾部计数 | conversation缓存／尾部计数专项通过，非时间断言 |
| 4.4 先测量再决定快照优化 | 当前release满历史测量，不凭直觉改owner | 定位回退measurements.log两项通过：239KiB发布约63μs，32KiB重排约290μs；同机参考不作阈值，未改快照owner |
| 全局不变量 | Core唯一事实owner、typed命令、稳定IDs/generation、Gate证据释放、隐私和未知副作用不重放 | 所有领域/transport/gate回归及真实医院/初始化/楼栋断连/取消/恢复证据；原始失败记录保留 |

## 1.1删除旧入口的实际能力审计

1. **已达到**：项目目录、单层/范围/整栋选择及重复候选；真实医院结构具体批准／构建；487原始来源身份／姿态全量核对；16组assumed评估；三批208构件实际建成及279未决明示；图纸实际PNG、判断及作者图受控修正；实际楼栋产物／总报告；相邻两层来源楼梯写入及新楼栋／恢复；初始化中断、实际Blender断连unknown及受控暂停下取消。
2. **已达到——已应用家具编辑**：`prepare_component_edit` 支持完整选中组件的移动、形状／参数替换和整组件删除；模板与旧无模板部件均覆盖，复用原 writer 和具体版本授权，保留稳定part IDs、作者依赖、共享定义及未选中对象。来源、保护对象、未授权／旧版本、共享定义、失去支撑和删除后误增补反例通过。实际TUI编辑／批准／构建及新会话只读恢复和两图实际观察证据见下方；删除来源保持待确认，只有明确绑定原删除提案才能重新开启。
3. **已补齐——定位回退**：`inspect_project_floor_location`提供绑定当前目录/选择/CAD的完整逐图层线实体证据；`prepare_project_floor.wall_layer_choice`提出已见图层、理由及exact location_id。未配置范围保持原20段阈值，已配置楼层不可覆盖；图层进入原结构具体批准摘要，生成配置/定位证书与来源依赖封存，在授权及writer处核对。实际TUI的非默认24线段图层→具体批准→一次应用/真实Blender已执行；上游回复失败保留，新TUI对同原提案/授权/操作/构建只读恢复三项通过，17个mesh身份在built/rebuilt/GLB一致。详见下方证据，不重做初始化或构建。
4. **边界继续明确**：连接scope仅指定下层与相邻上层的来源体块，不能编辑原楼板/任意其他层；缺孔洞/电梯/净高/规范保持未覆盖。医院279未决与14doubt是业务事实，不因软件验证通过消失。流程保留未完成与业务已完成是两项不同结论。
5. **已达到——入口删除**：删除`CliCommand::Modeling`、四个旧参数、`src/modeling.rs`、`src/modeling/feedback*`、`src/ui/modeling.rs`及两个旧child fixtures。没有将其子进程／模型循环迁入调用方；普通dsh领域会话继续使用Core批准与唯一writer。README和领域契约改为现行入口，旧整合文档标为历史。release帮助只列领域入口，四个旧参数均返回unknown option／退出2；全量验证通过。

## 2026-10-15 会话续接复核

续接会话 `01a133e5-1fb8-7743-8412-50ee790611a1` 后，以当前文件及实际运行记录重新核验完成条件。Modeler 再次通过 Ruff、326 文件格式检查及729项测试；当前 Rust 的 fmt/check/Clippy、655项 nextest（另4项可选跳过）及 doctest 通过；native fixtures 再次全套通过，普通项目入口证据为 `target/domain-project-0b397f6b`。完整 release/examples 和插件20项验证沿用下方删除入口后的日志，相关源码未再修改。

只读独立核验记录为 `target/dsh-session-resume-audit.json`：家具编辑恢复52个、定位恢复31个、连接恢复85个既有文件的哈希均与各自恢复前一致。重新从实际 dsh 持久会话读取本次 request IDs 的两图回执，确认进入后续模型步骤3和5，核对PNG哈希；原观察器FAIL报告保持原值。重新读取实际编辑权威场景及Blender验证：具体提案／token／候选哈希一致，三阶段各11个相同mesh IDs，带孔床体积约1.35m³，原结构、共享定义及未选组件未改变。

该记录还核对15组实际运行报告的通过步骤、整合提交祖先和零合并冲突；release 对四个旧参数均返回unknown option／退出2，旧owner及专用fixtures确已不存在。本次没有重新批准、应用或构建业务产物，也没有提交或推送代码。医院279未决来源、14doubt及人工／规范not_run仍按原事实保留。

## 2026-10-15 家具编辑与删除收口

- 真实`target/component-edit-live-711a5c95`完成移动带孔床、参数替换和第二张床删除的具体批准、应用与实际Blender三阶段，各11个mesh IDs一致，带孔床体积1.35m³；第三张床、原结构和共享定义不变，来源2 built／1 pending_confirmation。后续旧诊断观察器失败，原report保留FAIL。
- 首个相同路径上下文`component-edit-live-6c0a0f69`的原业务已执行，Core观察器失败未重做业务。`component-edit-recovery-cc8bcd12`真实新TUI恢复原提案／grant／operation／build，并实际取得删除来源增补拒绝。该会话两张PNG实际准入后续模型步骤，Agent观察到两张床和矩形孔，并明确透视遮挡及未覆盖区域。原夹具误计历史回执留下FAIL；独立重读本次request IDs、实际dsh持久会话和两PNG哈希，全部52个既有文件（46个业务文件）哈希不变，记录`independent-image-verification.json`。原report未改写。
- 恢复观察器现在仅统计当前request IDs与当前build；4项观察器测试包含历史回执、其他build、reference-only拒绝。缓存目录排除改为真实`review-images`，防止派生缓存混入业务集合。最新专项重跑通过。
- 后续hi403／openai402失败记录保留，未重放批准、应用或构建。实际图像证据来自已成功的`058bf794-ca1e-449d-bd6b-3ef7cbfb7ae5`会话，不依赖模型配置声明或失败回复。人工／合规仍not_run。
- 所有原迭代编号的证明见上表；0.1–0.2的整合提交ad6b27e、43df037、4785e50当前仍是HEAD祖先，无未解决合并条目。其他分支此后新增的提交不改变本计划固定整合基线。

## 2026-10-15 最新证据索引

- `target/dsh-session-resume-audit.json`：续接后的20组只读独立核验，包括实际图像准入、产物身份、保护几何、恢复前文件哈希、真实运行报告、整合基线和release旧参数拒绝。
- `target/dsh-component-edit-modeler-final-check-rerun.log`、`target/dsh-component-edit-native-final-check-rerun.log`：本次729项模型器及全部native fixtures复跑通过；Rust本次命令结果为655通过、4可选跳过，fmt/check/Clippy/doctest通过。
- `target/dsh-entry-retirement-verify.log`：当前源码fmt/check/Clippy、655测试通过＋4可选跳过、doctest、release/examples和全部原生夹具通过，含普通项目入口`target/domain-project-4d96b193`。
- `target/dsh-entry-retirement-modeler-tests.log`：当前模型器729项通过；Ruff及326文件格式检查通过。
- `target/dsh-entry-retirement-plugin-check.log`：插件20项通过。
- `target/component-edit-recovery-cc8bcd12/independent-image-verification.json`：本次两图准入、后续模型步骤、PNG哈希和52个原文件哈希独立核验；同目录`failed-04-conversation.txt`保留实际观察回复。
- `target/component-edit-live-711a5c95/independent-check.json`：实际编辑产物三阶段身份、体积与保护证据；原FAIL报告继续保留。
- 原release快照／排版测量在`target/dsh-project-location-measurements.log`，测量后未改快照owner，相关计数回归纳入当前全量测试。下方定位及其他日志是阶段历史，不作为当前计数。

## 2026-10-14定位回退验收

- 新公开工具与Core/F6均已接通，结构批准仍走原`Ctrl+Y`与独立授权通道，没有新增模型循环或第二批准owner。恢复带图层选择的scope前先恢复对应定位事实。
- 模型器`test_project_domain.py`37项，新增15项覆盖完整46图层证据、未初始化、假设未允许、错误/重复/未见图层、原阈值拒绝、已配置覆盖拒绝、来源竞态、批准前与writer时篡改、同提案恢复。Rust新增5项含Core来源/选择/图层绑定、只读不授权、数量边界、Unicode理由及窄屏展示。
- `target/project-location-live-84b57cfa`：初始只读证据检查PASS；提案`873f4c0a8f654ec7a9510e0a8a7449e2`已经实际批准、应用，`project-location-live-84b57cfa-build`实际built。随后回复遇到`upstream_http2_stream_error`，夹具整体**FAIL**保留；不是提案/构建失败，也不通过重试该写入来掩盖。
- `target/project-location-recovery-9cd230a7`：新实际TUI严格只读恢复同一定位证据、图层选择、applied提案、原grant/operation/build，**3项PASS**。所有business哈希和身份集合不变；原来源依赖仍匹配，实际built/rebuilt/glb_roundtrip均为17个相同mesh IDs。证据为`independent-check.json`、`before-hashes.json`、`bound-layer-choice.txt`、`applied-proposal.txt`、`built-artifact.txt`、`validated-artifact.txt`及两图哈希的`core-screen-admission.json`。两项产物图为Core业务面板的绑定built/current=true和valid=true/current=true行，不以Agent正文作为投影证据。初次恢复`d3450755`仍保留，后续`9cd230a7`补齐严格Core行捕获；观察器新增4项回归，拒绝正文伪造、其他build的成功及缺失面板。人工/合规仍not_run。
- 第一次全检查的测试阶段663通过，但release重链接撞上运行中的Windows TUI文件锁，日志`dsh-project-location-rust-native-check.log`保留失败。退出该TUI后release/examples成功；最后小修Unicode理由预算后的当前Rust664项及native复验成功。没有终止或重放领域业务以规避锁。
- 本项闭环不替代已应用家具编辑的缺口，也不满足删除旧入口的全部条件。goal保持active，未提交Git变更。

## 2026-10-14 阶段证据索引

- 当前Rust/Native：`target/dsh-project-location-rust-native-final-check.log`（664通过，4可选跳过；fmt/check/Clippy/doctest及全部native成功）。
- 当前新增4项观察器反例与native复验：`target/dsh-project-location-native-fixtures-final-check.log`。
- 当前release/examples：`target/dsh-project-location-release-check.log`成功。
- 当前模型器：`target/dsh-project-location-modeler-final-check.log`（691通过，Ruff与323文件格式检查成功）。
- 当前release性能测量：`target/dsh-project-location-measurements.log`（2项通过；239KiB快照约63.015μs，2/8/32KiB排版约50.257/77.538/290.473μs，当前并行测试环境的同机参考，不作时间断言或owner优化依据）。
- 当前插件：`target/dsh-project-location-plugin-check.log`（20通过）。
- 定位回退实际执行/失败保留及无重放恢复：`target/project-location-live-84b57cfa`、`target/project-location-recovery-9cd230a7`。
- 以下连接工作日志为前一阶段基线，非上述最新计数：

- Rust：`target/dsh-project-connections-rust-full-check.log`（659通过，4可选跳过）。
- 模型器：`target/dsh-project-connections-modeler-verified.log`（676及lint/format通过）。
- Native/release examples：`target/dsh-project-connections-native-check.log`全套通过。
- 性能：`target/dsh-project-connections-measurements.log`两项通过。
- 插件：`target/dsh-reference-plugin-final-check.log`20项通过。
- 实际交互：`target/dsh-conversation-live-47348cf8`14项通过。
- 医院最新批次/独立最终DXF：`target/hospital-component-batch-28f9dbb0`及`target/dsh-hospital-final-inventory-retry.log`。
- 医院当前图纸：`target/drawing-review-live-2206ec5e`16项记录（14doubt／2not_error），不是人工通过。
- 新跨层基线/批准执行/恢复：`target/project-live-708a4d47`、`target/project-connection-live-01f0fb5b`、`target/project-connection-recovery-4009bb8d`。
- 首次连接4c77ef5a的界面夹具失败保留，未授权；续验只读恢复同原proposal，没有重新prepare。
