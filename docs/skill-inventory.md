# 服务端 skill 目录

更新：2026-10-04。范围：锁定 Codex 0.159.2 的目录事实。

## 查看与刷新

实时 TUI 使用 Ctrl+K 打开工作目录的 skill 清单，Enter 请求刷新，Esc 或 Ctrl+K 返回；F2 可直接打开待处理请求。打开页面只读取快照。每项显示服务端返回的名称、路径、scope 和 enabled；最多保留 256 项，每项名称不超过 256 字节、路径不超过 1024 字节，截断会明确提示。目录属于 cwd，不代表所选 root 或 child 已加载这些 skill。

初次读取、`skills/changed` 通知和用户显式刷新是查询来源。Core 使用现有 RPC owner 发送 `skills/list`；没有定时轮询。Enter 使用 `forceReload=true` 绕过服务端缓存；忙碌期合并请求仍保留这一意图。Gate 等待、root/child 正在运行或有待回答请求时，刷新保留为待处理，旧目录标明有效性。初次读取不等待响应才分发任务。

## 证据含义

- 查询成功且为空表示服务端返回了空目录；尚未读取、查询失败、协议不支持和超时分别保留不可用状态。
- scope 是服务端的 `user/repo/system/admin` 分类。enabled 表示目录配置，不能证明 skill 已加载、调用或完成。
- `skills/changed` 没有 cwd、thread 或 turn 身份，仅使缓存失效。查询与 cwd、RPC ID 和失效代次关联；旧响应不能把新一代目录恢复为有效。
- 扫描错误只显示数量，不显示原始错误内容。目录读取失败不改变执行终态；真实传输故障仍按 Core 的断连和 Unknown 语义处理。
- 有效空目录与缺少字段或重复 cwd 的无效响应分开处理。部分结果只承诺已确认条目的下界；不可用状态显示 counts unavailable，不补成零条。

锁定 schema 没有提供 loaded、invoked、completed、failed 的 skill 生命周期事件，也没有版本/hash 字段。相关信息保持 unavailable；不通过本地文件存在、模型文字或工具日志猜测。

## 历史与隐私

journal、JSONL、历史详情和文本回放只保留目录可用性、有效性、计数、截断及来源汇总。名称、路径、description、默认提示正文、依赖内容和原始扫描错误不写入持久记录。旧日志没有目录字段时显示 unavailable。

历史表示记录当时的目录状态；只读回放不会扫描当前文件或重新查询 app-server，也不会启动模型、回答请求或恢复旧操作。实时目录内容有界并经过终端显示处理。

## 验证范围

```text
python scripts/verify.py --live
```

2026-10-04 全量验证通过：244 项默认测试、9 项 live 测试，包含 fmt、check、clippy、release build、schema 指纹以及 Windows 输入、进程清理、JSONL 和历史 TUI 夹具。

回归覆盖查询形状、空目录、错误、截断、刷新失效、迟到成功/错误/超时响应、Gate 延迟并合并刷新、初始查询未响应仍可启动任务、窄屏长列表和持久脱敏。真实 smoke 使用隔离 Codex home 和临时 skill，首次发现目录后新增 skill，再通过显式刷新发现新增项；根启动请求和模型轮次均为零。目录发现通过不等于 skill 生命周期验收；状态理解速度、真实开发任务集和跨平台发布仍需独立验收。
