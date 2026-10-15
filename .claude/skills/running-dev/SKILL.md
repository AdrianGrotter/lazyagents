---
name: running-dev
description: "Drive the lazyagents TUI in an isolated tmux session: start it, send keys, assert on the screen. Load for any end-to-end or terminal-behaviour check."
---

# Running dev

`scripts/tmux-harness.sh` runs a TUI in a detached session on a private tmux server (`-L lazyagents-harness`); the user's own tmux is untouched.

1. Build first so startup is instant: `cargo build -p lazyagents`.
2. Make a temp state dir and project with `mktemp -d` (the daemon's socket path must stay under ~104 bytes, so avoid long scratch paths), then start:
   `scripts/tmux-harness.sh start <name> 120 32 -- sh -c "cd <project> && exec env LAZYAGENTS_STATE_DIR=<temp> LAZYAGENTS_CONFIG=<temp>/config.toml SHELL=/bin/sh target/debug/lazyagents"`
   `LAZYAGENTS_CONFIG` keeps the user's `~/.config/lazyagents/config.toml` out of the run; write a config to that path to test one.
   Use an absolute path to the binary, since the command runs from `<project>`.
3. Drive: `type <name> '<text>'` sends literal text; `keys <name> Enter C-q M-Enter Escape` sends named keys. To leave a focused pane send the prefix and a sub-key: `keys <name> C-q q` (`C-q j`/`C-q k` switch agents, `C-q s` scrolls); `C-]` also leaves directly.
4. Assert: `wait-for <name> '<text>' [secs]` exits 1 and prints the screen on timeout; `capture <name>` prints the screen at any time; `alive <name>` exits 0 while the session exists.
5. Always finish, also after a failure, with `LAZYAGENTS_STATE_DIR=<temp> target/debug/lazyagents shutdown` (stops the daemon and its agents) and `stop <name>`.

Use the `shell` preset (`SHELL=/bin/sh`) for agents in automated runs. Real `claude` needs the user's login and belongs to manual checks. `scripts/smoke-tui.sh` is a worked example, and `scripts/smoke-worktree.sh` shows a git repo project with a worktree agent.

Gotchas:
- `C-q` is the pane prefix; send `C-q` and its second key in one `keys` call. tmux delivers `C-]` as the legacy byte 0x1d, which crossterm reports as ctrl+5; the direct leave handles both.
- Inside tmux, shift+enter arrives as plain enter unless tmux `extended-keys` is on; check newline insertion with `M-Enter` (alt+enter).
- Never kill lazyagents processes by pattern: the user may be running their own daemon and TUI. Stop yours through its state dir or by exact pid.
