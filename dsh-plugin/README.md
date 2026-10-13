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
