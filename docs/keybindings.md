# Keybindings

Status: implemented in crates/tui (app.rs, keys.rs), except `D` (plan 4), keyboard scroll
mode (plan 4) and configurable bindings (plan 4).

Two input modes:

**Sidebar mode** (lazygit-like):

| Key | Action |
|---|---|
| `j`/`k`, arrows | move within panel |
| `h`/`l`, `tab`/`shift-tab` | switch panel |
| `enter` | focus main pane (pane mode) |
| `n` | new agent popup: name, preset (`claude`, `shell`, custom command); worktree toggle and base branch come with plan 4 |
| `x` | stop agent (confirm) |
| `r` | restart agent (Claude: resume session) |
| `d` | remove agent (confirm; worktree removal comes with plan 4) |
| `R` | rename agent |
| `a` | add project (path input, defaults to cwd) |
| `A` | toggle between the selected project's agents and all agents |
| `g` | jump to next agent with `NeedsInput` |
| `D` | toggle diff view in main pane (plan 4) |
| `?` | help popup |
| `q`, `ctrl-c` | quit TUI (agents keep running) |
| `Q` | quit and stop daemon (confirm) |

Popups: `y`/`enter` confirms and `n`/`esc` cancels a confirmation. In the new agent form,
`tab`/`shift-tab` move between fields, `←`/`→` change the preset, `enter` starts the agent
and `esc` cancels.

**Pane mode**: all input goes to the agent except `ctrl-]` (back to sidebar) and
`alt-[`/`alt-]` (previous/next agent in the Agents panel). `alt-[`/`alt-]` need a
terminal that reports alt combinations as such (the TUI requests the kitty keyboard
protocol where available); in a legacy terminal, press `ctrl-]`, then `j`/`k`, then
`enter`. Mouse wheel scrolls the emulator's scrollback when the agent hasn't enabled mouse
reporting; otherwise it is forwarded. An exited or stopped agent's pane scrolls with the
wheel too. Planned (plan 4): mouse drag selects text inside the pane and copies it via
OSC 52 (fallback `pbcopy`/`wl-copy`), and `ctrl-]` then `[` enters keyboard scroll mode.

Status icons in the Agents panel:

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
