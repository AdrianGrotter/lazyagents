# 0033. ctrl-cmd-j/k move to the next and previous active agent

Date: 2026-10-19
Status: Accepted

## Context

`ctrl-j`/`ctrl-k` move to the next and previous agent from the sidebar and the pane. With
many agents, most of them idle, the user wants the same motion but stopping only at agents
that are starting, working or need input. `g` already jumps to agents that need input or
are `done`, so `done` stays out of the new motion. The key must work from the pane without
taking a key an agent uses, and the team uses kitty on macOS with Norwegian keyboard
layouts, where `[` and `]` need Option.

## Decision

`ctrl-cmd-j`/`ctrl-cmd-k` (actions `next_active` and `previous_active`, and
`direct_next_active` and `direct_previous_active` in the pane) move to the next and previous
listed agent whose status is starting, working or needs input, wrapping. After the pane
prefix, `J`/`K` do the same for terminals that cannot report `cmd`. Key strings gain a
`cmd-` modifier (alias `super-`), matched exactly, so `ctrl-j` no longer fires on
`ctrl-cmd-j`.

## Alternatives

- `alt-j`/`alt-k`: kitty on macOS sends Option as alt only with `macos_option_as_alt` set,
  which is off by default, so the keys would arrive as `∆` and `˚`.
- `ctrl-shift-j`/`ctrl-shift-k`: kitty binds them to scrolling by default, so everyone
  would have to unmap them in `kitty.conf`.
- `]`/`[` in the sidebar and after the prefix: they need Option on a Norwegian layout.
- `ctrl-n`/`ctrl-p`: shells and readline-style agents use them for history.
- Widening `g` to working agents: `g` would stop meaning "an agent wants me".

## Consequences

The motion is a modifier on the keys it filters, and no agent loses a key. `ctrl-cmd-j`
only works in terminals that speak the kitty keyboard protocol and pass the chord through;
elsewhere only `ctrl-q J`/`K` reach it. The pane's next and previous rows in the help popup
are paired so the popup still fits 100×40.
