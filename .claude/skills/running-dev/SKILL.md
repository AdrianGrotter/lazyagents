---
name: running-dev
description: "Drive the spike or the lazyagents TUI in an isolated tmux session: start it, send keys, assert on the screen. Load for any end-to-end or terminal-behaviour check."
---

# Running dev

`scripts/tmux-harness.sh` runs a TUI in a detached session on a private tmux server (`-L lazyagents-harness`); the user's own tmux is untouched.

1. Build first so startup is instant: `cargo build -p spike`.
2. Start: `scripts/tmux-harness.sh start <name> 120 40 -- target/debug/spike sh`
3. Drive: `type <name> '<text>'` sends literal text; `keys <name> Enter C-] M-Enter Escape` sends named keys.
4. Assert: `wait-for <name> '<text>' [secs]` exits 1 and prints the screen on timeout; `capture <name>` prints the screen at any time.
5. Finish with `stop <name>`, also after a failure.

Use `sh` as the agent in automated runs. Real `claude` needs the user's login and belongs to manual checks.

Gotchas:
- tmux delivers `C-]` as the legacy byte 0x1d, which crossterm reports as ctrl+5. Detach handles both.
- Inside tmux, shift+enter arrives as plain enter unless tmux `extended-keys` is on; check newline insertion with `M-Enter` (alt+enter).
