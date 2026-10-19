# 0029. Navigate with ctrl-h/j/k/l by default, even from inside the pane

Date: 2026-10-19
Status: Accepted

## Context

ADR 0007 kept the pane almost free of chords: switching agents from the pane took the
`ctrl-q` prefix and a second key, and one-chord switching (`direct_next`,
`direct_previous`) was opt-in with no default keys. In practice switching and leaving
are the most frequent things done from the pane, and two keystrokes for each is the
main friction. Users of vim, tmux and vim-tmux-navigator already move between panes
with `ctrl-h`/`j`/`k`/`l`.

## Decision

The defaults bind `ctrl-j` and `ctrl-k` to the next and previous agent and `ctrl-h` to
back to the sidebar, directly in the pane (`direct_next`, `direct_previous`,
`direct_leave`). In the sidebar `ctrl-j`/`ctrl-k` (`next_agent`, `previous_agent`) move
through the Agents box from either box, and `ctrl-l` (`focus_pane`) opens the selected
agent's pane from either box. `ctrl-l` stays unbound in the pane and `ctrl-h` in the
sidebar, where `h` already focuses Projects. The prefix and its keys stay. Each binding
can be rebound or emptied in `config.toml`.

## Alternatives

- Keep one-chord switching opt-in (ADR 0007): no keys taken from the agent, but the
  common moves stay two keystrokes, and few users find the setting.
- `alt-h`/`j`/`k`/`l`: on macOS Option produces characters unless the terminal sends
  Meta, the problem that ruled out `alt-[`/`alt-]` in ADR 0007.
- Bind `ctrl-l` in the pane as well: nothing for it to do there, and the agent would
  lose redraw.

## Consequences

- One chord switches agents or leaves the pane, in both directions.
- The agent never sees `ctrl-h`, `ctrl-j` or `ctrl-k`: in Claude Code `ctrl-j` no longer
  inserts a newline, `ctrl-k` no longer deletes to the end of the line, and shells lose
  readline's `ctrl-h`/`ctrl-k`. Users who need them empty the direct bindings.
- A terminal that sends `ctrl-h` (BS) for `backspace` leaves the pane on every
  `backspace`. macOS terminals send DEL, and terminals with the kitty keyboard protocol
  report the two keys apart.
- The help popup grows by four rows.
