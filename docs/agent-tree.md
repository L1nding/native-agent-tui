# Agent Tree Ordering

`AgentRegistry::snapshots()` returns agents in first-registration order. This is a local observation order for stable UI presentation; the app-server does not provide a creation timestamp that this view can use. `started_seq` tracks the current turn and must not be used to order agents because it changes when an agent starts another turn.

The UI projects that ordered snapshot into a parent-first depth-first tree. Children keep the order in which they first appeared in the snapshot, even if a child was registered before its parent. The snapshot's root thread ID identifies direct children of the root. A missing parent is shown as unavailable. Unconfirmed parent metadata and detached cyclic components are shown as unresolved.

Tree depth is capped at eight display levels to keep indentation bounded. Nodes beyond that depth remain visible and carry `[depth truncated]`. F3 selection follows the selected node within the visible panel; this is local rendering state and does not alter Core facts.

At 16 terminal rows or fewer, the panel switches to a three-line summary for the selected agent: its name, status and generation, then usage. Long names are shortened at Unicode grapheme boundaries and end with an ellipsis. Missing usage or generation remains `unavailable`; compact rendering reads the snapshot and sends no Core command.
