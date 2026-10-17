# 0017. The daemon picks the keys that approve an agent

Date: 2026-10-17
Status: Accepted

## Context

ADR 0011 answers a waiting agent by typing into its terminal, and `y` pressed `enter`
because Claude's permission dialog highlights yes. Claude's workspace-trust dialog, which
every new worktree shows, highlights "No, exit" instead, so `y` quit Claude and lost the
agent's first prompt. Approving the trust dialog needs arrow keys chosen from where the `❯`
cursor and the yes option sit on the screen. The daemon owns that screen and already
recognises the dialog; the TUI only sees the screen of the agent it subscribes to.

## Decision

The TUI sends `Request::Approve { id }` and the daemon types the answer. For a Claude
agent the daemon saw reach the trust dialog while starting, and that has reported no hook
nor left `NeedsInput` since, while its screen still shows the dialog, that is up or down
from the option carrying the `❯` cursor to the option starting `Yes,` (counting option
lines, not wrapped lines), then `enter`; it refuses with an error when it cannot find both,
rather than pressing `enter` on whatever is highlighted. For anything else it is `enter`,
as before. The screen alone does not decide: an agent working on lazyagents itself can
show the dialog's text in a diff while it waits on an ordinary permission prompt. The keys
go out in one write, so a second approval sent before Claude redraws follows the first
whole instead of interleaving with it. Deny and reply stay plain `Input` keystrokes.

## Alternatives

- The TUI matches `AgentInfo.waiting_for` (`trusting this folder`) and sends the arrows
  itself: a display string would become a contract between crates, and the TUI would still
  need the screen to know where the cursor is.
- The TUI reads the agent's subscribed screen: it holds only the screen of the agent shown
  in the main pane, while `y` works on any selected agent in the sidebar.
- Press the option's number: Claude's current trust dialog shows no numbers, and the
  number of yes would still have to be read from the screen.
- Write each key separately, spaced apart, in case a program takes each read as one
  keypress: Claude Code 2.1.302 accepted the arrow and `enter` in one write, and spaced
  writes from two quick approvals interleave (down, down, enter, enter), which ends on
  "No, exit" if the list wraps.

## Consequences

`y` trusts the folder whichever option is highlighted and in whichever order the two are
listed. The approve keys now live in the daemon, so other dialogs that need more than
`enter` can be taught there without changing the TUI. It still relies on Claude's layout:
options start with `Yes,` or `No,`, and the cursor is `❯`.
