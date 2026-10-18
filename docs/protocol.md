# Protocol

Status: implemented in crates/protocol (version 7); that crate is the source of truth.

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
   `Hello { protocol_version }`. The daemon closes a connection whose `Hello` names
   another protocol version, without reading or answering anything after it, because
   the variant indices of `Request` differ between versions.
3. The client then sends `Request { id, request }`. The daemon answers each request with
   exactly one `Response { id, result }`, where `result` is `Ok(Response)` or
   `Err(message)`. Requests that take time are answered when their work, done off the
   daemon's event loop, finishes: `KillAgent`, `RestartAgent`, `RemoveAgent`, `MergeAgent`,
   `GitStat`, `GitDiff`, and `CreateAgent` with a worktree. Their answers can follow later
   requests' answers, so a client matches answers by `id`; the rest come in request order.
4. Pushes (`AgentUpdated`, `AgentRemoved`, `AgentBell`, `ProjectsChanged`, `Frame`) can
   arrive at any time, between responses.

## Changing the wire format

postcard is not self-describing and encodes enum variants by index, so a client and a
daemon that disagree on any message type misread each other while both still pass the
version check. Any change to the encoded bytes of a message (a variant added, removed or
reordered, a field added, removed, reordered or retyped) bumps `PROTOCOL_VERSION`.
`crates/protocol/tests/wire.rs` enforces this: it encodes a fixed sample of every message
and compares it with `crates/protocol/tests/fixtures/wire-v<version>.txt`. After bumping,
`UPDATE_GOLDEN=1 cargo test -p protocol --test wire` writes the fixture for the new version;
it refuses to rewrite the fixture of a version that already has one. A new enum variant
does not compile in the test until its sample is added.

## Messages

`ClientMessage`:

- `Hello { protocol_version }`
- `Request { id, request }`

`Request`:

- `ListProjects`
- `AddProject { path, name }`
- `RemoveProject { id }`
- `ListAgents`
- `CreateAgent(CreateAgent)`: `project`, `name`, `kind`, `command`, `cols`, `rows`,
  `worktree: Option<WorktreeRequest>`, `prompt: Option<String>` and
  `setup: Option<String>`; a
  `WorktreeRequest { base: Option<String> }` makes the daemon create a git worktree for
  the agent first. A non-blank `prompt` goes to the agent on its first start only: a
  Claude agent gets `-- <prompt>` after its options, a generic agent has `<prompt>\r`
  written to its terminal once it first prints. A non-blank `setup` is a shell script
  run on the first start only, in the agent's terminal before its command, in a shell of
  its own: `sh -c 'sh -c "$1"; … shift; exec "$@"' lazyagents-setup <setup> <command>`,
  inside direnv when the worktree has an `.envrc`, so the command starts whatever the
  script does
- `KillAgent { id }`
- `RestartAgent { id, cols, rows }`
- `RemoveAgent { id, remove_worktree, force, delete_branch }`: `remove_worktree` also
  removes the agent's worktree and is refused while it has uncommitted changes unless
  `force` is set; `delete_branch` (with `remove_worktree`) also deletes its branch, and is
  refused, before anything is removed, unless the base contains every commit of the branch
- `MergeAgent { id }`: merges a worktree agent's branch into its base in the project's
  own checkout with `git merge --no-edit`; refused while the worktree has uncommitted
  changes, or the checkout is on another branch or has tracked changes. A failed merge is
  aborted, naming the conflicted files. Answered with `Done("merged <branch> into <base>")`
- `GitStat { id }`
- `GitDiff { id, scope }`: `scope` is `All` (committed and uncommitted changes since the
  comparison point), `Uncommitted` (working tree against `HEAD`) or `Committed` (comparison
  point to `HEAD`)
- `RenameAgent { id, name }`
- `Subscribe { id }`
- `Unsubscribe`
- `Input { id, event }`
- `Resize { id, cols, rows }`
- `Scroll { id, lines }`
- `Hook { id, kind, session_id, source, detail, subagent }`: `session_id` and `source`
  come from the Claude Code hook payload; `source` is `SessionStart`'s `startup`,
  `resume`, `clear` or `compact`. `detail` is one line of at most 120 characters: a
  `PermissionRequest`'s `tool_name` and the first of its `tool_input`'s `command`,
  `file_path`, `path`, `url`, `pattern`, `query`, `description` or `prompt`
  (`Bash: npm test`), or a `Notification`'s `message`. `subagent` is the payload's
  `agent_id`, set when the hook fired in a subagent
- `Usage { id, usage }`: from `lazyagents statusline`; ignored, like a `Hook`, for an agent
  that is not a running Claude agent
- `Shutdown`: answered after every agent's process tree has exited
- `Approve { id }`: answers yes to what the agent waits for by typing into its terminal.
  For a Claude agent that went from `Starting` to `NeedsInput` at the trust dialog and has
  neither left `NeedsInput` nor reported a hook since, while its screen still shows the
  dialog, that is up or down from the option carrying the `❯` cursor to the option
  starting `Yes,`, then `enter`; it is rejected when the screen shows no cursor on an
  option or no yes option. Otherwise it is `enter`. The keys go out in one write. Rejected,
  like `Input`, for an agent that is not running

`Response`:

- `Ok`
- `Projects(Vec<Project>)`
- `Project(Project)`
- `Agents(Vec<AgentInfo>)`
- `Agent(AgentInfo)`
- `GitStat(GitStat)`: `insertions`, `deletions`, `files` and `uncommitted`
- `Diff(String)`: unified diff text
- `Done(String)`: a finished operation, described for the user

`ServerMessage`:

- `Welcome { protocol_version, daemon_version }`
- `Response { id, result }`
- `AgentUpdated(AgentInfo)`
- `AgentRemoved(AgentId)`
- `AgentBell(AgentId)`: the agent's program rang the terminal bell (BEL); sent to every
  client, whatever it subscribes to
- `ProjectsChanged(Vec<Project>)`
- `Frame(Frame)`

`KillAgent`, `RemoveAgent` and `RestartAgent` stop the agent's whole process tree (see
[architecture.md](architecture.md#daemon-lifecycle)) before they are answered.
While one of them, or a `MergeAgent`, runs, any of the four or `RenameAgent` for the same
agent is rejected with
`<name> is stopping; try again once that is done` (`restarting`, `being removed` or `being
merged` in place of `stopping`). While a worktree `CreateAgent` runs, its name is reserved: a create
of the same name in the project, or a `RenameAgent` to it, is rejected with `an agent named
<name> is being created in this project`, and `RemoveProject` of its project is rejected.
A `Shutdown` that arrives meanwhile answers such a create with `the daemon shut down before
<name> started; its worktree was removed` (`its worktree is removed at the next start` when
the create still runs 13 s after shutdown began), and a restart with `the daemon is
shutting down` (see [architecture.md](architecture.md#nothing-slow-runs-on-the-event-loop)).
`Input` is rejected with an error for agents that are not running. `Resize` and `Scroll`
work for any agent that still has a terminal, including an exited one, and are rejected
for a stopped agent, which has none. `Subscribe` succeeds for any known agent.
A `Hook` for an agent that is not running, or is not a Claude agent, is ignored.

`AgentInfo.usage` is the last `Usage` a Claude agent's status line reported: `model` (the
model's display name), `cost_micros` (the session's estimated cost in millionths of a
dollar), `context_percent`, `five_hour_percent` and `seven_day_percent` (the plan's limits,
rounded to whole percent), `pr_number` and `pr_review`. Each is `None` when Claude left it
out, and the whole is `None` until the first report. It is kept in memory only.

`AgentInfo.waiting_for` says what a Claude agent in `NeedsInput` waits for: the `detail`
of the `PermissionRequest` that made it wait, else of the `Notification`, or `trusting
this folder` at the trust dialog. It is `None` whenever the agent does not need input, and
is not persisted.

`AgentInfo.title` is a Claude agent's session title: the terminal title Claude Code sets
(OSC 0/2), without its leading status glyph. `Claude Code` and an empty title are not
session titles and leave the stored title alone. A new session (a fresh restart, or a
`SessionStart` hook with a new session id that is not a resume or compact) clears it.
The title is kept in `state.toml`, so a stopped agent keeps it. Other agents' titles are
ignored and stay `None`.

## Worktrees and git queries

`AgentInfo` carries `worktree: Option<WorktreeInfo>` (`path`, `branch`, `base`) and
`last_error: Option<String>`, the error of the agent's last failed start; a successful
restart clears it. A worktree agent that direnv refused to start (its screen shows
direnv's `… .envrc is blocked` error when it exits) gets `direnv blocked the worktree's
.envrc; run: direnv allow <worktree path>`. A worktree agent's `cwd` is its worktree path. The daemon answers
`CreateAgent` with `Err` when the worktree cannot be made (see
[architecture.md](architecture.md#worktrees)), and creates no agent.

`GitStat` and `GitDiff` compare an agent's directory with the merge base of its `base`
and `HEAD` for a worktree agent, with `HEAD` for any other, counting untracked files that
are not ignored as new files (the committed-only diff scope has none). They answer
`Response::GitStat` or `Response::Diff`, or `Err` for an unknown agent, a directory that
is not a git repository, or a diff too large for one message. `RemoveAgent` with
`remove_worktree` answers `Err("worktree has uncommitted changes:\n<path>...")`, one
changed path per line, and leaves the agent running.

## Frames

`Subscribe { id }` makes the daemon push `Frame`s for that agent; one subscription is
active per connection, and a new `Subscribe` replaces it. Frames flow whenever the
agent has a terminal: an exited agent's last screen is sent, a stopped agent sends
nothing until it is restarted. A frame is full (`full: true`, every row) on subscribe and after a size change. Other
frames carry only the changed rows plus the cursor position. `Frame` has the fields
`agent`, `cols`, `rows`, `full`, `lines` and `cursor`. A cell with an empty `text`
is the spacer that follows a wide character.

`InputEvent` is `Key`, `Paste(String)`, `Wheel { up, col, row }` or `Click { col, row }`;
see `crates/protocol/src/input.rs`. `Click` is a left-button press and release at one cell
of the agent's screen; the daemon reports it only when the agent enabled mouse reporting
and drops it otherwise.
