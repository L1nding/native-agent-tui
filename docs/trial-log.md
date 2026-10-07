# 试用记录

按[产品计划](native-agent-tui-plan.md)第 4 节的试用指标记录真实后端任务。每条注明版本、环境和观察结论；agent 自述与系统验证分开。

| # | 日期 | 后端 / 环境 | 任务 | 结果 | 工具调用 | 审批 | 状态理解 | 发现的问题 |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| 1 | 2026-10-07 | Codex 0.159.2，Windows ConPTY 120×30，read-only，elevated 沙箱 | 找出 `src` 下行数最多的三个 `.rs` 文件（只读） | 成功；排序与 `wc -l` 一致：client.rs、ui.rs、scheduler.rs（系统验证）。agent 报告的行数（8116 / 4790 / 2118）不计空行，`wc -l` 为 8310 / 4927 / 2223，agent 未说明口径 | 4 次，2 次失败后自行恢复 | 0 | 运行中头部显示“Agent is writing”和最后事件；工具摘要逐条可见 | 长命令把状态挤到第二行（已改为状态在前）；单引号包装未去除（已修复） |
| 2 | 2026-10-07 | 同上 | 在当前目录创建文件（用于触发审批） | 审批正确出现并显示命令、目录、理由；未批准，文件未写入（系统验证） | 1 次（等待审批） | 1，仅 accept / cancel | 待审批面板显示线程 UUID（已改为 root） | 摘要带 pwsh 包装（已剥离） |
| 3 | 2026-10-07 | dsh 0.2.0-rc.2 ACP | 只回复 READY | 未完成：账户余额不足 | 0 | 0 | 原先锁死在 Unknown（已改为 Turn failed，可继续） | 需充值后补完整轮次 |
| 4 | 2026-10-07 | Codex 0.159.2，workspace-write，临时目录 `target/trial-ws` | 修复 `calc.py` 中 add 的减号 bug，只改该文件 | 成功；文件变为 `a + b`（系统验证），重复三次结果一致 | 2 次（读取 + 文件修改） | 0（工作区内写入无需审批） | 头部与摘要清楚 | 文件修改摘要只显示 `File`（已改为 `1 file: calc.py`，路径相对工作目录） |
| 5 | 2026-10-07 | dsh ACP，`--model hi/gpt-6-luna`，read-only | 同任务 1 | 成功；排序正确（系统验证） | 3 次 + 1 次权限请求 | 1（脚本批准） | 清楚 | 长任务超过 30 秒被判 Unknown（turn/start 改为立即确认）；读取大文件超 32 KiB 使整轮失败（改为截断）；权限请求只显示“ACP permission request”（改为显示 rawInput 命令、理由、目录） |
| 6 | 2026-10-07 | dsh ACP，`--model hi/gpt-6-luna`，workspace-write，`target/trial-ws` | 同任务 4 | 成功；文件变为 `a + b`（系统验证） | 5 次（glob/read/edit） | 0 | 清楚 | 工具摘要只有工具名（改为附带路径/模式） |
| 7 | 2026-10-07 | Codex 0.159.2，read-only，`--attention-tool 10000,25000` | 运行 `Start-Sleep 90`，静默中 Ctrl+C，再追问命令是否完成 | 成功：13 秒“no recent output”、30 秒“silent for a while, check it”；Ctrl+C 1 秒内 Interrupted；追问时模型记得上下文并说明命令可能仍在后台运行 | 1 | 0 | 头部提醒准确 | 状态区显示原始 `commandExecution`（已删除）；中断后工具行仍为 running（改为 ended, outcome unknown）；会话汇总因中断轮次记为 Failed（工作流严格语义，待定） |
| 8 | 2026-10-07 | 同上 | 运行中强杀 app-server 进程树 | 立即 Disconnected 并说明“外部结果未知”；再次提交被拒且草稿保留；`--recovery` 显示任务 Unknown、需要人工确认 | 1 | 0 | 清楚 | 工具行仍为 running（已修复）；`initialized` 通知在恢复报告中被误报为待检查（已修复） |
| 9 | 2026-10-07 | Codex 0.159.2，read-only，120 列 | 让根代理派两个子代理分别统计 `src/ui` 和 `tests` 的文件数，完成后汇总 | 成功：25 和 2，与 `ls` 一致（系统验证）；左侧代理列表、F3 切换到子代理对话均正常 | 根 2 次，子代理各 1 次 | 0 | 代理列表清楚 | 已结束代理显示 `Ended / quiet unknown`（已省略） |
| 10 | 2026-10-07 | Codex 0.159.2，workspace-write，`target/trial-ws` | 第一轮写 `fib`，运行中用 Ctrl+S 排队第二轮“写 unittest” | 成功：头部显示 `queued: 1`，第一轮结束后自动开始第二轮；生成的 3 个测试用 `python -m unittest` 全部通过（系统验证） | 4 + 1 | 0 | 清楚 | 排队提示“Queued 1 root tasks; dependencies remain enforced.”语法错且偏技术化（已改） |
| 11 | 2026-10-07 | Codex 0.159.2，read-only，`target/trial-ws` | 根代理派子代理用 shell 写 `child.txt` | 成功：审批来自子代理，面板标明 `/root/create_child_file`、命令与理由；头部 `GatePending`，输入框提示 Enter 排队；批准后文件内容为 `hi`（系统验证），根代理核实后汇总 | 根 6 次 + 子代理 2 次 | 1（批准） | 清楚 | 等待子代理的工具调用只显示 `Dynamic`（改为显示 `wait_for_subagent_completion` 等工具名；MCP 显示 server/tool） |
| 12 | 2026-10-07 | dsh ACP，`hi/gpt-6-luna`，read-only | 运行 `Start-Sleep 60`，Ctrl+C，再追问 | 成功：Interrupted 后会话可继续，模型说明了原因 | 3 + 1 次权限请求 | 1 | 清楚 | 权限请求只带 toolCallId，界面只显示“ACP permission request”（改为按 id 查回命令、理由、目录及申请的 `danger-full-access`） |
| 13 | 2026-10-07 | 同上 | 运行 `Start-Sleep 1`，对提权请求按 Ctrl+N 拒绝 | **修复前拒绝被当成批准**（dsh 选项名 `reject-once` 未被识别，回退到首个 `allow-once`，命令执行并返回 DONE）；修复后命令未执行，agent 回复权限被拒（系统验证） | 3 | 1（拒绝） | 清楚 | 已修复：按 ACP kind 选择，拒绝/取消绝不回退到批准选项 |

尚未覆盖：5 秒状态理解计时、Orca 内置终端。
