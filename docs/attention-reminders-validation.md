# 本地静默提醒验证

## 行为与所有权

`src/ui/reminders.rs` 保存 UI 本地关闭状态。Ctrl+W 关闭所选代理当前的
Quiet/AttentionNeeded 静默提醒，再按一次恢复；它不提交 Core 命令。
只允许 Running/Waiting 活动，审批、requires_action 和 Unknown 保留
原来的动作提示。F10 阈值编辑及离线历史仍由各自的界面处理键盘。

关闭记录绑定 session、clock epoch、activity ID、完整执行身份、
progress_seq 和注意级别。新证据只恢复对应活动；新轮次/尝试/时钟来源
和注意级别变化会重新提示。重绘、静默时间增长及无新证据的投影更新
保持关闭状态。过期记录删除，记录数量受当前 Core 活动预算约束。

界面保留 Core 的 attention 与待操作数，另显示本地 waiting/wait 数及
继续等待提示。F11 保留原 AttentionNeeded、最近证据和恢复条件，同时
标明提醒本地关闭。不会修改 journal、JSONL、Python 消费者或 Gate；
不会把关闭提醒保存成任务成功，也不新增提示音或桌面通知。

## 自动验证

```text
cargo nextest run --locked ui::
python scripts/verify.py
```

15 项 UI 测试通过，其中四项新增回归覆盖：

- 关闭/恢复不发送命令，任务与 secret 草稿保持原样，审批和 Unknown
  仍指向原来的操作；Core 观察投影保持相同。
- 不影响其他 agent/tool；重绘保持关闭，新证据只恢复对应活动。
- 注意级别、session/epoch、turn、attempt 和 generation 改变时恢复。
- 40×12、80×24 和 160×45 正常/证据视图保留 Core attention、本地
  计数及可见的 Ctrl+W 操作提示，不依赖颜色解释状态。

完整默认验证通过 147 项 Rust 测试、协议/启动 runner、十二个兼容
场景、三项进程身份检查、11 个 Windows 所有权场景以及历史/回放/
观察/实时 JSONL 夹具；fmt/check/clippy/doctest/release 通过。

## Windows 终端检查

在原生终端运行 release TUI，使用隐藏的 Python app-server fixture，
固定为持续运行的一轮，Model 阈值为 1000/2000 毫秒；没有调用真实模型
或认证。出现 AttentionNeeded 后输入中文/emoji 草稿，Ctrl+W 关闭提醒，
F11 查看原始证据，再次 Ctrl+W 恢复，草稿持续保留。

两个切换前后，fixture 均只有一次根 turn/start、一次 shell 预检，零
turn/interrupt；证据页保持 progress 3。之后明确 Ctrl+C，收到 fixture
的 Interrupted 终态，再 Ctrl+Q 退出并恢复终端。启动与退出的进程清理
和只读 journal 回放单独检查。

本次真实终端验证使用 fixture；五项真实 Codex 验证在前一项启动修复中
全部通过，没有把它们重新记作本次 UI 的真实模型验收。更广终端、搜索、
完整请求详情、用户试用任务集和 Alpha/V2 发布门禁仍需继续完成。
