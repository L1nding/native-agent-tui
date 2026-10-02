# Repository Guidelines

## Project Structure & Module Organization

This repository is one Rust 2021 Cargo package. `src/client.rs` owns Core execution
and publishes snapshots; `src/ui.rs` renders them with Ratatui. Configuration,
protocol validation, process management, transport, interactions, and state have
separate modules. Gate, scheduler, RPC, and diagnostics modules contain foundations
whose live integration is tracked in `docs/implementation-status.md`.

Unit tests live beside their implementations. `tests/` is reserved for integration
tests. Architecture and workflow documents are in `docs/`; the application has no
separate asset pipeline.

Before changing execution boundaries, read `docs/native-agent-tui-core.md`.
Consult `docs/native-agent-tui-ui.md` for views and
`docs/native-agent-tui-workflow.md` for scheduling.

## Build, Test, and Development Commands

- `cargo run --locked -- --check-shell`: verify shell execution without a model turn.
- `cargo run --locked`: launch the interactive terminal client.
- `cargo run --locked -- --run "TASK"`: execute and stream a headless task.
- `cargo fmt --all -- --check`: check formatting; omit `--check` to apply it.
- `cargo check --locked --all-targets`: type-check library, binary, and tests.
- `cargo clippy --locked --all-targets`: run Rust lints.
- `cargo nextest run --locked`: run tests; use `cargo test --locked --all-targets`
  if nextest is unavailable.
- `cargo build --locked --release`: build the optimized executable.

## Coding Style & Naming Conventions

Use rustfmt's four-space indentation, `snake_case` functions/modules, `PascalCase`
types, and `SCREAMING_SNAKE_CASE` constants. Core owns mutable execution facts;
views consume snapshots and submit typed commands. Bound queues and stored text.
Preserve thread, turn, item, request, and generation identities across async work.

## Testing Guidelines

Use Tokio duplex streams for protocol ordering, `ClientHandle` for lifecycle
behavior, and Ratatui `TestBackend` for rendering. Windows cleanup tests launch
owned hidden processes. Name tests after observable behavior, such as
`failed_preflight_cannot_be_bypassed_by_submitting_a_task`. Cover changed lifecycle
paths and uncertain outcomes; no numerical coverage threshold is configured.

## Commit & Pull Request Guidelines

Existing commits use `feat:` and `fix:` subjects. Keep commits focused. Pull
requests should describe behavior, link relevant design documents or issues,
list validation results, and include terminal captures for visible UI changes.

## Configuration

Select Codex through `--codex` or `CODEX_BIN`. On Windows,
`--windows-sandbox unelevated` explicitly overrides the sandbox implementation;
`--sandbox` still selects execution policy. Keep credentials and private answers
out of logs and screenshots.
