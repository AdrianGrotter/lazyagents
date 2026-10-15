# Architecture

Status: implemented: the daemon, protocol, client, CLI and TUI, including daemon-side git
worktrees with git stat and diff queries, the configurable keymap, presets and theme, the
diff view, keyboard scroll mode, and mouse focus and text copy.

## Purpose

Running many coding agents (mostly Claude Code) as separate tmux tabs forces constant
tab-hopping to find out which one is working, which one is waiting for input, and which one
is done. `lazyagents` is a lazygit-style terminal UI that hosts all agents in one place:
a sidebar of projects and agents with live status, and a main pane that *is* the selected
agent's fully interactive terminal.

## Crates

One binary, `lazyagents`, in a Cargo workspace:

| Crate | Responsibility | Depends on |
|---|---|---|
| `crates/protocol` | Message types, framing, protocol version | serde, postcard |
| `crates/daemon` | Agent registry, PTYs, terminal emulation, input encoding, persistence, hook ingestion, status, git worktrees | protocol, alacritty_terminal, rustix, tokio |
| `crates/client` | Connects to the daemon, starting it on demand; used by the CLI and the TUI | protocol, rustix, tokio |
| `crates/tui` | ratatui client: layout, panels, popups, rendering of remote grids, input capture | client, protocol, ratatui, crossterm, tokio |
| `crates/lazyagents` | Binary: CLI parsing and subcommand dispatch | daemon, client, tui, protocol, clap |

Subcommands:

- `lazyagents` — run the TUI; without an interactive terminal it exits with an error.
- `lazyagents daemon` — run the daemon. It first calls `setsid` to leave the caller's
  session and terminal; started by a job-control shell, where it leads its own process
  group, that fails and it stays in the foreground.
- `lazyagents hook <event>` — invoked by Claude Code hooks; forwards the JSON payload
  on stdin to the daemon.
- `lazyagents ls` — list projects and agents.
- `lazyagents project add [path] [--name n]` / `project rm <project>` — register or
  unregister a project.
- `lazyagents new <name> [--project p] [--kind claude|generic] [-- cmd...]` — start an
  agent.
- `lazyagents kill|restart|rm <agent>`, `lazyagents rename <agent> <name>`.
- `lazyagents shutdown` — stop the daemon and every agent.

Commands that talk to the daemon start it if its socket is missing or dead, except
`shutdown`, which only connects to a running one. The auto-started daemon runs in its own
session without a controlling terminal, from `/`, with null stdio and without the caller's
terminal variables or Claude Code's `CLAUDECODE` and `CLAUDE_CODE_*` variables.

### Daemon lifecycle

- **One daemon per state dir.** The daemon takes an exclusive lock on `daemon.lock`
  (and writes its pid into it) before it touches the socket, and holds it until it exits.
  A second daemon fails with "a daemon is already running (pid N)". A leftover socket
  nobody answers on is removed only while the lock is held.
- **Tracking agent processes.** Each agent leads its own session and process group.
  About once a second the daemon reads the process table (`ps -A -o pid=,ppid=,lstart=`)
  on a separate thread, so `ps` never blocks the event loop, and records every
  descendant of each running agent with its start time. Each reading is merged with the
  agent's earlier list: entries still running with the same start time stay, even after
  they left the agent's tree (reparented to init or moved to a new session, as Claude's
  Bash-tool shells do); entries that exited, or whose pid now belongs to a process with
  another start time, are pruned. A recycled pid is therefore never signalled.
- **Stopping an agent.** Kill, remove, restart and shutdown stop, in one pass, the
  agent's current process tree (read just before signalling, because a dying parent
  hands its children to init), its process group and its tracked descendants: SIGHUP
  and SIGTERM to the group, SIGTERM to every process, then one wait of up to the grace
  period until all of it is gone. Survivors, including descendants forked meanwhile,
  and the group get SIGKILL, followed by a short wait (500 ms). The grace period is 1 s
  for one agent and 2 s shared by all agents on shutdown, which stops every agent in
  that single pass. A snapshot still running when the stop begins is waited for (up
  to 2 s) first, so the descendants it reports are part of that pass. The request is
  answered only once this is done, and a restart spawns the new process only
  afterwards.
- **An agent that exits on its own.** When the agent's process exits, the daemon runs
  the same pass over its process group and tracked descendants, so background jobs it
  left behind go too. Descendants that a snapshot reports after its agent exited, was
  stopped, restarted or removed are stopped as soon as it finishes. Both passes run on a separate thread, so
  their grace period never blocks requests; if the thread cannot be spawned, the pass
  runs inline, as does a snapshot whose thread cannot be spawned. Until such a pass
  finishes, its processes stay listed with the agent, so a kill, remove, restart or
  shutdown that arrives meanwhile stops them too before it answers or exits; shutdown
  also covers passes whose agent was already removed.
- **The remembered group.** If members of an agent's process group outlive a stop or
  the agent's own exit, the daemon remembers the group and signals it again on the
  next kill, remove, restart or shutdown; it forgets the group once it is empty.
- **Known limit.** A process that double-forks and calls `setsid` within one snapshot
  interval leaves the agent's tree and group before any snapshot records it, and
  escapes.
- **Shutdown.** A `Shutdown` request, SIGTERM and SIGINT all take the same path:
  stop every agent as above, then remove the socket and release the lock.
- **Unreachable daemon.** About once a second the daemon checks that `daemon.sock` is
  still the socket it bound; if it was deleted or replaced, it shuts down.
- **Idle exit.** With no client connected and no agent running for
  `LAZYAGENTS_IDLE_EXIT_SECS` seconds (default 600; 0 disables it), it shuts down.

### The daemon is the terminal

For each running agent the daemon owns:

- a PTY + child process (`alacritty_terminal::tty`); the child leads its own session
  and process group,
- an `alacritty_terminal::Term` fed by the PTY output (screen, scrollback, modes),
- an input encoder that turns abstract key/mouse/paste events into bytes according to the
  modes the child has enabled (application cursor keys, bracketed paste, mouse reporting
  modes, kitty keyboard protocol flags).

The TUI never parses agent output. It receives cell-grid frames (full frame on subscribe,
then damaged rows) for the agent it is displaying, and sends abstract input. Because the
emulator lives in the daemon, reattaching after the TUI quits is instant and exact.

PTY size follows the main pane size of the most recently active client (tmux semantics).

### Agent environment

Agents are spawned in their working directory with the daemon's environment plus
`LAZYAGENTS_*` variables. If the working directory (or an ancestor up to the project root)
contains `.envrc` and `direnv` is on `PATH`, the command is wrapped as
`direnv exec <dir> <cmd...>` so nix/direnv environments load. The user's `claude` shell
function is not used; the binary on `PATH` is.

### Worktrees

The daemon runs every git command through the git CLI (`crates/daemon/src/git.rs`), so the
user's git config applies; it only sets `GIT_TERMINAL_PROMPT=0`, plus
`GIT_OPTIONAL_LOCKS=0` on the read-only queries so they never hold `index.lock` while an
agent commits.

Creating an agent with a `WorktreeRequest` runs `git -C <project> worktree add -b
lazyagents/<agent-name> <path> <base>`, where `<path>` is
`<state>/worktrees/<project-name>-<first 8 of project id>/<agent-name>` (a `/` in the
project name becomes `-`) and `<base>` is the requested base, else the project's current
branch, else (detached HEAD) its commit sha, which is then stored as the base. A base
starting with `-` is rejected. The branch name is checked with `git check-ref-format
--branch` first, and an existing `<path>` is refused.

A failure before or during the add (project not a git repo, invalid branch name, branch
exists, base missing, a failing checkout hook) answers `Err` and leaves no agent, no new
branch and no directory: an add that fails after git created the branch or the checkout
has both removed again, while a branch that existed before is kept. A spawn failure after
a successful add keeps the agent as `Exited` with its error in `AgentInfo.last_error`, and
its worktree, so it can be restarted. The agent's cwd is the worktree, and
`AgentInfo.worktree` holds its path, branch and base. Renaming the agent keeps its branch
and worktree path.

`GitStat` and `GitDiff` run on their own thread and answer when git finishes, so the event
loop keeps serving. For a worktree agent they compare the working tree with `git
merge-base <base> HEAD` (committed and uncommitted work); for any other agent with `HEAD`
(uncommitted changes only). Untracked files appear only in `uncommitted`, the count of
`git status --porcelain` entries, and not in the diff. A diff too large for one message is
an error.

`RemoveAgent { remove_worktree: true }` refuses while `git status --porcelain` lists
anything, answering `worktree has uncommitted changes:` and the changed paths, one per
line (the new path of a rename), and leaves the agent running. `force: true` skips the
check and runs `git worktree remove --force`. Without `remove_worktree` the worktree stays
on disk. An agent's branch is never deleted.

### The TUI

`crates/tui` is a client like the CLI; it holds no agent state of its own beyond what the
daemon last told it.

- **Model** (`app.rs`): `App` holds projects, agents, selection, focus (projects, agents,
  pane, diff or scroll), the open popup, the subscribed agent's screen, the git stats and
  diff the daemon sent, and the pane's text selection. Key, paste and mouse events and
  daemon messages go in; `Effect`s come out (`Send(Request)`, `Copy(String)`, `Quit`,
  `StopDaemonAndQuit`). It does no I/O, so behaviour is tested without a terminal or a
  daemon. Selection follows an agent's id across list changes. The TUI is subscribed to at
  most one agent's screen, the selected one, and sizes a running agent's PTY to the pane.
  It asks for the selected agent's `GitStat` when the selection changes and then every
  five seconds, with at most one request per agent in flight.
- **Focus rule**: keys reach an agent only while the pane has focus, and focus moves into
  the pane only on an explicit user action: `enter` on a running agent, a click in the
  pane on a running agent (a click on an Agents row only selects), or a successful create
  from the new agent form, provided no key was pressed and no popup opened between
  submitting and the daemon's answer. `ctrl-q j`/`k` switch agents from inside the pane
  and keep focus there only when the new agent is running. A failed request clears the
  pending create. Focus returns to the sidebar when the selected agent stops running or
  when the selection changes for any reason other than a key (the agent was removed by
  another client, the agent list was replaced).
- **Keys and config** (`keymap.rs`, `config.rs`): a `Keymap` maps key events to `Action`s
  in three scopes: the sidebar, the keys after the pane prefix (`ctrl-q`), and the direct
  leave keys (`ctrl-]`, `ctrl-5`). Key strings such as `ctrl-q`, `shift-tab` or `A` parse
  into `KeySpec`s, and `describe` renders an action's keys for the help popup and the
  hints. The pane prefix state lives in `App` and is cleared whenever focus leaves the
  pane. `config::load` reads the config file once at startup into a `Config` (keymap,
  presets, theme, warnings) that never fails: unusable entries keep their defaults and
  become warnings shown in the status bar ([configuration.md](configuration.md)).
- **Rendering** (`ui.rs`): draws the sidebar (Projects, Agents, Info), the agent pane or
  the diff view, the status line and popups from `App`; the pane copies the daemon's cell
  grid into the ratatui buffer, with the text selection drawn reversed on top. Colours
  come from the theme. The Info box is sized to its content (status, worktree branch, git
  stat, command, cwd, kind): it grows as long as the Agents box keeps 8 rows, and keeps 6
  rows while the content fits. The status line shows a transient message in red, then `│`
  and the key hint, cut off on the right. Below a 20×5 pane only "terminal too small" is
  drawn, and keys do nothing except quitting from the sidebar and leaving the pane. Status
  glyphs animate from a frame counter the runtime ticks every 100 ms while an agent is
  starting or working.
- **Mouse** (`app.rs`): the TUI captures the mouse. A click selects a row or focuses a box
  or the pane, a drag selects pane cells and copies them on release, a click outside a
  popup closes it, and the wheel scrolls the diff, the scroll-mode history or the pane's
  scrollback, or goes to the agent when it enabled mouse reporting. Clicks and drags never
  reach the agent. See [keybindings.md](keybindings.md#mouse).
- **Connection actor** (`connection.rs`): a task owning the `Client`. It forwards
  responses, errors and server events (agent list changes, screen frames) on one channel.
  `GitStat` and `GitDiff` go out without waiting for their replies, which the daemon may
  answer late; each reply comes back tagged with its agent id as `Incoming::GitStat` or
  `Incoming::Diff`, errors included, so a git error never looks like the failure of
  another request. Every other request waits for its reply before the next is sent, so
  those replies and errors arrive in request order. When the daemon closes the connection
  it reports `Closed`, and the TUI exits with "the daemon stopped".
- **Runtime and terminal guard** (`run.rs`): refuses to start without an interactive
  terminal, connects (starting the daemon if needed; on a protocol mismatch it opens the
  restart prompt), then enters raw mode, the alternate screen, mouse capture, bracketed
  paste and, where supported, keyboard enhancement. A blocking thread reads terminal
  events; a read error means the terminal is gone and the TUI exits, even with SIGHUP and
  SIGTERM ignored ([ADR 0005](decisions/0005-vendor-crossterm-dead-tty-fix.md)). SIGHUP,
  SIGTERM and SIGINT also exit. The guard and a panic hook restore every terminal mode on
  any exit path, ignoring errors from a vanished terminal.
- **Clipboard** (`clipboard.rs`): the runtime handles `Copy` between draws, writing OSC 52
  to the terminal and flushing it, and piping the text to `pbcopy` or `wl-copy` when one
  is on `PATH`. The copier runs on a short-lived thread that writes its stdin, closes it
  and waits for it, so it never blocks the UI or lingers as a zombie.

### Persistence and config

- State dir: `$LAZYAGENTS_STATE_DIR` (a relative path is taken from the current
  directory), default `~/.local/state/lazyagents/`, created with mode 0700: `state.toml`
  (projects, agents, session ids, worktree info), `daemon.sock`, `daemon.lock`,
  `daemon.log`, `hooks/<agent_id>.json`, `worktrees/`.
- Config: `$LAZYAGENTS_CONFIG`, else `$XDG_CONFIG_HOME/lazyagents/config.toml`, else
  `~/.config/lazyagents/config.toml`: keybindings, agent presets, theme colours. A missing
  file gives the built-in defaults; anything unusable becomes a warning in the TUI status
  line ([configuration.md](configuration.md)).
- `state.toml` is written atomically (write temp + rename) on every metadata change.

## Error handling

- Protocol version mismatch → the client fails with `VersionMismatch`, which suggests
  `lazyagents shutdown`. A mismatched daemon cannot be asked to stop over the socket, so
  `client::stop_daemon` sends SIGTERM to the pid in `daemon.lock` (only while the lock
  is held) and waits up to 5 s for the socket to go and the lock to be released. The
  daemon's SIGTERM handler stops every agent first. It never escalates to SIGKILL,
  which would orphan the agents' process trees; on timeout it fails with
  `daemon <pid> did not stop within 5s`. `shutdown` uses it and prints `daemon stopped`.
  The TUI shows a full-screen popup naming both protocol versions: `y` stops the daemon
  this way and continues with a new one, `n`, `q` or `esc` exits with the mismatch
  error.
- Socket exists but connect fails → spawn a daemon, which removes the stale socket once
  it holds the lock.
- A daemon that exits while the client waits for it to start → the client fails at
  once with "daemon exited during startup; see <log>", unless another daemon holds the
  lock (a concurrent start), in which case it keeps waiting for that one. The client
  reaps the daemon process it started.
- Daemon crash (SIGKILL) → its PTYs close and agents receive SIGHUP; processes that
  ignore it or left the agent's session can outlive the daemon. On the next start agents
  load as `Stopped` and can be restarted.
  Claude resumes only sessions that received a prompt
  (see [ADR 0006](decisions/0006-resume-only-prompted-sessions.md)).
- A corrupt `state.toml` → the daemon refuses to start and leaves the file untouched.
- Spawn failure (command not found, cwd missing) → agent `Exited`, error rendered in the
  pane and the status line.
- Worktree errors (branch exists, base missing) → shown in the status bar; nothing is
  created.
- `lazyagents hook` never blocks or fails Claude: it checks its environment, reads stdin and
  talks to the daemon within one 200 ms budget, prints nothing and always exits 0.
- Fatal errors while the daemon starts are written to `daemon.log`.
- `lazyagents shutdown` waits up to 5 s for the daemon's reply, which comes after every
  agent tree is gone, and up to 3 s for the socket to disappear.
- All daemon errors logged with `tracing` to `daemon.log`; TUI shows the last error in
  the status line.

## See also

- [protocol.md](protocol.md): wire format between the daemon, the TUI and the CLI.
- [keybindings.md](keybindings.md): the TUI key map.
- [configuration.md](configuration.md): the config file.
- [status-detection.md](status-detection.md): how agent status is derived from hooks and the
  pane.
- [decisions/](decisions/): ADRs recording the design choices behind this system.
