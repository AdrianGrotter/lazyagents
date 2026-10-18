# Keybindings

Status: implemented in crates/tui (app.rs, keymap.rs, keys.rs, config.rs).

The keys below are the defaults. Every sidebar key, the pane prefix, the keys after it and
the direct pane keys can be changed in `config.toml` (`[keys.sidebar]` and `[keys.pane]`);
the help popup, the status bar hints and the messages name the configured keys. A status bar
message shows for 5 s; an error stays after that until the next key or click. A message wider
than the status bar is cut between its ` · `-separated parts, or with `…`. Popup keys
and the keys inside the diff view and scroll mode are fixed; there only the configured quit
keys and, in the diff view, the configured diff key follow the config. The format is in
[configuration.md](configuration.md).

Two input modes:

**Sidebar mode** (lazygit-like):

| Key | Action |
|---|---|
| `j`/`k`, `↑`/`↓` | move within the focused box |
| `h`/`l`, `←`/`→`, `tab`/`shift-tab` | switch between Projects and Agents |
| `1`/`2` | focus Projects / Agents; each box's title starts with the first plain digit key bound to focus it (`[1] Projects`, `[2] Agents`), or no number when none is |
| `enter` | focus main pane (pane mode); on a stopped or exited agent, start it (Claude: resume session) and focus the pane once it runs |
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
| `/` | filter the Agents box by name or branch (`s:<status>` by status): `enter` keeps the filter, `esc` clears it |
| `c` | menu of the custom commands (`[[commands]]` in [configuration.md](configuration.md#commands)); `j`/`k` move, `enter` runs, `esc` closes. A command's own key runs it directly |
| `y` / `N` | approve / deny what the selected agent waits for; only while the agent needs input. `N` sends `esc`. `y` sends `enter`, which takes the highlighted choice (yes) of Claude's permission dialog; at Claude's trust dialog it first moves to "Yes, I trust this folder", whichever option is highlighted |
| `i` | reply: type a line and `enter` sends it to the selected agent with `enter` after it, as if typed in its pane; `esc` cancels. A pasted newline becomes a space |
| `J`/`K`, `pagedown`/`pageup` | scroll the selected agent's pane half a page down / up without leaving the sidebar |
| `+` | screen mode: the pane beside a narrow sidebar, then the pane alone, then back. The sidebar keys keep working while it is hidden |
| `F` | finish a worktree agent: a menu to merge its branch into its base, push it and open a pull request against its base branch when that is a local branch (with `gh` on `PATH`), remove the agent and worktree but keep the branch, or remove them with the branch (only once it is merged). Picking merge or pull request does it; the two removals ask `Remove <name> and its worktree, keep <branch>?` or `Remove <name>, its worktree and <branch> (merged only)?` first (`y`/`enter` confirm), and once done the status bar says `removed <name> and its worktree, kept branch <branch>` or `removed <name>, its worktree and branch`. `j`/`k` move, `esc` closes |
| `?` | help popup, the complete list of keys. `j`/`k`, `↓`/`↑`, `ctrl-d`/`ctrl-u`, `pagedown`/`pageup` and `g`/`G` scroll it when it does not fit, with a row saying how many rows are above and below; long descriptions wrap. `/` starts a filter by key or description, typed in the popup's bottom row (`backspace` deletes, `ctrl-u` empties, `enter` keeps it, `esc` clears it). `esc` also clears a kept filter, as in the Agents box; otherwise `esc`, `q`, `enter`, `?` or `ctrl-c` closes the popup, and `ctrl-c` closes it while typing the filter too |
| `q`, `ctrl-c` | quit TUI (agents keep running) |
| `Q` | quit and stop daemon (confirm) |

Actions that cannot act on the selected agent are dimmed in the help popup, and pressing one
says why in the status bar instead of doing nothing: `no agent selected`, `<agent> is not
running` (stop, reply), `<agent> has no terminal yet — enter starts it` (scroll mode and
`J`/`K` on a stopped agent), `<agent> is not waiting for an answer` (`y`, `N`).

The status bar hints offer only what applies to the focus and the selection, most
important first, and a hint that does not fit is dropped whole, with every hint after it;
the help popup stays the complete list. In the sidebar they start with what the selected
agent's state calls for: the answers to a waiting agent (`y approve · N deny · i reply`),
`enter start` for a stopped one, `enter restart` for an exited one, and `D <n> comments to
send` while it has review comments. Then come `? help`, `g next input` while another agent
needs input or is `done`, the agent's own actions (`enter open`, `x stop` and `r restart`
only while it runs, `s scroll` only once it has a terminal, `D diff` for an agent in a git
repository, `F finish` for a worktree agent), and the general ones, ending with `q quit`. With
Projects focused they offer the project actions (`n new agent`, `a add project`, `d remove
project` once the project has no agents) instead of the agent's. With no project yet they
are `a add a project · ? help · q quit`; while a filter is kept they start with `esc clear
filter`. In full screen (`+` twice) the first hint is the way back: `+ exit full screen` in
the sidebar, `ctrl-q + exit full screen` in the pane.

The pane of a stopped or exited agent keeps its last screen, with a bar over the bottom row
naming the state and the way back: `exited (<code>) · enter restart · d remove` (red for a
non-zero code), or `stopped · enter start · d remove`. Scroll mode hides the bar.

An agent whose turn ends (working or needs input → idle) while it is out of view is
listed as `done`, with a blue dot, until you view it: select it, or, for the agent already
selected, bring the terminal back into focus. Out of view means not selected, or selected
while the terminal lacks focus; terminals that report no focus changes (tmux without
`focus-events on`) always count as focused. An agent that works again, or stops, is no
longer `done`. The marks live in the TUI and start empty when it starts.

The filter (`/`) is typed in the status bar and narrows the Agents box as it changes, in
both the project and the all-agents view. It keeps an agent when the text appears, ignoring
case, in its listed name, its name or its worktree branch. Text after `s:` matches the status
as listed instead: `s:needs`, `s:done`, `s:exited`. `enter` keeps it, shown in the box's title as
`Agents · /<text>`, and `esc` while typing, or in the sidebar while one is kept, clears it.
`backspace` deletes, `ctrl-u` empties it, and a paste adds its first line. While a filter is
kept, `g` only visits agents that match it. When it hides every agent, the Agents box and
the pane say `no agents match /<text> · esc clears`.

`J`/`K` scroll the selected agent's pane by half its height and leave the focus in the
sidebar. Moving to another agent, or opening the pane with `enter`, scrolls it back to the
bottom. A stopped agent has no terminal to scroll.

Before the first project is added, the Projects box, the Agents box and the pane all point
at `a` (add a project), since `n` needs a project to put the agent in.

The add project picker lists `+ new project…` and then every known project that is not in
the sidebar, most recently added first. `j`/`k` (or `↓`/`↑`) move, `enter` adds the
highlighted project with its old name or opens the new project form, `x` forgets the
highlighted project, and `esc`/`q` closes. A project removed with `d` stays known. The new
project form has Path (starts at the cwd; relative paths resolve against the cwd, a
leading `~` is the home directory) and Name (empty means the directory name); `tab` moves
between them, `enter` adds and `esc` cancels.

The worktree setup approval popup wraps long script lines and shows control characters
escaped (`\r`, `\t`, `\u{1b}`, and `\u{200b}` for zero-width and direction-changing
characters), with a literal backslash shown as `\\`, so the whole script is on screen. When it does not fit,
`j`/`k` or `↓`/`↑` scroll a row, `pageup`/`pagedown` or `ctrl-u`/`ctrl-d` half a page, `g`/`G`
jump to the top or bottom, and the popup says how many rows are above or below. `y`
does nothing until the last row has been shown, and on a terminal too small to show the script
it never acts; `s` (skip the setup) and `n`/`esc` work at any time.

Popups: `y`/`enter` confirms and `n`/`esc` cancels a confirmation. In the rename popup
`enter` renames and `esc` cancels. In the new agent form,
`tab`/`shift-tab` (or `↓`/`↑`) move between the visible fields, `enter` starts the agent and
`esc` cancels. The fields are Name, Preset (`←`/`→`/`space` change it), Command (only for the
custom preset), Worktree (`space`/`←`/`→` toggle it; for a project outside a git repository it
reads `off (not a git repository)` and `tab` skips it), Base (only while Worktree is on) and
Prompt, the agent's first message (empty means none; a pasted newline becomes a space).
Spaces in an agent's name (new agent form and rename) become dashes: `my new agent` is
stored as `my-new-agent`. An empty name becomes `<preset>-<n>` with the lowest number no
agent in the project uses (`claude-1`, `shell-2`). The Agents box and the pane's header
show a Claude agent by its session title once Claude has set one, and by its name until
then; the name stays the agent's identity for the CLI and its worktree branch, and the Info
box shows it as `name` while the title stands in for it. A name too
long for its row or the pane's header is cut with `…`, so the status after it stays visible.
With Worktree on, the agent runs in a new git worktree on branch `lazyagents/<name>`, made
from Base; an empty Base means the project's current branch. `d` never deletes an agent's branch, so a new worktree agent with a removed agent's name
fails with `branch lazyagents/<name> already exists; delete it or choose another
agent name` until that branch is deleted (the finish menu does that for a merged one) or renamed.

The new agent form, the new project form and rename stay open after `enter` until the
daemon answers, showing `waiting for the daemon · esc close`; other keys do nothing
meanwhile, and `esc` closes the form and leaves the answer to the status bar. When the daemon
refuses, the form keeps what was typed and shows the refusal in red above its hint, until
the next `enter`. A form that cannot be sent (a custom preset without a command, an unreadable
`.lazyagents.toml`) says why the same way.

Removing a worktree agent asks `Remove <name>? y: agent + worktree · k: keep worktree ·
n: cancel`:

| Key | Action |
|---|---|
| `y` | remove the agent and its worktree (the branch stays); once done the status bar says `removed <name> and its worktree, kept branch <branch>` |
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
changes), against `HEAD` otherwise. Untracked files that are not ignored show as new files.
Added lines are green, removed lines red, hunk headers
cyan and file headers bold (`[theme]` `diff_add`, `diff_remove`, `diff_hunk`). The view
shows at most 20 000 lines. A git error closes it and shows the error in the status bar.

| Key | Action |
|---|---|
| `j`/`k`, `↓`/`↑` | move the cursor one line |
| `ctrl-d`/`ctrl-u` | move it half a page |
| `pagedown`/`pageup` | move it a page |
| `g`/`G` | first / last line |
| `s` | switch what the diff shows: all changes, uncommitted only, committed only (the title names it, with the file count) |
| `]`/`[` | next / previous file |
| `}`/`{` | next / previous hunk |
| `c` | comment on the cursor line (an added, removed or context line); on a commented line, edit the comment; saving it empty removes it |
| `n`/`N` | next / previous commented line |
| `S` | send every comment to the agent as one prompt and return to the sidebar |
| `D`, `q`, `esc` | close, back to the Agents box |
| `ctrl-c` | quit the TUI (agents keep running) |

The view scrolls to keep the cursor line on screen. It is drawn reversed, so its colour
stays visible as a bar: green on an added line, red on a removed one. The mouse wheel scrolls
the diff and keeps the cursor inside it. The view closes by itself if its agent is removed.

Review comments belong to the agent and stay, drawn under their line as `✎ <text>`, when the
diff is closed and opened again, until `S` sends them or the TUI quits. A comment is tied to
its file and line (a new-file line, or an old-file line for a removed one), so it is shown
again only where the new diff has that line. `S` pastes this into the agent and presses
`enter`:

```
Review comments on your changes. Address each one:

- src/a.rs:11: name this better
  +let fresh = old();
- src/a.rs:11 (removed line): why remove this?
  -check();
```

The comment popup shows the line it is about; `enter` saves, `esc` cancels.

**Pane mode**: all input goes to the agent except the `ctrl-q` prefix and `ctrl-]`.
After `ctrl-q`, one more key picks a pane action:

| Keys | Action |
|---|---|
| `ctrl-q j` | next agent in the Agents panel, staying in the pane |
| `ctrl-q k` | previous agent in the Agents panel, staying in the pane |
| `ctrl-q q`, `ctrl-q esc` | back to the sidebar |
| `ctrl-q s` | scroll mode |
| `ctrl-q +` | next screen mode, staying in the pane |
| `ctrl-q ctrl-q` | send one `ctrl-q` to the agent |

While the prefix waits for its second key the status bar shows `ctrl-q …`. Any other key
after `ctrl-q` cancels the prefix, reaches nobody, and the status bar lists the prefix
keys (`ctrl-q: j/k switch · q/esc sidebar · s scroll · + screen · ctrl-q send`). Switching
onto an agent that is not running returns focus to the sidebar. A
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
shows `SCROLL · q/esc exit · j/k move · ctrl-d/ctrl-u half page · g/G top/bottom ·
pagedown/pageup page`.

| Key | Action |
|---|---|
| `j`/`k`, `↓`/`↑` | one line towards the bottom / back in history |
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

**Mouse**: drags never reach the agent; clicks in the pane and the wheel may.

| Action | Effect |
|---|---|
| click a Projects row | select that project and focus Projects |
| click an Agents row | select that agent and focus Agents (never the pane) |
| click elsewhere in the Projects or Agents box | focus that box |
| click in the pane | focus the pane if the agent is running; otherwise the status bar says `<name> is not running — enter starts it` (`enter restarts it` for an exited agent) |
| release in the cell the click started in | send the click to a running agent that enabled mouse reporting, such as Claude Code's subagent list; outside scroll mode only |
| drag in the pane | select text, shown reversed; the selection stays inside the pane |
| release after a drag | copy the selected text and show `copied N characters` (in the `accent` colour) |
| click outside an open popup | close it, like `esc` |
| wheel over the pane | see below |

The selection runs in reading order from where the drag started to where it ends, whole
lines in between; trailing spaces are trimmed from each line. A click without a drag
copies nothing. The agent gets its click, press and release together, when the button
is released, and only if the release lands in the cell the press did; a drag that leaves
that cell and comes back still counts as a click. Clicking a sidebar row also closes the diff view, or scroll mode (scrolling
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
"terminal too small (<width>×<height>)" and the size it needs in the current screen mode
(`needs at least 46×8` beside the usual sidebar, 40×8 beside the narrow one, 22×8 in full
screen), and keys and pastes do nothing except the quit keys (`q`/`ctrl-c`,
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
