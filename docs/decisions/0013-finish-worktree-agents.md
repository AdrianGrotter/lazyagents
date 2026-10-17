# 0013. Finish a worktree agent from one menu; delete its branch only once merged

Date: 2026-10-17
Status: Accepted

## Context

A worktree agent's work ends in one of a few ways: merged into its base, pushed as a pull
request, or thrown away. Until now lazyagents offered only removal, and removal never
deleted the agent's branch, so finished branches piled up and a new agent could not reuse a
removed agent's name until its `lazyagents/<name>` branch was deleted by hand.

Merging touches the user's own checkout, and deleting a branch can lose work, so both need
guard rails that are hard to get wrong.

## Decision

`F` on a worktree agent opens a finish menu in which picking an entry is the confirmation,
each entry naming what it does and to which branch:

- merge the branch into its base, done by the daemon in the project's own checkout with
  `git merge --no-edit`, refused unless that checkout is on the base with no tracked
  changes and the agent's worktree has nothing uncommitted; a failed merge is aborted;
- push the branch and open a pull request with `gh pr create --fill`, run in the terminal
  so `gh` can ask what it needs, shown only when `gh` is on `PATH`;
- remove the agent and its worktree and keep the branch, as `d` does;
- remove the agent, its worktree and its branch, refused unless the base already contains
  every commit of the branch.

`RemoveAgent` gains `delete_branch`, and a branch is deleted only through it. Plain `d`
still keeps the branch.

## Alternatives

- Keep never deleting branches: safest, but leaves every finished agent's branch behind and
  blocks reusing names.
- Delete with `git branch -D` on request, merged or not: one keystroke could destroy
  unmerged work.
- Merge inside the worktree (rebasing the base onto the branch there): git refuses to move a
  branch checked out elsewhere, and the user expects the base to change where they look at
  it, in their checkout.
- Leave merging and pull requests to custom commands only: possible today, but every user
  would rebuild the same guard rails, and the merged-only branch deletion needs the
  daemon.

## Consequences

Finishing an agent takes one menu, and `lazyagents/<name>` branches go away once merged.
The daemon now changes the user's checkout, but only through a merge git itself would
perform, from a clean state, and it never leaves a half-done merge behind. Squash and
rebase merges are not offered; a custom command can do those.
