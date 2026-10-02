# Codex 兼容门禁

## 支持表

| 后端与环境 | 执行状态 | 证据与限制 |
| --- | --- | --- |
| Codex CLI 0.159.2 / 本机 Windows | 唯一允许的后端版本 | 真实初始化、零模型 shell 预检、localhost Gate 和 CLI JSONL；预检仍有间歇超时 |
| 其他 CLI 版本，包括 prerelease | 禁止执行 | 模型目录查询前拒绝，没有忽略门禁的 CLI 开关 |
| Linux/macOS、其他 Windows 环境 | 尚未获得发布验收 | 源码路径和版本判断不能替代原生验证 |
| 历史会话 | 只读可用 | 列表、历史、回放和脱敏导出不查询或启动 Codex |

0.159.2 是项目验证的固定基线，不表示最新版本。选择已有兼容安装：

```text
cargo run --locked -- --codex PATH_TO_CODEX --check-shell
```

`CODEX_BIN` 也可选择执行文件。项目不会安装、下载或自动切换后端。

## 启动边界

1. `--version` 查询最多读取 4096 字节，等待五秒。只接受准确的
   `codex-cli 0.159.2`，可带一个 LF 或 CRLF；相似版本、额外文本、空输出
   和查询失败均拒绝。
2. 版本通过后读取有效模型目录，随后启动 app-server。
3. `initialize` 必须提供有界、非空字符串 `userAgent`、`codexHome`、
   `platformFamily`、`platformOs`。缺失或类型错误时，不发送 `initialized`、
   `thread/start` 或 shell 预检。其他字段允许保留在协议响应中，但不会
   从初始化 metadata 写入 Core 快照或 journal。
4. 新线程必须报告 `thread.cliVersion = 0.159.2` 和非空、有界的 `thread.id`。
   不符时拒绝 shell 预检和任务。版本 banner 不能替代新线程返回的版本。
5. shell 预检成功后才能 dispatch 模型轮次；旧的 30 秒 deadline 和禁止
   自动重试继续有效。

版本和目录查询共用进程输出接口。无论查询成功、超量还是超时，均执行
有界清理；Windows 确认 Job 树退出。清理未获确认时保存
`cleanupConfirmed=false` / `cleanupUncertain`，不把失败保存为安全关闭。
普通版本拒绝的进程已清理后，CLI 返回启动错误 3，journal 保留零根请求。

## Schema 与夹具

`tests/fixtures/codex-0.159.2/` 保存两个小型原始 schema、脱敏初始化响应、
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

九个本地 CLI 场景覆盖版本不符、空 banner、私有 banner、超大输出、
非零退出、超时查询、初始化缺字段、新线程版本不符及正常执行。前六种
只调用版本查询；初始化与线程拒绝场景分别停在其响应边界。测试确认
拒绝时零根请求、错误/日志不含私有夹具文本、关闭状态可回放；Windows
超时场景在结束前持有父子进程句柄，随后确认退出。

本轮 fmt/check/clippy/doctest/release、134 项默认测试、九个兼容 CLI 场景、
11 个 Windows 所有权场景以及其余历史/回放/JSONL 夹具通过。最新完整
`python scripts/verify.py --live` 中五项真实测试四项通过，Ready 检查约
31 秒后因 shell 预检超时进入 Unknown；整个命令返回失败。schema、
Windows 配置、localhost Gate 和 CLI JSONL 四项通过。不能将该记录
写成完整真实验证通过。

最终默认验证再次通过，Cargo 原生测试路径同样通过 134 项；最终源码的
真实 schema 专项也通过。单独把导出 schema 的初始化字段类型改为
number 后，指纹检查返回 1，确认差异会阻止兼容放行。

握手字段和 schema 指纹不是能力清单。当前只在调用边缘确认所用方法
和响应形状；usage 等可选事实缺失时保留 unavailable。完整 typed event
迁移、更广协议快照和 Alpha 启动可靠性门禁仍需继续完成。最终基线和
真实验证结果见[实施状态](implementation-status.md)。
