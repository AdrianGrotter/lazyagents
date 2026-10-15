# Keybindings

Status: implemented in crates/tui (app.rs, keymap.rs, keys.rs, config.rs), except `D`
(plan 4) and keyboard scroll mode (plan 4).

The keys below are the defaults. Every sidebar key, the pane prefix, the keys after it and
the direct-leave keys can be changed in `config.toml` (`[keys.sidebar]` and `[keys.pane]`);
the help popup, the status bar hints and the messages name the configured keys. Popup keys
are fixed.

Two input modes:

**Sidebar mode** (lazygit-like):

| Key | Action |
|---|---|
| `j`/`k`, `↑`/`↓` | move within the focused box |
| `h`/`l`, `←`/`→`, `tab`/`shift-tab` | switch between Projects and Agents |
| `enter` | focus main pane (pane mode) |
| `n` | new agent popup: name, preset (`claude`, `shell`, custom command); worktree toggle and base branch come with plan 4 |
| `x` | stop agent (confirm) |
| `r` | restart agent (confirm while it is running; Claude: resume session) |
| `d` | remove agent (confirm; worktree removal comes with plan 4) |
| `R` | rename agent |
| `a` | add project (path input, defaults to cwd; relative paths resolve against the cwd, a leading `~` is the home directory) |
| `A` | toggle between the selected project's agents and all agents |
| `g` | jump to next agent with `NeedsInput` |
| `D` | toggle diff view in main pane (plan 4) |
| `?` | help popup |
| `q`, `ctrl-c` | quit TUI (agents keep running) |
| `Q` | quit and stop daemon (confirm) |

Popups: `y`/`enter` confirms and `n`/`esc` cancels a confirmation. In the new agent form,
`tab`/`shift-tab` move between fields, `←`/`→` change the preset, `enter` starts the agent
and `esc` cancels.

**Pane mode**: all input goes to the agent except the `ctrl-q` prefix and `ctrl-]`.
After `ctrl-q`, one more key picks a pane action:

| Keys | Action |
|---|---|
| `ctrl-q j` | next agent in the Agents panel, staying in the pane |
| `ctrl-q k` | previous agent in the Agents panel, staying in the pane |
| `ctrl-q q`, `ctrl-q esc` | back to the sidebar |
| `ctrl-q ctrl-q` | send one `ctrl-q` to the agent |

While the prefix waits for its second key the status bar shows `ctrl-q …`. Any other key
after `ctrl-q` cancels the prefix, reaches nobody, and the status bar lists the prefix
keys. Switching onto an agent that is not running returns focus to the sidebar. A
pending prefix is dropped whenever focus leaves the pane, and when a paste arrives; the
paste still reaches the agent.

`ctrl-]` is an alias that goes straight back to the sidebar; legacy terminals report it as
`ctrl-5`, which works too. `alt-[` and `alt-]` are not bound: they reach the agent like
any other key.

Mouse wheel scrolls the emulator's scrollback when the agent hasn't enabled mouse
reporting; otherwise it is forwarded. An exited or stopped agent's pane scrolls with the
wheel too. Planned (plan 4): mouse drag selects text inside the pane and copies it via
OSC 52 (fallback `pbcopy`/`wl-copy`), and a keyboard scroll mode.

**Terminal too small**: while the pane would be smaller than 20×5 cells the TUI shows only
"terminal too small", and keys and pastes do nothing except the quit keys (`q`/`ctrl-c`,
from the sidebar) and the prefix then a leave key (`ctrl-q` then `q` or `esc`, leaving the
pane). Nothing reaches an agent.

Bindings match exactly. A bare character matches whether or not the terminal adds SHIFT.
With `ctrl` or `alt`, SHIFT counts: `ctrl-Q` (or `ctrl-shift-q`) is a different key from
`ctrl-q`, so in kitty-protocol terminals ctrl-shift-q reaches the agent instead of acting as
the prefix. Legacy terminals cannot send ctrl with SHIFT, so there `ctrl-Q` never fires.

Status icons in the Agents panel, in the default colours (`[theme]` in `config.toml`
changes them):

| Icon | Status |
|---|---|
| `◌`/`○` (blinking, dim) | starting |
| `⠋⠙⠹…` (animated spinner, yellow) | working |
| `!` (red, bold; whole row red) | needs input |
| `✓` (green) | idle |
| `✗` (dim for exit 0, red otherwise) | exited |
| `■` (grey) | stopped |

Attention: `NeedsInput` agents show `!`, are highlighted, sort first within their project,
and their project row shows a count.
