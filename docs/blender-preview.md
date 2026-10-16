# Blender 预览会话接入

审批审阅图采用独立的系统看图入口，Core 读取领域封存事实中的 `image_path`、`image_sha256` 和具体提案绑定，核验文件后向 UI 投影只读可用路径。首用模板的 Blender 单构件／CAD／尺寸对照图与总览、局部图一同出现在本版审批页；缺失、篡改或版本不符的图片不能成为本版可打开证据。打开时 Core 再次校验，系统程序启动失败保留未打开状态；成功打开不等于人工复核通过。Blender 实时预览仍由下述独立 bridge 管理。

2026-10-16 在实际 Orca 1.4.227、Windows ConPTY 新建独立终端进行图形协议探测：开启控制台 VT 输入／输出，分别发送 DA1 `ESC[c`、Sixel XTSMGRAPHICS `ESC[?1;1;S`、Kitty 一像素 `a=q`、iTerm2 `OSC 1337;ReportCellSize`、XTVERSION `ESC[>q`，每项读取输入两秒。必须先用 `terminal create --focus` 打开终端、确认前台已挂载，再发送探测；后台输出后才切到前台会得到不同结果。前台 DA1 返回 `ESC[?61;4c`，Sixel 返回 `ESC[?1;0;4096S`，Kitty 返回 `ESC_Gi=424242;OK ESC\\`，iTerm2 返回 `ReportCellSize=16.000;8.000;1.000`，版本为 `xterm.js(6.1.0-beta.303)`。继续分别发送 Sixel、Kitty 直接 PNG、iTerm2 IIP 的 192×72 蓝框黑色细线测试图，实际窗口截图中三图均可见。后台对照只有 DA1 和 `xterm.js(6.0.0)` 回复，三图未呈现，不能把后台探测当作前台能力结论。复测脚本与回包保存在本地 `target/orca-graphics-probe.py`／`.json`，发送脚本为 `target/orca-graphics-display.py`，前台截图为 `target/orca-graphics-display-foreground.png`；这些是忽略的验收产物。

当前先交付外部看图，未引入 `ratatui-image`，未使用半块字符显示 CAD。前台三种协议实际可用；内嵌显示仍需专项处理 ConPTY 回包、窗口缩放后的尺寸／字体单元重查与图片重编码、切换后台后重新前台时强制重传，以及有限时探测失败时回到外部打开。协议探测不能与 TUI 输入读取器争用 stdin，也不能将超时自动降级为半块字符。后台和前台不同意味着仅启动时缓存一次检测不足以保证后续可见。

依赖评估使用 crates.io 实际下载的原始 manifest／源码：`ratatui-image 11.0.0` 依赖 `ratatui ^0.30.0`，`12.0.0-rc.2` 依赖 `^0.30.1`，均可与当前 `0.30.2` 在版本约束上统一，库最低 Rust 1.86 不高于本项目 1.89。此结论只涉及声明兼容，尚未构建集成。若后续接入，应关闭默认 `chafa-dyn` 和全格式 image 特性，仅启用必要 PNG／crossterm；11.0.0 在查询失败时默认选择 Halfblocks，必须由本项目显式改为外部打开。12.0.0-rc.2 已增加 ConPTY 缺字体回包处理与 Windows 限时读取，但它仍是候选版本，不因单项兼容就直接升级生产依赖。

来源：2026-10-12 Blender 实时预览 MVP 授权及模型器领域协议 v2。
基线：`4f932c749cde12507da7797471c6cffd8c88f50c`；在独立 Orca 工作树
`L1nding/blender-preview-tui` 实施，原 TUI 和建模工作树保持只读。

建模会话默认开启预览：`--domain-context FILE` 楼层上下文、`--domain-project PATH` 项目，以及启动选择页或 `/project` 选中的项目都适用；`--no-domain-preview` 关闭且对后续切换的项目同样生效。项目模式下，预览服务以项目身份显示当前激活楼层作用域里 Agent 最近写入的方案；换楼层时序号单调递增，作用域失效后保留原显示但不再是当前版本、不能提交意见。Core 持有外部 Python
`blender_agent.integrations.preview_service` 子进程，其连接文件路径显示于 F6 业务页。
配套模型器必须包含预览 publisher/service，并在独立 `.venv` 安装 `.[mcp]`。
用户在 Blender 安装本地 Extension ZIP、选择连接文件并连接；完整安装/协议见模型器
`docs/blender-live-preview.md`，实际闭环见 `docs/blender-live-preview-acceptance.md`。

0.2.0 配套采用 `blender-preview/v2`：Blender 的选择是只读观察，Core 展示绑定版本的
部件、实例、尺寸、来源和诊断；150 ms 去抖、乱序丢弃、EOF/新版清空，最多 200 部件。
用户在 TUI 直接输入意见时会附带当前未应用且未批准候选的选择上下文，控制消息保持原审批语义。
插件提供原生多行意见窗口，意见仍通过同一个 Core 提交原平台会话。

Core 向服务发送 typed activity（1 秒心跳、3 秒租约），使 GUI 在准备目标候选时显示蓝色轮廓
和 Agent 操作说明；不以自然语言回复猜测对象或进度。服务向 Core 直接发布封存 proposal facts，
解决平台工具详情缺失或裁剪时业务页没有当前方案的问题；这些事实不生成授权。
事实按 run/context/epoch/sequence 接纳，旧工具详情不能覆盖更近的封存方案。

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

验证：`scripts/verify.py --cargo-test` 包含 fmt、locked check/Clippy、544 项 Rust tests passed
（另 2 项可选真实测试 ignored）、doctest、release/examples，
journal/replay、终端输入/会话流、Windows 进程身份与 ownership；专项覆盖旧 epoch 不投影、
版本/上下文/已应用/超长反馈拒绝及 instance/part 映射消息。真实验收使用 dsh 的
hi/gpt-6.1-sol、实际 F6/Ctrl+Y 和 Blender 5.1.2，不用假模型替代。
