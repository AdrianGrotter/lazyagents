# Architecture

Status: implemented: the daemon, protocol, client, CLI and TUI, including daemon-side git
worktrees with git stat and diff queries, the configurable keymap, presets and theme, the
diff view with review comments, keyboard scroll mode, mouse focus, text copy and click
forwarding, custom commands, notifications, project setup scripts, and agent status
detection from hooks, the screen and silence timers.

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
- `lazyagents new <name> [--project p] [--kind claude|generic] [--prompt text] [-- cmd...]`
  — start an agent. `--prompt` is the agent's first message: Claude gets it as its
  positional prompt argument (after `--`), any other command has it typed in, followed
  by Enter, when it first prints. Restarts never repeat it.
- `lazyagents statusline` (hidden): the status line lazyagents gives a Claude agent whose
  user has one; see [status-detection.md](status-detection.md). `ls` shows what it
  reported after the agent's command, as `(Opus · 23% context · $0.42)`.
  An agent whose command could not start is kept (`new` says so and names `restart` and
  `rm`); `ls` prints its error under it as `last error: ...`.
- `lazyagents send <agent> <text>` — type `text` into the agent and press enter, as `i` does
  in the TUI.
- `lazyagents kill|restart|rm <agent>`, `lazyagents rename <agent> <name>`. An `<agent>` is
  its id, `<project>/<name>`, a name unique across projects, or an id prefix of at least
  4 hex characters. `rm` keeps a worktree agent's worktree and branch and says so:
  `removed <name>; kept worktree <path> (branch <branch>)`.
- `lazyagents shutdown` — stop the daemon and every agent.

Commands that talk to the daemon start it if its socket is missing or dead, except
`shutdown`, which only connects to a running one. A state directory whose `daemon.sock`
path would exceed the platform's socket limit (104 bytes on macOS, 108 on Linux) is
refused before anything starts (ADR 0023). The auto-started daemon runs in its own
session without a controlling terminal, from `/`, with null stdio and without the caller's
terminal variables or Claude Code's `CLAUDECODE` and `CLAUDE_CODE_*` variables.

### Daemon lifecycle

- **One daemon per state dir.** The daemon takes an exclusive lock on `daemon.lock`
  before it touches the socket, and holds it until it exits. Right after taking it, the
  daemon writes two lines into the file: its pid, then its start time as
  `ps -o lstart=` prints it (just the pid when `ps` cannot tell). A client trusts the
  pid only while the lock is held and, when a start time is recorded, `ps` still reports
  that start time for the pid, so an entry outliving its daemon never names a recycled
  pid. A pid-only file, as older daemons write, is trusted on the lock alone. A client
  checking whether a daemon runs takes the lock for a moment, so a starting daemon tries
  for up to 200 ms before it gives up; a second daemon then fails with "a daemon is
  already running (pid N)". A leftover socket nobody
  answers on is removed only while the lock is held.
- **Reading start times.** Every start time, in the lock file and in process tracking,
  comes from one `ps` command (`protocol::lockfile::ps`): `/bin/ps` when it exists, else
  `ps` on `PATH` (NixOS has no `/bin/ps`), run with `LC_ALL=C` and `TZ=UTC0`. `lstart`
  is otherwise printed in the caller's locale and time zone, so a client in another
  locale, or a change of the system time zone, would make a live process look recycled.
  Debug builds run `$LAZYAGENTS_PS` in place of `ps` when it is set, so tests can
  substitute a slow `ps`; release builds ignore it.
- **Tracking agent processes.** Each agent leads its own session and process group.
  About once a second the daemon reads the process table (`ps -A -o pid=,ppid=,lstart=`)
  on a separate thread and records every
  descendant of each running agent with its start time. Each reading is merged with the
  agent's earlier list: entries still running with the same start time stay, even after
  they left the agent's tree (reparented to init or moved to a new session, as Claude's
  Bash-tool shells do); entries that exited, or whose pid now belongs to a process with
  another start time, are pruned. A recycled pid is therefore never signalled. A
  reading that fails (`ps` cannot run, exits non-zero or lists nothing) leaves every
  list as it was.
- **Stopping an agent.** Kill, remove, restart and shutdown stop, in one pass, the
  agent's current process tree (read just before signalling, because a dying parent
  hands its children to init), its process group and its tracked descendants: SIGHUP and
  SIGTERM to the group, SIGTERM to every process, then one wait of up to the grace
  period until all of it is gone. Survivors, including descendants forked meanwhile, and
  the group get SIGKILL, followed by a short wait (500 ms). A survivor other than a tree
  root gets SIGKILL only when a fresh reading still shows it with the start time it had
  when it got SIGTERM, so a pid reused during the grace period is left alone. The grace
  period is 1 s for one agent and 2 s shared by all agents on shutdown, which stops
  every agent in that single pass. A failed process-table reading is retried once; if it
  fails again, the pass signals only the tree roots and the groups, and logs an error.
  If the reading before SIGKILL fails, only the tree roots and the groups get SIGKILL.
  The tracked descendants a pass could not verify get no further signal from it; they
  are kept and passed to a new pass at most once a second until a reading confirms
  whether they still run, and every later stop covers them too. Any still unverified
  when shutdown finishes are logged with their pid and start time and left alone. A
  snapshot still running when the stop begins is waited for (up to 2 s) first when it
  covers an agent being stopped, so the descendants it reports are part of that pass. A
  stop of agents the snapshot does not cover leaves it running, and a snapshot that
  takes longer than the wait stays running too; either way its results are applied, or
  the descendants it reports for stopped agents are stopped as orphans, when it
  finishes. Shutdown waits up to 10 s for any running snapshot and logs an error if it
  is still running then, because the processes it would report cannot be checked. A
  kill, remove or restart runs this pass, snapshot wait included, on a thread of its own
  and is answered only once it is done; a restart spawns the new process only afterwards.
  Shutdown runs it on the event loop, which ends with it.
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
- **Known limit.** A descendant escapes when it detaches (a double fork and `setsid`)
  and its parent exits before a reading links it to the agent. That window is one
  snapshot interval plus one `ps` run, since readings run one after another, and it
  grows when `ps` is slow under load.
- **Shutdown.** A `Shutdown` request, SIGTERM and SIGINT all take the same path:
  stop every agent as above, finish the requests still in flight (see below) for up to
  13 s after it began, then remove the socket and release the lock.
- **Unreachable daemon.** About once a second the daemon checks that `daemon.sock` is
  still the socket it bound; if it was deleted or replaced, it shuts down.
- **Idle exit.** With no client connected, no agent running and no request in flight for
  `LAZYAGENTS_IDLE_EXIT_SECS` seconds (default 600; 0 disables it), it shuts down.

### Nothing slow runs on the event loop

One event loop serves every client: it takes requests and hooks, feeds agent output to the
terminals and sends frames. Anything that can take long runs on a thread of its own and
answers the request when it finishes: `ps` readings, stops with their grace period, git
commands (worktree add with the user's checkout hooks, `.worktreeinclude` copies, status
checks, worktree removal, merges, stats and diffs) and direnv calls. A new connection gets
its `Welcome` from its own task before the loop sees it, so a client is greeted at once and
a hook never waits behind other work. Shutdown is the one exception: it runs on the loop,
which ends with it.

Nothing waits for a client either: each connection has a bounded queue, which its own
task writes to the socket (`outbox.rs`, ADR 0022). Frames go only while fewer than 4 are
queued; a frame that finds 4 is dropped, and the next frame for that connection is full,
so a client that stalled sees the current screen as soon as it reads again. Every other
message (replies and events) must arrive: up to 16384 of them may queue, and a connection
whose queue overflows is closed, since its client stopped reading.

While such work is in flight the daemon keeps its state consistent (ADR 0019):

- **Kill, restart, remove and merge** mark the agent busy until they are answered. A kill,
  restart, remove, merge or rename of it is refused meanwhile with `<name> is stopping; try
  again once that is done` (`restarting`, `being removed` or `being merged` in place of
  `stopping`), so a removal never takes the worktree or branch from under a merge.
  Everything else (input, resize, scroll, subscribe, hooks, usage, git queries) is served
  against the agent as it is; a removal's checks (merged branch, uncommitted changes) run on its thread
  before the stop, and a refusal leaves the agent as it was.
- **A worktree create** reserves its name in the project until it is answered: a create of
  the same name, or a rename to it, is refused with `an agent named <name> is being created
  in this project`, and the project cannot be removed meanwhile. The agent appears, and
  starts, only once its worktree is ready. A create without a worktree is answered at once.
  A worktree git will not let go of at the next start keeps its name and its project: the
  refusals then name its path and say it is removed at the next start.
  Agent names in a project compare without regard to case, here and for existing agents,
  because they name worktree directories and branches, which a case-insensitive file
  system (the macOS default) cannot tell apart.
- **A worktree in the making is recorded.** Before git adds the worktree, the create
  records its path and branch in `state.toml` as a pending worktree, unless the path or
  the branch exists already (git then refuses, and nothing needs undoing). The agent is
  recorded in the same save that drops the pending worktree; a failed add drops it after
  git undid its work. When a daemon starts and finds pending worktrees, a crash or a
  shutdown cut a create short: it removes each one with its branch off the event loop
  (keeping one an agent records) and keeps its name reserved until then (ADR 0020). One
  git will not let go of (a locked worktree, or a branch checked out elsewhere) stays
  pending, with a warning in the log, and is tried again at the next start. One whose
  repository is definitely gone is dropped, with a line at info level, so the project can
  be removed; its leftover directory is deleted too when it lies inside the state
  directory's `worktrees` (no `..` components), and left alone otherwise.
- **Shutdown** first stops every agent, including the processes a kill, restart or
  removal in flight took from it, then waits for the requests in flight and finishes each
  without starting anything: a kill answers as usual, a restart leaves its agent
  `Stopped` and answers `the daemon is shutting down`, a removal completes, and a
  worktree made for an agent that never started is removed with its branch, the create
  answered `the daemon shut down before <name> started; its worktree was removed`. It
  waits until 13 s after it began (ADR 0021), so a checkout hook that never ends cannot
  hold it: a create still running then is answered `the daemon shut down before <name>
  started; its worktree is removed at the next start`, and the next start removes what
  it made (ADR 0020); other requests still running are logged and get no answer.
  Processes found meanwhile get a second pass.

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

The daemon's environment is that of the process that started it, which may be a shell
where direnv loaded some other project's `.envrc`. `direnv exec` reverts that diff before
loading, so agents with an `.envrc` of their own never see it. For the rest, the daemon
runs `direnv export json` in `/` once at startup; when that prints a revert, every agent
without an `.envrc` runs as `direnv exec / <cmd...>` (direnv keeps the working directory)
and sees the variables as they were before direnv loaded (ADR 0016). With no `direnv` on
`PATH`, no inherited diff, or a `DIRENV_DIFF` direnv cannot read (logged), agents start
without the wrapper.

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
has both removed again, while a branch that existed before is kept. The cleanup runs
`git worktree prune` only when git had registered the failed path and `git worktree
remove` could not remove it, so other worktrees whose directories are missing stay
registered. A spawn failure after
a successful add keeps the agent as `Exited` with its error in `AgentInfo.last_error`, and
its worktree, so it can be restarted. The agent's cwd is the worktree, and
`AgentInfo.worktree` holds its path, branch and base. Renaming the agent keeps its branch
and worktree path.

After the add, files named by the project's `.worktreeinclude` that git ignores are copied into
the worktree, skipping any already there. `git ls-files --others --ignored --exclude-standard
--directory` lists the ignored files and each directory ignored as a whole once, without
walking it; such a directory is searched only when an include pattern names it or a path in
it (ADR 0018). Within those paths, the files `ls-files` lists with the include patterns are
copied when git ignores them: a file the first listing names on its own is ignored, and
within a directory it lists whole `ls-files --exclude-standard` tells. A failure is logged
and the agent starts anyway. A `CreateAgent.setup` the user approved in the TUI (ADR 0014) wraps the
agent's first start in `sh -c`, which runs the script in a shell of its own in the agent's
terminal and then starts the agent whatever the script did.

direnv allows an `.envrc` by its path and content, so a new worktree's checked-out
`.envrc` starts out blocked, and `direnv exec` refuses it until it is allowed. Right after
the worktree is added, and only when `direnv` is on `PATH`, the daemon runs `direnv allow
<worktree path>` when the worktree's `.envrc` is byte-identical to the project's and
`direnv status --json`, run in the project, reports the project's `.envrc` as allowed
(ADR 0008). In every other case (different content, a project `.envrc` the user has not
allowed, no project `.envrc`, or a direnv call that fails or prints something unparsable)
it leaves direnv untouched, logs any error, and still creates the agent, which then exits
at once on the blocked `.envrc`. When a worktree agent exits and its screen shows direnv's
`.envrc is blocked` error, the daemon sets `last_error` to `direnv blocked the worktree's
.envrc; run: direnv allow <worktree path>`, and the TUI shows it in the pane as `failed to
start: …`; after the allow, `r` restarts the agent.

`GitStat` and `GitDiff` run on their own thread and answer when git finishes, so the event
loop keeps serving. For a worktree agent they compare the working tree with `git
merge-base <base> HEAD` (committed and uncommitted work); for any other agent with `HEAD`
(uncommitted changes only). Untracked files that are not ignored count as new files in both:
the diff runs against a copy of the index, in the temp dir, where `git add --intent-to-add
--all` has listed them, so the agent's own index is never written. `uncommitted` is the
count of `git status --porcelain --untracked-files=normal` entries. A diff too large for
one message is an error.

`RemoveAgent { remove_worktree: true }` refuses while `git status --porcelain
--untracked-files=normal` lists anything, answering `worktree has uncommitted changes:`
and the changed paths, one per line (the new path of a rename), and leaves the agent
running. Both this check and the `git -c status.showUntrackedFiles=normal worktree
remove` that follows see untracked files even when the user's git config hides them.
`force: true` skips the check and runs `git worktree remove --force`. Without `remove_worktree` the worktree stays
on disk. Its branch is deleted only by a `RemoveAgent` with `delete_branch`, which needs
the branch merged into its base (`git merge-base --is-ancestor`); see ADR 0013.

### The TUI

`crates/tui` is a client like the CLI; it holds no agent state of its own beyond what the
daemon last told it.

- **Model** (`app.rs`): `App` holds projects, agents, selection, focus (projects, agents,
  pane, diff or scroll), the open popup, the subscribed agent's screen, the git stats and
  diff the daemon sent, and the pane's text selection. Key, paste and mouse events and
  daemon messages go in; `Effect`s come out: `Send(Request)`, `Copy`, `Notify`,
  `ApproveSetup` (remember an approved worktree setup script), `RunInTerminal` (a
  `terminal` command, with the TUI suspended), `RunCommand` (a background command),
  `RememberProjects`, `ForgetProject`, `Quit` and `StopDaemonAndQuit`. It does no I/O, so
  behaviour is tested without a terminal or a daemon. Selection follows an agent's id
  across list changes. The TUI is subscribed to at most one agent's screen, the selected
  one, and sizes a running agent's PTY to the pane.
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
  keys, which act without the prefix: `direct_leave` (`ctrl-]`, `ctrl-5`) and
  `direct_next` and `direct_previous`, which are unbound by default. Key strings such as
  `ctrl-q`, `shift-tab` or `A` parse into `KeySpec`s, and `describe` renders an action's
  keys for the help popup and the hints. The pane prefix state lives in `App` and is
  cleared whenever focus leaves the pane. `config::load` reads the config file once at
  startup into a `Config` (keymap, presets, theme, notifications, custom commands,
  warnings) that never fails: unusable entries keep their defaults and become warnings
  shown in the status bar
  ([configuration.md](configuration.md)).
- **Rendering** (`ui.rs`): draws the sidebar (Projects, Agents, Info), the agent pane or
  the diff view, the status line and popups from `App`; the pane copies the daemon's cell
  grid into the ratatui buffer, with the text selection drawn reversed on top. Colours
  come from the theme. The Info box lists one fact per row, a dim label with its value
  aligned after it: what a waiting agent waits for, worktree branch, git stat, then the PR,
  model, context (a bar where it fits), cost and plan limits its status line reported, and
  cwd. Context and limits turn yellow from 60 % and red from 80 %; paths and branches too
  long for the box are cut on the left. The box is sized to its rows: it grows as long as
  the Agents box keeps 8 rows, and keeps 6 rows while the content fits. The status line shows a transient message in red, then `│`
  and the key hint, cut off on the right. Below a 20×5 pane only "terminal too small" is
  drawn, and keys do nothing except quitting from the sidebar and leaving the pane. Status
  glyphs animate from a frame counter the runtime ticks every 100 ms while an agent is
  starting or working.
- **Mouse** (`app.rs`): the TUI captures the mouse. A click selects a row or focuses a box
  or the pane, a drag selects pane cells and copies them on release, a click outside a
  popup closes it, and the wheel scrolls the diff, the scroll-mode history or the pane's
  scrollback, or goes to the agent when it enabled mouse reporting. A click in the pane
  that ends in the cell it started in also goes to the agent, which the daemon reports
  only when the agent enabled mouse reporting; drags never reach the agent (ADR 0015). See
  [keybindings.md](keybindings.md#mouse).
- **Connection actor** (`connection.rs`): a task owning the `Client`. It forwards
  responses, errors and server events (agent list changes, screen frames) on one channel.
  `GitStat` and `GitDiff` go out without waiting for their replies, which the daemon may
  answer late; each reply comes back tagged with its agent id as `Incoming::GitStat` or
  `Incoming::Diff`, errors included, so a git error never looks like the failure of
  another request. Every other request also goes out without waiting for earlier replies,
  so input and server events keep flowing while the daemon works on a slow create, kill or
  removal; a relay task passes those replies and errors on in request order. When the daemon closes the connection
  it reports `Closed`, and the TUI exits with "the daemon stopped".
- **Runtime and terminal guard** (`run.rs`): refuses to start without an interactive
  terminal, connects (starting the daemon if needed; on a protocol mismatch it opens the
  restart prompt), then enters raw mode, the alternate screen, mouse capture, bracketed
  paste, focus reporting and, where supported, keyboard enhancement. A thread reads terminal
  events, polling in 50 ms slices so it can be paused; a read error means the terminal is
  gone and the TUI exits, even with SIGHUP and
  SIGTERM ignored ([ADR 0005](decisions/0005-vendor-crossterm-dead-tty-fix.md)). SIGHUP,
  SIGTERM and SIGINT also exit. The guard and a panic hook restore every terminal mode on
  any exit path, ignoring errors from a vanished terminal.
- **Custom commands** (`commands.rs`): `[[commands]]` from the config, expanded with
  shell-quoted values and run with `sh -c` by the runtime ([ADR
  0010](decisions/0010-custom-commands-run-by-the-tui.md)). A `terminal` command pauses the
  reader thread, hands it the terminal out of raw mode and the alternate screen, and
  restores both afterwards, dropping the SIGINT a ctrl-c also sent the TUI. `popup` and
  `none` commands run on a blocking task and report back to `App::command_finished`;
  `agent` commands become a `CreateAgent` request.
- **Project setup scripts** (`project_config.rs`, `approved.rs`): a project's
  `.lazyagents.toml` may carry a `[worktree] setup` script. The user approves it once per
  exact text (`ApproveSetup`); an edited script asks again ([ADR
  0014](decisions/0014-approve-project-setup-scripts.md)).
- **Review comments** (`review.rs`): maps each diff line to its file and line number,
  following the hunk headers' counts, and builds the prompt that sends an agent the
  comments on its diff. `App` keeps the comments per agent in memory.
- **Notifications** (`notify.rs`): bell, OSC 9 or OSC 777 for the `Notify` effect, and the
  window title, saved on start and restored on exit.
- **Clipboard** (`clipboard.rs`): the runtime handles `Copy` between draws, writing OSC 52
  to the terminal and flushing it, and piping the text to `pbcopy` or `wl-copy` when one
  is on `PATH`, with `LC_CTYPE=UTF-8` and no `LC_ALL`, so pbcopy never reads the text
  as MacRoman under a C locale. The copier runs on a short-lived thread that writes its stdin, closes it
  and waits for it, so it never blocks the UI or lingers as a zombie.

### Persistence and config

- State dir: `$LAZYAGENTS_STATE_DIR` (a relative path is taken from the current
  directory), default `~/.local/state/lazyagents/`, created with mode 0700: `state.toml`
  (projects, agents, session ids and titles, worktree info, worktrees being created), `daemon.sock`, `daemon.lock`,
  `daemon.log`, `hooks/<agent_id>.json`, `worktrees/`, and `known-projects.toml`, which
  only the TUI writes: every project it has seen, kept after removal for the add picker
  (`known.rs`, ADR 0009).
- Config: `$LAZYAGENTS_CONFIG`, else `$XDG_CONFIG_HOME/lazyagents/config.toml`, else
  `~/.config/lazyagents/config.toml`: keybindings, agent presets, theme colours,
  notifications, custom commands. A missing file gives the built-in defaults; anything
  unusable becomes a warning in the TUI status line ([configuration.md](configuration.md)).
- `state.toml` is written atomically and durably on every metadata change: a temp file is
  written and synced, renamed over it, and the directory synced.

## Error handling

- Protocol version mismatch → the client fails with `VersionMismatch`, which suggests
  `lazyagents shutdown`. A mismatched daemon cannot be asked to stop over the socket, so
  `client::stop_daemon` sends SIGTERM to the pid in `daemon.lock` and waits up to
  `protocol::SHUTDOWN_WAIT` (20 s) for the lock to be released. A daemon writes its pid just after taking the lock, so
  an empty pid, or one whose recorded start time does not match, is read again for up
  to 500 ms; the lock, the pid and its start time are checked once more right before
  the signal. An entry that still fails the check is never signalled: `stop_daemon`
  fails with `<lock> names pid <pid>, which is no longer the daemon; try again`. The
  daemon's SIGTERM handler stops every agent first.
  It never escalates to SIGKILL, which would orphan the agents' process trees; on
  timeout it fails with `daemon <pid> did not stop within 20s`. A daemon that answers on
  the socket while nothing holds the lock cannot be identified, so `stop_daemon` fails
  and asks the user to stop it by hand, as it does when the lock stays without a pid.
  `shutdown` uses it, adds which daemon answered (version and protocol) to an error,
  and prints `daemon stopped` only on success. The TUI shows a full-screen popup naming
  both protocol versions: `y` shows `stopping daemon <pid>…`, stops the daemon this way
  and continues with a new one (an error there says `starting a new daemon`); `ctrl-c`,
  SIGINT, SIGTERM, SIGHUP or a vanished terminal meanwhile abandon the wait and quit. `n`, `q` or `esc` exits with
  the mismatch error.
- Socket exists but connect fails → spawn a daemon, which removes the stale socket once
  it holds the lock.
- A daemon that exits while the client waits for it to start → the client fails at
  once with "daemon exited during startup; see <log>", unless another daemon holds the
  lock (a concurrent start), in which case it keeps waiting for that one. The client
  reaps the daemon process it started.
- Daemon crash (SIGKILL) → its PTYs close and agents receive SIGHUP; processes that
  ignore it or left the agent's session can outlive the daemon. On the next start agents
  load as `Stopped` and can be restarted.
  A worktree it was creating, and its branch, are removed at the next start.
  Claude resumes only sessions that received a prompt
  (see [ADR 0006](decisions/0006-resume-only-prompted-sessions.md)).
- A corrupt or empty `state.toml` → the daemon refuses to start and leaves the file
  untouched; for an empty one it says `<path> is empty; restore it from a backup or delete
  it to start with no agents`. The daemon never writes an empty file (even no projects and
  no agents are written out), so an empty one is the sign of a lost write, not of an empty
  state.
- Spawn failure (command not found, cwd missing) → agent `Exited`, error rendered in the
  pane and the status line.
- Worktree errors (branch exists, base missing) → shown in the status bar; nothing is
  created.
- `lazyagents hook` never blocks or fails Claude: it checks its environment, reads stdin and
  talks to the daemon within one 200 ms budget, prints nothing and always exits 0.
- Fatal errors while the daemon starts are written to `daemon.log`.
- `lazyagents shutdown` waits up to `protocol::SHUTDOWN_WAIT` (20 s) in all for the
  daemon's reply, which comes after every agent tree is gone, and for the socket to
  disappear. It stays above the daemon's slowest shutdown: the first pass (up to 10 s
  for a running process snapshot, the 2 s grace period and the `ps` readings after it)
  ends by the 13 s at which shutdown stops waiting for requests in flight, and the
  second pass adds the grace period and its readings.
- All daemon errors logged with `tracing` to `daemon.log`; TUI shows the last error in
  the status line.

## See also

- [protocol.md](protocol.md): wire format between the daemon, the TUI and the CLI.
- [keybindings.md](keybindings.md): the TUI key map.
- [configuration.md](configuration.md): the config file.
- [status-detection.md](status-detection.md): how agent status is derived from hooks and the
  pane.
- [decisions/](decisions/): ADRs recording the design choices behind this system.
