# Configuration

Status: implemented in crates/tui (config.rs, keymap.rs).

The TUI reads one TOML file at startup. Changes apply the next time the TUI starts.

## Location

The first of these that is set and non-empty:

1. `$LAZYAGENTS_CONFIG`
2. `$XDG_CONFIG_HOME/lazyagents/config.toml`
3. `~/.config/lazyagents/config.toml`

A missing file gives the defaults. Every section and every setting is optional; what the
file does not mention keeps its default.

## Warnings

The loader never fails. Anything it cannot use stays at its default and becomes a
warning, and all warnings are shown together in the status bar when the TUI starts. The
full list:

- a file that cannot be read, as `<path>: <error>`, or a syntax error, as `<path>: line N:
  <message>`; the whole file is then ignored;
- an unknown table or setting (`unknown table [keybindings]`, `unknown setting theme.glow`,
  `unknown setting keys.extra`);
- `keys`, `theme`, `keys.sidebar` or `keys.pane` that is not a table (`keys.pane: expected
  a table`), or `presets` that is not a list of `[[presets]]` tables;
- an unknown action (`unknown action keys.sidebar.frobnicate`), including a pane action
  under `[keys.sidebar]` or a sidebar action under `[keys.pane]`;
- a key string that does not parse (`keys.sidebar.quit: <reason>`), or a value that is not
  a key or a list of keys (`keys.sidebar.quit: expected a key or a list of keys`);
- a `prefix` that is not exactly one key (`keys.pane.prefix: expected one key`);
- a key bound to two actions in one scope (`keys.sidebar: d is bound to both remove and
  stop; stop wins`); the later binding wins and the other action loses that key;
- `keys.pane.direct_leave` containing the prefix (`keys.pane.direct_leave: ctrl-q is the
  prefix`); the prefix is dropped from it;
- no way left to leave the pane (`keys.pane: no way to leave the pane; leave restored to
  q/esc`); `leave` gets its defaults back;
- a preset that is not a table (`preset #2: expected a table`), has an unknown setting
  (`unknown setting colour in preset #2`), lacks a name or a command (`preset #2: needs a
  name and a command`), or has a `kind` other than `claude` or `generic`;
- a colour that does not parse (`theme.accent: unknown colour "mauve"`).

## Keys

```toml
[keys.sidebar]
new_agent = "N"
quit = ["q", "ctrl-c"]

[keys.pane]
prefix = "ctrl-a"
leave = ["q", "esc"]
direct_leave = []
```

A value is one key string or a list of them. Naming an action replaces all of its keys;
the other actions keep theirs. An empty list unbinds an action.

Key strings: a single character (`q`, `?`, `]`), `space`, `enter`, `esc`, `tab`,
`backtab`, `backspace`, `up`, `down`, `left`, `right`, `home`, `end`, `pageup`,
`pagedown`, `f1` to `f12`, optionally with the modifiers `ctrl-`, `alt-` and `shift-` in
front (`ctrl-q`, `alt-enter`, `shift-tab`). Write a shifted letter as the letter itself
(`R`, not `shift-r`). A bare character matches whether or not the terminal reports
SHIFT; with `ctrl` or `alt`, SHIFT counts, so `ctrl-Q` is not `ctrl-q`.

### `[keys.sidebar]`

| Action | Default |
|---|---|
| `quit` | `q`, `ctrl-c` |
| `stop_daemon` | `Q` |
| `help` | `?` |
| `down` | `j`, `down` |
| `up` | `k`, `up` |
| `focus_projects` | `h`, `left`, `backtab` |
| `focus_agents` | `l`, `right` |
| `toggle_focus` | `tab` |
| `open` | `enter` |
| `new_agent` | `n` |
| `add_project` | `a` |
| `stop` | `x` |
| `restart` | `r` |
| `remove` | `d` |
| `rename` | `R` |
| `next_attention` | `g` |
| `toggle_all_agents` | `A` |
| `diff` | `D` |
| `scroll` | `s` |

### `[keys.pane]`

While the pane has focus every key goes to the agent except the prefix and the direct
leave keys. The prefix is followed by one more key that picks an action.

| Setting | Default | Meaning |
|---|---|---|
| `prefix` | `ctrl-q` | exactly one key; in the pane it is not sent to the agent but waits for one of the keys below |
| `next` | `j` | next agent, staying in the pane |
| `previous` | `k` | previous agent, staying in the pane |
| `leave` | `q`, `esc` | back to the sidebar |
| `scroll` | `s` | scroll mode |
| `send_prefix` | the prefix | send one prefix key to the agent |
| `direct_leave` | `ctrl-]`, `ctrl-5` | leave without the prefix |

Setting `prefix` also makes pressing it twice send it to the agent, unless `send_prefix`
is set too. `direct_leave` may not contain the prefix.

`alt-[` and `alt-]` are not bound by default, so they reach the agent
([ADR 0007](decisions/0007-ctrl-q-pane-prefix.md)).

If `leave` and `direct_leave` both end up empty, no key could return from the pane to the
sidebar, so `leave` gets its defaults back (`q`, `esc`, taking them from any other pane
action) with the warning `keys.pane: no way to leave the pane; leave restored to q/esc`.
A click on the sidebar also leaves the pane. `quit = []` leaves `Q` (stop the daemon and
quit) and a signal as the only ways to end the TUI.

## Presets

Each `[[presets]]` table adds an entry to the new agent form, after the built-in
`claude`, `shell` and `custom`:

```toml
[[presets]]
name = "codex"
command = ["codex", "--full-auto"]

[[presets]]
name = "opus"
kind = "claude"
command = ["claude", "--model", "opus"]
```

| Setting | Meaning |
|---|---|
| `name` | label in the form; required |
| `command` | program and arguments; required, non-empty |
| `kind` | `generic` (default) or `claude`; `claude` agents report status through hooks |

## Theme

```toml
[theme]
accent = "blue"
attention = "#ff8800"
working = 208
```

| Slot | Default | Used for |
|---|---|---|
| `accent` | green | the border of the focused box, the `copied N characters` message |
| `attention` | red | agents that need input, exited agents, error messages |
| `working` | yellow | the working spinner, the pending prefix |
| `idle` | green | the idle check mark |
| `dim` | dark gray | hints, secondary text, stopped agents |
| `border` | dark gray | unfocused borders |
| `scroll` | yellow | the pane border in scroll mode |
| `diff_add` | green | added lines in the diff view |
| `diff_remove` | red | removed lines |
| `diff_hunk` | cyan | hunk headers |

A colour is a name (`red`, `light-green`, `dark-gray`, ...; case-insensitive), `#rrggbb`,
or a 0-255 palette index, as a number (`208`) or a string (`"12"`).

## Clipboard

Dragging in the pane copies the selection with OSC 52 and, where `pbcopy` or `wl-copy` is
on `PATH`, through that too. OSC 52 has caveats:

- Inside tmux it reaches the outer terminal only with `set -g set-clipboard on` in
  `tmux.conf`.
- Some terminals cap the size of an OSC 52 payload and drop longer copies.
- A selection covers screen cells, not text. If the agent's output scrolls while the
  selection stays, the cells hold different text and a copy gets what is there on release.

To use the terminal's own selection instead, hold the modifier that bypasses mouse
capture while dragging: shift in Ghostty, kitty and WezTerm, Option in iTerm2, Fn in
Terminal.app.
