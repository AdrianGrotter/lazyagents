# 0015. Forward clicks to the agent, keep drags for copying

Date: 2026-10-17
Status: Accepted

## Context

The TUI captures the mouse: a click in the pane focuses it and a drag selects and copies
pane text. No click reached the agent, so Claude Code's clickable elements, such as the
subagent list under its status line, did nothing. Claude Code enables mouse reporting for
these, and with it reporting on, it expects presses, releases and drags. Only the daemon
knows the agent's terminal modes; the TUI does not.

## Decision

A left-button press and release in the same pane cell, outside scroll mode and the diff
view, on a running agent, is sent as `InputEvent::Click { col, row }` when the button is
released. The daemon encodes it as a press followed by a release (SGR or X10, as the agent
asked) when the agent enabled mouse reporting, and drops it otherwise. A drag that leaves
the cell stays a lazyagents selection and copy, and never reaches the agent.

## Alternatives

- Forward every mouse event to an agent that enabled mouse reporting, as tmux does: Claude
  Code would get drags and do its own selection, but lazyagents' drag-to-copy would stop
  working for exactly the agents it is used with most, and the TUI would need the agent's
  mouse mode in every frame to decide where a press goes.
- Forward clicks only while a modifier is held: no change to copying, but nothing in
  Claude Code hints at the modifier, and terminals already reserve shift, Option and Fn
  for bypassing mouse capture.

## Consequences

Clicking things in Claude Code works, and copying by drag is unchanged. The agent sees a
click only once the button is released, so press-and-hold or drag interactions inside the
agent are impossible. A drag that returns to its starting cell is sent as a click. The
TUI does not need to know the agent's mouse mode, but adding `Click` changes the protocol
to version 6.
