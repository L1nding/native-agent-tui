# Windows 进程所有权

## 创建边界

`src/owned_process.rs` 是 app-server 与模型目录查询共用的私有启动接口。
Windows 实现先创建非继承的 Job lease，再通过 `CreateProcessW` 的
`PROC_THREAD_ATTRIBUTE_JOB_LIST` 将进程纳入 Job。主线程保持 suspended，
只有创建成功且所有权建立后才恢复；创建或恢复失败时，RAII 关闭句柄和
lease。应用被硬终止时也由 Job 负责结束已纳入的后代。

继承列表只包含子进程的 stdin/stdout/stderr。属性数组的内存和借用句柄
在创建调用期间保持有效。父端管道由 Tokio 文件适配器读取和写入，每次
最多缓冲 8192 字节；transport 保持独立 reader、单 writer 和原有队列上限。
进程等待保留一个 blocking waiter，取消 await 后可以重新等待，并缓存
已确认的退出码；实际退出码 259 不表示进程仍在运行。

EXE 使用 UTF-16 参数；CMD/BAT 通过系统 `cmd.exe` 启动，关闭 autorun 和
延迟变量展开。引号、反斜杠、Unicode、空格与字面 `%` 有原生往返验证。
NUL、batch 换行以及超过 EXE/CMD 长度上限的命令在创建前拒绝。启动错误
直接返回，不退回无 Job 的进程，也不扩大沙箱权限。

## 关闭边界

正常关闭先收尾 transport，并最多等待根进程两秒。随后终止 Job，查询
`ActiveProcesses`，最多三秒确认整棵已纳入的进程树退出；查询错误或超时
返回清理失败并关闭 lease。需要强制关闭时，继续确认根进程的退出。

Drop 只能请求终止，不能等待或证明所有后代已退出。取消等待不会丢失
进程身份；关闭依据持有的句柄，避免按 PID 搜索和误杀复用后的进程。
Unix 仍沿用 Tokio 的进程接口，本轮没有新增 Unix 进程组退出保证。

## 原生验证

```text
cargo build --locked --release --examples --bin native-agent-tui
python tests/fixtures/process_ownership_check.py --fixture target/release/examples/process_fixture.exe
python scripts/verify.py
```

Windows fixture 不调用 Codex 或模型，覆盖 11 个场景：

1. 子进程尚未恢复时硬终止 owner，确认子进程未执行用户代码。
2. 拒绝 suspended 启动，确认子进程退出且没有执行用户代码。
3. 父子进程开始运行后硬终止 owner，持有句柄确认两者退出。
4. 正常关闭并确认整个 Job 的进程树退出。
5. 取消 wait 后重新清理。
6. EXE 参数与工作目录往返。
7. CMD 参数与工作目录往返。
8. Null stdin 的目录查询。
9. 退出码 259、重复 wait 和 kill。
10. 预热一次后，100 次无效可执行映像创建的句柄数保持有界。
11. NUL、batch 换行及两种命令长度上限在创建前拒绝。

第一次失败创建曾使冷句柄数从 68 增至 87，后续保持 87；预热后比较
100 次失败的增量。初次增长的原因尚未确定，不能据此宣称零泄漏。

## Shell 启动仍不稳定

相同原工作目录、read-only policy、显式 unelevated 沙箱和零模型轮次的
Ready 测试进行了启动对照。旧版固定为 `f6e7a70`；对照只改测试工作目录，
避免归档目录改变信任根。第一批十组中旧版 10/10 通过，当前版 7/10 通过。
后续创建后附加 Job 的临时探针曾与当前版本共享编译目录，发现其编译
诊断被当前构建复用；该三路径统计不作为验收证据。探针没有进入产品，
最终源码清理本包缓存后重新构建和验证。

之后单独确认旧版：前五次通过，第六次约 31 秒后出现同一
`Shell preflight timed out`，状态 `Unknown`，没有开始模型轮次。超时期间
后代采样未发现预检 shell；采样只记录进程名称和线程数。这个瞬时采样
不能证明 shell 从未启动或确定阻塞位置。

这些证据说明旧路径也能复现失败，创建时 Job 属性不是复现失败的必要
条件。样本不证明新旧路径具有相同可靠性，也没有确定故障根因。已有
30 秒预检 deadline 和禁止自动重试保持有效；Alpha 的启动可靠性验收
尚未通过。

## 验证范围

清理本包编译缓存后，`python scripts/verify.py --live` 全部通过：128 项
默认测试、4 项真实 Codex 测试、上述 11 个所有权场景，以及历史/导出、
回放/观察消费者和实时 JSONL 管道检查；fmt/check/clippy/doctest/release
也通过。此前完整验证中的 Ready 测试曾超时，最终单次通过不代表该
间歇问题已经消除。

ConPTY 中保留一个未提交的秘密回答，进入历史观察后动作被拒绝，返回
实时界面时回答仍被掩码。退出后恢复 alternate screen、bracketed paste
和光标；RPC 没有回答或审批响应，journal 没有测试秘密值，未发现测试
父子进程残留。这个交互检查不替代原生 fixture 的句柄退出确认。

本轮使用本机 Windows、Rust 1.96.0 和 Python 3.12；不代表已经验证最低
Rust 版本、所有 Windows 版本、Linux/macOS 或远端 CI。真实 Codex 验证
结果与原生所有权场景分开记录，见[实施状态](implementation-status.md)。
