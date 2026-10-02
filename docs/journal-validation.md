# 日志回放验证

本轮在 Windows、Rust 1.96.0、Codex 0.159.2 上验证：102 项默认测试、3 项可选真实 app-server 测试和 Python 持久消费者通过。fmt、check、clippy、doctest（无示例）和 release build 通过。`rust-version=1.89` 是 manifest 下限，本轮未单独运行该版本工具链。

## 自动验证

```text
cargo fmt --all -- --check
cargo check --locked --all-targets
cargo clippy --locked --all-targets
cargo nextest run --locked
cargo test --locked --doc
cargo build --locked --release
```

定向运行 `cargo nextest run --locked journal`，CLI 集成另用 `cargo nextest run --locked --test journal_cli`。验证穿过真实文件与 `ClientHandle`/duplex transport，不依赖文本日志推断完成。

| 场景 | 可观察断言 |
| --- | --- |
| 游标与并发追加 | 返回游标基线、之后的状态和捕获高水位；回放打开后的追加不混入当前读取 |
| 崩溃/部分尾部 | 未提交尾部不参与重建；缺终态保留 unknown 和 review 提示 |
| 元数据写入失败 | 注入 `.cursor.new` 目录；保留原游标，Core 进入 Unknown，不启动下一根任务 |
| 启动失败 | 无效日志目录先于 Codex 启动失败，零 app-server/模型请求 |
| 有界队列 | 持有目录锁使写线程停顿；入队返回显式 Overloaded，退出可在释放锁后排空 |
| 磁盘预算/保留 | 活动 lease 与未知结果受保护；安全移除后读取明确返回 Removed |
| 正常退出 | 清理后最终状态提交完成，已提交序号与 UI 一致，执行结果与清理状态分开 |
| 反馈与隐私 | 写入确认不再生成日志；prompt、命令、对话与配置不进入持久投影 |
| 无效输入/输出 | 错误 workspace/session/schema/order/cursor、超大记录和断开的输出 writer 显式失败 |

## Python 与真实 CLI

```powershell
cargo build --locked --example observation_fixture
cargo build --locked --bin native-agent-tui
python tests/fixtures/journal_replay_check.py --fixture target/debug/examples/observation_fixture.exe --binary target/debug/native-agent-tui.exe
```

非 Windows 环境去掉可执行文件的 `.exe` 后缀。脚本在 `target/` 创建临时日志，退出时清理自己的目录。

Rust fixture 将父等待、两个 child、并行工具、数字/字符串审批 ID 的投影写入日志，再关闭为 Unknown。Python 校验持久化序列、历史 freshness、独立进展/attention、等待目标和隐私。CLI 选择一个调用后会留下标记的假 Codex：正常回放、游标回放、列表、无效参数和撕裂尾部均不执行它。脚本比较内容摘要与修改时间，确认只读行为。默认 Rust 集成测试还覆盖活动写入期间回放与缺少最终结果的语义。

## 可选真实 app-server

```powershell
$env:NATIVE_AGENT_TUI_PYTHON = (python -c 'import sys; print(sys.executable)')
cargo nextest run --locked --run-ignored only --no-capture
```

三项测试要求本机 Codex 0.159.2；Ready 测试还要求有效本机配置，均默认忽略。Gate fixture 使用独立 CODEX_HOME、localhost provider 和日志目录，同时保留两轮 Gate 的零额外父请求断言；完成后回放核对执行结果、清理和三次根任务启动，并排除任务/输出正文。Ready 测试也使用独立日志目录。

这些证据覆盖 Windows 的当前实现；尚未运行其他操作系统的文件锁与目录同步验证。实时 JSONL 的慢消费者/断管、强杀真实进程、磁盘设备故障和终端恢复完整发布矩阵仍需后续验收。
