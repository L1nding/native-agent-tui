# native-agent-tui

A Rust terminal client for the local Codex app-server. Interactive and headless runs share the same Core, process transport, and lifecycle rules.

## Run

Install Rust and make an authenticated Codex CLI **0.159.2** available on `PATH`. Execution checks the exact CLI release before catalog lookup, then validates initialization and the new thread's reported release. `--codex PATH` or `CODEX_BIN` selects a compatible executable; Windows uses `codex.cmd` by default. Other versions are refused before model execution. See [backend compatibility](docs/codex-compatibility.md).

```text
cargo run --locked -- --check-shell
cargo run --locked
cargo run --locked -- --tui "Inspect this repository"
cargo run --locked -- --workflow docs/workflow-example.json
cargo run --locked -- --run "Reply with exactly READY" --sandbox read-only
cargo run --locked -- --help
```

The TUI requires an interactive terminal. `--run` prints streamed agent text and returns a nonzero exit code for failure, interruption, or an uncertain result. Headless runs decline approvals and interrupt when user input is required.

### Windows sandbox setup

The client inherits Codex's Windows sandbox implementation. The elevated sandbox needs a one-time administrator setup (repeated after some Codex upgrades). Started from app-server, that setup cannot show its UAC prompt, so shell preflight times out. Run it once in an interactive terminal and approve the prompt:

```text
codex sandbox -- cmd /c echo ok
```

Preflight runs `powershell.exe` because the elevated Codex 0.159.2 runner does not resolve a bare `pwsh` installed under the user profile. An explicit unelevated override is also available:

```text
cargo run --locked -- --check-shell --windows-sandbox unelevated
cargo run --locked -- --sandbox read-only --windows-sandbox unelevated
```

To keep this choice without passing the flag, add the following to the Codex `config.toml`; the client never edits that file for you:

```toml
[windows]
sandbox = "unelevated"
```

This flag leaves the selected `--sandbox` policy in place. Preflight uses that policy, runs before any model turn, and stops execution if it fails.

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
| Ctrl+P | Open the local command palette; type to filter, Enter to open a panel, Esc to close |
| Ctrl+S / Ctrl+Enter when supported | Explicitly queue a root task during a running turn; submit an active answer form |
| Escape | Close help, panels, and notices; the draft is kept (Ctrl+U clears it) |

Each turn shows its tool calls as compact gray lines in the conversation (command, state, exit code, duration); Ctrl+T opens full details.

Chinese, combining characters, and emoji are edited as whole graphemes. Secret answers are masked and kept outside conversation history. A task draft is saved while answering questions. Messages and queues have byte limits; history truncation is visible.

Windows framed paste now preserves multiline Unicode input. Paste is limited to 32 KiB and cannot execute keyboard shortcuts; oversized paste is discarded. See [terminal input validation and host limits](docs/windows-terminal-input.md).

## Implementation status

Live execution covers startup checks, repeated conversation turns, streamed/final root and child messages, approvals, questions, usage, interruption, disconnects, and Windows process cleanup. The UI sends typed commands and reads Core snapshots.

The root can call `wait_for_subagent_completion` with `{"targets":[]}` to capture all currently known direct children, or list child thread IDs, paths, or registered nicknames. Waiting has no deadline. Current child turns release it when all complete or any fails/is interrupted; old completions and status messages cannot release a new turn. Child requests remain answerable, and root input queues until the current root turn completes (at most eight tasks). Interruption or failure retains dependent tasks as blocked for inspection or explicit retry.

Startup obtains the effective catalog with `codex debug models`, writes a private temporary copy with `tool_mode: direct`, and removes it when the app-server owner exits. User configuration stays intact. This requires a Codex version supporting that command and the pinned protocol.

Dependency scheduling and task controls now run through Core. `--workflow FILE [--headless]` validates a JSON task DAG before launching; headless execution succeeds only when all root tasks succeed. Native child count, depth, and active turn limits are configurable with `--max-native-children`, `--max-native-depth`, and `--max-native-turns`. See [task controls and limits](docs/scheduler-usage.md) and [scheduler validation](docs/scheduler-validation.md). Fine-grained resource budgets, execution recovery, and detailed diagnostics remain planned; see [remaining work](docs/implementation-status.md).

## Roadmap

The planned V1 / `0.1.0-alpha` focuses on reliable single-agent work, activity evidence, attention hints, and journal-based observation recovery. Existing child/Gate behavior remains covered by regression tests; V2 targets scheduling for 1–3 direct children.

See the [product plan](docs/native-agent-tui-plan.md), [observability contract](docs/native-agent-tui-observability.md), and [GitHub roadmap](https://github.com/L1nding/native-agent-tui/issues/1). The JSONL/replay CLI shown in the design is planned, not an available command.

## Activity observation

Activity evidence and silence attention are now projected by Core for each agent, tool, and interaction. F10 changes session thresholds; F11 shows evidence, waiting targets, elapsed/silence times, and configuration sources. See [activity observation](docs/activity-observation.md) and [validation](docs/observation-validation.md).

## Journal and replay

Sessions now persist redacted state in the OS user data directory. `--sessions` lists this workspace's history; `--replay SESSION_ID [--since SEQ] --json-events` reads a fixed committed prefix without launching Codex or answering historical requests. F11 shows the session and committed sequence. Select a storage directory with `--journal-dir PATH`.

Default retention is 30 days / 500 MiB; active and uncertain sessions are protected. Missing terminal records remain unknown. New sessions use schema 2; schema 1 history stays readable. See [storage and replay](docs/journal-replay.md) and [validation](docs/journal-validation.md).

## History and export

F12 opens retained sessions while live execution continues. `--history [SESSION_ID]` opens the same read-only view without launching Codex. Browse events with Left/Right or `g`; press `e` to preview a range and save it to a new file.

```text
cargo run --locked -- --history
cargo run --locked -- --export SESSION_ID --since 0
cargo run --locked -- --export SESSION_ID --since 0 --output diagnostic.jsonl
```

Exports preserve recorded states and relationships, replace string identities with stable aliases, and exclude prompts, answers, and full text. Historical ages stay frozen; uncertain outcomes require review before starting a new task. See [history and export](docs/history-export.md) and [validation](docs/history-validation.md).

From the retained-session list or an opened session, press `/` or `Ctrl+F` to search committed evidence metadata. The search scans the selected retained sessions, `F6` changes the lifecycle/output/tool/request/waiting category, and Up/Down plus Enter opens the selected session/event hit. Search results are bounded and redacted, and never include prompts, answers, secrets, commands, paths, or raw tool output. See [historical evidence search](docs/history-evidence-search.md).

## Live JSONL

`--run TASK --json-events` and `--workflow FILE --headless --json-events` stream committed redacted state on stdout. A separate writer catches up from the journal; blocked output cannot hold Core. Broken pipes and sustained stalls stop the owner with a nonzero exit code. Final snapshots keep execution results and cleanup confirmation separate.

Approvals are declined; unavailable decline decisions and user input request interruption with fixed recorded reasons. JSONL does not accept answers on stdin. See [the stream contract and Python consumer](docs/jsonl-events.md) and [pipe / real Codex validation](docs/jsonl-validation.md).

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

### Optional live verification (Windows)

The local provider fixture requires Codex 0.159.2 and Python. It uses an isolated `CODEX_HOME`, no API keys, and localhost Responses SSE. It holds two child turns and counts provider requests before releasing each one:

```powershell
$env:NATIVE_AGENT_TUI_PYTHON = (python -c 'import sys; print(sys.executable)')
cargo nextest run --locked --run-ignored only live_app_server_gate --no-capture
```

Set `NATIVE_AGENT_TUI_PYTHON` to the actual Python executable if `python` resolves to a Windows Store alias. The fixture changes `use_responses_lite` only in its isolated catalog so its ordinary SSE responses match the selected wire format.

## Conversation search

Ctrl+F opens a separate search editor without changing task or secret-answer drafts. Enter searches retained live conversation text; Up/Down or n/N select matches, Enter locates the selected message, and Esc closes the search. Tab changes thread/subtree/path/all-agent scope; Ctrl+T edits an exact turn filter, F6 changes role, and F7 changes message state. F1 shows all search controls.

Results use a fixed snapshot. Press r to refresh; changed or evicted content cannot be opened from an old result. Search is literal, case-sensitive, and supports Chinese and emoji. It excludes answer drafts and historical journal text. See [search scope, limits and validation](docs/conversation-search.md).
