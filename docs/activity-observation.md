# 活动证据与注意级别

Core 为每个当前 root/child 轮次、工具 item 和交互请求维护独立观察记录。执行状态、最近证据和注意级别分别呈现；静默提醒不会发送模型请求、重试、中断或释放 Gate。中断结果显示为 `Interrupted`。

## 使用

- 顶栏显示所选代理的活动、静默时长，以及全局待操作/需要关注的活动数；宽屏还显示证据和下一步动作。
- **F3** 切换代理；宽屏代理列表保留各 child 的注意级别。
- **F11** 打开证据详情，**PageUp/PageDown** 滚动，查看每个工具、thread/turn/generation/attempt、等待对象、恢复条件、最近证据及阈值来源。
- **F10** 编辑本次会话的阈值。**Up/Down** 选类别，**Tab** 选字段，输入毫秒，**Ctrl+U** 清空字段，**Enter** 提交，**Esc** 关闭。关闭设置保留任务草稿和秘密回答。
- 审批和问题立即显示 `requires_action`。回答进入 `Responding`，直到服务端确认 resolved 或 Core 明确结束该交互；其他输出不会清除待操作状态。

40×12 的紧凑布局保留会话和输入区域，完整证据通过 F11 查看。注意计数按活动统计，同一代理可能有多个待关注工具。

## 阈值与配置

| 类别 | Quiet | Attention |
| --- | --- | --- |
| Model | 15 秒 | 30 秒 |
| Tool | 30 秒 | 60 秒 |
| Children | 60 秒 | 120 秒 |
| Transport | 15 秒 | 30 秒 |

Starting/Unknown 不使用猜测阈值；终态记录停止计时。参数要求 `0 < quiet_ms < attention_ms <= 604800000`。

```text
cargo run --locked -- --attention-model 10000,20000
cargo run --locked -- --attention-config docs/attention-example.json --attention-tool 45000,90000
```

覆盖顺序为 Default → 显式全局 JSON 文件 → CLI → TUI。CLI 优先级与参数排列无关。JSON 最大 8 KiB，只接受已知类别和完整阈值对；配置无效时在启动 app-server 前报错。当前全局文件由 `--attention-config` 指定；临时设置仅在会话内生效。

## 证据语义与边界

`snapshot_version` 表示投影变化，`progress_seq` 仅随该活动接受的新证据增长；原始消息计数单独保存。刷新、阈值调整、空增量、重复终态和旧轮消息不会增加进展。相同文本增量可能是合法重复内容：当前协议没有 offset，因此不会按文本去重。

child A 输出只更新 A；工具 A 输出也不会刷新工具 B。工具完成缺少开始事件时，开始时间和耗时为 unknown。轮次结束时仍未收到终态的工具保持 Unknown，不能据此推断工具成功。`turn/started` 只证明轮次已开始，provider 的内部执行状态为 unavailable。

观察投影只保存身份、类型、时间和字节计数，排除任务正文、命令参数、工具输出、问题正文和秘密回答。输出字节计数是接受的通知字节数，包含 final 正文，不能当作去重后的生成长度。每次会话最多保留 1024 个活动/已完成消息身份，每个活动最多保留 8 条最近证据；超限明确进入 Unknown 并关闭执行所有者。

当前实现为内存投影。Python fixture 的运行方式和验收结果见[验证记录](observation-validation.md)；持久 journal、JSONL CLI 和只读回放仍属后续阶段。
