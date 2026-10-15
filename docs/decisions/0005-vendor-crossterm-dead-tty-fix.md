# 0005. Vendor crossterm with a dead-terminal fix

Date: 2026-10-15
Status: Accepted

## Context

When the terminal running a lazyagents UI disappears without the process receiving SIGHUP or SIGTERM (for example when it runs under `just` in another process group and the tmux pane is killed), the tty stays readable and every `read` returns 0 bytes. crossterm 0.29.0's default event source (`src/event/source/unix/mio.rs`) loops on that read forever, so `event::poll` never returns and the process burns a full CPU core. Upstream has the bug open as crossterm-rs/crossterm#1067, unreleased.

## Decision

`vendor/crossterm` holds crossterm 0.29.0 from crates.io, wired in with `[patch.crates-io]`. It differs from upstream in two places: the mio event source returns `UnexpectedEof` on a zero-byte read and propagates read errors other than `WouldBlock`/`Interrupted`, and one closure drops redundant parentheses that newer rustc warns about. `scripts/test-tui-hangup.sh` (part of `just check`) kills the TUI's terminal while it blocks SIGHUP, SIGTERM and SIGINT and requires the process to exit with status 0.

## Alternatives

- Pin the PR's fork as a git dependency: less code in the repo, but builds would depend on a third-party branch.
- Work around it in the app (input thread, write-failure detection): the spin stays inside crossterm and still burns a core until detected.
- Rely on SIGHUP: delivery depends on process groups the app does not control.

## Consequences

Any UI built on crossterm in this workspace exits with an error when its terminal is gone. The patch is dropped, and this ADR superseded, once a crossterm release contains the fix.
