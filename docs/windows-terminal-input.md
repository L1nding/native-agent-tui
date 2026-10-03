# Windows 终端输入与粘贴

验证日期：2026-10-03。

## 使用

- `Ctrl+O` 插入换行；`Ctrl+S` 提交当前答案，或明确排队一个依赖当前根任务的新任务。
- 终端保留 Enter 修饰符时，`Shift+Enter`、`Ctrl+Enter` 继续使用原路径。
- 终端发送完整 bracketed-paste 标记时，多行中文、组合字符和 emoji 作为一个文本事件进入当前编辑区。CRLF、CR、LF 统一为 LF；搜索再把换行转为空格。
- 一次粘贴最多 32 KiB UTF-8 字节。超限整段丢弃，保留原草稿并显示提示；搜索字段仍有自己的 1024 字节上限。
- 粘贴正文中的控制字符属于文本，不触发提交、审批、搜索切换、中断或退出。任务、秘密答案和搜索草稿继续分开保存。

## 根因与实现边界

原生最小复现中，Crossterm 0.29.0 的 Windows `WindowsEventSource` 将输入解码为单独按键，没有 `Paste` 分支。换行成为 Enter，搜索编辑因此在首行后结束。开启 VT 输入但继续使用旧解析器，又会丢失部分控制字符。

`src/ui/input/` 现在拥有 Windows 输入适配：读取 `CONIN$` 的 Win32 记录，启用 VT 输入，按粘贴标记生成 `InputEvent::Paste`。UTF-16 高低代理项跨批次配对，并接受 Windows 用 Alt 键释放记录携带的字符；本机直接 `ReadFile` 实验会把这类 emoji 转为替代字符。

输入只有一个读取者。每批最多 64 条记录，每次调用最多处理 16 批；UI 每个 tick 最多消费 128 个事件。序列缓存最多 64 字节，粘贴缓存最多 32 KiB；超限后继续丢弃到结束标记，不把余下正文重新解释为按键。未结束的粘贴不交付部分文本。退出或初始化失败时恢复输入模式与 raw/alternate-screen 状态，释放自有原生句柄。非 Windows 路径继续使用 Crossterm。

这些事件只属于 UI。Core 的执行事实、RPC、Gate 和 journal 接口没有输入解码职责。

## 自动验证

```text
cargo nextest run --locked -E "test(ui::input::) | test(terminal_paste) | test(portable_newline)"
cargo build --locked --example input_fixture
python tests/fixtures/windows_terminal_input_check.py --fixture target/debug/examples/input_fixture.exe
python scripts/verify.py --live
```

13 项 Rust 回归覆盖 Unicode 分段、秘密草稿、粘贴控制字符、字节上限、序列缓存、未完成帧、原生记录修饰符和类型化提交。12 项直接 ConPTY 检查覆盖三种换行、控制字符正文、32 KiB 边界、超限丢弃、分段正文、未结束帧、功能键/导航、原生按键、兼容提交键、未知序列和终端模式恢复。原生检查已加入仓库验证脚本。

本轮 `python scripts/verify.py --live` 全部通过：182 项默认 Rust 测试、八项真实 Codex 检查及全部原生夹具；fmt/check/clippy/doctest/release 通过。它没有运行下述两项宿主能力探针。

真实 TUI 配合假 app-server 另行验证秘密多行粘贴、搜索、返回表单、`Ctrl+O` 增加第三行和 `Ctrl+S` 提交：粘贴/搜索零额外 RPC，三行中文/emoji 答案精确返回一次，根轮次启动数为 1，中断请求为 0。退出后进程身份检查通过，只读回放确认 Completed、清理和 journal 关闭；搜索标记及秘密答案未进入 journal/回放。该交互使用进程夹具，不调用模型。

## 仍需验收的宿主行为

本机 ConPTY 写入端若在 `ESC[200~` 或 `ESC[201~` 标记中间拆块，会在应用收到记录前吞掉前缀；5 ms 和 120 ms 间隔均已复现。完整标记、分段正文通过。应用收到的 UTF-16 记录批次分段由解析回归覆盖；它无法从已丢失标记的普通按键还原粘贴意图。

本机 VT 路径也会把编码 Win32 记录中的物理 Shift/Ctrl+Enter 转为普通 Enter。默认原生检查验证收到的真实记录形态，`Ctrl+O`、`Ctrl+S` 保持独立。以下额外探针要求宿主保留对应能力，在本机当前失败，未纳入通过结论：

```text
python tests/fixtures/windows_terminal_input_check.py --fixture target/debug/examples/input_fixture.exe --split-markers
python tests/fixtures/windows_terminal_input_check.py --fixture target/debug/examples/input_fixture.exe --enter-modifiers
```

Windows Terminal、Orca 系统剪贴板及 IME 预编辑仍需单独验收。ConPTY UTF-8 注入通过不代表这些界面路径已经通过；Alpha/V2 完整交付仍未完成。
