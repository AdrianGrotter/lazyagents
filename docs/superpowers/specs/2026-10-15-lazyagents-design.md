# lazyagents — design

Status: approved in brainstorming, pending written-spec review
Date: 2026-10-15

## 1. Purpose

Running many coding agents (mostly Claude Code) as separate tmux tabs forces constant
tab-hopping to find out which one is working, which one is waiting for input, and which one
is done. `lazyagents` is a lazygit-style terminal UI that hosts all agents in one place:
a sidebar of projects and agents with live status, and a main pane that *is* the selected
agent's fully interactive terminal.

### Success criteria

1. A user can run all their day-to-day Claude Code sessions inside lazyagents instead of
   tmux tabs, with their existing Claude Code configuration (`~/.claude`, project
   `.claude/`, plugins, skills, MCP, hooks, statusline, keybindings) working unchanged.
2. Interactive Claude Code in the main pane is good enough for daily use: correct colors,
   cursor, shift+enter, bracketed paste, mouse wheel scrolling, text selection + copy.
3. At a glance the sidebar shows which agents need input, are working, are idle or exited.
4. Agents survive closing the TUI; reopening it reattaches instantly.
5. A new agent can optionally get its own git worktree + branch; its diff is viewable.
6. The repository is agents-first: an agent with no prior context can pick up any task
   using `AGENTS.md`, `docs/`, skills and hooks, and its work is checked automatically.

### Non-goals (v1)

- Desktop notifications or terminal bell (attention is shown in the TUI only).
- First-class adapters for agents other than Claude Code (any command runs as a
  generic agent with heuristic status).
- Remote/multi-machine daemons, Windows support.
- Surviving a daemon crash with live processes (metadata survives; processes do not).
- Full-terminal passthrough mode (rejected in favour of the embedded pane).

## 2. Architecture

One binary, `lazyagents`, in a Cargo workspace:

| Crate | Responsibility | Depends on |
|---|---|---|
| `crates/protocol` | Message types, framing, protocol version | serde, postcard |
| `crates/daemon` | Agent registry, PTYs, terminal emulation, input encoding, persistence, git worktrees, hook ingestion, status | protocol, portable-pty, alacritty_terminal, tokio |
| `crates/tui` | ratatui client: layout, panels, popups, rendering of remote grids, input capture | protocol, ratatui, crossterm, tokio |
| `crates/lazyagents` | Binary: CLI parsing, daemon auto-spawn, subcommand dispatch | all of the above, clap |

Subcommands:

- `lazyagents` — run the TUI; spawn the daemon if its socket is missing or dead.
- `lazyagents daemon` — run the daemon in the foreground (auto-spawn detaches it).
- `lazyagents hook <event>` — invoked by Claude Code hooks; forwards the JSON payload
  on stdin to the daemon.

### 2.1 The daemon is the terminal

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

### 2.2 Protocol

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

Full message definitions live in `docs/protocol.md` and `crates/protocol`; that crate is the
source of truth.

### 2.3 Status detection

Status enum: `Starting`, `Working`, `NeedsInput`, `Idle`, `Exited(i32)`, `Stopped`.

- **Claude agents** are launched as
  `claude --session-id <uuid> --settings <state>/hooks/<agent_id>.json` (first start) or
  `claude --resume <uuid> --settings ...` (restart). The generated settings file only adds
  hooks; Claude Code merges it with the user's own settings. Each hook runs
  `lazyagents hook <event>` with `LAZYAGENTS_AGENT_ID` and `LAZYAGENTS_SOCKET` in the
  environment:
  - `SessionStart` → `Idle` (also records the session id from the payload, so `/clear`
    and similar don't break resume)
  - `UserPromptSubmit`, `PreToolUse` → `Working`
  - `Notification` → `NeedsInput`
  - `Stop` → `Idle`
- **Generic agents**: `Working` if output arrived in the last 2 s, otherwise `Idle`.
- **Both**: process exit → `Exited(code)`; agents loaded from disk at daemon start →
  `Stopped`.
- The merge behaviour of `--settings` with user hooks is verified in the spike
  (section 6, milestone 0) before being relied on.

### 2.4 Agent environment

Agents are spawned in their working directory with the daemon's environment plus
`LAZYAGENTS_*` variables. If the working directory (or an ancestor up to the project root)
contains `.envrc` and `direnv` is on `PATH`, the command is wrapped as
`direnv exec <dir> <cmd...>` so nix/direnv environments load. The user's `claude` shell
function is not used; the binary on `PATH` is.

### 2.5 Worktrees

Creating an agent with "worktree" enabled runs
`git -C <project> worktree add -b lazyagents/<agent-name> <state>/worktrees/<project>/<agent-name> <base>`
via the git CLI (so the user's git config applies). The agent's cwd is the worktree.
The Info panel shows branch and `git diff --shortstat <base>...HEAD` plus uncommitted
changes. `D` shows the full diff (read-only, scrollable) in the main pane. Removing the
agent offers to remove the worktree; if it has uncommitted changes they are listed and an
explicit confirmation is required. The branch is never deleted automatically.

### 2.6 Persistence and config

- State dir: `$LAZYAGENTS_STATE_DIR`, default `~/.local/state/lazyagents/`:
  `state.toml` (projects, agents, session ids, worktree info), `daemon.sock`,
  `daemon.log`, `hooks/<agent_id>.json`, `worktrees/`.
- Config: `~/.config/lazyagents/config.toml` (overridable with `LAZYAGENTS_CONFIG`):
  keybindings, agent presets (`claude`, `shell`, custom commands), theme colors.
  Missing file → built-in defaults. Unknown keys → warning in the TUI status line.
- `state.toml` is written atomically (write temp + rename) on every metadata change.

## 3. UI

```
┌ Projects ─────────┐┌ api-refactor · claude · ⠋ working ───────────────┐
│ ▸ lazyagents   2  ││                                                  │
│   umain-web   1 ! ││          interactive Claude Code                 │
├ Agents ───────────┤│                                                  │
│ ! fix-auth  input ││                                                  │
│ ⠋ api-refac  work ││                                                  │
│ ✓ docs       idle ││                                                  │
├ Info ─────────────┤│                                                  │
│ lazyagents/api-re ││                                                  │
│ +42 −7 · 3 files  ││                                                  │
└───────────────────┘└──────────────────────────────────────────────────┘
 enter focus · n new · x kill · r restart · g next-attention · ? help
```

Two input modes:

**Sidebar mode** (lazygit-like):

| Key | Action |
|---|---|
| `j`/`k`, arrows | move within panel |
| `h`/`l`, `tab`/`shift-tab` | switch panel |
| `enter` | focus main pane (pane mode) |
| `n` | new agent popup: name, preset, worktree toggle, base branch |
| `x` | kill agent (confirm) |
| `r` | restart agent (Claude: resume session) |
| `d` | remove agent (offers worktree removal) |
| `R` | rename agent |
| `a` | add project (path input, defaults to cwd) |
| `g` | jump to next agent with `NeedsInput` |
| `D` | toggle diff view in main pane |
| `?` | help popup |
| `q` | quit TUI (agents keep running) |
| `Q` | quit and stop daemon (confirm) |

**Pane mode**: all input goes to the agent except `ctrl-]` (back to sidebar) and
`alt-[`/`alt-]` (previous/next agent in the current project). Mouse wheel scrolls the
emulator's scrollback when the agent hasn't enabled mouse reporting; otherwise it is
forwarded. Mouse drag selects text inside the pane and copies it via OSC 52 (fallback
`pbcopy`/`wl-copy`). `ctrl-]` then `[` enters keyboard scroll mode.

Attention: `NeedsInput` agents show `!`, are highlighted, sort first within their project,
and their project row shows a count. All bindings are configurable.

## 4. Error handling

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

## 5. Agents-first repository

- `AGENTS.md`: canonical agent guide — product summary, crate map, commands, conventions,
  definition of done, links into `docs/`. `CLAUDE.md` contains only `@AGENTS.md`.
- `README.md`: human-facing intro, install, usage.
- `docs/`: `architecture.md`, `protocol.md`, `keybindings.md`, `status-detection.md`,
  `decisions/NNNN-*.md` (ADRs; initial: daemon over tmux, embedded emulator over
  passthrough, status via Claude hooks), `superpowers/specs/`, `superpowers/plans/`.
- Tooling: `flake.nix` (stable Rust via rust-overlay, rust-analyzer, just, cargo-nextest,
  cargo-insta, cargo-deny), `.envrc` (`use flake`), `justfile`
  (`fmt`, `lint`, `test`, `check`, `dev`), `rust-toolchain.toml`, `.gitignore`.
- `.claude/skills/`:
  - `committing` — Conventional Commits; types `feat fix docs refactor test chore build ci perf`;
    scopes `protocol daemon tui cli docs nix repo`; one logical change per commit.
  - `verifying` — run `just check` and read its output before claiming done.
  - `running-dev` — start an isolated daemon + TUI in a tmux session with a temp
    `LAZYAGENTS_STATE_DIR`, drive with `send-keys`, inspect with `capture-pane`.
  - `recording-decisions` — ADR template and when one is required.
- `.claude/settings.json`:
  - `PostToolUse` on `Edit|Write` of `*.rs` → `rustfmt` the file.
  - `Stop` → `cargo clippy --workspace --all-targets -- -D warnings`; on failure block
    with the errors so the agent fixes them.
  - `PreToolUse` on `Bash` → reject `git commit` whose message isn't a Conventional
    Commit.
  - Permissions allowlist for `cargo`, `just`, read-only `git`, `nix develop`.
- CI: GitHub Actions running `nix develop -c just check`.

## 6. Milestones

0. **Repo foundation**: git, flake, toolchain, workspace skeleton, AGENTS.md, docs, skills,
   hooks, CI. `just check` green on an empty workspace.
1. **Embedded terminal spike** (throwaway, `examples/spike`): ratatui app with a dummy
   sidebar and real `claude` in the main pane via `alacritty_terminal` in-process.
   Verifies rendering fidelity, key encoding, paste, mouse, `--settings` hook merge.
   Gate: the user tries it and approves continuing with the embedded design.
2. **Protocol crate** with round-trip tests.
3. **Daemon core**: registry, PTY + emulator, input encoder, frames, socket server,
   persistence. Integration tests with scripted fake agents.
4. **TUI core**: connect/auto-spawn, sidebar panels, main pane rendering, pane mode input.
   Snapshot tests.
5. **Status**: hook subcommand + Claude settings generation, generic heuristic,
   attention sorting/highlighting.
6. **Agent lifecycle UI**: new/kill/restart/remove/rename, projects, config + keybindings.
7. **Worktrees + diff view**.
8. **Polish**: scrollback, selection + copy, version-mismatch flow, help popup, docs pass.

## 7. Testing

- Unit: input encoder (table of key × modes → bytes), status state machine, protocol
  round-trips, config parsing, frame diffing.
- Snapshot: TUI rendering via ratatui `TestBackend` + `insta`.
- Integration: daemon in a temp state dir, scripted fake agents (`sh -c` scripts printing
  known output, reading input, exiting with codes), assertions on frames, status
  transitions and hook effects; git worktree tests against temp repos.
- E2E (agent-driven, `running-dev` skill): TUI in tmux, driven by `send-keys`, verified by
  `capture-pane`.
- Every milestone ends with `just check` green and an E2E smoke check where applicable.
