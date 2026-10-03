# Windows 启动可靠性诊断

## 当前结论

真实 Core 启动仍有间歇性 shell 预检超时，Alpha 启动门禁尚未通过。
请求已经写入并 flush；失败样本中 PowerShell 曾启动并退出，最终 RPC
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

## 本轮验证与后续工作

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

下一步应验证能保留线程版本门禁、MCP 功能及有界清理的隔离预检方案，
或验证修复 reader/继承边界的兼容后端。生产代码没有采用停止 MCP、
更换启动顺序、增加等待时间、自动重试或扩大 sandbox 权限的做法。
完整真实验证、启动重复验收及 Alpha/V2 仍未完成。
