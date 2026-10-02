# 持续验证

## 本地入口

```text
python scripts/verify.py
python scripts/verify.py --cargo-test
python scripts/verify.py --fixtures-only
python scripts/verify.py --live
```

默认从仓库根目录运行 fmt、check、clippy、默认测试、doctest、release 构建及原生 Python fixture；从其他目录调用脚本也使用同一工作区。安装了 nextest 时优先使用它，否则运行 `cargo test --locked --all-targets`。`--cargo-test` 显式使用后者。

脚本检查每个子命令的退出码，任何失败即停止。观察 JSONL 以原始字节传给消费者，同时检查生产者与消费者；Python 子进程统一使用调用脚本的解释器。`--fixtures-only` 使用已有 release 二进制，不代表重新验证或构建了当前 Rust 源码。

`--toolchain 1.96.0` 选择已安装的 rustup toolchain。普通本地执行沿用当前工具链；不会安装或切换 Rust。`--live` 额外运行 ignored 测试，并为 provider fixture 设置真实 Python 路径，需要本地安装兼容的 Codex 0.159.2。默认检查不启动真实 Codex、不依赖认证或远程模型。

## GitHub Actions

`.github/workflows/verify.yml` 在 push、pull request 和手动触发时运行 Windows 检查。CI 使用 Windows 2025、Rust 1.96.0 与 Python 3.12；两项 action 固定 commit SHA，checkout 不保留凭据。job 只有 `contents: read`，同一 ref 的新运行取消旧运行，最长 30 分钟。

CI 直接运行 `python scripts/verify.py --toolchain 1.96.0 --cargo-test`。测试覆盖 Core/审批/Gate、Windows 已附加进程树清理、历史只读与身份脱敏、schema 1/2、回放/Python 消费者，以及慢管道与断管。默认不运行依赖本机 Codex 的四项 ignored 测试。

该 workflow 不发布构建、不上传日志或诊断，不设置远程模型凭据。真实 app-server smoke test、交互终端、进程创建与 Job 附加竞态、Linux/macOS 和完整 Alpha 发布仍需各自证据。配置已可在本地执行；远端 CI 结果须以实际 Actions run 为准。

## 本地验证结果

Rust 1.96.0 / Python 3.12 环境运行 `python scripts/verify.py --cargo-test` 通过：128 项默认测试、doctest、fmt/check/clippy/release，以及历史/导出、schema 1/2、journal/观察消费者和 11 个 JSONL 管道场景。选择不存在的工具链时，首个 fmt 命令失败，脚本立即返回 1；后续构建和 fixture 未启动。

从 `target/` 调用 `python ../scripts/verify.py --fixtures-only --live` 也通过，确认工作区定位、原生 fixture 与 nextest 的四项真实 Codex 测试接线。workflow 经 actionlint 1.7.10 校验通过。本次没有运行远端 Actions，没有验证 Cargo 声明的 Rust 1.89 最低版本，也没有新增非 Windows 平台支持声明。
