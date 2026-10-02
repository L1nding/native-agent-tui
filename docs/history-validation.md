# 观察恢复与导出验证

## 本轮结果

128 项默认测试与 fmt/check/clippy/doctest/release build 通过；4 项可选真实 Codex 0.159.2 测试通过，覆盖启动、Windows 配置、Gate 零父请求和 localhost provider 的 JSONL CLI。真实测试未验证远程 provider 的开发任务完成质量。

release 二进制通过历史/导出 CLI、schema 1/2、journal 回放与观察 Python 消费者；实时 JSONL 的 11 个管道场景和非法命令检查继续通过。Windows ConPTY 的离线导出、实时秘密输入隔离与退出清理结果见下方记录。

## 自动回归

默认测试覆盖：

- 预览固定提交高水位；源继续写入后，导出仍只包含预览范围。
- 8 KiB 预览上限包含结尾换行；保存保留完整选择范围。编辑目标路径时可翻页查看预览。
- Bearer、cookie、私有路径、原始 session/item/request 字符串被别名替换；跨字段关联、数字 ID 和证据计数保留。
- 历史 freshness 和年龄冻结；历史控制键不能触发执行或回答请求。
- 已有目标、托管目录保护；取消和部分写入故障不发布目标，临时文件清理。
- 预览后源前缀截断时拒绝导出，不用缓存终态掩盖缺失记录。
- 真实 Core 保持 Running 和待审批；导出失败后仍可正常回答、接收真实 Completed 并关闭。
- CLI 不依赖 Codex 安装，导出 Unknown 成功与任务成功分开；已有文件和源目录保持原内容。
- 30×10、60×20、80×24、100×30、120×40、160×50 的历史和预览布局。

```text
cargo nextest run --locked history
cargo nextest run --locked export
```

## Native CLI 与 Python

```powershell
cargo build --locked --release --examples --bin native-agent-tui
python tests/fixtures/history_cli_check.py --fixture target/release/examples/observation_fixture.exe --binary target/release/native-agent-tui.exe
```

脚本生成父等待两个 child 的持久 fixture，以会留下 marker 的 poison Codex 检查零执行。游标 0、1、3 的导出保留相同等待关系、静默值和 Unknown；身份替换后关系仍可核对。比较源文件 hash 和修改时间，确认无 journal 写入。

检查预览、中文目标名、拒绝覆盖、错误 session/游标、托管目录保护，以及非交互终端拒绝打开历史 TUI。旧 schema 1 继续导出，保留其原版本。测试文件仅在 `target/` 自有临时目录创建。

## Windows ConPTY

实际使用 release 二进制验证两个交互路径：

1. 离线直接打开 Unknown 会话，`g` 跳转到事件 0，`e` 获取预览，保存到中文路径。所得文件逐行解析，保持 Unknown 和 `live_attached=false`；Ctrl+Q 恢复 alternate screen、bracketed paste 和光标。
2. 真实应用 + 独立协议夹具进入秘密输入请求，未提交答案时按 F12。历史页仍显示当前 Running / 1 个请求；审批和重试键被拒绝。查看旧会话并导出后回到实时页，答案仍为掩码。RPC 文件证明根 turn 只启动一次、没有答案或审批响应；secret 未进入 journal 或导出。退出后检查拥有的父子 PID 均已结束。

这验证 TUI 与服务的集成边界；协议夹具不构成远程模型开发任务质量证据。锁定 Codex 的真实启动、Gate 和 JSONL smoke test 继续单独回归。

Linux/macOS 终端、网络或不支持硬链接的文件系统、设备故障、强杀恢复和完整 Alpha 放行仍需要专项验收。自动执行恢复、完整 transcript 重建和导出导入没有实现。
