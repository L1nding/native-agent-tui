# Durable outbox seam

`native_agent_tui::outbox::Outbox` is the first durable boundary for effects that
may change an external system. It is deliberately separate from the transport:
the Core records an intent before a writer sends anything, and the writer records
what was observed afterward.

Each intent stores only:

- workflow and task/attempt identity;
- request identity and method;
- a stable payload hash;
- one of `pending`, `sent`, `confirmed`, `unknown`, or `failed`.

The payload itself is never written. `Outbox` owns the append-only file and is
not cloneable, so callers have one mutable writer. Every append is followed by
`sync_data`; reopening the file reconstructs the committed state in sequence.
The Core uses this seam for app-server requests and responses: response-bearing
requests become `confirmed` only after the matching RPC response, while timeout
or disconnect leaves them `unknown`.

`OutboxSnapshot` is the read-only recovery view. `--recovery SESSION_ID` opens
that view without creating or appending a file, and reports status counts beside
the committed journal summary.

The state machine intentionally rejects `unknown -> sent` and all other replay
transitions. An uncertain side effect therefore stays uncertain until a future
user-facing command creates a new attempt with a new intent ID. This module does
not send RPCs, retry effects, or reconstruct the scheduler; those integrations
must remain explicit Core decisions.

Example:

```rust
let mut outbox = Outbox::open(path)?;
outbox.record_intent(OutboxIntent::new(
    7, workflow_id, Some(attempt), request_id, "turn/start", payload,
))?;
// The unique writer may now send the payload.
outbox.mark_sent(7)?;
outbox.mark_confirmed(7)?; // or mark_unknown(7) after a disconnect
```
