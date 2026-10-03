# Windows 启动可靠性诊断

## 当前结论

Windows Core 已接入独立连接的 shell 预检：主线程完成版本门禁后，辅助
app-server 只做版本查询、初始化和 shell 检查，不创建线程或模型轮次。
结果通过且辅助进程树清理确认后才进入 Ready。最终源码的五次独立启动
及完整真实验证全部通过；这不能证明其他 Windows 环境或 Alpha 发布
门禁已通过。

原路径存在间歇性预检超时。请求已经写入并 flush；失败样本中 PowerShell 曾启动并退出，最终 RPC
响应却没有返回。不能把超时归因于本地写队列，也不能把进程消失当作
已确认的 shell 执行结果。

因果探针支持 Windows 沙箱输出管道被并发启动的 MCP 进程继承这一解释：
四次延迟启动中，所观察的 MCP 子进程有六个可继承管道写句柄，通常成功
样本为四个。五秒后只结束本轮创建、已持有句柄的该进程，预检均在约
0.2 秒内返回成功。该干预仅在独立诊断副本中进行，没有模型任务。
其他超时样本的早期句柄计数仍为四；尚未确认所有泄漏持有者或精确窗口。

固定上游源码 `rust-v0.159.2` 的 commit 为
`ff6aec96948b70d94983af2641a6b67c94faeff5`。其中
`codex-rs/windows-sandbox-rs/src/lib.rs` 的
`run_windows_sandbox_capture_with_filesystem_overrides` 将子进程 stdio
设置为可继承，等待子进程后无界等待 stdout/stderr reader 线程。
`timeoutMs` 不覆盖这段 reader join。源码解释与干预结果一致；本轮没有
编译或修改安装的 Codex，不能将此记录写成上游补丁已验证。

## 对照条件与结果

诊断源码来自本项目固定提交 `6b7aebb`。每份副本有自己的 Cargo target；
执行目录保持原工作区，使用 read-only、显式 unelevated 沙箱、零初始
模型任务。配对试验串行执行，交替先后顺序；不同试验的计数不合并。

| 试验 | 正常顺序 | 诊断条件 | 结论与限制 |
| --- | --- | --- | --- |
| 交换顺序 | 9/10 | 先预检 10/10 | 会让 shell 早于新线程版本校验，不能直接采用 |
| 关闭配置的 MCP | 13/15 | 15/15 | 支持并发初始化方向；关闭工具会改变功能 |
| 线程范围的 MCP inventory 查询 | 19/20 | 20/20 | 上游实现可能额外建立连接，不能作为可靠初始化屏障 |
| 早期句柄观察 | 11/15 | 四次原始超时 | 句柄计数受取样时点影响 |
| 延迟后结束所持有的 MCP 子进程 | 16 次自然通过 | 四次干预后通过 | 是因果诊断证据，不是 20 次无干预启动通过 |
| 独立 app-server 预检 | 18/20 | 隔离预检 20/20 | 主线程版本门禁仍在 shell 之前；未干预主进程的 MCP |

源码副本、临时探针和原始逐次日志位于被忽略的 `target/startup-*`；输出
只保留固定方法类别、状态、字节数和进程类别，不输出 MCP 名称、管道
名称、命令行、配置内容或凭据。此前使用共享 Cargo target 的统计继续
排除。早期配置格式错误的 MCP 探针也未纳入上述计数。

## 可重复的真实启动检查

```text
python scripts/check_startup.py --trials 5
python scripts/check_startup.py --cargo-test --trials 1
```

需要原生 Windows 和兼容的本地 Codex 安装。每次运行完整的
initialize → thread/start → command/exec → Ready → 关闭流程，使用独立
临时 journal，不提交任务。保留正常线程版本门禁和 30 秒 RPC deadline。
这比跳过线程创建的 `--check-shell` 更接近实际 TUI 启动。

脚本只输出经字段、类型和枚举校验的 JSONL 摘要。通过必须同时满足
Ready、零根轮次、零根请求、已确认进程清理及 journal 关闭；缺失摘要、
空测试选择、非零 runner 退出或不确定清理都失败。nextest 强制零重试，
第一次失败立即停止剩余试验；`--trials` 是用户明确选择的独立启动次数。
编译时间计入首个试验的耗时。摘要不可用时，单独运行 Rust 测试查看
本地构建或握手错误：

```text
cargo nextest run --locked --run-ignored only --retries 0 --no-capture -- --exact client::tests::live_windows_core_reaches_ready_without_a_model_turn
```

## 接入前的验证记录

`--trials 5` 第一项通过，第二项约 31.7 秒后因预检超时进入 Unknown；
脚本返回 1，并停在第二项。两项均确认零根轮次、零根请求、进程清理和
journal 关闭。此结果证明检查可以捕获问题，不表示启动已修复。

八项 Python runner 夹具验证失败停止、禁止重试、Cargo 精确选择、摘要
缺失/非法、零请求与清理边界、私有 runner 文本不会输出。它们不调用
Codex，也不能替代真实启动验收。`python scripts/verify.py` 通过 134 项
默认 Rust 测试、Python runner 测试、九个兼容场景、11 个 Windows
所有权场景及历史/回放/观察/JSONL 夹具；fmt/check/clippy/doctest/release
同样通过。随后补充 Cargo 的输出前缀识别，八项 runner 测试通过；
精确选择的 nextest 与 Cargo 真实启动专项各一次通过。本轮未重新运行完整
`--live`；上一轮的最新完整结果仍为
五项真实检查四项通过、Ready 超时失败。

## 隔离预检的生产边界

正常 Windows 启动保留主 app-server、MCP 和新线程。辅助进程使用相同
cwd、catalog 副本和显式 Windows sandbox 配置；版本与 initialize 响应
分别校验。catalog 由两端共享持有，最后一个所有者结束时才删除副本。
`--check-shell` 不创建线程，继续使用单连接；其他平台路径保持原来的
预检方式，尚未获得发布验收。

`src/shell_check.rs` 持有一次检查的取消信号和 join handle，只向 Core
交付有类型的结果与清理确认。两条连接的 RPC ID 独立，主连接不能替代
辅助响应。辅助端不接受交互请求，通知数量有界；metadata、错误载荷和
原始 shell 输出不会写入结果或 journal。

30 秒 deadline 包含辅助启动、握手、检查和清理。成功即使已返回，清理
完成前也不能放行排队任务；超过 deadline 的结果保持 Unknown。取消后
必须完成已启动的版本查询清理，再停止辅助进程并 join；正常退出会等待
主、辅助两端。清理未获确认保存 `cleanupConfirmed=false`，响应缺失、
断连或无效响应保留 Unknown，不重试。版本、握手拒绝和已确认的 shell
检查失败保存启动失败。

诊断对照与生产验证单独计数。隔离对照使用独立源码副本和 Cargo target，
共 20 组交替顺序的串行试验：正常路径两次原始超时，隔离路径全部通过。
初次接入生产路径的十次启动均确认 Ready、零根请求、零根轮次、清理
及 journal 关闭；补齐响应不确定性的处理后，最终源码再独立验证五次，
全部通过，耗时约 2.7–3.8 秒。两批统计分开保存。该实现没有修补已安装
Codex 的 reader/句柄继承实现。

## 最终验证与限制

`python scripts/verify.py --live` 通过 fmt/check/clippy/doctest/release、
143 项默认 Rust 测试、八项启动 runner 测试、十二个 Windows 兼容
场景、三项进程身份检查、11 个进程所有权场景以及历史/回放/观察/实时
JSONL 夹具。五项真实检查均通过：schema、Windows 配置、localhost
Gate、零模型 Core Ready 和真实 CLI JSONL。

九项 Core 隔离预检回归覆盖主线程门禁、连接身份隔离、排队阻塞、握手
拒绝、取消 join、deadline、清理不确定性、EOF 和响应校验。错误响应、
无效字段、错 ID 与交互请求均不能放行模型，私有响应文本不进入错误。

完整验证首次在临时慢读 fixture 的 PID 存活检查失败。清理证据现同时
保存主/辅助父子进程的 PID 与创建时间，并有界等待退出信号，避免 PID
复用或退出信号的短暂延迟影响判定；仍在运行的同一进程会失败。原记录
未保存创建时间，不能将该失败归因于已确认的某一种原因。修改后的
JSONL 专项与完整验证通过。

这些证据限于本机 Windows、锁定 Codex 0.159.2 和现有配置。localhost
Gate 的专用后端并未走正常启动的辅助路径；真实 Core Ready 和正常
CLI JSONL 已覆盖该路径。Alpha 仍需交互、试用任务集和其他发布门禁，
V2 需要独立验收；本轮通过不能替代这些工作。
