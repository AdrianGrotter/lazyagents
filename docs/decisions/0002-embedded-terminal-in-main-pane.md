# 0002. Agents are interactive inside the main pane

Date: 2026-10-15
Status: Accepted (pending spike verdict, milestone 1)

## Context

The user wants the sidebar visible while working with an agent, like a tmux split, rather than switching to a full-screen agent.

## Decision

The daemon runs an `alacritty_terminal` emulator per agent and encodes input according to the modes the agent enabled. The TUI renders the emulator's grid in the main pane and forwards abstract key, paste, mouse and resize events. `ctrl-]` returns focus to the sidebar.

## Alternatives

- Full-terminal passthrough (detach the UI, pipe raw bytes, like `tmux attach`): perfect fidelity, but the sidebar disappears while working.
- `vt100` crate as emulator: smaller, but less complete than alacritty's, which also powers Zed's terminal.

## Consequences

lazyagents is a small terminal multiplexer: key encoding, paste, mouse, scrollback and selection are ours to get right. A spike gates the decision before the daemon is built.
