# 0007. Leave the pane with a ctrl-q prefix

Date: 2026-10-16
Status: Accepted

## Context

While the main pane has focus, every key belongs to the agent, so lazyagents can claim
only a few chords and each one is a key the agent can no longer receive. The first
design used `alt-[` and `alt-]` to switch agents and `ctrl-]` to leave. On Norwegian Mac
layouts `[` and `]` need Option, so those chords are awkward to type, and legacy
terminals send `alt-[` as ESC followed by `[`, the start of every escape sequence, so the
key after it is swallowed or misread.

## Decision

`ctrl-q` is a prefix. The next key picks the action: `j` and `k` switch to the next and
previous agent, `q` and `esc` go back to the sidebar, `s` enters scroll mode, and `ctrl-q`
again sends one `ctrl-q` to the agent. A pending prefix is dropped when focus leaves the
pane. `ctrl-]` (and `ctrl-5`, how legacy terminals report it) stays as a direct leave, and
`alt-[`/`alt-]` are unbound. The prefix, the sub-keys and the direct leave keys are
configurable in `[keys.pane]`.

## Alternatives

- `alt-j`/`alt-k`: one chord, but on macOS Option produces characters unless the terminal
  is set to send Meta, and agents and shells bind alt-letter keys themselves.
- `ctrl-,` and `ctrl-.`: terminals do not distinguish ctrl with punctuation from the
  plain key without the kitty keyboard protocol.
- Function keys: far from the home row, and often taken by the OS or the terminal.
- Keeping `alt-[` and `alt-]`: the layout and escape-sequence problems above.

## Consequences

- One reserved chord covers every pane action, and new pane actions need no new chord.
- Agents never see `ctrl-q` unless it is pressed twice. Programs that use it (XON, some
  editors) need that second press, or another prefix in the config.
- Switching agents from the pane takes two keystrokes instead of one.
- Binding no way out of the pane traps the user; the config documentation warns about it.
