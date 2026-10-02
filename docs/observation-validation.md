# 活动观察验证

## 本次实现

S2.5a 的内存活动证据和注意级别已接入 Core、CLI 配置与 TUI。独立一秒计时器仅发布投影；漏过的 tick 使用 Skip，避免休眠恢复后集中刷新。快照中的观察版本与 Core 版本一致。

## 自动验证

本次实跑 **85 项默认测试、3 项可选真实 app-server 测试**通过；fmt、check、clippy、release build、Python 消费 fixture 通过。Doctest 目标无测试。

```text
cargo fmt --all -- --check
cargo check --locked --all-targets
cargo clippy --locked --all-targets
cargo nextest run --locked
cargo test --locked --doc
cargo build --locked --release
cargo run --locked --quiet --example observation_fixture | python tests/fixtures/observation_consumer.py
```

验证覆盖：

- 静默边界 Active → Quiet → AttentionNeeded → Active；tick/配置不增加证据和进展。
- 多 child、多工具分别计时；等待对象保留 turn/generation、最近证据、静默时长和恢复条件。
- 暂停 Gate、child 输出及注意变化均不能释放等待；审批始终需要明确回答。
- 重复工具/消息终态、空增量、旧轮/无关消息过滤；相同非空 delta 按协议接受。
- 工具缺少开始事件时保留 unknown 时间；Interrupted 独立于 Completed 活动类别；未结束工具不继承轮次成功。
- 时钟倒退变为 unknown；终态耗时冻结；重试替换 attempt 身份；记录和最近证据有界。
- 暂停后排队重试保留最后已执行 attempt 的终态；实际派发时才切换活动身份。
- 全局文件/CLI/TUI 优先级、非法配置原子拒绝、字段来源及秘密内容排除。
- 40×12、80×24、160×45 的顶栏、证据和阈值界面；设置缓冲不混入任务草稿或秘密回答。

Python 读取 Rust `Observer` 序列化的三份内存快照，验证同一字段语义、数字/字符串 request ID、独立活动进展、待操作状态及敏感正文排除。示例进程不创建 app-server，不代表已有持久 JSONL 或回放接口。

## 真实 app-server fixture

```powershell
$env:NATIVE_AGENT_TUI_PYTHON = (python -c 'import sys; print(sys.executable)')
cargo nextest run --locked --run-ignored only --no-capture
```

本次环境为 Windows，安装的 Codex 基线为 0.159.2。Gate 测试使用隔离 `CODEX_HOME` 和 localhost provider，持有两轮 child 响应。每轮临时设置 Children 的 Quiet/Attention 为 10/20 毫秒，等待 Core 实际显示 AttentionNeeded，再保持 300 毫秒采样。

第一次等待时根/child 请求数维持 2/1，第二次为 4/2；两个持有窗口内新增根 provider 请求均为零，根 `progress_seq` 保持不变。Gate 保持 pending，只有实际 child 终态及显式恢复调度后才回复。最后根完成后请求数为 5；随后两项依赖根任务使总数到 7。另两项可选测试验证 Windows sandbox 配置覆盖和零模型轮次到 Ready。

本次未进行新的交互终端人工试用；布局证据来自 Ratatui TestBackend。持久化、回放故障注入、外部 provider 的开发任务集及 V2 多 child 并发验收仍待后续阶段。
