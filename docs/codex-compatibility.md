# Codex 兼容门禁

## 支持表

| 后端与环境 | 执行状态 | 证据与限制 |
| --- | --- | --- |
| Codex CLI 0.161.0 / 本机 Windows | 唯一允许的后端版本（2026-10-08 起） | schema 差异经人工审核；真实初始化、零模型 shell 预检、localhost Gate 和 CLI JSONL 见下方升级记录 |
| Codex CLI 0.159.2 | 禁止执行（旧基线） | 2026-10-08 前的验证记录保留为历史证据 |
| 其他 CLI 版本，包括 prerelease | 禁止执行 | 模型目录查询前拒绝，没有忽略门禁的 CLI 开关 |
| Linux/macOS、其他 Windows 环境 | 尚未获得发布验收 | 源码路径和版本判断不能替代原生验证 |
| 历史会话 | 只读可用 | 列表、历史、回放和脱敏导出不查询或启动 Codex |

0.161.0 是项目当前验证的固定基线（此前为 0.159.2），不表示最新版本。选择已有兼容安装：

```text
cargo run --locked -- --codex PATH_TO_CODEX --check-shell
```

`CODEX_BIN` 也可选择执行文件。项目不会安装、下载或自动切换后端。

## 启动边界

1. `--version` 查询最多读取 4096 字节，等待五秒。只接受准确的
   `codex-cli 0.161.0`，可带一个 LF 或 CRLF；相似版本、额外文本、空输出
   和查询失败均拒绝。
2. 版本通过后读取有效模型目录，随后启动 app-server。
3. `initialize` 必须提供有界、非空字符串 `userAgent`、`codexHome`、
   `platformFamily`、`platformOs`。缺失或类型错误时，不发送 `initialized`、
   `thread/start` 或 shell 预检。其他字段允许保留在协议响应中，但不会
   从初始化 metadata 写入 Core 快照或 journal。
4. 新线程必须报告 `thread.cliVersion = 0.161.0` 和非空、有界的 `thread.id`。
   不符时拒绝 shell 预检和任务。版本 banner 不能替代新线程返回的版本。
5. 正常 Windows 启动在主线程通过上述门禁后，为 shell 预检启动独立
   app-server。辅助进程重新核对准确版本和 initialize 必需字段，使用
   相同启动配置及共享 catalog 副本，不创建线程或模型轮次。主进程的
   MCP 保留；辅助端 RPC ID 不进入主连接的 pending 表。
6. shell 预检成功且辅助进程树清理确认后才能 dispatch 模型轮次。
   30 秒 deadline 覆盖辅助启动、预检和清理，过期的成功不能进入 Ready；
   取消保留查询/进程所有权直到清理结束。无法验证响应或清理时保留
   Unknown 和相应清理标志，不自动重试。`--check-shell` 使用单连接；
   Linux/macOS 继续使用原路径，尚未获得平台验收。

版本和目录查询共用进程输出接口。无论查询成功、超量还是超时，均执行
有界清理；Windows 确认 Job 树退出。清理未获确认时保存
`cleanupConfirmed=false` / `cleanupUncertain`，不把失败保存为安全关闭。
普通版本拒绝的进程已清理后，CLI 返回启动错误 3，journal 保留零根请求。

## Schema 与夹具

`tests/fixtures/codex-0.161.0/` 保存两个小型原始 schema、脱敏初始化响应、
合成启动 transcript，以及八份导出 schema 的规范化 SHA-256 指纹。
这些指纹覆盖初始化、线程创建、shell 响应、轮次终态、两类审批与输入。
指纹反映被审核的完整源 schema，合成 transcript 用于 Core 行为回归，
不宣称来自真实用户会话，也不覆盖所有工具或协议字段。

默认检查不调用 Codex：

```text
python scripts/check_protocol_schema.py
python tests/fixtures/compatibility_cli_check.py --binary target/release/native-agent-tui.exe
```

显式复核安装二进制导出的 schema：

```text
codex app-server generate-json-schema --experimental --out target/codex-schema-review
python scripts/check_protocol_schema.py --schema-dir target/codex-schema-review
```

`scripts/verify.py --live` 还运行 ignored schema 导出测试，先确认固定版本，
在独立临时目录导出并比较八份指纹，不发模型请求。脚本为该测试和
provider 测试设置实际 Python 路径；单独运行时应显式设置：

```powershell
$env:NATIVE_AGENT_TUI_PYTHON = (python -c 'import sys; print(sys.executable)')
cargo nextest run --locked --run-ignored only live_codex_schemas
```

升级需要手动审核 schema 差异，更新固定版本和夹具，再运行协议/Core
回归、历史回放及真实 app-server 验证。指纹脚本只有比较入口，不能自动
更新基线或绕过差异。

## 验证范围

九个共有本地 CLI 场景覆盖版本不符、空 banner、私有 banner、超大输出、
非零退出、超时查询、初始化缺字段、新线程版本不符及正常执行。前六种
只调用版本查询；初始化与线程拒绝场景分别停在其响应边界。测试确认
拒绝时零根请求、错误/日志不含私有夹具文本、关闭状态可回放；Windows
超时场景在结束前持有父子进程句柄，随后确认退出。Windows 额外验证
辅助版本拒绝、辅助初始化拒绝、辅助 shell 失败，共十二种场景；前两个
主连接拒绝场景不能启动辅助端。清理检查累计主、辅助父子进程，并核对
PID 与创建时间，防止已退出进程的 PID 复用造成误判。

接入隔离预检前，fmt/check/clippy/doctest/release、134 项默认测试、九个兼容 CLI 场景、
11 个 Windows 所有权场景以及其余历史/回放/JSONL 夹具通过。该轮完整
`python scripts/verify.py --live` 中五项真实测试四项通过，Ready 检查约
31 秒后因 shell 预检超时进入 Unknown；整个命令返回失败。schema、
Windows 配置、localhost Gate 和 CLI JSONL 四项通过。不能将该记录
写成完整真实验证通过。

该版本的默认验证再次通过，Cargo 原生测试路径同样通过 134 项；当时源码的
真实 schema 专项也通过。单独把导出 schema 的初始化字段类型改为
number 后，指纹检查返回 1，确认差异会阻止兼容放行。

握手字段和 schema 指纹不是能力清单。当前只在调用边缘确认所用方法
和响应形状；usage 等可选事实缺失时保留 unavailable。完整 typed event
迁移、更广协议快照和 Alpha 启动可靠性门禁仍需继续完成。最终基线和
真实验证结果见[实施状态](implementation-status.md)。

隔离预检接入后的最终 `python scripts/verify.py --live` 全部通过：143
项默认 Rust 测试、十二个 Windows 兼容 CLI 场景、八项启动 runner
测试、三项进程身份检查、11 个 Windows 所有权场景及原有原生夹具。
五项真实测试均通过，包括此前超时的 Core Ready。最终源码的五次独立
启动另行验证，全数确认零根请求/轮次、Ready、进程清理和 journal 关闭。
这些是本机固定版本证据，Alpha、V2 和其他环境的支持验收仍待完成。

重复启动检查及 Windows 管道继承的诊断证据见
[启动可靠性诊断](startup-reliability.md)。检查失败会立即停止剩余独立
启动，不重试模型任务；诊断干预后的成功不能作为无干预启动验收。

## 升级记录：0.159.2 → 0.161.0（2026-10-08）

本机 Codex 升到 0.161.0 后，门禁按设计拒绝执行，`verify.py --live` 的九项真实测试全部在版本检查处失败。为对照，在独立临时目录安装官方 0.159.2，两版分别导出 schema；0.159.2 导出与原指纹 14/14 一致，证明对照有效。

人工审核结论：

- 清单内 14 份 schema 中 12 份不变；`ThreadStartResponse` 与 `TurnCompletedNotification` 只有内嵌的 `CodexErrorInfo` 变化：`oneOf` 改为 `anyOf`，并新增兜底分支 `{"type": ["string","object"]}`。Core 不解析该字段，没有影响。两份的 `required` 列表不变。
- 全部 440 份旧 schema 中 28 份变化，其余差异均为新增可选字段（`loginId`、`origin`）、描述文字和新方法：Bedrock GovCloud 检查、`thread/prediction` 请求及其通知。客户端不发 `thread/prediction`，未知通知在分发处忽略，不计为进展。
- 审批请求（`ServerRequest`）与 item 完成通知不变。

更新内容：固定版本常量、`tests/fixtures/codex-0.161.0/`（两项指纹、合成 transcript 的 `cliVersion`）、测试与 Python 夹具中的版本字符串。近似版本拒绝用例随之改为 `0.161.00`、`0.161.0-dev` 等，语义不变。

验证：`python scripts/verify.py --live` 全部通过——391 项默认测试、九项真实 Codex 0.161.0 测试（schema 导出、sandbox 覆盖、命令取消、文件审批、输入回答、localhost Gate、skills、Core Ready、CLI JSONL）及全部原生夹具。release 版 `--check-shell --windows-sandbox unelevated` 通过；默认 elevated 沙箱返回 `setup refresh had errors`，属于升级后需重新执行一次管理员设置（见 README 的 Windows sandbox setup），不是协议不兼容。
