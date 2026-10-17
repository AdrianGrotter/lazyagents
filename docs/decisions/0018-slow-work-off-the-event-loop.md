# 0018. Run slow daemon work off the event loop and refuse overlapping lifecycle requests

Date: 2026-10-17
Status: Accepted

## Context

The daemon runs one event loop that owns all agent state. Kill, restart, remove and shutdown stopped agents inline: a `ps` reading, a grace period of up to a second and a wait of up to two seconds for a running snapshot. Creating a worktree agent ran `git worktree add` (with the user's checkout hooks), the `.worktreeinclude` copies and direnv inline, and removing one ran `git status` and `git worktree remove`. Meanwhile every pane froze and no connection was greeted: `lazyagents hook` gives up after 200 ms, so a `PermissionRequest` or `Notification` arriving during a kill or a create was lost and the agent never showed that it needed input, and a CLI command gave up after its 2 s greeting timeout and started a redundant daemon. `GitStat`, `GitDiff` and `MergeAgent` already ran on threads and answered later.

Moving the work to threads lets several requests touch the same agent, or the same name, while one of them is still running.

## Decision

The connection's own task sends `Welcome`. Kill, restart, remove and worktree creates run their slow part on a thread of their own and continue on the loop, from its 33 ms tick, when it finishes; only then are they answered. The loop keeps all state; a thread gets copies and returns results.

- An agent with a kill, restart or remove in flight is busy: another of the three is refused with `<name> is stopping; try again once that is done` (or `restarting`, `being removed`). Other requests are served against the agent as it is.
- A worktree create reserves its name in its project; a second create of that name, a rename to it and removing the project are refused until it is answered. The agent is added, and started, only when its worktree is ready.
- A stop that a running process snapshot covers waits, on its thread, for what the loop forwards from that snapshot, as the inline stop did.
- Shutdown stays on the loop, which ends with it: it stops every agent, then waits for every job and finishes it without starting anything, removing a worktree whose agent never started, then stops what turned up meanwhile.

## Alternatives

- Queue requests per agent and run them in order: a kill queued behind a restart would answer only after the restart spawned a process it then stops, and the queue state has to survive shutdown; with the TUI waiting for each answer, overlaps only come from a second client, which a clear refusal serves as well.
- Coalesce identical requests (a second kill joins the first): needs per-request reply lists for a case the TUI never produces, and does not help for kill-then-remove.
- Show the busy state as a new `Status`: a protocol change every client must learn; the refusal message is enough for the rare overlap.
- Make shutdown abandon running jobs: git would finish a worktree add in an orphaned process and leave a worktree and branch nobody owns.
- `tokio::task::spawn_blocking` in place of threads: the daemon's existing off-loop work (snapshots, reapers, git queries) uses `std::thread` with an inline fallback, and its unit tests drive `Daemon` without a runtime.

## Consequences

Panes keep streaming and hooks are applied while agents are stopped, created or removed; the daemon only blocks while shutting down, when nothing is left to serve. Answers to these requests arrive up to one tick after their work finishes and may follow later requests' answers. A client that sends overlapping lifecycle requests for one agent gets an error to retry. Shutdown can take as long as the slowest create in flight, since a checkout hook cannot be cut short without leaving a half-made worktree. The TUI still waits for each answer before sending its next request, so its own input waits behind a long create.
