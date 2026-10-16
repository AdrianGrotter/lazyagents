# lazyagents

A lazygit-style terminal UI for running many coding agents (mostly Claude Code) side by side: a sidebar of projects and agents with live status, and a main pane holding the selected agent's interactive terminal. A background daemon owns the agent processes; the TUI is a client that can quit and reattach.

`docs/` describes the system as it is now; `docs/superpowers/specs/` and `docs/superpowers/plans/` are dated history. Start with `docs/architecture.md`.

## Environment

All tooling comes from the nix dev shell: run `direnv allow` once, or prefix commands with `nix develop -c`. The shell also points git at `.githooks`.

## Commands

- `just check`: fmt check, clippy with `-D warnings`, tests, hook tests, tmux harness self-test, TUI smoke run (`scripts/smoke-tui.sh`), TUI hangup test (`scripts/test-tui-hangup.sh`), worktree smoke run (`scripts/smoke-worktree.sh`). The done gate.
- `just fmt`, `just lint`, `just test`, `just test-hooks`, `just smoke-tui`, `just test-tui-hangup`, `just smoke-worktree`
- `just dev [args]`: run the TUI (no args) or a CLI subcommand (`just dev ls`) from any directory. For experiments, point `LAZYAGENTS_STATE_DIR` at a temp dir so your real agents stay untouched.
- `just install`: release build to `~/.local/bin/lazyagents`, the user's real binary. Its path is written into running Claude agents' hook settings, so it stays fixed; install only when the user asks.
- `lazyagents ls|new|kill|restart|rm|shutdown`: manage agents through the daemon without the TUI. Tests set `LAZYAGENTS_STATE_DIR` to a temp dir and shut the daemon down.

## Layout

| Path | Purpose |
|---|---|
| `crates/protocol` | client↔daemon messages and framing |
| `crates/daemon` | agents, PTYs, terminal emulation, persistence, git worktrees |
| `crates/client` | connects to the daemon, starting it on demand; used by the CLI and the TUI |
| `crates/tui` | ratatui client: model (app.rs), rendering (ui.rs), keymap and config, connection, runtime |
| `crates/lazyagents` | the binary and CLI |
| `vendor/crossterm` | crossterm 0.29.0 with a dead-terminal fix (ADR 0005); drop when upstream releases it |
| `scripts/` | tmux harness, hook tests, TUI smoke, hangup and worktree scripts |
| `docs/` | architecture, protocol, keybindings, configuration, status detection, ADRs (`decisions/`) |

## Working rules

- Test first: write the failing test, watch it fail for the expected reason, then implement.
- Done means `just check` is green and you read its output (skill: verifying). Anything visible in a terminal also gets a harness run (skill: running-dev).
- Commit each logical change with a Conventional Commit (skill: committing). Git hooks gate message format, fmt and clippy; Claude Code hooks format edited Rust files and run clippy before you stop.
- A choice between real alternatives gets an ADR in `docs/decisions/` (skill: recording-decisions).
- A change to the protocol, keybindings or status detection updates its doc in `docs/` in the same commit.
- Comments: default to none. Write one only for a non-obvious invariant or constraint that stays true as the code evolves; change history belongs in commit messages.
- Tests and dev runs keep state in temp dirs (they set `LAZYAGENTS_STATE_DIR` to a temp dir); `~/.local/state/lazyagents` belongs to the user's real agents.
- The tmux harness runs on its own server (`tmux -L lazyagents-harness`), leaving the user's tmux sessions alone.
