# Protocol

Status: implemented in crates/protocol (version 1); that crate is the source of truth.

## Transport and framing

Unix domain socket at `$LAZYAGENTS_STATE_DIR/daemon.sock`. Every message is
`u32 length (big endian) + postcard payload`. A length above 16 MiB (`MAX_MESSAGE_LEN`)
is rejected.

## Connection contract

1. The daemon sends `Welcome { protocol_version, daemon_version }` as soon as a client
   connects.
2. The client compares the Welcome's `protocol_version` with its own. On a mismatch it
   fails with `client::VersionMismatch` without sending Hello. The fix is to stop that
   daemon: `lazyagents shutdown`, or `y` in the TUI's mismatch popup, sends it SIGTERM,
   and the next command starts a matching daemon. On a match it sends
   `Hello { protocol_version }`.
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
- `Hook { id, kind, session_id, source }`: `session_id` and `source` come from the
  Claude Code hook payload; `source` is `SessionStart`'s `startup`, `resume`, `clear`
  or `compact`
- `Shutdown`: answered after every agent's process tree has exited

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

`KillAgent`, `RemoveAgent` and `RestartAgent` stop the agent's whole process tree (see
[architecture.md](architecture.md#daemon-lifecycle)) before they are answered.
`Input` is rejected with an error for agents that are not running. `Resize` and `Scroll`
work for any agent that still has a terminal, including an exited one, and are rejected
for a stopped agent, which has none. `Subscribe` succeeds for any known agent.
A `Hook` for an agent that is not running, or is not a Claude agent, is ignored.

## Frames

`Subscribe { id }` makes the daemon push `Frame`s for that agent; one subscription is
active per connection, and a new `Subscribe` replaces it. Frames flow whenever the
agent has a terminal: an exited agent's last screen is sent, a stopped agent sends
nothing until it is restarted. A frame is full (`full: true`, every row) on subscribe and after a size change. Other
frames carry only the changed rows plus the cursor position. `Frame` has the fields
`agent`, `cols`, `rows`, `full`, `lines` and `cursor`. A cell with an empty `text`
is the spacer that follows a wide character.

`InputEvent` is `Key`, `Paste(String)` or `Wheel { up, col, row }`; see
`crates/protocol/src/input.rs`.
