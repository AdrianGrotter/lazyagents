# Architecture

Status: daemon, protocol, client and CLI implemented; the TUI lands in plan 3 (the spike is the interim UI).

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
| `crates/daemon` | Agent registry, PTYs, terminal emulation, input encoding, persistence, git worktrees, hook ingestion, status | protocol, portable-pty, alacritty_terminal, tokio |
| `crates/client` | Connects to the daemon, starting it on demand; used by the CLI and the TUI | protocol, tokio |
| `crates/tui` | ratatui client: layout, panels, popups, rendering of remote grids, input capture | protocol, ratatui, crossterm, tokio |
| `crates/lazyagents` | Binary: CLI parsing and subcommand dispatch | daemon, client, protocol, clap |

Subcommands:

- `lazyagents` — run the TUI (until it lands, prints help).
- `lazyagents daemon` — run the daemon in the foreground (auto-spawn detaches it).
- `lazyagents hook <event>` — invoked by Claude Code hooks; forwards the JSON payload
  on stdin to the daemon.
- `lazyagents ls` — list projects and agents.
- `lazyagents project add [path] [--name n]` / `project rm <project>` — register or
  unregister a project.
- `lazyagents new <name> [--project p] [--kind claude|generic] [-- cmd...]` — start an agent.
- `lazyagents kill|restart|rm <agent>`, `lazyagents rename <agent> <name>`.
- `lazyagents shutdown` — stop the daemon and every agent.

Commands that talk to the daemon start it if its socket is missing or dead, except `shutdown`, which only connects to a running one.

### The daemon is the terminal

For each running agent the daemon owns:

- a PTY + child process (`portable-pty`),
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

Creating an agent with "worktree" enabled runs
`git -C <project> worktree add -b lazyagents/<agent-name> <state>/worktrees/<project>/<agent-name> <base>`
via the git CLI (so the user's git config applies). The agent's cwd is the worktree.
The Info panel shows branch and `git diff --shortstat <base>...HEAD` plus uncommitted
changes. `D` shows the full diff (read-only, scrollable) in the main pane. Removing the
agent offers to remove the worktree; if it has uncommitted changes they are listed and an
explicit confirmation is required. The branch is never deleted automatically.

### Persistence and config

- State dir: `$LAZYAGENTS_STATE_DIR`, default `~/.local/state/lazyagents/`:
  `state.toml` (projects, agents, session ids, worktree info), `daemon.sock`,
  `daemon.log`, `hooks/<agent_id>.json`, `worktrees/`.
- Config: `~/.config/lazyagents/config.toml` (overridable with `LAZYAGENTS_CONFIG`):
  keybindings, agent presets (`claude`, `shell`, custom commands), theme colors.
  Missing file → built-in defaults. Unknown keys → warning in the TUI status line.
- `state.toml` is written atomically (write temp + rename) on every metadata change.

## Error handling

- Protocol version mismatch → TUI shows a popup offering to restart the daemon.
- Socket exists but connect fails → remove stale socket, spawn daemon.
- Daemon crash → agents die; on next start they load as `Stopped` and can be restarted
  (Claude resumes its session).
- Spawn failure (command not found, cwd missing) → agent `Exited`, error rendered in the
  pane and the status line.
- Worktree errors (branch exists, base missing) → shown in the popup; nothing is created.
- `lazyagents hook` never blocks or fails Claude: 200 ms total timeout, always exits 0.
- All daemon errors logged with `tracing` to `daemon.log`; TUI shows the last error in
  the status line.

## See also

- [protocol.md](protocol.md): wire format between the daemon, the TUI and the CLI.
- [keybindings.md](keybindings.md): the TUI key map.
- [status-detection.md](status-detection.md): how agent status is derived from hooks and the pane.
- [decisions/](decisions/): ADRs recording the design choices behind this system.
