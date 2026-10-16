# Plan 4 overnight notes

Plan 4 (worktrees, config, keys, mouse) ran unattended overnight on 2026-10-15/16 and was
merged into `main` locally by rebase and fast-forward. Nothing was pushed. This file lists
what needs a decision, what to know before using the new build, and every ruling made
without you, with what it costs if it was wrong.

## Decide together

1. **Auto-allow direnv in new worktrees.** In a repo with a tracked `.envrc`, this one
   included, a new worktree agent fails at once because direnv blocks the new directory.
   Today the pane shows `run: direnv allow <worktree path>`. The reviewer proposed that
   lazyagents runs `direnv allow <worktree>` itself, but only when the worktree's
   `.envrc` is byte-identical to the project's and the project's is already allowed. This
   was not done, because it writes your direnv allow list outside the repo. If you want
   it, it needs an ADR and about half a day.
2. **False "failed to start" hint.** The direnv hint fires for any exited worktree agent
   whose last screen contains direnv's "is blocked" text. That can happen after a normal
   run, for example a shell that hit a nested `.envrc`. Fix: fire only on an early
   non-zero exit. This is small; it waits on decision 1.
3. **Stops can pause the daemon for up to 2 s.** A kill, restart or remove now waits for
   any running `ps` snapshot, so processes found late are reaped too. This closed a real
   leak. The wait also applies when the snapshot doesn't involve the stopped agent, which
   pauses frames and hooks briefly. Fix: skip the wait when none of the stopped agents is
   running.
4. **Two rare process-tracking gaps remain.**
   - If `ps` fails twice in a row during a stop, that agent's tracked detached processes
     are neither signalled nor kept.
   - Just after a daemon has been SIGKILLed, the lock file can briefly hold the old pid.
     A `stop_daemon` racing a new daemon's start in that window could signal a recycled
     pid. This predates plan 4.

   Both need very unlucky timing. Worth a small hardening task.
5. **Status polling costs about 2–3% of a core** while agents run (a `ps` call once a
   second). This is carried from plan 3 and accepted so far.

## Before you use the new build

- **Your running daemon speaks protocol 1.** The new TUI shows a popup offering to restart
  it. `y` stops every running agent; restart them with `r`, and Claude resumes its
  sessions. Answer `y` soon after installing: old agents' hooks start sending v2 messages
  that the old daemon drops, so their status freezes until the restart.
- **Leaving the pane on a Norwegian Mac.** `ctrl-]` may be unreachable, so use `ctrl-q q`,
  or bind `[keys.pane] direct_leave` in `~/.config/lazyagents/config.toml` (see
  `docs/configuration.md`).
- **Worktrees live in `~/.local/state/lazyagents/worktrees/`.** Deleting the state dir
  deletes them, including uncommitted work. Branches (`lazyagents/<name>`) are never
  deleted by lazyagents. To reuse a removed agent's name, delete or rename its branch
  first.
- **Claude asks to trust each new worktree directory** the first time it starts there.
- **tmux needs a setting for OSC 52 copy.** Set `set -g set-clipboard on`. pbcopy covers
  the Mac either way, now as UTF-8.
- **WezTerm's left Option key** is sent as Alt by default, so `@`, `[` and `|` typed with
  left Option reach agents as alt chords. Change that in WezTerm's settings if it bites.
- **Detached processes can escape.** A process that double-forks and calls setsid within
  about a second of starting, and whose parent exits in that time, escapes tracking. This
  is a documented limit.
- **A slow `git worktree add` pauses the daemon**, for example on a huge repo or with heavy
  post-checkout hooks.
- **`lazyagents new` has no `--worktree` option.** Worktrees are created from the TUI.

## Rulings made without you

Each ruling gives what was decided, why, and the cost if it is wrong.

- **Detached exit reaper** (Task 1). In-flight reaps stay visible to the stop paths, so a
  shutdown or kill can't race them. Cost: small.
- **Test git config** (Task 3). Test repos ignore your global git config; the daemon's git
  calls use it. Cost: small, and the daemon follows things like `textconv`.
- **Spawn failure keeps the worktree** (Task 3). If an agent fails to start after its
  worktree was created, the agent stays as Exited, with its error and the worktree.
  Branches can't be deleted, so a rollback would block a retry anyway. Cost: a leftover
  worktree until you `rm`.
- **Failed `worktree add` cleanup** (Task 3). The path is removed, and the branch is
  deleted only if the failed add created it. Cost: small.
- **Existing target path** (Task 3). The add refuses a target path that already exists,
  so cleanup only touches what it created. Cost: a stale directory blocks that agent
  name until removed by hand.
- **Git queries don't take locks** (Task 3). Read-only git queries run with
  `GIT_OPTIONAL_LOCKS=0`, so polling never breaks an agent's commit. On a detached HEAD
  the base is stored as the commit sha. Cost: none.
- **One prefix flag** (Task 4). The too-small screen's prefix flag became the single
  prefix state, and is cleared only on the transition back from too-small. Cost: none.
- **Exact sidebar modifiers** (Task 4). Sidebar keys match modifiers exactly, so `alt-j`
  and `shift-down` no longer move the selection. Cost: noticed only if you used those
  keys.
- **Tests never read your config** (Task 5). Every test and script sets
  `LAZYAGENTS_CONFIG` to a temp path. Cost: none.
- **Config details** (Task 5). Duplicate bindings resolve in file order. A configured
  prefix also becomes `send_prefix` unless that is set too. A pending prefix shows in the
  `working` colour. Cost: small.
- **New theme slots** (Tasks 6, 7). Diff colours (`diff_add`, `diff_remove`, `diff_hunk`)
  and the scroll border (`scroll`) are theme slots. Cost: none.
- **Git requests don't block keys** (Task 6). The TUI sends git requests without waiting
  for earlier replies, and replies are tagged by agent. Cost: medium if wrong. Reviewed
  twice and found sound.
- **Info panel** (Task 6). It grows to fit its content when the sidebar has room. Cost:
  small.
- **Scroll-mode exit** (Task 7). Leaving scroll mode returns to the pane only if you
  entered from the pane and the agent still runs; otherwise it goes to Agents. Cost: none.
- **No SIGKILL** (Task 8). Stopping a mismatched daemon never sends SIGKILL, because that
  would orphan its agents. A hung daemon must be stopped by hand. Cost: manual stop in
  that rare case.
- **Tested against the real old daemon** (Task 8). The mismatch popup was tested against
  a real daemon built from the old main, in a temp directory. Cost: none.
- **Mouse clicks stay local** (Task 9). Clicks and drags in the pane always select text,
  never go to the agent; the wheel still does. Cost: a mouse-driven TUI agent can't be
  clicked.
- **Copy safety** (Task 9). OSC 52 is written between frames, and pbcopy children are
  always reaped. Cost: none.
- **Mouse details** (Task 9). A click on a box's border focuses that box. A click or drag
  in scroll mode selects history. An empty selection copies nothing. Cost: small.
- **direnv is not auto-allowed** (final review). The pane shows a hint instead; see
  decision 1. Cost: one manual `direnv allow` per worktree in direnv repos.
- **Exec timing bounds** (final fix wave). The CLI exec tests only catch hangs (10 s). The
  hook's 200 ms budget is tested in-process, because a debug binary takes 1–2.6 s to
  start under the parallel suite. Cost: a slow-hook regression is caught only by the
  in-process test.
- **Stops wait for snapshots** (final fix wave). A stop waits up to 2 s for a running
  snapshot; see decision 3. Cost: kill and restart are up to 2 s slower when `ps` is slow.
