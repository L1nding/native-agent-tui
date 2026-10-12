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
- 模型 provider 配在 profile 自己的 `cordis.patch.yml` 里，和 dsh 其他 profile 的写法相同；凭据在 `~/.dsh/.credentials.yaml`，各 profile 共用。

## 方法

| 方法 | 返回 |
|---|---|
| `zf/hello` | 协议版本、插件版本、ACP 服务的状态（`loading` / `mounted` / `unavailable`）、本插件的方法列表 |
| `zf/session/list` | dsh 会话持久化服务列出的会话（`header`、`revision`、`sizeBytes`） |
| 其他 | ACP v1，见 dsh 的 `@deepseek-ai/dsh-acp` |

未知的 `zf/` 方法返回 `-32601`。输入流关闭时，ACP 连接随之关闭并排空它的 Agent，然后宿主正常退出。

## 约定

- stdout 只写协议帧，日志走 dsh 的 logger。
- 只通过 `ctx` 上的服务读写 dsh 的状态，不直接读写 `~/.dsh` 下的文件。
- dsh 的包不是本包的依赖：ACP 服务在运行时从加载本插件的那个 `dsh` 安装里解析，保证只有一个 Cordis 实例、版本始终与宿主一致。用到的宿主接口以结构类型声明在 `src/host.ts`。
