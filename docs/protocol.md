# Protocol

Status: implemented in crates/protocol (version 1); that crate is the source of truth.

## Transport and framing

Unix domain socket at `$LAZYAGENTS_STATE_DIR/daemon.sock`. Every message is
`u32 length (big endian) + postcard payload`. A length above 16 MiB (`MAX_MESSAGE_LEN`)
is rejected.

## Connection contract

1. The daemon sends `Welcome { protocol_version, daemon_version }` as soon as a client
   connects.
2. The client sends `Hello { protocol_version }`. A mismatch is reported by the client as
   a version error; the fix is `lazyagents shutdown`, so the next command starts a
   matching daemon.
3. The client then sends `Request { id, request }`. The daemon answers each request with
   exactly one `Response { id, result }`, where `result` is `Ok(Response)` or
   `Err(message)`.
4. Pushes (`AgentUpdated`, `AgentRemoved`, `ProjectsChanged`, `Frame`) can arrive at any
   time, between responses.

## Messages

`ClientMessage`:

- `Hello { protocol_version }`
- `Request { id, request }`

`Request`:

- `ListProjects`
- `AddProject { path, name }`
- `RemoveProject { id }`
- `ListAgents`
- `CreateAgent(CreateAgent)`
- `KillAgent { id }`
- `RestartAgent { id, cols, rows }`
- `RemoveAgent { id }`
- `RenameAgent { id, name }`
- `Subscribe { id }`
- `Unsubscribe`
- `Input { id, event }`
- `Resize { id, cols, rows }`
- `Scroll { id, lines }`
- `Hook { id, kind, session_id }`
- `Shutdown`

`Response`:

- `Ok`
- `Projects(Vec<Project>)`
- `Project(Project)`
- `Agents(Vec<AgentInfo>)`
- `Agent(AgentInfo)`

`ServerMessage`:

- `Welcome { protocol_version, daemon_version }`
- `Response { id, result }`
- `AgentUpdated(AgentInfo)`
- `AgentRemoved(AgentId)`
- `ProjectsChanged(Vec<Project>)`
- `Frame(Frame)`

`Input`, `Resize` and `Scroll` are rejected for agents that are not running. A `Hook` for
an agent that is not running is ignored.

## Frames

`Subscribe { id }` makes the daemon push `Frame`s for that agent; one subscription is
active per connection. The first frame has `full: true` and carries every row. Later
frames carry only the changed rows plus the cursor position. A cell with an empty `text`
is the spacer that follows a wide character.

`InputEvent` is `Key`, `Paste(String)` or `Wheel { up, col, row }`; see
`crates/protocol/src/input.rs`.
