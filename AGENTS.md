# Repository Guidelines

## Project Structure & Module Organization

This repository is a single Cargo package for a Rust agent TUI:

- `src/` contains the public module seams and implementations for Core, protocol, transport, scheduler, gate, client, diagnostics, configuration, and the Ratatui UI.
- `docs/` contains the Core, UI, workflow, and Rust-development design documents.
- `tests/` is reserved for integration tests; focused unit tests live beside their modules.

Keep the module boundaries described in `docs/native-agent-tui-core.md`, `docs/native-agent-tui-ui.md`, and `docs/native-agent-tui-workflow.md`. Core owns execution facts; UI should consume read-only snapshots and submit typed commands.

## Build, Test, and Development Commands

- `cargo fmt --all -- --check` — verify formatting.
- `cargo check --all-targets` — type-check all targets.
- `cargo clippy --all-targets` — run configured lints.
- `cargo test --all-targets` — run unit and integration tests.
- `cargo build --release` — build the optimized distributable binary.
- `cargo run -- --help` — run the binary while CLI options are being added.

Run formatting without `--check` when editing: `cargo fmt --all`.

## Coding Style & Naming Conventions

Use Rust 2021 conventions with four-space indentation. Prefer clear, typed interfaces and one owner for mutable execution state. Use `snake_case` for functions and files, `PascalCase` for types, and `SCREAMING_SNAKE_CASE` only for constants. Keep JSON-RPC, process management, and gate logic out of views.

## Testing Guidelines

Test through public seams such as `ClientHandle`, `TransportAdapter`, and scheduler command/event interfaces. Cover protocol validation, event ordering, gate release, stale generations, approvals, disconnects, Windows process cleanup, and UI rendering with a fake terminal backend. Name tests after observable behavior, for example `gate_does_not_release_on_idle`.

## Commit & Pull Request Guidelines

There is no Git history yet, so no repository-specific commit convention has been established. Use concise imperative subjects such as `Add protocol envelope types`; keep each commit focused. Pull requests should explain the behavior or design change, link relevant issues or documents, list validation commands, and include terminal screenshots or recordings for visible TUI changes.

## Architecture and Safety Notes

Treat Core as the single owner of execution facts. Preserve stable IDs and generation fields when handling asynchronous events. Do not log credentials, full prompts, or unsanitized tool output; represent uncertain external outcomes explicitly instead of retrying side effects automatically.

## Project Skills

For Rust implementation, Cargo changes, async boundaries, protocol transport, scheduler state, terminal UI, or related tests, load `.agents/skills/rust-development/SKILL.md`.
