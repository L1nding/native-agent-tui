# 任务调度与操作

本次实现将内存调度器接入 Core、真实 app-server 和 TUI。它属于 V2 的实验功能；Alpha 的活动证据、journal 和观察恢复要求仍待完成。

## 启动工作流

```powershell
cargo run --locked -- --workflow docs/workflow-example.json
cargo run --locked -- --workflow docs/workflow-example.json --headless --sandbox read-only --windows-sandbox unelevated
```

第二条命令中的 Windows sandbox 覆盖只适用于 Windows。工作流文件路径相对于启动目录，`--cwd` 指定任务的工作目录。

文件是 UTF-8 JSON，包含 `tasks` 数组。每项必填 `text`，可选 `id`、`dependencies`、`priority`、`policy` 和 `failure`。任务 ID 是非零整数；依赖可以引用数组中稍后出现的任务。Core 原子校验整批任务，未知依赖、重复 ID、环和非法策略均拒绝，不会先执行半个计划。

```json
{
  "tasks": [
    {"id": 1, "text": "Analyze the repository"},
    {"id": 2, "text": "Implement the findings", "dependencies": [1], "priority": 10}
  ]
}
```

所有根任务沿同一 Codex 根线程串行执行，共享线程上下文。优先级只影响已满足依赖的未运行任务，同分按入队顺序选择；每等待 30 秒增加一个有效优先级点。Gate 或审批等待会释放根执行槽位，但当前根任务仍阻止另一个根轮次启动。

## 依赖策略

| JSON 值 | 条件 |
| --- | --- |
| `"allRequired"`（默认） | 全部前驱成功；`continueWithErrors`/`optional` 可接受已失败或取消的前驱 |
| `"any"` | 任意前驱收到确定终态 |
| `{"quorum": 2}` | 至少两个前驱成功；`optional` 允许用失败/取消计入数量 |
| `"collectAll"` | 全部前驱收到确定终态，保留失败/取消 |

`failure` 默认 `"failFast"`，另有 `"continueWithErrors"`、`"optional"`。未知结果不会算作终态。永远无法满足条件的后继显示 `Blocked`，不会偷偷绕过依赖。重试前驱后会重新计算尚未运行的后继。

这些策略决定何时执行用户提供的任务正文；当前没有自动拼接前驱结果、跳过其他分支或取消剩余分支。需要结果汇总时应在正文中明确提出。

## 键盘操作

| 键 | 操作 |
| --- | --- |
| F4 / Up、Down | 打开任务面板 / 选择任务 |
| F5 | 暂停或恢复工作流派发 |
| F6 | 暂停或恢复选中的根任务 |
| F7 | 取消选中任务；已运行任务等待真实终态 |
| F8 | 显式重试已失败或取消的根任务，创建新 attempt；可能重复已发生的外部副作用 |
| `+` / `-` | 调整未运行根任务优先级，范围 -1000 到 1000 |
| 连按两次 F9 | 停止新派发，取消已知工作流任务 |
| Ctrl+Enter | 在运行期间明确排队新根任务，依赖当前根任务成功 |

正常 Enter 保留原来的草稿行为：根轮运行时不发送；Gate 等待时加入顺序依赖队列。失败或中断后依赖任务保留为阻塞，用户可以检查、取消或重试。F7 只针对选中任务；停止整个工作流使用 F9。

暂停只停止新派发和 Gate 回复。当前模型或 shell 操作可以继续；审批、输入和子代理终态仍可处理。即使子代理已全部完成，暂停期间也不会回复父 Gate，恢复后才回复一次。

任务面板显示本地状态、暂停/取消意图、依赖、等待 attempt、请求数和外部 thread/turn/generation。任务操作携带观察到的 attempt，迟到命令不能作用于新 attempt。RPC 中断确认不会将任务标记为完成。

## 边界与验证

最多 8 个待执行根任务、256 条任务记录；每项正文最多 32 KiB UTF-8，文件最多 2 MiB。成功任务释放正文，失败/取消任务保留正文供本次进程内重试。达到历史上限需要新会话。

原生子代理由 app-server 创建，客户端登记实际轮次并支持 `turn/interrupt`。身份已知但还未开始第一轮时，取消意图等到真实 `turn/started` 才发送。原生 child 的暂停、重试、优先级控制被拒绝；后续轮次由根代理的原生工具启动。child 计数表示观察值，尚没有客户端强制并发上限、深度限制或多工作流支持。

断连将活动任务标记为 Unknown；不会自动重试。持久 journal、outbox 和重启恢复仍未实现。Headless 工作流仅在所有根任务成功时退出 0；失败、取消、阻塞和未知结果退出非零。

验证记录见[调度验收](scheduler-validation.md)。
