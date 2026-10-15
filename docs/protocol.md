# Protocol

Status: designed, not implemented. crates/protocol is the source of truth once it exists.

## Wire format

Unix domain socket at `$LAZYAGENTS_STATE_DIR/daemon.sock`. Frames are
`u32 length (big endian) + postcard payload`. First exchange is
`Hello { protocol_version }` ↔ `Welcome { protocol_version, daemon_version }`.

Client → daemon requests: `ListProjects`, `AddProject`, `RemoveProject`, `ListAgents`,
`CreateAgent`, `KillAgent`, `RestartAgent`, `RemoveAgent`, `RenameAgent`,
`Subscribe { agent_id }`, `Unsubscribe`, `Input { agent_id, InputEvent }`,
`Resize { agent_id, cols, rows }`, `Scroll { agent_id, delta }`, `GitDiff { agent_id }`,
`HookEvent { agent_id, event, payload }`, `Shutdown`.

Daemon → client events: `Response(..)` per request, `AgentUpdated(AgentInfo)`,
`AgentRemoved(id)`, `Frame { agent_id, full: bool, rows: Vec<(u16, Row)>, cursor }`,
`Error { message }`.

`InputEvent` = `Key { code, modifiers, kind }` | `Paste(String)` |
`Mouse { kind, button, col, row, modifiers }`.

Full message definitions live in `crates/protocol`; that crate is the source of truth.
