# zf-tui-bridge

本 TUI 的 dsh 插件，用 TypeScript 编写。它运行在 dsh 进程内部，通过 stdio 上逐行的 JSON-RPC 2.0 为 Rust 界面服务。

```text
native-agent-tui (Rust) ──stdio JSON-RPC──> dsh --profile zf-tui
                                              └─ zf-tui-bridge（本插件）
                                                   ├─ zf/* 方法：直接读 dsh 的服务
                                                   └─ 其余帧：交给 dsh 自己的 ACP 服务
```

一条连接上有两套方法：

- **`zf/` 开头的方法**由本插件回答，数据来自 dsh 的会话、标题、命令、设置等服务，TUI 不再另存一份。
- **其余的帧是 ACP**，原样转给 dsh 自己的 ACP 服务。那个服务由本插件挂载在一条内存流上，所以对话轮次、审批和模型选择用的是 dsh 维护的实现。

## 构建

```powershell
cd dsh-plugin
pnpm install
pnpm run build      # 输出到 lib/；pnpm run check 只做类型检查
pnpm test           # 构建并验证历史中的思考、工具状态与身份关联
```

`lib/` 和 `node_modules/` 不入库。改动源码后需要重新构建，再重启 profile。

图像上下文修复还需要给**实际运行的 DSH 安装**补齐适配器接缝。在仓库根目录执行（下面是桌面版安装位置；自装 CLI 应传它自己的入口）：

```powershell
node scripts/install-dsh-context.mjs --host-entry "$env:APPDATA/dsh-tauri/dependencies/dsh/node_modules/@deepseek-ai/dsh/lib/bin.js"
node scripts/install-dsh-context.mjs --host-entry "$env:APPDATA/dsh-tauri/dependencies/dsh/node_modules/@deepseek-ai/dsh/lib/bin.js" --check
```

安装脚本核对 MCP client、ACP、图像适配器和摘要四个宿主模块的原始 SHA-256，再完整暂存新文件和原字节，读回核验后逐文件 rename 替换；提交失败时恢复所有已尝试替换的原字节，并核验恢复结果。恢复本身失败时保留对应原字节备份并报告路径。重复执行可校验，宿主升级或补丁被外部修改时拒绝覆盖。此机制处理安装过程的写入/替换失败，不承诺跨文件的断电原子性。图像修复逻辑保存在 `src/context.ts`；ACP 的身份与输出排空接缝及 MCP session 恢复接缝保存在安装脚本。桌面更新可能重置宿主模块；桥接启动时检查两类修复版本，缺失时明确拒绝挂载 ACP。正常重启由会话所有者执行。

共享 HTTP MCP 服务可能继续运行而客户端 session 已过期。仅当工具调用收到真实 HTTP 404，且 JSON-RPC 错误明确为 `-32600 / Session not found` 时，MCP client 退休旧 generation、确认 transport 关闭，再沿原有有限重连策略初始化新 session。并发失败共用一次退休，旧工具闭包不能继续发请求，取消或插件关闭不能复活连接。失败调用无论读写均不自动重放；原错误附加未重放及副作用尚未确认提示，恢复后先检查持久回执再决定业务动作。其他 404、普通工具错误及调用取消不触发这条恢复。现有 `events.test.mjs` 用实际安装的 Client/HTTP transport 和内存 fetch fixture 验证此契约，不启动服务或业务。

## 后台 parent 续行

一次 Native root 执行包含本次输入派生的本地后台 children，以及 DSH 自动触发的 parent 续行。child 工具返回后，parent 可以继续独立工作；DSH 自己向 parent Queue/Steer 结束消息，Native 不重投消息、不补发模型 prompt。Native 保留同一执行身份，直到 children 结束、parent 续行空闲、ACP 已排空对应输出。首段 prompt 返回不代表整项任务结束。

插件从宿主 `subagent/start` / `subagent/end` 转发带 `runId` 的直接 child 生命周期。ACP 更新用 `_meta.zfHostTurn` 标注原宿主轮，原始 `turn/end` 只表示宿主计算结束；输出队列排空后另发 `host-settled`。桥接按宿主轮隔离迟到更新，并将重新从 1 编号的宿主 step 转为同一 Native 执行内的递增 step。运行期最多保留 256 个 child run，越界明确中断连接，不能当作已完成。

失败的 `host-settled` 只携带 `provider`、`output_delivery`、`unknown` 类别与白名单错误码，插件和 Core 再次过滤；不传 provider 原文。旧宿主缺少分类时保留通用失败提示。已确认的 `gateway_concurrency_limit` 或 `Concurrency limit exceeded for user` 归为 `RATE_LIMIT`，复用 provider 原有的有限退避与取消；其他未识别错误仍为不可重试的 `PI_AI_ERROR`。

停止时通过 `zf/session/background/cancel` 排空该 parent 的直接 continuable children，再确认 parent 空闲；取消失败保持不确定结果，不启动下一轮。此接口只用于取消，不调用模型。关闭 ACP 会话仍由宿主负责完整 descendant 清理。

`events.test.mjs` 在 `DSH_CONTEXT_SOURCE=1` 且提供 `DSH_CONTEXT_HOST_ENTRY` 时，内存加载同一份 ACP 补丁，验证延迟输出的身份和终态顺序，不写宿主模块、不调用模型。Rust 现有测试还覆盖两个 child 分别完成、两次 parent 续行、失败/停止及旧轮迟到；这些是离线 fake 验证，真实模型验收由会话所有者另行执行。

## 图像上下文边界

SUP023 修复只作用于 `hi/gpt-6-luna`，保留 profile 声明的 context window 和 overflow 校验。按适配器真实请求尺寸与相同 handle/placeholder 文本，对每个图像出现项计价，供宿主 token meter 和请求预算共用。视觉项采用 `ceil(width/32) × ceil(height/32) × 2` 的保守估价，**不是已确认的供应商计费公式**；供应商报告用量仍由 DSH 校验。该路由普通输出上限收至 16,384，摘要上限为 8,192，另留 16,384 输入余量。

超过预算时，复用宿主 `image/offload` 事件，只移出已有后续助手文字记录的最老图像前缀。包括适配器原有 base64 字节限制在内的所有卸载来源，都先验证完整请求的卸载数不超过这个前缀；不足时明确拒绝，不减少请求数后静默卸载。原始图像、工具回执和助手文字仍在持久会话中。新交付而尚无助手文字的批次受保护；无法容纳时明确停止，保留待观察批次。供应商实报仍超窗时，每次最多移出可恢复前缀的一半，剩余项严格递减；没有可移出的已记录图像时保留原失败，不盲目重试。

摘要另外带上确定性的 `ZF_IMAGE_OBSERVATIONS_V1` 记录：完整 attachment SHA、尺寸、原回执中的 build/evidence 绑定、原 reader 调用和助手观察的消息 ID/原文。再次摘要沿用这些记录，LLM 摘要不负责重写它们；交付不等于复核通过或审批授权。记录最多 512 个图像出现项、192 KiB，超过即拒绝摘要，不截断结论。恢复已有候选时先读取精简事实和已有观察，按明确缺失身份补读原图，沿用已有 prepare/build/render 产物。

现有 `events.test.mjs` 包含纯内存回归。另可设置 `DSH_CONTEXT_HOST_ENTRY` 和 `DSH_CONTEXT_TRACE`（SUP023 的 `session.v4.jsonl.zstd`），再执行 `pnpm test`：真实宿主模块会离线重放 81 图历史，校验预算、持久 offload 投影和摘要身份/观察保留。此项只读 trace，以假摘要输出代替 provider，不调用模型；未设置时显式跳过。

已有 TUI probe 运行时，保持编译产物和宿主文件原样。设置 `DSH_CONTEXT_SOURCE=1` 后直接执行 `node --test tests/*.test.mjs`，测试会在内存编译当前源码、生成同一份宿主补丁并加载，既不运行安装器，也不写 `lib/` 或宿主模块。安装器故障测试同样只操作内存文件系统。

## 安装到 dsh

需要 `dsh` 和 `pnpm`。在一个只含 `dsh-base` 的 profile 里链接本插件：

```powershell
# 1. 用 acp 模板建 profile，然后把应用层换掉
dsh zf-tui --from-default-profile acp --dump-default-config | Out-Null
#    编辑 ~/.dsh/profiles/zf-tui/package.json，
#    把 dsh.profile.bundles 改成 ["@deepseek-ai/dsh-base"]

# 2. 以链接方式安装；dsh 会把它记为这个 profile 的 bundle
dsh plugin --profile zf-tui add link:D:/project/native-agent-tui/dsh-plugin
```

- 模板自带的 `@deepseek-ai/dsh-acp-app` 必须去掉：它和本插件都要独占 stdout。
- 用 `link:` 而不是 `file:`。`file:` 会在安装时复制一份，之后的构建结果不会生效。
- 本插件的 `cordis.patch.yml` 同时装入 `@deepseek-ai/dsh-tool-ask-user`：`dsh-base` 不带模型的提问工具 `ask_user_question`，而本插件能把问题送到 TUI 的请求面板，所以由它带上。问题里的 `multiSelect` 原样转给 TUI。
- 模型 provider 配在 profile 自己的 `cordis.patch.yml` 里，和 dsh 其他 profile 的写法相同；凭据在 `~/.dsh/.credentials.yaml`，各 profile 共用。

## 方法

| 方法 | 返回 |
|---|---|
| `zf/hello` | 协议版本、插件版本、ACP 服务的状态（`loading` / `mounted` / `unavailable`）、本插件的方法列表 |
| `zf/session/list` | 参数 `cwd`（可选，只列这个工作区）。返回根会话 `sessions[]`：`id`、`cwd`、`createdAt`、`updatedAt`、`title`（dsh 的标题，没有则缺省）、`live`（是否已在本进程加载），按时间倒序；子代理会话不列出。`titles` 说明标题服务是否可用 |
| `zf/skills/list` | 参数 `cwd`（可选）。返回 dsh 技能注册表里所有已安装来源的技能 `skills[]`：`name`、`description`、`source`、`path`（由插件直接提供、没有文件的技能缺省）、`modelInvocable`、`userInvocable`；不加载技能正文 |
| `zf/settings/describe` | dsh 设置服务的各命名空间：`ns`、`applies`、`revision`、`value`（密钥字段由宿主脱敏）；带参数 `ns` 时只返回该命名空间并附上 `schema` |
| `zf/permission/describe` | 参数 `sessionId`（已在本进程加载的会话）。返回 `current`（该会话生效的权限预设，与任何预设都不匹配时为 `custom`）、`default`（新会话的默认预设）、`presets[]`（`value`、`name`、`description`） |
| `zf/permission/set` | 参数 `sessionId`、`preset`，可选 `remember`。通过 dsh 的权限预设服务切换该会话的预设（沙箱模式 + 审批策略），立即生效；未知预设由 dsh 拒绝。`remember` 为真时同时写入设置，成为新会话的默认值，返回里的 `remembered` 说明是否写成 |
| `zf/provider/list` | `llm-pi-ai` 命名空间里可配置的供应商：`name`、`baseURL`、`api`、`apiKeyEnv`、`models[]`（`id`、`name`、`contextWindow`），以及设置是否可写 |
| `zf/provider/save` | 参数 `provider`、`baseURL`、`api`、`model`、`credential`，可选 `modelName`、`contextWindow`、`apiKey`。新增供应商，或修改它并新增/修改其中一个模型；未提到的模型和字段保留。经设置服务写入，**立即生效**，不合法的值由 dsh 的 schema 拒绝。`apiKey` 交给 dsh 的凭据服务，不会回传 |
| `zf/provider/remove` | 参数 `provider`。撤销用户对该供应商的修改；profile 自带的供应商仍在 |
| `zf/session/history` | 参数 `sessionId`。返回 `messages[]`：`role`（`user` / `assistant` / `reasoning` / `tool`）、`text`、`truncated` 以及可用时的 `id`、`turnId`。保留真人输入、助手正文、服务端思考文本与工具状态摘要；不包含注入上下文和原始工具输出；最多 200 条，更早的计入 `omitted`。需要 dsh 的 `sessionQuery` 服务 |
| 其他 | ACP v1，见 dsh 的 `@deepseek-ai/dsh-acp` |

未知的 `zf/` 方法返回 `-32601`。输入流关闭时，ACP 连接随之关闭并排空它的 Agent，然后宿主正常退出。

## 约定

- stdout 只写协议帧，日志走 dsh 的 logger。
- 只通过 `ctx` 上的服务读写 dsh 的状态，不直接读写 `~/.dsh` 下的文件。
- dsh 的包不是本包的依赖：ACP 服务在运行时从加载本插件的那个 `dsh` 安装里解析，保证只有一个 Cordis 实例、版本始终与宿主一致。用到的宿主接口以结构类型声明在 `src/host.ts`。
