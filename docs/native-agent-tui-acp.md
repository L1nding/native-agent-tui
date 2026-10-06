# DeepSeek Harness ACP backend

The first ACP backend is selected with `--backend deepseek-acp`. Codex remains
the default and keeps its pinned `0.159.2` compatibility checks.

## Process boundary

The ACP process is started as:

```text
dsh --profile acp
```

`--dsh PATH` and `--profile NAME` override the executable and profile. ACP
stdout is newline-delimited JSON-RPC. `PipeTransport` bounds each frame and
queued bytes, gives the writer one owner, and drains stderr in fixed chunks.
On Windows the existing owned-process job cleans up the complete child tree.

## Core seam

`src/backend/acp.rs` owns the process and translates ACP messages through a
bounded duplex bridge. Core continues to receive the existing typed
`Envelope` stream, so Ratatui only sees `CoreSnapshot` and typed commands.

The bridge maps `initialize`, `session/new`, `session/prompt`,
`session/update`, `session/request_permission`, `session/cancel`, and
`session/close`. ACP updates become existing assistant, reasoning, tool,
usage, approval, and terminal facts. ACP-only fields that do not have a Core
equivalent stay unavailable; the bridge never fabricates Codex child or model
metadata.

ACP prompt turns are single-flight. Core request deadlines still apply to
startup, prompt, and interruption acknowledgements. A cancel notification is
never treated as a completed turn; only the ACP stop reason or a disconnect can
end the turn.

## Validation

Use the normal Rust checks:

```text
cargo fmt --all -- --check
cargo check --all-targets
cargo test --all-targets
```
