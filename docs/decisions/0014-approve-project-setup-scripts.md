# 0014. Run a project's worktree setup only after the user approves its exact text

Date: 2026-10-17
Status: Accepted

## Context

A new worktree starts without what the main checkout has accumulated: dependencies, build
output, local env files. Projects want a setup step (`npm install`, `cp .env.example .env`)
and gitignored files copied in, as Conductor, Superset and Claude Code's `.worktreeinclude`
offer. The step has to come from the repository to be shared by a team, which means
lazyagents would run code it found in a repository, perhaps one just cloned. ADR 0008 met
the same question for `.envrc` and allowed only what the user had already allowed.

## Decision

A project's `.lazyagents.toml` may hold `[worktree] setup = "<script>"`. When the user
creates a worktree agent, the TUI reads it and, unless the user has approved that exact text
for that project before, shows the script and asks: `y` runs it and remembers the approval,
`s` creates the agent without it, `n` creates nothing. Approvals live in the state
directory's `approved-setups.toml`, written by the TUI (as ADR 0009 keeps known projects);
an edited script asks again. The approved text travels in `CreateAgent.setup`, so the daemon
runs what the user saw, in the agent's terminal and inside its direnv environment, before
the agent starts, on its first start only. `.worktreeinclude` needs no approval: the daemon
copies the gitignored files it names, which runs nothing.

## Alternatives

- Run the setup without asking, as the repository says: a cloned repository could run
  anything the moment an agent starts.
- Approve once per project, whatever the script: a later commit could change the script
  behind the approval.
- Keep approvals in the daemon and have it check them: the daemon would need its own user
  state, and the script could change between the check and the run; sending the approved
  text avoids both.
- Run the setup in the daemon before spawning the agent: a long `npm install` would either
  block the daemon or need progress reporting of its own; in the agent's terminal the user
  watches it as they would by hand.

## Consequences

Teams can share worktree setup in the repository, and nothing from a repository runs
without the user reading it first. Setup output scrolls by in the agent's pane before the
agent starts, and a failing setup is reported there without stopping the agent. Agents
created by the CLI never run a setup, since the CLI cannot ask.

The popup shows the whole script: long lines wrap, control and invisible characters appear
escaped, and a script taller than the screen scrolls. `y` is refused until its last row has
been shown, and on a terminal too small to show it, so text that was never on screen cannot
be approved. `s` and `n` run nothing from the repository and work at any time.
