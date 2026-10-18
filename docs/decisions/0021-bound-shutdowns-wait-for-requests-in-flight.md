# 0021. Bound shutdown's wait for requests in flight

Date: 2026-10-18
Status: Accepted

## Context

Shutdown stops every agent, waits for every request running off the event loop and finishes it, then stops what turned up meanwhile (ADR 0019). The wait had no limit. A worktree create runs the user's checkout hooks, and a merge or a removal runs git with the user's hooks and config; one that never ends held the daemon, its socket and its lock forever. `lazyagents shutdown` and the TUI's version-mismatch flow give up after `protocol::SHUTDOWN_WAIT` (20 s) and report that the daemon did not stop, and a new daemon cannot start while the old one holds the lock.

ADR 0019 waited because abandoning a create leaves a worktree and branch nobody owns. ADR 0020 now records a worktree being created in `state.toml` and removes it at the next start.

## Decision

Shutdown waits for the requests in flight until 13 s after it began, then stops waiting. A create still running is answered `the daemon shut down before <name> started; its worktree is removed at the next start`; other requests still running are logged by name and get no answer. Their threads end with the daemon process. The tracked processes a kill, restart or removal in flight took from its agent stay listed with the agent until that stop finishes, so both of shutdown's passes cover them even when their stop is abandoned. The deadline counts from the start of shutdown, so the first pass (up to 10 s for a running snapshot, the 2 s grace period and `ps` readings) ends by it and the second pass (grace and readings) still fits in `SHUTDOWN_WAIT`; a test pins that sum.

## Alternatives

- Keep waiting without a limit: one hung hook keeps the daemon, and every client's attempt to stop or replace it, stuck.
- A fixed wait after the first pass: either too short for an ordinary create, or the total exceeds `SHUTDOWN_WAIT`, since the first pass can take 12 s.
- Raise `SHUTDOWN_WAIT`: only moves the limit; a hook that never ends still exceeds it.
- Kill the git processes still running: git and its hooks would stop half way, and their children are not ours to signal; the next start's cleanup handles a half-made worktree anyway.

## Consequences

The daemon stops within `SHUTDOWN_WAIT` whatever its requests' hooks do. A create cut short is answered and cleaned up at the next start, while the git command it ran may still finish on its own after the daemon exited. A removal or merge cut short is not answered; its git command may still finish after the daemon exited, so the worktree or branch it was removing may or may not be gone at the next start.
