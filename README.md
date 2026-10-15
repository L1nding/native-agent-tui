# native-agent-tui

A Rust terminal client built on [dsh](https://github.com/deepseek-ai) (DeepSeek Harness). dsh runs the agent, its plugins and its sessions; this program is the terminal front end. Interactive and headless runs share the same Core, process transport, and lifecycle rules.

```text
native-agent-tui (Rust) ──stdio JSON-RPC──> dsh --profile zf-tui ──> zf-tui-bridge plugin
                                                                      ├─ zf/* methods: answered from dsh services
                                                                      └─ everything else: dsh's own ACP server
```

## Run
Install Rust and dsh, then install the bridge plugin from this repository into a dsh profile named `zf-tui` (see [dsh-plugin/README.md](dsh-plugin/README.md)):

```text
cd dsh-plugin && pnpm install && pnpm run build
dsh plugin --profile zf-tui add link:<absolute path to this repository>/dsh-plugin
```

`--dsh PATH` or `DSH_BIN` selects the launcher (Windows uses `dsh.cmd` by default); `--profile NAME` selects another profile. Providers, models and credentials are dsh's own configuration.

```text
cargo run --locked
cargo run --locked -- --tui "Inspect this repository"
cargo run --locked -- --workflow docs/workflow-example.json
cargo run --locked -- --run "Reply with exactly READY" --model PROVIDER/MODEL
cargo run --locked -- --run "Summarize this repository" --permission read-only
cargo run --locked -- --help
```

The TUI requires an interactive terminal. `--run` prints streamed agent text and returns a nonzero exit code for failure, interruption, or an uncertain result. Headless runs decline approvals and interrupt when user input is required.

A session is ready once dsh confirms it; nothing is executed at startup. Command execution and its isolation belong to dsh: a session runs under one of dsh's **permission presets** (a sandbox mode bundled with an approval policy, for example `read-only`, `workspace-write`, `danger-full-access`). `--permission PRESET` selects one for a run. The session is not ready, and no task starts, until dsh confirms exactly that preset; an unknown preset or a refusal ends the run with an error.

## 建模

Windows 可运行 `./scripts/start-modeling.ps1` 打开医院项目（默认使用 `target/workflow-final/release/native-agent-tui.exe`）。脚本固定只读通用工具权限并检查项目路径，支持 `-ModelerRepo`、`-Project`、`-Model`、`-Binary`。F4 查看节点图，↑↓ 选择节点看详情，Tab 单独查看 Agent 调度；F6 查看具体建模方案。

建模使用普通 dsh 会话、模型和权限配置。Agent 通过领域工具选择楼层或楼栋、准备方案、应用及构建；F6 展示 Core 校验后的业务事实，`Ctrl+Y` / `Ctrl+N` 批准或拒绝当前具体版本。

```text
cargo run --locked -- --cwd D:/project/blender-agent-modeler --domain-project projects/hospital-site --permission read-only --tui "建模八层，先给我看方案"
```

恢复既有运行时使用其 `--domain-context FILE`，先查询原操作和产物，再继续复核。图像模型需要声明并实际接收图像输入。项目入口、来源未决项及验收边界见下方领域会话说明和[入口迁移记录](docs/dsh-modeling-entry-migration.md)。旧 `--modeling`、`--modeling-project`、`--target` 和 `--change-context` 参数已移除。

## Controls

| Key | Action |
| --- | --- |
| Enter / Ctrl+O (Shift+Enter when supported) | Send task or answer / insert newline |
| Left, Right, Home, End | Move through input |
| Backspace, Delete, Ctrl+U | Edit or clear input |
| Ctrl+C | Request interruption; wait for the server's terminal event |
| Ctrl+Q / Ctrl+D | Quit and clean up owned processes |
| Ctrl+F | Search retained conversation text; Enter searches/opens, Esc closes |
| Ctrl+T | Browse live evidence; F1 lists filters, navigation and local bookmarks |
| Ctrl+E / `/tools` | Cycle terminal, diff and generic tool cards through preview (three output rows), expanded output and hidden tools |
| PageUp / PageDown, Ctrl+Home / Ctrl+End | Scroll conversation |
| Ctrl+Y / Ctrl+N / Ctrl+B | Accept / decline / cancel the selected approval when allowed |
| F1 / F2 | Show the grouped key overlay / select next pending request |
| F3 | Cycle root and child conversations; input still submits a root task |
| F4 / Up, Down | Open tasks / select a task |
| F5 / F6 | Pause workflow dispatch / pause the selected root task |
| F7 / F8 | Cancel selected task / explicitly retry a failed root task |
| + / - | Adjust queued root priority in the task panel |
| F9 twice | Stop new dispatch and interrupt known workflow tasks |
| F10 | Edit temporary attention thresholds; Up/Down class, Tab field, Enter apply |
| F11 / PageUp, PageDown | Open activity evidence / scroll its details |
| F12 | Browse retained sessions and export redacted historical evidence |
| Left on an empty task input | Sessions page: this workspace's dsh sessions by dsh's own titles, newest first; the one waiting for an answer comes first. Enter resumes the selected session here and shows its earlier messages; `r` refreshes; Right or Esc returns. Sessions are stored by dsh (`sessions/` under `DSH_HOME`) and are the same ones its other front ends list; this program keeps no copy |
| Ctrl+P | Open the local command palette; type to filter, Enter to open a panel, Esc to close |
| `/model` + Enter | Pick the model from the backend's list; `/model NAME` selects it directly. Applies from the next turn and is remembered in `.zf/preferences.json` in the user's home directory; `--model` overrides it for one run |
| Ctrl+N / Ctrl+E in the `/model` list | Add a provider and model, or edit the highlighted one. Saved through dsh's settings service, the same store its own settings pages use; an entered API key goes to dsh's credential service and is not kept by this program. A saved provider is usable at once, without restarting dsh |
| Ctrl+T in the `/model` list or the provider form | Test a model: a separate dsh process sends one short message through it and reports the reply time or the provider's error. The conversation is not touched. In the form, Ctrl+T saves first and keeps the form open |
| Ctrl+R in the `/model` list | Restart dsh without leaving the TUI, for changes dsh reads only at start (a newly installed plugin, a hand-edited profile file): a new dsh process resumes the same session, so the conversation continues. Saving a provider does not need it |
| `/setting` | Language and theme (saved to `.zf/preferences.json` in the user's home directory, applied at once; the `NATIVE_AGENT_TUI_LANG` / `NATIVE_AGENT_TUI_THEME` variables take precedence), dsh's permission preset, and the attention thresholds. The permission row shows the preset dsh reports for this session and offers only the presets dsh lists; a change applies to the session as soon as dsh confirms it and becomes dsh's default for new sessions. The header shows the confirmed preset |
| `/search` `/timeline` `/context` `/skills` `/requests` `/workflow` `/evidence` `/attention` `/help` `/agent` `/tools` `/quit` | Typed in the task input and sent with Enter; each opens the same panel as its key. A single `/word` is never sent to the model |
| `/SKILL-NAME [request]` | dsh's skills (built in, yours, and this project's, including those installed plugins provide) are listed in the same hint popup after the commands. Enter sends a task asking the agent to use that skill for the request. The agent loads the skill itself: dsh's protocol gives a client no direct way to run one, so this is a request to the model, not a guaranteed invocation |
| Ctrl+S / Ctrl+Enter when supported | Explicitly queue a root task during a running turn; submit an active answer form |
| Escape | Close help, panels, and notices; the draft is kept (Ctrl+U clears it) |

Each turn shows its tool calls as compact gray lines in the conversation (state, exit code, duration, command); Ctrl+T opens full details.

Chinese, combining characters, and emoji are edited as whole graphemes. Secret answers are masked and kept outside conversation history. A task draft is saved while answering questions. Messages and queues have byte limits; history truncation is visible.

Windows framed paste now preserves multiline Unicode input. Paste is limited to 32 KiB and cannot execute keyboard shortcuts; oversized paste is discarded. See [terminal input validation and host limits](docs/windows-terminal-input.md).

## Implementation status

Live execution covers startup checks, repeated conversation turns, streamed/final root and child messages, approvals, questions, usage, interruption, disconnects, and Windows process cleanup. The UI sends typed commands and reads Core snapshots.

The root can call `wait_for_subagent_completion` with `{"targets":[]}` to capture all currently known direct children, or list child thread IDs, paths, or registered nicknames. Waiting has no deadline. Current child turns release it when all complete or any fails/is interrupted; old completions and status messages cannot release a new turn. Child requests remain answerable, and root input queues until the current root turn completes (at most eight tasks). Interruption or failure retains dependent tasks as blocked for inspection or explicit retry.

Dependency scheduling and task controls now run through Core. `--workflow FILE [--headless]` validates a JSON task DAG before launching; headless execution succeeds only when all root tasks succeed. Native child count, depth, and active turn limits are configurable with `--max-native-children`, `--max-native-depth`, and `--max-native-turns`. See [task controls and limits](docs/scheduler-usage.md) and [scheduler validation](docs/scheduler-validation.md). Fine-grained resource budgets, execution recovery, and detailed diagnostics remain planned; see [remaining work](docs/implementation-status.md).

## Roadmap

The next release, `0.1.0-alpha`, covers reliable single-agent work, activity evidence, attention hints, and journal-based observation recovery. Existing child/Gate behavior remains covered by regression tests; V2 first validates 1–3 real concurrent direct children and handoff, with scheduling recovery and resource budgets after that.

### Supported platforms

`0.1.0-alpha` supports **Windows only**. Linux and macOS may compile, but process cleanup, terminal input, and startup have not been validated there; treat them as unsupported.

The dsh integration is **experimental** and may change without notice.

See the [product plan](docs/native-agent-tui-plan.md), [release checklist](docs/release-0.1.0-alpha.md), [observability contract](docs/native-agent-tui-observability.md), and [GitHub roadmap](https://github.com/L1nding/native-agent-tui/issues/1).

## Activity observation

Activity evidence and silence attention are now projected by Core for each agent, tool, and interaction. F10 changes session thresholds; F11 shows evidence, waiting targets, elapsed/silence times, and configuration sources. See [activity observation](docs/activity-observation.md) and [validation](docs/observation-validation.md).

## Journal and replay

Sessions now persist redacted state in the OS user data directory. `--sessions` lists this workspace's history; `--replay SESSION_ID [--since SEQ] --json-events` reads a fixed committed prefix without launching dsh or answering historical requests. F11 shows the session and committed sequence. Select a storage directory with `--journal-dir PATH`.

Default retention is 30 days / 500 MiB; active and uncertain sessions are protected. Missing terminal records remain unknown. New sessions use schema 2; schema 1 history stays readable. See [storage and replay](docs/journal-replay.md) and [validation](docs/journal-validation.md).

## History and export

F12 opens retained sessions while live execution continues. `--history [SESSION_ID]` opens the same read-only view without launching dsh. Browse events with Left/Right or `g`; press `e` to preview a range and save it to a new file.

```text
cargo run --locked -- --history
cargo run --locked -- --export SESSION_ID --since 0
cargo run --locked -- --export SESSION_ID --since 0 --output diagnostic.jsonl
```

Exports preserve recorded states and relationships, replace string identities with stable aliases, and exclude prompts, answers, and full text. Historical ages stay frozen; uncertain outcomes require review before starting a new task. See [history and export](docs/history-export.md) and [validation](docs/history-validation.md).

From the retained-session list or an opened session, press `/` or `Ctrl+F` to search committed evidence metadata. The search scans the selected retained sessions, `F6` changes the lifecycle/output/tool/request/waiting category, and Up/Down plus Enter opens the selected session/event hit. Search results are bounded and redacted, and never include prompts, answers, secrets, commands, paths, or raw tool output. See [historical evidence search](docs/history-evidence-search.md).

## Live JSONL

`--run TASK --json-events` and `--workflow FILE --headless --json-events` stream committed redacted state on stdout. A separate writer catches up from the journal; blocked output cannot hold Core. Broken pipes and sustained stalls stop the owner with a nonzero exit code. Final snapshots keep execution results and cleanup confirmation separate.

Approvals are declined; unavailable decline decisions and user input request interruption with fixed recorded reasons. JSONL does not accept answers on stdin. See [the stream contract and Python consumer](docs/jsonl-events.md) and [pipe validation](docs/jsonl-validation.md).

## Verify

```text
cargo fmt --all -- --check
cargo check --locked --all-targets
cargo clippy --locked --all-targets
cargo nextest run --locked
cargo build --locked --release
```

Use `cargo test --locked --all-targets` when nextest is unavailable. Contributor guidance and architecture documents are under [docs/](docs/AGENTS.md).

`python scripts/verify.py` runs these checks plus doctests and native CLI/Python fixtures, stopping on the first failure. Windows CI uses the same entry point with Rust 1.96.0; see [continuous verification](docs/ci-validation.md).

## Conversation search

Ctrl+F opens a separate search editor without changing task or secret-answer drafts. Enter searches retained live conversation text; Up/Down or n/N select matches, Enter locates the selected message, and Esc closes the search. Tab changes thread/subtree/path/all-agent scope; Ctrl+T edits an exact turn filter, F6 changes role, and F7 changes message state. F1 shows all search controls.

Results use a fixed snapshot. Press r to refresh; changed or evicted content cannot be opened from an old result. Search is literal, case-sensitive, and supports Chinese and emoji. It excludes answer drafts and historical journal text. See [search scope, limits and validation](docs/conversation-search.md).


## 建模业务视图与领域工具

普通 dsh 会话现在可以从项目目标开始，用 `--domain-project` 指定模型器工作目录内、包含 `project.yaml` 的相对项目路径：

~~~powershell
cargo run --locked -- --cwd D:/project/blender-agent-modeler --domain-project projects/hospital-site --model hi/gpt-6.1-sol --permission read-only --tui "建模八层，先读取项目和具体结构方案"
~~~

Agent 通过领域工具读取真实楼层目录、解析单层／范围／整栋目标并处理重复图纸，随后为每个选定楼层准备隔离 CAD 结构候选。项目可在 `project.yaml` 的 `component_sources.layers` 和可选 `rules` 声明构件来源图层及已有分类依据；F6 显示该范围。未配置时使用默认观察图层，扫描数量不能当作整个 CAD 的家具／洁具总数。每层具体方案都经 F6 的 `Ctrl+Y` 批准后，由同一 Agent 应用、构建及验证；目标选择不会授予场景写入批准。恢复使用原运行生成的上下文文件，`inspect_project.recovery_index` 可发现原目标与作用域，先读取持久操作／构建回执再决定后续动作。

已准备的楼层使用 `activate_project_floor` 切换并恢复原结构；每次切换保存新的激活代次，同一当前楼层重复调用保持幂等。`inspect_project_floor` 只读恢复当前楼层的丢失回执，不切换或初始化。源范围可以在项目 `component_sources` 中明确图层及已有规则；未配置时的默认观察范围不代表全图构件。

看图复核要求 dsh 的模型配置声明 `input: [text, image]`，并且实际图片进入模型上下文。`attached_png` 只说明领域工具返回了图片；遇到 `image unavailable` 时保持复核未执行。实际看图观察由 Agent 回复记录，人工复核仍需独立完成。所选楼层都已具体批准应用，且 CAD 轴网和绝对标高完整时，Agent 可用 `assemble_project_building` 生成独立楼栋快照，通过 `inspect_project_building`、`validate_project_building` 等待并验证，取消使用 `cancel_project_building`。同一 `build_id` 重复调用只查询原任务，失去 owner 的非终态保持 `unknown`。`check_project_building_connections` 和 `read_project_building_report` 提供封存连接检查及总报告；缺冻结来源证明、未验证相邻连接和人工复核保持 attention／not_run。F6 分别显示产物有效性、来源对账、连接检查和总报告；`audit_drawing` 展示当前楼层的分页检测候选；Agent 可读取候选 PNG 并记录判断，必要修正经 `prepare_drawing_correction` 形成新的具体批准版本，保留原 DXF。候选和记录判断都不等于审查通过。医院整层收尾、楼栋连接和旧入口删除条件见 [入口迁移记录](docs/dsh-modeling-entry-migration.md)。

已验证且当前的楼栋可以使用 `prepare_project_connection_change` 准备相邻两层的来源楼梯／竖井增补：明确下层、原楼栋输入绑定和已有楼板孔洞，返回独立连接作用域及具体提案。原楼层、上层与楼板保持只读，新连接必须单独 `Ctrl+Y` 批准，再复用应用／构建接口。通过 `assemble_project_building(..., connection_scope_id=...)` 创建含该授权的新楼栋任务；不覆盖旧任务。恢复用 `activate_project_connection_scope`／只读 `inspect_project_connection_scope`。缺孔洞或参数时保持未完成，不推断切孔、净高或建筑合规。详见 [领域契约](docs/modeling-domain.md)。

通过 --domain-context FILE 可将 modeling_domain MCP 与 F6 建模业务页挂到普通 dsh 会话。用户输入仍使用平台编辑器和 SubmitRootInput；dsh Agent 通过建模器 .agents/skills/building-modeling/SKILL.md 理解流程，使用平台现有模型、会话和权限。Ctrl+Y 只批准已展示的具体方案版本，由 Core 调受控人类授权 seam 后交给同一 Agent 应用。启动/会话切换后必须先确认 read-only。

~~~powershell
cargo run -- --cwd D:/project/blender-agent-modeler --domain-context PATH/TO/context.json --model hi/gpt-6.1-sol --permission read-only --tui "在首层新增两张床，先给我看方案"
~~~

运行上下文绑定 scene、floor、scope/protected_ids。领域工具支持来源扫描与实例对账、单层落地床/桌/椅/柜/盆/坐便器、未应用方案修订、依赖完整的部分采纳，以及已应用家具的移动、形状/参数替换和整组件删除。编辑保留稳定部件身份和共享定义，删除来源保持待确认，重新增补需明确重新开启。Python 独立异步 worker 构建封存快照，Core 不接管领域几何。详见 [建模业务协议](docs/modeling-domain.md)。

验证时可设置 CARGO_TARGET_DIR 使用独立目标目录，scripts/verify.py 会从该目录读取 release fixtures。避免占用用户正在运行的默认二进制文件。
