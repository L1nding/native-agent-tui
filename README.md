# native-agent-tui

A Rust terminal client for the local Codex app-server. Interactive and headless runs share the same Core, process transport, and lifecycle rules.

## Run

Install Rust and make an authenticated Codex CLI available on `PATH`. The current protocol baseline is Codex **0.159.2**. `--codex PATH` or `CODEX_BIN` selects another executable; Windows uses `codex.cmd` by default.

```text
cargo run --locked -- --check-shell
cargo run --locked
cargo run --locked -- --tui "Inspect this repository"
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
| Escape | Close help and clear input |

Chinese, combining characters, and emoji are edited as whole graphemes. Secret answers are masked and kept outside conversation history. A task draft is saved while answering questions. Messages and queues have byte limits; history truncation is visible.

## Implementation status

Live execution covers startup checks, repeated conversation turns, streamed/final agent messages, approvals, questions, usage, interruption, disconnects, and Windows process cleanup. The UI sends typed commands and reads Core snapshots.

The child completion Gate and dependency scheduler currently have isolated tests. Their connection to live child threads, dynamic tools, workflow controls, and durable recovery remains planned work. See [validation and remaining work](docs/implementation-status.md).

## Verify

```text
cargo fmt --all -- --check
cargo check --locked --all-targets
cargo clippy --locked --all-targets
cargo nextest run --locked
cargo build --locked --release
```

Use `cargo test --locked --all-targets` when nextest is unavailable. Contributor guidance and architecture documents are under [docs/](docs/AGENTS.md).
