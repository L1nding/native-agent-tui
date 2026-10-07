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
`session/close` and `session/set_config_option`. ACP updates become existing
assistant, reasoning, tool, usage, approval, and terminal facts. In-progress
tool updates stay open; only ACP terminal tool statuses become completed,
failed, or cancelled facts. ACP-only fields that do not have a Core equivalent
stay unavailable; the bridge never fabricates Codex child or model metadata.

`Config::mcp_servers` is a typed seam for ACP stdio and HTTP MCP definitions.
It is encoded at the protocol edge, so future configuration loading does not
expose raw ACP JSON to Core or UI.

ACP prompt turns are single-flight. Core request deadlines still apply to
startup, prompt, and interruption acknowledgements. A cancel notification is
never treated as a completed turn; only the ACP stop reason or a disconnect can
end the turn.

A JSON-RPC error response to `session/prompt` is a definite end of that turn,
so the bridge reports `turn/completed` with status `failed` and the agent's
error message. The session stays usable for a new task; only a disconnect or
an unparseable update leaves the outcome Unknown.

`--model` selects an ACP `model` config option after `session/new`, either as
`provider/model` (for example `hi/gpt-6-luna`) or as a model name offered by one
provider only. Unknown or ambiguous names fail thread start and list the
choices. dsh resets the reasoning effort when the model changes, so the bridge
restores the session's previous value when the new model offers it. Without
`--model`, the header shows the agent's current model.

The bridge acknowledges `turn/start` as soon as `session/prompt` is sent; the
prompt response arrives only when the whole turn ends and becomes
`turn/completed`. Tool output larger than 32 KiB is truncated and marked
instead of failing the turn. dsh puts the real command, justification, and
working directory in `rawInput`, so approvals and tool lines show those fields
rather than the bare tool name.

Permission decisions use the standard ACP option `kind` (`allow_once` before
`allow_always`, `reject_once` before `reject_always`), then option-id keywords.
Decline and cancel never fall back to an allow option: without a matching
reject option the bridge answers `cancelled`. dsh sends permission requests that
carry only `toolCallId`, so the bridge remembers each turn's `tool_call` input
and shows its command, justification, working directory, and requested sandbox
(for example `danger-full-access`).

Real backend check (2026-10-07, `dsh` 0.2.0-rc.2): the default DeepSeek
provider returned "Insufficient Balance". With `--model hi/gpt-6-luna` (a
provider copied from the user's dsh tauri profile into the acp profile patch
layer), read-only and file-editing tasks completed in the TUI with streaming,
tool calls, and a permission request.

## Validation

Use the normal Rust checks:

```text
cargo fmt --all -- --check
cargo check --all-targets
cargo test --all-targets
```

The bridge lifecycle and permission mapping are covered by a duplex fake ACP
server test. A Windows smoke check with the installed Harness is:

```text
cargo run -- --backend deepseek-acp --check-shell
```
