# 请求投递身份与观察活动

更新：2026-10-03。

## 行为

app-server 可以在同一 thread、同一 turn 中，在请求解决后再次使用相同的 RPC ID。第二次投递是新的请求，应重新显示 `requires_action`，并记录新的 `RequestCreated` 证据。已解决的旧活动保留自己的终态。

Observer 原先仅以 agent 和 RPC ID 定位交互活动。新投递命中旧活动后，活动仍为 Completed，导致顶部观察提示漏掉待处理审批。Core 的请求列表和回答身份校验已经能区分两次投递；本次修复让观察路径使用相同的身份边界。

## 关联规则

- 使用完整 `RequestRef`：RPC ID、thread ID、turn ID、Core 接受投递时的 `received_seq`。
- 数字 `7` 和字符串 `"7"` 是不同的 RPC ID。
- 同一请求的重复消息沿用既有投递身份，不增加创建证据。
- Core 将 `serverRequest/resolved` 关联到当前请求，再把完整引用传给 Observer。Observer 不接受其他投递、thread 或 turn 的引用。
- 如果当前投影替换了请求而旧活动尚未解决，旧活动标记 Expired；新活动保持 Pending。

`activity_id` 是不透明的活动标识。消费者应比较完整值，不解析其内部格式。`received_seq` 是投递身份的一部分，不是 journal 游标，也不是活动进展序号。

## 验证

```powershell
cargo nextest run --locked -E 'test(reused_request) | test(request_delivery_identity) | test(approval_remains)'
python scripts/verify.py --live
```

Core 回归覆盖审批提醒恢复、旧活动保留、重复消息及零新增 RPC；Observer 回归覆盖过期请求和陈旧解决引用。原生 JSONL 夹具在同一 turn 两次投递数字 ID `7`，检查实时输出和持久回放均保留两个独立的创建及解决活动，并沿用秘密脱敏和进程清理检查。

本次完整验证通过：184 项默认 Rust 测试、8 项真实 Codex 检查及全部原生夹具；格式、编译、Clippy、文档测试和 release 构建通过。日志位于 `target/request-identity-verification.txt`。

活动仍受既有 1024 项预算约束，超限显式报错。此次修复未提供完整事件时间线，也未完成 Windows Terminal、Orca 剪贴板或 IME 的验收。
