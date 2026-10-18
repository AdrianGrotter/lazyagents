# 0023. Bound each client's queue: drop frames, close a connection that overflows

Date: 2026-10-18
Status: Accepted

## Context

The event loop never writes to a socket itself: each connection has a queue that its own task writes out. The queue was unbounded, and the loop feeds a subscribed client a frame at every 33 ms tick while its agent's screen changes, each up to the whole screen. A client that stops reading (a hung or stopped TUI process) made the daemon's memory grow without limit for as long as it stayed connected, and when it read again it worked through a backlog of stale frames before reaching the current screen.

Frames are diffs against what the daemon last sent that connection, so a frame cannot simply be skipped. Replies and events (`AgentUpdated`, `AgentRemoved`, `ProjectsChanged`, `AgentBell`) carry state a client cannot recover from a later message of the same kind without asking again.

## Decision

Each connection gets a bounded queue (`outbox.rs`). At most 4 frames may be queued or being written; a frame beyond them is dropped, and the subscription forgets what it sent, so the next frame that goes through is full, exactly as on subscribe. The wire is unchanged: a full frame already means "replace the screen". Any other message is queued up to 16384; one that does not fit marks the connection, and the next tick closes it (both its reading and its writing task), with a warning in the log. A reply from a git query thread goes through the same queue.

## Alternatives

- A bounded queue that drops nothing and makes the loop wait for room: one stalled client freezes every pane and hook.
- Drop events as well as frames: the client's agent list would silently go stale; closing the connection makes the failure visible and a new connection starts from a full list.
- A "resync" message telling the client to fetch everything again: a protocol change every client must learn, where closing reuses the reconnect path.
- Bound the queue in bytes rather than messages: frames are already capped by count, and the other messages are small, so a count bounds memory well enough (about 16 MB at 1 KB a message) with simpler accounting.
- Coalesce frames in the writer (keep only the newest diff): impossible without composing diffs, which the full frame after a drop makes unnecessary.

## Consequences

A client that stops reading costs at most 4 frames and 16384 other messages of memory. One that resumes after a short stall sees the current screen in one full frame. One that stays stalled while agents keep reporting (16384 events is minutes of hooks from many busy agents) is disconnected; the TUI then exits with "the daemon stopped", although the daemon still runs.
