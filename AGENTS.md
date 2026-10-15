# lazyagents

A lazygit-style terminal UI for running many coding agents (mostly Claude Code) side by side: a sidebar of projects and agents with live status, and a main pane holding the selected agent's interactive terminal. A background daemon owns the agent processes; the TUI is a client that can quit and reattach.

`docs/` describes the system as it is now; `docs/superpowers/specs/` and `docs/superpowers/plans/` are dated history. Start with `docs/architecture.md`.

## Environment

All tooling comes from the nix dev shell: run `direnv allow` once, or prefix commands with `nix develop -c`. The shell also points git at `.githooks`.

## Commands

- `just check`: fmt check, clippy with `-D warnings`, tests, hook tests, tmux harness self-test, spike smoke run. The done gate.
- `just fmt`, `just lint`, `just test`, `just test-hooks`
- `just spike [cmd...]`: the milestone-1 embedded-terminal spike (default command `claude`); runs from any directory.

## Layout

| Path | Purpose |
|---|---|
| `crates/protocol` | client↔daemon messages and framing |
| `crates/daemon` | agents, PTYs, terminal emulation, persistence, git worktrees |
| `crates/tui` | ratatui client |
| `crates/lazyagents` | the binary and CLI |
| `spike/` | throwaway embedded-terminal spike; removed once the TUI replaces it |
| `vendor/crossterm` | crossterm 0.29.0 with a dead-terminal fix (ADR 0005); drop when upstream releases it |
| `scripts/` | tmux harness, hook tests, spike smoke and hangup scripts |
| `docs/` | architecture, protocol, keybindings, status detection, ADRs (`decisions/`) |

## Working rules

- Test first: write the failing test, watch it fail for the expected reason, then implement.
- Done means `just check` is green and you read its output (skill: verifying). Anything visible in a terminal also gets a harness run (skill: running-dev).
- Commit each logical change with a Conventional Commit (skill: committing). Git hooks gate message format, fmt and clippy; Claude Code hooks format edited Rust files and run clippy before you stop.
- A choice between real alternatives gets an ADR in `docs/decisions/` (skill: recording-decisions).
- A change to the protocol, keybindings or status detection updates its doc in `docs/` in the same commit.
- Comments: default to none. Write one only for a non-obvious invariant or constraint that stays true as the code evolves; change history belongs in commit messages.
- Tests and dev runs keep state in temp dirs (`LAZYAGENTS_STATE_DIR` once the daemon exists); `~/.local/state/lazyagents` belongs to the user's real agents.
- The tmux harness runs on its own server (`tmux -L lazyagents-harness`), leaving the user's tmux sessions alone.
