# native-agent-tui

A Rust terminal client for the local Codex app-server. Interactive and headless runs share the same Core, process transport, and lifecycle rules.

## Run

Install Rust and make an authenticated Codex CLI available on `PATH`. The current protocol baseline is Codex **0.159.2**. `--codex PATH` or `CODEX_BIN` selects another executable; Windows uses `codex.cmd` by default.

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

The client inherits Codex's Windows sandbox implementation. If shell preflight times out with an elevated sandbox, check that sandbox's setup. An explicit unelevated override is also available:

```text
cargo run --locked -- --check-shell --windows-sandbox unelevated
cargo run --locked -- --sandbox read-only --windows-sandbox unelevated
```

This flag leaves the selected `--sandbox` policy in place. Preflight uses that policy, runs before any model turn, and stops execution if it fails.

## Controls

| Key | Action |
| --- | --- |
| Enter / Shift+Enter | Send task or answer / insert newline |
| Left, Right, Home, End | Move through input |
| Backspace, Delete, Ctrl+U | Edit or clear input |
| Ctrl+C | Request interruption; wait for the server's terminal event |
| Ctrl+Q / Ctrl+D | Quit and clean up owned processes |
| PageUp / PageDown, Ctrl+Home / Ctrl+End | Scroll conversation |
| Ctrl+Y / Ctrl+N | Approve once / decline the selected request |
| F1 / F2 | Show help / select next pending request |
| F3 | Cycle root and child conversations; input still submits a root task |
| F4 / Up, Down | Open tasks / select a task |
| F5 / F6 | Pause workflow dispatch / pause the selected root task |
| F7 / F8 | Cancel selected task / explicitly retry a failed root task |
| + / - | Adjust queued root priority in the task panel |
| F9 twice | Stop new dispatch and interrupt known workflow tasks |
| F10 | Edit temporary attention thresholds; Up/Down class, Tab field, Enter apply |
| F11 / PageUp, PageDown | Open activity evidence / scroll its details |
| Ctrl+Enter | Explicitly queue a root task during a running turn |
| Escape | Close help/tasks and clear input |

Chinese, combining characters, and emoji are edited as whole graphemes. Secret answers are masked and kept outside conversation history. A task draft is saved while answering questions. Messages and queues have byte limits; history truncation is visible.

## Implementation status

Live execution covers startup checks, repeated conversation turns, streamed/final root and child messages, approvals, questions, usage, interruption, disconnects, and Windows process cleanup. The UI sends typed commands and reads Core snapshots.

The root can call `wait_for_subagent_completion` with `{"targets":[]}` to capture all currently known direct children, or list child thread IDs, paths, or registered nicknames. Waiting has no deadline. Current child turns release it when all complete or any fails/is interrupted; old completions and status messages cannot release a new turn. Child requests remain answerable, and root input queues until the current root turn completes (at most eight tasks). Interruption or failure retains dependent tasks as blocked for inspection or explicit retry.

Startup obtains the effective catalog with `codex debug models`, writes a private temporary copy with `tool_mode: direct`, and removes it when the app-server owner exits. User configuration stays intact. This requires a Codex version supporting that command and the pinned protocol.

Dependency scheduling and task controls now run through Core. `--workflow FILE [--headless]` validates a JSON task DAG before launching; headless execution succeeds only when all root tasks succeed. See [task controls and limits](docs/scheduler-usage.md) and [scheduler validation](docs/scheduler-validation.md). Enforced native capacity, execution recovery, and detailed diagnostics remain planned; see [remaining work](docs/implementation-status.md).

## Activity observation

Activity evidence and silence attention are now projected by Core for each agent, tool, and interaction. F10 changes session thresholds; F11 shows evidence, waiting targets, elapsed/silence times, and configuration sources. See [activity observation](docs/activity-observation.md) and [validation](docs/observation-validation.md).

## Journal and replay

Sessions now persist redacted state in the OS user data directory. `--sessions` lists this workspace's history; `--replay SESSION_ID [--since SEQ] --json-events` reads a fixed committed prefix without launching Codex or answering historical requests. F11 shows the session and committed sequence. Select a storage directory with `--journal-dir PATH`.

Default retention is 30 days / 500 MiB; active and uncertain sessions are protected. Missing terminal records remain unknown. New sessions use schema 2; schema 1 history stays readable. See [storage and replay](docs/journal-replay.md) and [validation](docs/journal-validation.md).

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

### Optional live verification (Windows)

The local provider fixture requires Codex 0.159.2 and Python. It uses an isolated `CODEX_HOME`, no API keys, and localhost Responses SSE. It holds two child turns and counts provider requests before releasing each one:

```powershell
$env:NATIVE_AGENT_TUI_PYTHON = (python -c 'import sys; print(sys.executable)')
cargo nextest run --locked --run-ignored only live_app_server_gate --no-capture
```

Set `NATIVE_AGENT_TUI_PYTHON` to the actual Python executable if `python` resolves to a Windows Store alias. The fixture changes `use_responses_lite` only in its isolated catalog so its ordinary SSE responses match the selected wire format.
