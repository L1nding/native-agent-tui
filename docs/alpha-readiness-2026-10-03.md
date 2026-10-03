# Alpha 就绪度验证（2026-10-03）

这份记录保存本机 Windows、锁定 Codex 0.159.2 环境下的最新可复核结果。它补充历史实施记录，不替代跨平台发布验收。

## 已通过

### 启动可靠性

```text
python scripts/check_startup.py --trials 20
```

20/20 次独立启动通过。每次都满足：

- 进入 `Ready`；
- 根线程轮次为 0，根 `turn/start` 请求为 0；
- shell 预检未超时；
- app-server 清理和 journal 关闭均确认。

单次耗时约 2.6–4.8 秒。该结果证明当前环境下未复现历史间歇性超时，不代表其他 Windows 环境没有同类风险。

### 完整门禁

```text
python scripts/verify.py --live
```

本次通过 fmt、check、Clippy、doc test、release 构建、210 项默认测试、8 项真实 Codex 检查，以及协议兼容、journal/replay、JSONL、Windows 输入、时间线和进程所有权夹具。

## 尚未放行

- 尚未完成 Linux/macOS 和不同 Windows 宿主的真实启动采样。
- Alpha 试用任务集、跨平台发布门禁和用户可理解性指标尚未收集。
- 细粒度资源预算、持久调度恢复、outbox、完整代理/工具树和 compaction/skill 服务端事实仍属于后续范围。
- V2 的 1–3 个直属 child 完整并发与交付报告仍需独立验收。

## 复现边界

验证使用本地锁定版本和隔离测试数据；没有上传诊断数据，也没有把启动成功推断为模型任务成功。
