# 0009. The TUI keeps the list of known projects

Date: 2026-10-16
Status: Accepted

## Context

Adding a project means typing its path, and a project removed from the sidebar is gone,
so finding a repository again takes as long as the first time. The add popup should offer
every project added before, and let the user forget entries. The daemon's projects are
what the sidebar shows; this list outlives them. Changing what the daemon stores or sends
bumps the protocol version, and a version mismatch makes the user restart the daemon,
which stops every running agent.

## Decision

The TUI keeps `known-projects.toml` in the state dir: name and path, most recently added
first, one entry per path. It records every project it sees the daemon report that it had
not seen before, including ones added with `lazyagents project add` and every project
on its first connect. `a` opens a picker of known projects not in the sidebar; `x` there
forgets one. Each change reads the file, applies the change and writes it back through a
temp file, so several TUIs merge their changes; a file that does not parse is never
overwritten.

## Alternatives

- The daemon stores the list in `state.toml`: one owner for all state and the CLI could
  use it, but it needs new requests and a protocol bump, so every user restarts their
  agents once for a convenience feature.
- Scan `~/git` or similar for repositories: finds projects never added, but guesses at
  the user's layout and slows the popup on large trees.

## Consequences

The list is a UI convenience the daemon never reads, so the CLI cannot list or pick from
it. A project added while no TUI runs is recorded when a TUI next connects. Two TUIs
writing at the same instant can lose one change, which costs one re-add or one `x`.
