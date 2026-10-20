# 0034. Load an `.envrc` above the project root only when direnv already allows it

Date: 2026-10-21
Status: Accepted

## Context

Agents run as direct children of the daemon, under `direnv exec <dir>` only when an `.envrc` is found from the agent's working directory up to the project root (ADR 0016). Some setups keep the `.envrc`, often a nix flake shared by several repos, in a directory above the project: the project is `~/git/acme/app` and the `.envrc` is `~/git/acme/.envrc`. In a shell, direnv finds it by searching ancestors and loads it once allowed; agents never got it, so tools such as `node` or `pnpm` from the flake were missing. The search stopped at the project root on purpose: an unrelated `.envrc` higher up, in a home directory or a parent folder, would otherwise be loaded into every agent, or block every agent's start while unallowed. A worktree agent's working directory is under the state directory's `worktrees/`, so searching its own ancestors would find nothing of the project's.

## Decision

When no `.envrc` in the project applies to an agent, the daemon looks for the nearest `.envrc` above the project root, starting from the project root for worktree agents too. If `direnv status --json`, run in that directory, reports it as allowed, the agent runs as `direnv exec <that dir> <cmd...>`. A blocked one, or a direnv call that fails, is ignored as before (logged in the failure case), never prompted for and never allowed by lazyagents; a farther `.envrc` past a blocked nearer one is not considered, matching direnv's own search. An `.envrc` in the project still wins, and with nothing loaded the `direnv exec /` revert of ADR 0016 still applies. The status check runs a subprocess, so it runs off the event loop (ADR 0019): with the worktree in a worktree create, with the stop in a restart, and on a thread of its own for a plain create when an `.envrc` above the project exists.

## Alternatives

- Keep stopping at the project root and tell users to add a project `.envrc` with `source_up`: works, but needs a file in every repo, often one that cannot be committed.
- Search all ancestors and load whatever direnv finds, as `direnv exec <cwd>` would: a blocked `.envrc` in a parent folder would stop every agent from starting, and the user would be pushed to allow content just to get agents running.
- Allow the outer `.envrc` automatically, as ADR 0008 does for an identical worktree copy: there is no already-allowed copy to vouch for it, so it would trust content the user never reviewed.
- Make the outer `.envrc` a per-project setting: explicit, but asks the user to repeat a choice direnv's allow list already records.

## Consequences

An allowed `.envrc` above a project loads into its agents as it would in the user's shell, with no configuration. Allowing or revoking it takes effect at the agent's next start or restart. An `.envrc` the user allowed in a parent folder for another purpose, such as one in their home directory, now also loads into agents of every project under it that has no `.envrc` of its own. A create in a project with an `.envrc` above it is answered after one `direnv status` instead of at once.
