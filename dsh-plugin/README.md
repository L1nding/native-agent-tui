# zf-tui-bridge

本 TUI 的 dsh 插件。它运行在 dsh 进程内部，通过 stdio 上逐行的 JSON-RPC 2.0 为 Rust 界面提供 dsh 自己的会话、标题、命令和设置，TUI 不再另存一份。

```text
native-agent-tui (Rust) ──stdio JSON-RPC──> dsh --profile zf-tui ──进程内──> dsh 的服务
```

## 安装到 dsh

需要 `dsh` 和 `pnpm`。在一个只含 `dsh-base` 的 profile 里加入本插件：

```powershell
# 1. 用 acp 模板建 profile，然后把应用层换掉
dsh zf-tui --from-default-profile acp --dump-default-config | Out-Null
#    编辑 ~/.dsh/profiles/zf-tui/package.json，
#    把 dsh.profile.bundles 改成 ["@deepseek-ai/dsh-base"]

# 2. 安装本插件；dsh 会把它记为这个 profile 的 bundle
dsh plugin --profile zf-tui add file:D:/project/native-agent-tui/dsh-plugin
```

`acp` 模板自带的 `@deepseek-ai/dsh-acp-app` 必须去掉：它和本插件都要独占 stdout。

## 当前提供的方法

| 方法 | 返回 |
|---|---|
| `initialize` | `protocolVersion`、`serverInfo` |
| `session/list` | dsh 会话持久化服务列出的会话（`header`、`revision`、`sizeBytes`） |

未知方法返回 `-32601`。输入流关闭时宿主进程退出。

## 约定

- stdout 只写协议帧，日志走 dsh 的 logger。
- 只通过 `ctx` 上注入的服务读写 dsh 的状态，不直接读写 `~/.dsh` 下的文件。
- 插件源码是不需要构建的 ES 模块，改动后重启 profile 即生效。
