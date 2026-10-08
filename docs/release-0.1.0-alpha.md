# 0.1.0-alpha 发布清单

创建：2026-10-08。依据[产品计划](native-agent-tui-plan.md)第 4、7 节；GitHub 跟踪 #14。勾选项必须对应可复核的记录；历史通过数不能代替发布前复跑。

## 范围

- 平台：只支持 Windows。Linux/macOS 标为未支持。
- 后端：Codex CLI 0.161.0 为正式后端；`deepseek-acp` 为实验后端，不进入放行条件。
- 功能：单 agent 对话、审批/输入、中断/停止、活动证据与 Attention、JSONL/Python 消费者、journal 回放、历史搜索和脱敏导出。现有 child/Gate 路径只做回归，不宣称 V2 可用。

## 已有证据

- [x] 远端 CI 首跑通过：windows-2025、Rust 1.96.0（[持续验证](ci-validation.md)）。
- [x] 本机启动 20/20 进入 Ready，零模型轮次（[Alpha 就绪度](alpha-readiness-2026-10-03.md)）。
- [x] 16 条真实后端试用，期间发现的问题已修复并补回归（[试用记录](trial-log.md)）。
- [x] Windows Terminal 与 Orca 内置终端的输入、粘贴标记和退出清理（[终端输入](windows-terminal-input.md)）。

## 发布前必须完成

1. [ ] 推送本地 `main`，确认远端 `Verify` workflow 在最终提交上通过。
2. [ ] 在最终提交上运行 `python scripts/verify.py --live`，记录通过数与失败项；环境失败不算通过。2026-10-08 在锁定 0.161.0 的提交上通过（391 项默认、9 项真实），之后若有代码提交需重跑。
3. [ ] 人工验收（需真人操作）：
   - [ ] Codex 升级到 0.161.0 后，在交互终端运行一次 `codex sandbox -- cmd /c echo ok` 并批准 UAC，再确认默认 elevated 沙箱下 `--check-shell` 通过。
   - [ ] Orca 窗口内真实 Ctrl+V 粘贴多行中文与 emoji。
   - [ ] Orca 窗口内中文输入法组字、候选确认与退格。
   - [ ] 5 秒状态理解：选 3 个试用场景（审批待处理、子代理静默、断连），记录能否在 5 秒内说出当前活动、等待对象和所需动作。
4. [ ] 把第 3 项结果补进[试用记录](trial-log.md)，未达目标的项写明原因，不伪报。
5. [ ] 打 tag 并发布：

   ```text
   git tag -a v0.1.0-alpha -m "0.1.0-alpha"
   git push origin v0.1.0-alpha
   cargo build --locked --release
   ```

   Release 说明写明：只支持 Windows；需要 Codex CLI 0.161.0；ACP 为实验后端；journal 只保存脱敏投影，不能重建完整对话。

## 不阻塞 Alpha

- 第二轮删除式精简（`refactor-goal.md`）。
- V2：真实 1–3 child 并发、handoff；之后的持久调度恢复与资源预算。
- ACP 兼容门禁与正式支持。
- 观察恢复率的故障注入统计。
