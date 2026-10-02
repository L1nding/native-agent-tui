# 实时 JSONL 验证

## 默认回归

本轮 **115 项默认测试、4 项可选真实测试**全部通过；fmt、check、clippy、doctest 和 release build 通过。release 二进制通过 11 个原生管道场景，Python 活动观察与持久回放消费者也通过。Rust 实测版本为 1.96.0，平台为 Windows。

`cargo nextest run --locked` 覆盖 journal 追赶、慢读后最终高水位、缺终态、断管、Core 在输出停顿时退出、已确认终态保留、秘密输入中断、审批拒绝和不支持 decline 的中断路径。`cargo nextest run --locked headless` 与 `cargo nextest run --locked live_jsonl` 可定向运行。

schema 1 兼容测试读取旧格式并拒绝混合版本；已有 Gate、调度、UTF-8、身份校验、Windows Job 清理和 TUI 回归继续保留。

## 真实 CLI 管道与 Python

```powershell
cargo build --locked --bin native-agent-tui
python tests/fixtures/live_jsonl_check.py --binary target/debug/native-agent-tui.exe
```

脚本启动真实应用二进制与独立的 JSONL app-server 协议夹具，不使用认证或远程模型。每轮持有一个后代进程，退出后在 Windows 检查两个 PID 都已结束。

畸形工作流和非法配置在执行前返回 2；JSONL 模式的固定诊断不回显字段名、任务或配置值，且不创建 journal 或启动 app-server。

| 场景 | 断言 |
| --- | --- |
| 成功 | stdout 每行均可解析，完整连续序列、最后快照、Completed、清理确认、回放一致 |
| 两次审批 | 数字/字符串 request ID 保留，实际回应都是 decline，动作原因可回放 |
| 审批无法 decline | 实际发起 interrupt，未发送 accept，真实 Interrupted 终态保留 |
| 秘密输入 | 不回答问题，真实中断返回 130，prompt/命令/问题/输出不出现在 stdout 或 stderr |
| 中断无终态 | 五秒内进入显式关闭流程，返回 4，历史 Unknown |
| 失败/断连 | 分别返回 1/4，固定脱敏诊断，清理结果与执行状态保留 |
| 多根工作流 | 前一任务失败、最后任务成功，CLI、journal 和 Python 消费者均报告 Failed；两个任务各自终态保留 |
| 临时慢读 | 管道暂时填满，Core 可独立结束，恢复消费后读到所有提交状态和最终快照 |
| 断管 | 关闭读端后有界退出，活动结果为 Unknown，journal 关闭摘要及清理确认可回放 |
| 永久慢读 | 读端保持打开且不读，真实 Windows 写入阻塞；12 秒内返回 4，根启动数为 1，拥有进程已清理 |

同一脚本还用通用 Python stdin 消费者检查完成、失败、中断、Unknown 和缺少最终快照的流；不把 EOF 当 Completed。未知 schema、序号错误、非法 payload 和缺少字段返回 2，诊断不回显输入或 traceback。消费者限制单行大小为 4 MiB。脚本只在 `target/` 创建并清理自己的数据。

## 可选真实 Codex CLI

```powershell
$env:NATIVE_AGENT_TUI_PYTHON = (python -c 'import sys; print(sys.executable)')
cargo nextest run --locked --run-ignored only live_cli_jsonl --no-capture
```

测试运行真实 Codex **0.159.2** app-server 和应用 CLI。独立 CODEX_HOME 无认证数据，provider 指向 localhost；bundled catalog 仅在夹具副本关闭 lite wire format，模型输出使用普通 Responses SSE。默认忽略该测试，缺少依赖时显式失败。

检查真实根 turn 恰好启动一次、接收到有内容的输出证据、最终 Completed 和清理确认，以及实际 `--replay --json-events` 的一致性。不声称 provider 正在计算，也不将固定 SSE fixture 视为远程模型任务质量测试。

四项可选真实测试包含此次 CLI 验证和原来的 Windows sandbox、Ready、Gate 回归。完整检查仍包括 fmt、check、clippy、doctest 和 release build。Linux/macOS 的 I/O 取消、设备故障和强杀恢复没有在本机验证，Alpha 完整放行尚未完成。
