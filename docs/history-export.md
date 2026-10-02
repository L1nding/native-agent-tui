# 观察恢复与脱敏导出

## 打开历史

```text
cargo run --locked -- --sessions
cargo run --locked -- --history
cargo run --locked -- --history SESSION_ID --cwd PATH --journal-dir PATH
```

`--history` 打开离线 TUI，不启动 Codex；指定 session 时直接打开其最新已提交状态。实时 TUI 的 **F12** 打开同一工作区的历史列表；顶部继续显示当前执行阶段和待处理请求数，后台执行保持原来的状态。历史读取通过独立服务完成，视图不访问文件或发送执行命令。

| 操作 | 按键 |
| --- | --- |
| 选择会话 / 打开 | Up、Down / Enter |
| 前后事件 / 指定序号 | Left、Right / `g`，输入序号，Enter |
| 手动刷新最新已提交前缀 | `r` |
| 浏览证据和等待关系 | PageUp、PageDown |
| 导出所选会话 | `e` |
| 关闭表单 / 返回列表 | Esc |
| 返回实时会话 / 退出应用 | F12 / Ctrl+Q |

每次打开或刷新只捕获当时已提交的前缀，`live_attached=false`。顶部的最终结果与所选历史事件的 recorded phase 分别展示；此前 Running 不代表任务现在仍在运行。缺少关闭记录、Unknown 或未确认清理明确显示需要检查，未提交尾部另行标记。

elapsed、quiet、attention 和 progress 保留历史数值。原来为 Current 的 freshness 改为 Unknown；不按当前进程时钟继续计时。历史请求无法回答，原任务正文、秘密答案和完整对话没有保留，不能恢复它们。

继续执行需要离开历史、输入一个新任务，或使用新的 `--run TASK`。新执行使用新的任务/会话身份；历史外部副作用可能已经发生，检查证据后再决定新任务内容。历史页中的审批、中断、取消或重试按键不会操作旧会话。

## 预览并保存

```text
cargo run --locked -- --export SESSION_ID --since 0
cargo run --locked -- --export SESSION_ID --since 10 --output diagnostic.jsonl
```

不带 `--output` 时只显示范围、脱敏说明和最多 8 KiB 的预览。指定新文件时保存固定前缀；读取或导出成功返回 0，与历史任务结果独立。错误返回 2，已有目标不会被覆盖。

TUI 中按 `e`，输入基线序号，Enter 获取预览。PageUp/PageDown 浏览预览；检查范围和脱敏方式，编辑目标路径，再按 Enter 保存。Ctrl+U 清空路径，Left/Right 按完整 grapheme 移动光标；中文路径可用。Esc 关闭表单。路径编辑器与任务草稿、秘密答案分开保存。

预览固定结束序号，确认保存时不会加入后来的事件。范围包含游标处基线、严格晚于游标的状态、捕获高水位处最新快照，以及读取结束标记。未知版本、不可用游标、源基线/终态变化、序号或版本倒退、前缀截断显式失败。

## 导出格式与隐私

首行 `kind=export_manifest`、`export_version=1`，记录源 schema、起止游标、历史结果、清理、缺口及不包含的正文类型。后续记录保持源 schema 1 或 2、序号和 `historical=true`，最后是 `replay_end`。

导出统一替换自由字符串身份，包括 session、thread、turn、item、活动及字符串 request ID，使用同一文件内稳定的 `id-N` 别名。关系、task/attempt/generation、数字 request ID、状态、来源、证据计数和等待条件保留。原始身份可在本地历史页查看；导出不包含反向映射，也不承诺不同导出范围使用相同别名。可选 provider 文本设为 unknown，不输出未经约束的字符串。

这避免依赖凭据样式猜测：即使协议身份里含 Bearer、cookie、环境变量值或私人路径，它们也不会原样导出。任务、对话、命令、问题/答案、配置和原始错误从源投影中已被排除。导出不是完整 transcript，不能作为已验证任务完成的证明。

每次导出最多保存 8192 个不同字符串、2 MiB 原身份数据，预览最多 8 KiB。超出预算时选择较晚游标；不使用无限增长的别名表。

## 文件与退出

目标目录必须存在，且位于托管 journal 之外。独立线程逐条写同目录临时文件、同步文件，再通过硬链接原子发布到新路径；发布不替换已有文件。发布前取消、写入或校验失败会清理临时文件，不留下部分目标。需要支持硬链接的本地文件系统；不支持时显式报错。Unix 目录同步失败会明确提示文件已发布、持久性未确认。

导出错误只影响本次历史操作，当前 agent 和审批/Gate 继续由 Core 处理。退出请求取消历史工作，并最多等待一秒确认线程结束；底层磁盘调用本身无法强制取消。当前支持与验证范围见[观察恢复验证](history-validation.md)。
