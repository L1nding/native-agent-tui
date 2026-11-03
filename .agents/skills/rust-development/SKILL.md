---
name: rust-development
description: "Use for Rust work in this repository: Cargo setup, async execution, JSONL/RPC transport, scheduler state, terminal UI, tests, formatting, linting, and Windows process cleanup."
---

# Rust development

Use this skill when changing Rust code, Cargo metadata, execution boundaries, or tests for the native agent TUI.

## Working sequence

1. **Read the boundary first.** Check `AGENTS.md` and the relevant design document in `docs/`. Keep Core as the owner of execution facts; UI consumes read-only snapshots and submits typed commands.

   **Done when:** the change has a named module owner, public seam, and event/command path.

2. **Make the smallest typed seam.** Prefer enums and newtypes for protocol IDs, lifecycle states, commands, events, generations, and errors. Keep JSON decoding, process I/O, scheduling, and rendering behind their module interfaces. Preserve stable IDs and generation fields across asynchronous work.

   **Done when:** invalid states are rejected at the seam and callers do not need to inspect raw JSON or mutable Core state.

3. **Keep async ownership explicit.** Use one owner for mutable execution state. Background tasks send commands or events through bounded channels; they do not mutate Core directly. Use watch-style snapshots for readers and join/exit handles for task lifetime. Treat cancellation, EOF, disconnects, and unknown external outcomes as state transitions.

   **Done when:** every spawned task has an owner, a shutdown path, and a testable result or event path.

4. **Validate protocol and transport at the edge.** Decode one complete JSONL frame at a time, enforce configured byte limits, validate the envelope before applying it, and preserve unknown or unsupported messages as explicit errors. Keep the stdin writer single-owner and make stdout ordering observable in tests. Do not retry side effects unless the protocol proves the operation is safe to repeat.

   **Done when:** malformed input, oversized input, EOF, disconnect, stale generation, and unsupported schema cases have explicit behavior.

5. **Test observable behavior.** Test through public seams such as `ClientHandle`, `TransportAdapter`, and scheduler command/event interfaces. Cover protocol validation, event ordering, gate release, stale generations, approvals, disconnects, Windows child-process cleanup, and UI rendering with a fake terminal backend. Name tests after the behavior they prove.

   **Done when:** the changed behavior has a deterministic regression test or a documented reason a test cannot exercise it.

6. **Run the repository checks.** When a Cargo package exists, inspect its toolchain, manifest, and CI configuration first. Run the narrowest relevant checks, then the repository's configured baseline. Common checks are:

   ```text
   cargo fmt --all -- --check
   cargo check --all-targets
   cargo clippy --all-targets
   cargo test --all-targets
   ```

   Match lint levels and feature flags to the repository configuration; do not make `-D warnings`, `--all-features`, or strict Clippy groups a universal rule. If a check is unavailable because the package has not been created yet, report that exact limitation.

   **Done when:** formatting, compilation, linting, and tests pass, or each failure is tied to an existing unrelated issue.

## Project-specific rules

- Keep the planned modules under `src/` and integration tests under `tests/`.
- Keep `ClientHandle`, `TransportAdapter`, scheduler command/event interfaces, and UI snapshot boundaries small and stable.
- Represent `Unknown`, `Disconnected`, stale generations, pending approvals, and uncertain external outcomes explicitly. Do not infer them from idle time, log text, or a redraw.
- Keep JSON-RPC, process management, gate logic, and mutable execution facts out of views.
- Bound log, telemetry, and queue memory. Redact credentials, full prompts, and unsanitized tool output before diagnostics or export.
- On Windows, test process-tree termination and handle cleanup through the transport seam; do not rely on Unix-only signals.
- When monitoring a live TUI, correlate the current process, journal session, DSH session events, and durable tool-operation receipts. Orca screen snapshots and `tui-idle` alone do not prove completion or a stall. Keep delegated monitors read-only; the session owner checks outstanding operations before requesting graceful shutdown. After an interruption, inspect the original operation ID before deciding whether recovery permits another invocation.
- For DSH background delegation or parent continuation changes, read the lifecycle contract in [`dsh-plugin/README.md`](../../../dsh-plugin/README.md). Verify two independently settled children across the real ACP bridge transport, including delayed output, failure and stop; keep DSH as the sole sender of its settlement messages and automatic model requests.

## DSH image context

- DSH may merge a complete small domain receipt and an image-spill footer into one text block. Recover only the exact known zero-text-loss wrapper at the ACP normalization seam, within the original wrapper, structured-result and content-block budgets; never follow the spill locator. Keep private fact SHA, byte count, context and proposal binding verification unchanged. Truncated text, ambiguous receipts and unknown suffixes remain explicit synchronization failures. Replay the actual result to distinguish receipt transport failure from an invalid proposal; do not replay modeling operations or rerender images to clear the error.
- For ACP context overflow, distinguish the provider's terminal response, adapter window checks, and DSH compaction events. A successful compaction marker alone does not prove pressure fell; inspect retained image occurrences and the next request's usage.
- Read [`dsh-plugin/README.md`](../../../dsh-plugin/README.md) before changing the image-budget seam. Keep model selection and domain authority intact; apply the checked host repair to the actual DSH entry and verify it before the session owner restarts. When multiple DSH installations exist, pin `--dsh` or `DSH_BIN` to the launcher belonging to the checked `--host-entry`, run the installer with `--check` before launch, and verify the spawned host path; checking another installation does not validate this session.
- Bind each image batch's observations to full image identities, reader arguments, build/evidence receipts, and assistant message IDs. Carry these records unchanged across summaries; use existing observations and sealed artifacts for recovery, and reread only explicitly missing evidence.
- Complete the offline trace regression with the installed host modules: meter pricing and request selection must agree, new unobserved images must survive every offload source including byte limits, summary inputs must fit their budget, and all original identities/observations must remain recoverable. Verify installer rollback after partial writes or replacement failures. During an active probe use the documented in-memory source mode; only install after the session owner confirms closure. Report conservative visual estimates separately from provider-reported token usage and online validation.

## References

Use the source notes when choosing an API or resolving a rule: [`docs/rust-development-skill-research.md`](../../../docs/rust-development-skill-research.md). The notes link to the owning Rust, Cargo, Clippy, rustfmt, Tokio, and Ratatui documentation.
