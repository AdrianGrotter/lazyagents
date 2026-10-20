# 0016. Revert an inherited direnv environment by running agents under `direnv exec /`

Date: 2026-10-17
Status: Accepted; amended by 0034

## Context

The daemon inherits the environment of whatever first started it, usually a shell in which direnv may have loaded some project's `.envrc`. Agents inherit the daemon's environment, so an agent in an unrelated project saw that project's exported variables (PATH entries, `DATABASE_URL`, credentials) for the daemon's whole life. direnv records what it changed in `DIRENV_DIFF` (zlib-compressed, base64url-encoded JSON of previous and next values) next to `DIRENV_DIR`, `DIRENV_FILE` and `DIRENV_WATCHES`.

direnv 2.37 behaves as follows, checked with an isolated HOME: `direnv exec <dir> <cmd>` first reverts the current `DIRENV_DIFF` and then loads `<dir>`'s `.envrc`, if any, without changing the working directory; so `direnv exec /` gives the pre-direnv environment, and agents with an `.envrc` of their own, already run under `direnv exec <envrc dir>`, never saw the leak. `direnv export json` run in `/` prints the revert as JSON, and nothing when there is no diff. An unreadable `DIRENV_DIFF` makes both commands exit 1. The daemon cannot change its own environment (`unsafe_code` is forbidden, and `set_var` is unsafe), and `alacritty_terminal` can add variables to a PTY child but not remove them.

## Decision

At startup, when `direnv` is on its `PATH`, the daemon runs `direnv export json` in `/`. If that succeeds and prints a revert, every agent with no `.envrc` of its own (within the project) runs as `direnv exec / <cmd...>`; agents with one keep `direnv exec <dir>`. If direnv is missing, prints nothing, or fails (a corrupt `DIRENV_DIFF`), agents start as before and the failure is logged.

## Alternatives

- Revert in the client before it spawns the daemon, applying `direnv export json`'s output with `Command::env`/`env_remove`: also cleans the daemon's own git and direnv subprocesses, but misses a daemon started directly with `lazyagents daemon`.
- `direnv exec <agent cwd>` for every agent: direnv searches ancestors of the cwd, so it would load, or refuse as blocked, an `.envrc` outside the project, which lazyagents deliberately ignores.
- Decode `DIRENV_DIFF` in Rust and apply it per agent: needs zlib and base64 dependencies, works without direnv but duplicates its format, and still cannot unset variables through the PTY spawn without a wrapper such as `env -u`.
- The daemon re-execs itself under `direnv exec /` at startup: it would also clean the daemon's own subprocesses. But a direnv failure (a corrupt `DIRENV_DIFF`, a blocked `/.envrc`) would then keep the daemon from starting, where the probe only costs the revert. The re-exec also replaces the process, so the daemon would need a guard to avoid re-exec loops.
- Wrap only when `DIRENV_DIFF` is set, without probing: a corrupt diff would then stop every agent from starting.

## Consequences

Agents see the environment the starting shell had before direnv loaded anything, plus their own project's `.envrc`. One `direnv export` runs at daemon start, and affected agents pay one `direnv exec`. The daemon's own subprocesses (git, `direnv status`/`allow`) still run with the inherited environment. Like direnv itself on `cd`, the revert restores the recorded previous values, so a change the shell made after direnv loaded to a variable direnv touched is undone too. The startup probe and `direnv exec /` run direnv in `/`, so a `/.envrc` the user has allowed would be evaluated by the probe and its variables would reach every agent without an `.envrc` of its own.

Amended by [0034](0034-load-an-allowed-envrc-above-the-project.md): an agent with no `.envrc` in the project loads the nearest one above the project root when direnv already allows it, and runs under `direnv exec /` only when that does not apply either. A blocked `.envrc` above the project is still ignored.
