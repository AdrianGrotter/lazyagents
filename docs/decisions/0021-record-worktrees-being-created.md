# 0021. Record worktrees being created in state.toml and remove them at the next start

Date: 2026-10-18
Status: Accepted

## Context

A worktree create runs `git worktree add -b lazyagents/<name>` with the user's checkout hooks off the event loop, and the agent is recorded only once the worktree is ready (ADR 0019). A daemon that crashes, is killed, or stops before the create finishes leaves a worktree directory and a branch that no agent records. Both block creating that name again (`'<path>' already exists`, `a branch named … already exists`), and the user has to find and remove them by hand.

Removing them later needs to know which worktree and branch the create made. A branch named `lazyagents/<name>` can also exist before the create (kept when an earlier agent was removed without deleting its branch); git then refuses the add, and deleting that branch would lose the user's work.

## Decision

The create plans its worktree first, off the loop: branch, base, path, and whether the path or the branch exists already. Only when neither does, the loop records `{ project, name, path, branch }` under `pending_worktrees` in `state.toml` before git adds the worktree. The agent is recorded in the same save that drops the pending entry; a failed add drops it once git undid its work. A daemon that starts with pending worktrees removes each one, with its branch, off the loop, unless an agent records that path, and reserves its name until then. A pending worktree is dropped only once its directory, git's record of it and its branch are all gone; one git keeps (a locked worktree) stays pending and is tried again at the next start. The field is optional and left out when empty, so files written before it load unchanged and an older daemon reading a newer file ignores it.

## Alternatives

- Record the create before checking the branch: a crash during a create that git would refuse makes the next start delete a branch the user already had.
- A marker file per create, written by the create's thread: no change to `state.toml`, but two files that must agree; a crash between saving the agent and removing the marker needs its own rule at startup.
- Scan `worktrees/` at startup for directories no agent records: cannot tell which branch was made for one, and removes whatever else happens to be there.

## Consequences

A crash or a cut-short shutdown no longer leaves a worktree that blocks its name; the next start cleans up after it. A worktree create takes one more tick, and one more save of `state.toml`, before git starts. A user who makes the branch `lazyagents/<name>` by hand in the instant between the plan and the add, and whose daemon then crashes, would lose that branch at the next start.
