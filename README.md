# native-agent-tui

`native-agent-tui` is a Rust foundation for a terminal workspace that observes and coordinates agent execution.

## 0.1.0

The first release implements the testable Core slice described by the design documents:

- validated JSONL envelopes with numeric and string request IDs;
- explicit session phases and monotonic read-only snapshots;
- generation-aware child completion gates;
- dependency-aware scheduler readiness and priorities;
- pending approval/user-input request storage;
- a scripted transport seam for deterministic tests;
- a Tokio client command loop and a small CLI.
- a Ratatui/Crossterm dashboard that exposes the Core snapshot without owning execution state.

The external Codex app-server adapter and Ratatui renderer are intentionally the next integration layer. They should consume the existing `ClientHandle`, `TransportAdapter`, `CoreSnapshot`, and scheduler seams.

## Run

```text
cargo run
cargo run -- --tui "inspect the repository"
cargo run -- --help
cargo run -- --version
cargo run -- --check-shell
cargo run -- --run "inspect the repository"
```

Running without arguments opens the TUI in an interactive terminal. Use `q` or `Esc` to quit; `s` starts the session, `r` submits a demo root goal, `c` completes the root turn, `g` opens a demo child wait, `a` completes that child, and `x` stops the session.

## Verify

```text
cargo fmt --all -- --check
cargo check --all-targets
cargo clippy --all-targets
cargo test --all-targets
cargo build --release
```

See `docs/` for the Core, UI, workflow, and Rust-development design boundaries.
