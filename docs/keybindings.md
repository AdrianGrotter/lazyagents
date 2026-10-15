# Keybindings

Status: designed, not implemented. The spike uses enter (focus pane), ctrl-] (back to sidebar) and q (quit).

Two input modes:

**Sidebar mode** (lazygit-like):

| Key | Action |
|---|---|
| `j`/`k`, arrows | move within panel |
| `h`/`l`, `tab`/`shift-tab` | switch panel |
| `enter` | focus main pane (pane mode) |
| `n` | new agent popup: name, preset, worktree toggle, base branch |
| `x` | kill agent (confirm) |
| `r` | restart agent (Claude: resume session) |
| `d` | remove agent (offers worktree removal) |
| `R` | rename agent |
| `a` | add project (path input, defaults to cwd) |
| `g` | jump to next agent with `NeedsInput` |
| `D` | toggle diff view in main pane |
| `?` | help popup |
| `q` | quit TUI (agents keep running) |
| `Q` | quit and stop daemon (confirm) |

**Pane mode**: all input goes to the agent except `ctrl-]` (back to sidebar) and
`alt-[`/`alt-]` (previous/next agent in the current project). Mouse wheel scrolls the
emulator's scrollback when the agent hasn't enabled mouse reporting; otherwise it is
forwarded. Mouse drag selects text inside the pane and copies it via OSC 52 (fallback
`pbcopy`/`wl-copy`). `ctrl-]` then `[` enters keyboard scroll mode.

Attention: `NeedsInput` agents show `!`, are highlighted, sort first within their project,
and their project row shows a count. All bindings are configurable.
