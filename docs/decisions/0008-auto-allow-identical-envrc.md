# 0008. Auto-allow a worktree's `.envrc` only when it matches the project's allowed one

Date: 2026-10-16
Status: Accepted

## Context

direnv allows an `.envrc` by its path plus its content. A worktree agent gets a fresh checkout at a new path, so its `.envrc` starts out blocked even when the identical file in the project is allowed. `direnv exec` then refuses, and the agent exits at once. Every new worktree agent in a direnv repo needed a manual `direnv allow <worktree path>` and a restart.

## Decision

Right after adding a worktree, the daemon runs `direnv allow <worktree path>` only when the worktree's `.envrc` is byte-identical to the project's `.envrc` and `direnv status --json`, run in the project, reports that `.envrc` as allowed. Anything else, including an error from direnv or output it cannot parse, leaves direnv untouched; the error is logged and the agent is still created. The existing "direnv blocked the worktree's .envrc; run: direnv allow <path>" hint covers every case it does not allow.

## Alternatives

- A manual hint only: safe, but every new worktree agent in a direnv repo fails its first start.
- Skipping direnv in worktrees: the agent starts, but without the nix/direnv environment it needs.
- Allowing every worktree's `.envrc`: trusts content the user never reviewed, such as a base branch that changed `.envrc`.

## Consequences

New worktree agents in a repo whose `.envrc` the user already allowed start without a manual step. lazyagents now writes to the user's direnv allow list. The trust extends to files the `.envrc` loads from the worktree: `use flake`, for example, evaluates the worktree's `flake.nix`, and those files come from the chosen base branch. A base branch that changed them, with an unchanged `.envrc`, therefore runs them without review.
