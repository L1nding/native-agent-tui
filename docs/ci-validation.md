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

CI 直接运行 `python scripts/verify.py --toolchain 1.96.0 --cargo-test`。测试覆盖 Core/审批/Gate、Windows 创建时的 Job 所有权和进程树清理、版本拒绝及协议启动门禁、历史只读与身份脱敏、schema 1/2、回放/Python 消费者，以及慢管道与断管。Windows 额外运行 11 个原生进程场景，包含 suspended owner 被硬终止及无效创建的句柄检查，见[进程所有权](windows-process-ownership.md)。兼容夹具的九个 CLI 场景和本地 schema 检查默认运行，不依赖实际 Codex。默认不运行依赖本机 Codex 的五项 ignored 测试。

该 workflow 不发布构建、不上传日志或诊断，不设置远程模型凭据。真实 app-server 启动可靠性、交互终端、Linux/macOS 和完整 Alpha 发布仍需各自证据。配置已可在本地执行；远端 CI 结果须以实际 Actions run 为准。

## 本地验证结果

Rust 1.96.0 / Python 3.12 环境运行 `python scripts/verify.py --cargo-test` 通过：128 项默认测试、doctest、fmt/check/clippy/release，以及历史/导出、schema 1/2、journal/观察消费者和 11 个 JSONL 管道场景。选择不存在的工具链时，首个 fmt 命令失败，脚本立即返回 1；后续构建和 fixture 未启动。

从 `target/` 调用 `python ../scripts/verify.py --fixtures-only --live` 也通过，确认工作区定位、原生 fixture 与 nextest 的四项真实 Codex 测试接线。workflow 经 actionlint 1.7.10 校验通过。本次没有运行远端 Actions，没有验证 Cargo 声明的 Rust 1.89 最低版本，也没有新增非 Windows 平台支持声明。

新增创建时的进程所有权后，清理本包缓存并运行 `python scripts/verify.py --live` 通过：128 项默认测试、4 项真实 Codex 测试、11 个 Windows 所有权场景和其余原生 fixture。先前一次完整运行的 Ready 测试曾预检超时，旧版也单独复现同类失败；这次通过不能关闭启动可靠性问题。对照条件、缓存排除与限制见[进程所有权验证](windows-process-ownership.md)。远端 Actions 仍未运行。

兼容门禁接入后，默认检查增至 134 项 Rust 测试，并新增九个原生兼容场景和本地 schema 指纹检查。最新完整 `--live` 的五项真实 Codex 测试四项通过，Ready 再次因 shell 预检超时失败；该次脚本返回失败。新增真实 schema 导出检查通过，使用明确的 Python 解释器和独立临时目录。启动可靠性与远端 Actions 仍未获得放行，详见[兼容门禁](codex-compatibility.md)。
