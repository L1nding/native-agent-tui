# 平台验证记录（2026-10-04）

本记录补充本机锁定 Rust 1.96.0 和 Codex 0.159.2 环境下的跨平台证据，不替代各平台的真实运行验收。

## Windows 启动稳定性

```text
python scripts/check_startup.py --trials 20
```

20/20 次独立启动进入 `Ready`。每次根启动请求和根轮次均为 0，shell 预检没有超时，app-server 清理和 journal 关闭均得到确认。单次耗时约 2.6–3.5 秒。该结果覆盖当前 Windows 主机和锁定 Codex 安装，不代表所有 Windows 宿主都具备相同结果。

## Linux 与 macOS 目标

以下命令在 Windows 主机上完成 Rust 目标级检查：

```text
cargo check --locked --all-targets --target x86_64-unknown-linux-gnu
cargo check --locked --all-targets --target aarch64-apple-darwin
```

两个目标均通过，且没有非 Windows 专用代码的死代码警告。目标级检查只证明源码和依赖可以完成类型检查；它没有证明 Linux/macOS 的终端、进程树清理、app-server 启动或真实 Codex 交互已经验收。

## 当前边界

Windows 本机默认测试、Clippy 和完整仓库验证继续作为合并门禁。Linux/macOS 真实运行、打包、终端输入和进程清理仍需要对应平台执行环境；远端 Actions 当前未运行。
