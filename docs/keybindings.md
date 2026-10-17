# Keybindings

Status: implemented in crates/tui (app.rs, keymap.rs, keys.rs, config.rs).

The keys below are the defaults. Every sidebar key, the pane prefix, the keys after it and
the direct pane keys can be changed in `config.toml` (`[keys.sidebar]` and `[keys.pane]`);
the help popup, the status bar hints and the messages name the configured keys. Popup keys
and the keys inside the diff view and scroll mode are fixed; there only the configured quit
keys and, in the diff view, the configured diff key follow the config. The format is in
[configuration.md](configuration.md).

Two input modes:

**Sidebar mode** (lazygit-like):

| Key | Action |
|---|---|
| `j`/`k`, `↑`/`↓` | move within the focused box |
| `h`/`l`, `←`/`→`, `tab`/`shift-tab` | switch between Projects and Agents |
| `1`/`2` | focus Projects / Agents |
| `enter` | focus main pane (pane mode) |
| `n` | new agent popup: name (optional), preset (`claude`, `shell`, custom command), worktree toggle, base branch and first prompt |
| `x` | stop agent (confirm) |
| `r` | restart agent (confirm while it is running; Claude: resume session) |
| `d` | remove agent (confirm; for a worktree agent, choose whether the worktree goes too); with Projects focused, remove the project (confirm; refused while it has agents) |
| `R` | rename agent |
| `a` | add project: a picker of projects added before, or the new project form when there are none |
| `A` | toggle between the selected project's agents and all agents |
| `g` | jump to the next agent that needs input; when no other agent does, to the next one listed as `done` |
| `D` | diff view of the agent's changes in the main pane |
| `s` | scroll mode: browse the agent's terminal history (running or exited agents) |
| `?` | help popup |
| `q`, `ctrl-c` | quit TUI (agents keep running) |
| `Q` | quit and stop daemon (confirm) |

An agent whose turn ends (working or needs input → idle) while it is out of view is
listed as `done`, with a blue dot, until you view it: select it, or, for the agent already
selected, bring the terminal back into focus. Out of view means not selected, or selected
while the terminal lacks focus; terminals that report no focus changes (tmux without
`focus-events on`) always count as focused. An agent that works again, or stops, is no
longer `done`. The marks live in the TUI and start empty when it starts.

The add project picker lists `+ new project…` and then every known project that is not in
the sidebar, most recently added first. `j`/`k` (or `↓`/`↑`) move, `enter` adds the
highlighted project with its old name or opens the new project form, `x` forgets the
highlighted project, and `esc`/`q` closes. A project removed with `d` stays known. The new
project form has Path (starts at the cwd; relative paths resolve against the cwd, a
leading `~` is the home directory) and Name (empty means the directory name); `tab` moves
between them, `enter` adds and `esc` cancels.

Popups: `y`/`enter` confirms and `n`/`esc` cancels a confirmation. In the new agent form,
`tab`/`shift-tab` (or `↓`/`↑`) move between the visible fields, `enter` starts the agent and
`esc` cancels. The fields are Name, Preset (`←`/`→`/`space` change it), Command (only for the
custom preset), Worktree (`space`/`←`/`→` toggle it), Base (only while Worktree is on) and
Prompt, the agent's first message (empty means none; a pasted newline becomes a space).
Spaces in an agent's name (new agent form and rename) become dashes: `my new agent` is
stored as `my-new-agent`. An empty name becomes `<preset>-<n>` with the lowest number no
agent in the project uses (`claude-1`, `shell-2`). The Agents box and the pane's header
show a Claude agent by its session title once Claude has set one, and by its name until
then; the name stays the agent's identity for the CLI and its worktree branch.
With Worktree on, the agent runs in a new git worktree on branch `lazyagents/<name>`, made
from Base; an empty Base means the project's current branch. Removing an agent never
deletes its branch, so a new worktree agent with a removed agent's name fails with git's
`a branch named 'lazyagents/<name>' already exists` until that branch is deleted or
renamed.

Removing a worktree agent asks `Remove <name>? y: agent + worktree · k: keep worktree ·
n: cancel`:

| Key | Action |
|---|---|
| `y` | remove the agent and its worktree (the branch stays) |
| `k` | remove the agent and keep the worktree |
| `n`, `esc` | cancel |

If the worktree has uncommitted changes the daemon refuses, and a second prompt lists the
changed files (up to 8, then `… and N more`) and asks `Remove anyway? y/n`: `y` removes the
worktree with its changes, `n`/`esc` keeps everything. `enter` does nothing in these two
prompts. Ignored files (`.env.local`, build output) do not count as changes, so removing
the worktree without force still deletes them. The second prompt opens only while focus is
still in the sidebar and no key or click came after the `y`; otherwise the status bar shows
`<name>: worktree has uncommitted changes — d to try again` (with the configured remove
key), and focus stays where it is.

**Protocol mismatch**: when the running daemon speaks another protocol version, the TUI
opens on a full-screen prompt instead of the sidebar. `y` stops that daemon and its agents
(SIGTERM, up to 5 s; the prompt shows `stopping daemon <pid>…` meanwhile, and `ctrl-c`
abandons the wait and quits) and continues with a new daemon; `n`, `q`, `esc` and `ctrl-c`
quit with the mismatch error.

**Diff view** (`D`): the main pane shows `git diff` of the selected agent's directory:
against the merge base with its base branch for a worktree agent (committed and uncommitted
changes), against `HEAD` otherwise. Added lines are green, removed lines red, hunk headers
cyan and file headers bold (`[theme]` `diff_add`, `diff_remove`, `diff_hunk`). The view
shows at most 20 000 lines. A git error closes it and shows the error in the status bar.

| Key | Action |
|---|---|
| `j`/`k`, `↓`/`↑` | scroll one line |
| `ctrl-d`/`ctrl-u` | scroll half a page |
| `pagedown`/`pageup` | scroll a page |
| `g`/`G` | top / bottom |
| `D`, `q`, `esc` | close, back to the Agents box |
| `ctrl-c` | quit the TUI (agents keep running) |

The mouse wheel scrolls the diff too. The view closes by itself if its agent is removed.

**Pane mode**: all input goes to the agent except the `ctrl-q` prefix and `ctrl-]`.
After `ctrl-q`, one more key picks a pane action:

| Keys | Action |
|---|---|
| `ctrl-q j` | next agent in the Agents panel, staying in the pane |
| `ctrl-q k` | previous agent in the Agents panel, staying in the pane |
| `ctrl-q q`, `ctrl-q esc` | back to the sidebar |
| `ctrl-q s` | scroll mode |
| `ctrl-q ctrl-q` | send one `ctrl-q` to the agent |

While the prefix waits for its second key the status bar shows `ctrl-q …`. Any other key
after `ctrl-q` cancels the prefix, reaches nobody, and the status bar lists the prefix
keys. Switching onto an agent that is not running returns focus to the sidebar. A
pending prefix is dropped whenever focus leaves the pane, and when a paste arrives; the
paste still reaches the agent.

`ctrl-]` is an alias that goes straight back to the sidebar; legacy terminals report it as
`ctrl-5`, which works too. `alt-[` and `alt-]` are not bound: they reach the agent like
any other key.

Switching agents without the prefix is opt-in: `direct_next` and `direct_previous` in
`[keys.pane]` bind keys that switch in one chord, staying in the pane (none by default;
see [configuration.md](configuration.md#keyspane)). Those keys no longer reach the agent.

The mouse works in the pane too; see **Mouse** below.

**Scroll mode** (`s` in the sidebar, `ctrl-q s` in the pane): keys move the selected
agent's terminal through its history instead of reaching the agent. It needs an agent with
a terminal, running or exited; for a stopped agent the status bar says `nothing to scroll`.
The pane border takes the `[theme]` `scroll` colour (default yellow) and the status bar
shows `SCROLL · j/k ctrl-u/d g/G · q exit`.

| Key | Action |
|---|---|
| `j`/`k` | one line towards the bottom / back in history |
| `ctrl-d`/`ctrl-u` | half a page towards the bottom / back |
| `pagedown`/`pageup` | a page towards the bottom / back |
| `g`/`G` | top of the history / bottom |
| `q`, `esc` | scroll back to the bottom and exit |
| `ctrl-c` | quit the TUI (agents keep running) |

Exiting returns to the pane if scroll mode was entered from the pane and the agent still
runs, otherwise to the Agents box. The mode closes by itself, back to the Agents box, if its
agent is removed or stopped, or if the selection moves to another agent; in that last case
the agent it leaves is scrolled back to the bottom. The mouse wheel scrolls the history
three lines at a time instead of reaching the agent.

**Mouse**: clicks and drags never reach the agent; the wheel may.

| Action | Effect |
|---|---|
| click a Projects row | select that project and focus Projects |
| click an Agents row | select that agent and focus Agents (never the pane) |
| click elsewhere in the Projects or Agents box | focus that box |
| click in the pane | focus the pane if the agent is running; otherwise the status bar says `<name> is not running — r restarts` |
| drag in the pane | select text, shown reversed; the selection stays inside the pane |
| release after a drag | copy the selected text and show `copied N characters` (in the `accent` colour) |
| click outside an open popup | close it, like `esc` |
| wheel over the pane | see below |

The selection runs in reading order from where the drag started to where it ends, whole
lines in between; trailing spaces are trimmed from each line. A click without a drag
copies nothing. Clicking a sidebar row also closes the diff view, or scroll mode (scrolling
its agent back to the bottom). In scroll mode a click or drag in the pane selects from the
history on screen and keeps scroll mode; in the diff view it does nothing.

Copying writes the text to the terminal as OSC 52, which Ghostty, kitty and WezTerm put on
the system clipboard, and also pipes it to `pbcopy` (macOS) or `wl-copy` (Wayland) when one
is on `PATH`. Inside tmux, OSC 52 reaches the outer terminal only with
`set -g set-clipboard on` in `tmux.conf`, and some terminals cap the size of an OSC 52
payload and drop longer copies. A selection covers screen cells, not text: if the agent's
output scrolls while the selection stays, a copy gets whatever those cells hold on release.
Holding the modifier that bypasses mouse capture while dragging uses the terminal's own
selection instead: shift in Ghostty, kitty and WezTerm, Option in iTerm2, Fn in
Terminal.app. See [configuration.md](configuration.md#clipboard).

The wheel over the pane scrolls the emulator's scrollback when the agent hasn't enabled
mouse reporting; otherwise it is forwarded to the agent. An exited agent's pane scrolls with
the wheel too; over a stopped agent, which has no terminal, the wheel does nothing. In the
diff view it scrolls the diff, and in scroll mode the history.

**Terminal too small**: while the pane would be smaller than 20×5 cells the TUI shows only
"terminal too small", and keys and pastes do nothing except the quit keys (`q`/`ctrl-c`,
from the sidebar) and the prefix then a leave key (`ctrl-q` then `q` or `esc`, leaving the
pane). Nothing reaches an agent, and releasing the mouse drops a selection without copying.

Bindings match exactly. A bare character matches whether or not the terminal adds SHIFT.
With `ctrl` or `alt`, SHIFT counts: `ctrl-Q` (or `ctrl-shift-q`) is a different key from
`ctrl-q`, so in kitty-protocol terminals ctrl-shift-q reaches the agent instead of acting as
the prefix. Legacy terminals cannot send ctrl with SHIFT, so there `ctrl-Q` never fires.

Status icons in the Agents panel, in the default colours (`[theme]` in `config.toml`
changes them):

| Icon | Status |
|---|---|
| `○` (grey, blinking) | starting |
| `●` (pulsing yellow to orange) | working |
| `●` (purple; whole row purple) | needs input |
| `●` (green) | idle |
| `●` (grey for exit 0, red otherwise) | exited |
| `○` (grey) | stopped |

The colours are GitHub's pull request colours, as lazygit shows them. Attention:
`NeedsInput` agents are highlighted, sort first within their project, and their project
row shows a count (`● 2`).
