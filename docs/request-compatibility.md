# 真实请求兼容验证

## 范围与复现

本机 Windows 使用锁定的 **Codex 0.159.2**，经正常 `ClientHandle::spawn` 启动真实 app-server，包含隔离 shell 预检和生产进程清理。模型响应来自 localhost 的确定性 Responses SSE 夹具，不访问远程模型。

```powershell
python scripts/verify.py --live

# 仅运行三项真实请求测试
$env:NATIVE_AGENT_TUI_PYTHON = (python -c 'import sys; print(sys.executable)')
cargo nextest run --locked --run-ignored only -E 'test(client::live_requests)' --no-capture --no-fail-fast
```

测试代码位于 `src/client/live_requests.rs`，provider 位于 `tests/fixtures/request_provider.py`。缺少锁定 Codex 或 Python 时显式失败。独立 fixture home 不包含认证信息；父进程环境和用户 Codex 配置保持原样。bundled catalog 的夹具副本只关闭 lite wire format，以匹配普通 SSE。

## 审批决策

锁定上游 `ff6aec96948b70d94983af2641a6b67c94faeff5` 的 `codex-rs/protocol/src/approvals.rs` 定义普通命令的默认选项为批准、可能的策略提案和 Abort；app-server 的 v2 协议将 Abort 表达为 `cancel`。实际测试也确认命令请求提供 accept/cancel，没有 decline。

| 决策 | UI | 含义 |
| --- | --- | --- |
| accept | Ctrl+Y | 接受本次审批，执行仍受有效沙箱约束 |
| decline | Ctrl+N | 拒绝操作，允许代理继续 |
| cancel | Ctrl+B | 拒绝操作并中断所属轮次 |

有 `availableDecisions` 时严格按服务端列表启用；输入请求不能发送审批决策。重复、过期、已提交请求继续受完整 `RequestRef` 校验。cancel 只发审批回答，不另发根轮次中断；UI 和 Core 等待服务端确认事实。

Headless 保留现有策略：允许 decline 时拒绝，否则显式请求根中断，并记录原有脱敏动作理由。

## 三项真实往返

| 场景 | 验证证据 |
| --- | --- |
| 命令 cancel | 写入文件未出现；一个 provider 请求；重复取消未续接；resolved 与 Interrupted 均获服务端确认 |
| 文件与命令审批 | 文件 decline 未创建文件；接受命令输出标记恰好一次；接受写命令仍被 read-only 拒绝，工具状态 Failed；明确接受文件 patch 后内容只追加一次；五个 provider 请求，最终 Completed |
| 输入回答 | 保留真实问题 ID、item、两项选项；中文/emoji 答案按问题 ID 返回；provider 只记录校验布尔值；两个请求，resolved 与 Completed 均获服务端确认 |

三个场景均要求具体交互活动保留 `RequestResolved` 服务端证据，不能以终态时请求列表清空替代。退出后检查 journal 关闭、进程清理确认和只读回放中的执行结果，排除命令、diff、答案与模型文本。

## 输入能力与提示

锁定版本默认关闭普通模式下的 `request_user_input` 功能；输入夹具在独立 home 显式开启 `features.default_mode_request_user_input=true`。生产启动没有自动打开该功能，不能据此声称默认模式一定提供输入工具。

真实请求给出 `isBlocking=false`，没有 `autoResolutionMs`。客户端只展示有类型的服务端提示，缺失时显示 unavailable；废弃的时间提示不启动定时器，不推断执行状态，也不自动提交答案。

## 最终检查与限制

最终 `python scripts/verify.py --live` 全部通过：160 项默认 Rust 测试、八项真实 Codex 检查及全部原生夹具；fmt/check/clippy/doctest/release 通过。八项包含这里的三项请求检查，以及已有 schema、sandbox、Ready、Gate 和 CLI JSONL 检查。

Windows ConPTY 使用假 app-server 单独确认 Ctrl+B 可达、无重复回答、零额外根中断、草稿保留及脱敏 Interrupted 回放，见[请求详情](request-details.md)。上述证据限于本机锁定版本与确定性模型响应；远程模型任务质量、子代理真实审批、策略 amendment、更多终端/IME 和完整 Alpha/V2 发布门禁仍未验收。
