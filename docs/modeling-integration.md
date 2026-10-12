# 建模入口与 dsh 基线整合

日期：2026-10-12。工作树：`D:/project/native-agent-tui-modeling`，分支：`feat/modeling-workflow`。

## 来源与原因

用户要求先更新 TUI 工作树并整合现有建模入口。执行 `git fetch origin` 后，远程 `origin/main` 为 `68c060d`；原仓库 `feat/dsh-first` 的最新已提交版本为 `a614d89c726eed8c3eb6d0e09fe92009a855192c`，包含该远程基线。建模工作树从 `2c7de49` 快进到这一版本，共纳入 12 个提交，然后恢复并整合原有建模修改。

## 当前边界

普通入口沿用 Rust Core → dsh → bridge/ACP。新版会话、技能、provider/model 和权限预设走 dsh 服务。

`--modeling` 作为独立过渡入口保留：Rust 建模 owner 管理 Python 工作进程并发布状态，UI 消费状态；Python 拥有建模阶段、AI 调用、Scene IR 与 Blender 产物。通用模式的 `/model` 不控制此入口；建模模型仍来自模型器的 `config/models.yaml`。此次未实施业务调度迁移。

三处冲突位于 `src/config.rs`、`src/lib.rs` 与 `src/ui.rs`。整合保留新版 dsh 配置和模型模块，并注册建模模块与命令。旧 Codex 后端及已删除的 `--check-shell` 未重新引入。README 的建模章节位置和入口说明已修正。

## 验证证据

所有 Rust 命令均使用 `--locked`（fmt 除外），编译、测试和构建使用隔离目录 `target/modeling-integration`：

- `cargo fmt --all -- --check`：通过。
- `cargo check --all-targets`：通过。
- `cargo clippy --all-targets`：通过。
- `cargo nextest run --all-targets`：494 项通过，2 项跳过；包含建模协议、确认应答、子进程取消与界面回归。
- `cargo test --doc`：通过（0 项 doctest）。
- `cargo build --release --examples --bin native-agent-tui`：通过。
- dsh bridge：`pnpm install --frozen-lockfile`、`pnpm run check`、`pnpm run build` 均通过。
- 用上述 release 二进制执行仓库验证脚本的等效非 live fixtures：历史导出、日志重放、JSONL 消费、Windows 终端输入、进程身份（3 项）和进程所有权均通过。`scripts/verify.py` 本身未运行，因为其路径固定为 `target/release`。
- CLI help 同时列出 dsh 与建模入口；版本为 `0.1.0-alpha`；非交互建模启动返回退出码 2；旧 `--check-shell` 返回未知选项。
- 真实 PTY 启动空建模页并用 Ctrl+Q 退出，退出码 0。
- `git diff --check` 通过，无未解决的合并条目。

未执行远程模型调用或新的 Blender 重建，未安装或切换用户 dsh profile。本轮没有验证真实 dsh/provider 会话或建模全流程运行。原 TUI 仓库的未提交改动仍由其当前工作持有；核对期间观察到并发修改，未复制、覆盖或回滚这些文件。建模旧修改的备份与 stash 保留。

## 启动

在此工作树中执行：

```powershell
.\target\modeling-integration\release\native-agent-tui.exe --modeling D:\project\blender-agent-modeler
```

进入后输入楼层或楼栋目标。直接指定目标会立即启动工作流并可能调用模型。
