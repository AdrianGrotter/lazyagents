# 0028. Answer a dirty worktree with a typed failure

Date: 2026-10-18
Status: Accepted

## Context

`RemoveAgent` with `remove_worktree` is refused while the worktree has changes. The daemon
said so as text, `worktree has uncommitted changes:` and one path per line, and the TUI
recognised the refusal by matching its own copy of that prefix, then parsed the paths from
the rest to offer a forced removal. Two copies of one string had to stay in step, and any
other error that happened to start with the prefix would have offered to force-remove the
worktree. Every request failure was a plain `String` on the wire.

## Decision

A response's error is a `Failure` (protocol version 8): `Message(String)` for every
failure meant only for the user, and `DirtyWorktree { files }` for the refusal a client
acts on. `Failure` displays as the old text, so the CLI prints what it printed before. The
client passes a `Failure` on as the error's type, and the TUI downcasts it to tell the
refusal from any other error.

## Alternatives

- A `Response::DirtyWorktree` variant answered as `Ok`: a refusal on the success path,
  where the TUI's removal report and the CLI's exit status would each have to check for
  it.
- A structured error for every daemon failure: many variants no client acts on.
- Keep the text and share the prefix as a constant in `protocol`: still matching text, and
  still parsing the paths back out of it.

## Consequences

A refusal that a client must act on gets its own `Failure` variant and a protocol bump;
everything else stays a message. Clients built before version 8 cannot talk to the
daemon, as with any wire change.
