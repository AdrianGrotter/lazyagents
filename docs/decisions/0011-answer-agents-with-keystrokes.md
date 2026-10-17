# 0011. Answer waiting agents with keystrokes, not hook decisions

Date: 2026-10-17
Status: Accepted; amended by 0017

## Context

An agent waiting on a permission prompt should be answerable from the sidebar (`y`
approve, `N` deny, `i` reply) without entering its pane. Claude Code offers two ways in. A
`PermissionRequest` hook may answer with `decision.behavior` `allow` or `deny`, so the hook
could block until the user decides in lazyagents. Or lazyagents can type into the agent's
terminal, which the daemon already owns, exactly as the user would in the pane.

## Decision

The TUI sends keystrokes through the existing `Input` request: `enter` approves (the
dialog's highlighted choice is yes), `esc` denies, and a reply is a paste followed by
`enter`. The hooks stay fire-and-forget; the `PermissionRequest` hook only reports what is
being asked, which the daemon keeps as `AgentInfo.waiting_for` for the Info box.

## Alternatives

- Block the `PermissionRequest` hook and return the user's decision: the answer would not
  depend on the dialog's layout, but the hook would hold Claude for as long as the user
  takes, against ADR 0003's rule that hooks never slow Claude down, and the hook's 5 s
  timeout would have to go. Whether Claude still draws a usable dialog in its pane while
  such a hook is pending is undocumented, so answering in the pane could stop working.
- Claude's cross-session messaging socket: it can start a turn, but its docs say a message
  can never approve a permission prompt, and its message format is not documented.

## Consequences

Approving works for any agent the same way typing in its pane does, and `lazyagents send`
gives scripts the reply half. It relies on Claude's dialog keeping yes as the highlighted
default and `esc` as no; if that changes, `y` and `N` need new keys, not a new mechanism.
`y` on an agent that waits for something other than a permission (an elicitation, the trust
dialog) presses `enter` on whatever that dialog highlights.

Amended by [0017](0017-daemon-picks-the-keys-that-approve.md): the trust dialog highlights
"No, exit", so `y` now sends `Approve` and the daemon picks the keys, moving to "Yes, I
trust this folder" before `enter`. Answers are still keystrokes.
