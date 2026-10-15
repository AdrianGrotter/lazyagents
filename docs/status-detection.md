# Status detection

Status: implemented in crates/daemon (status.rs, server.rs).

## Detection rules

Status enum: `Starting`, `Working`, `NeedsInput`, `Idle`, `Exited(i32)`, `Stopped`.

- **Claude agents** are launched as
  `claude --session-id <uuid> --settings <state>/hooks/<agent_id>.json` or
  `claude --resume <uuid> --settings ...`. The generated settings file only adds
  hooks; Claude Code merges it with the user's own settings. Each hook runs
  `lazyagents hook <event>` with `LAZYAGENTS_AGENT_ID` and `LAZYAGENTS_SOCKET` in the
  environment:
  - `SessionStart` → `Idle`
  - `UserPromptSubmit`, `PreToolUse`, `PostToolUse` → `Working` (`PostToolUse` moves an
    agent out of `NeedsInput` once an approved tool finishes)
  - `Notification` → `NeedsInput`, except an idle-prompt notification
    (`notification_type` `idle_prompt`, or a `message` containing
    "waiting for your input", case-insensitive), which the hook forwards as `Stop`
  - `Stop` → `Idle`
- **Claude silence**: a `Working` Claude agent with no output for 10 s becomes `Idle`.
  The 10 s count from the latest of its start, its last output and its last status
  change, so a prompt submitted after a long quiet spell is not cut short. This covers an Esc
  interrupt, which fires no `Stop` hook. Other Claude statuses are left to the hooks.
- **Session rules**: every start uses a fresh `--session-id` until a prompt has been
  submitted. `UserPromptSubmit` marks the agent resumable, and from then on restarts use
  `--resume`. A `SessionStart` carrying a different session id replaces the stored id.
  When its `source` is `resume` or `compact` (`/resume`, compaction) the agent is
  resumable afterwards; for `startup`, `clear` or a missing source it is not until the
  next prompt. See
  `docs/decisions/0006-resume-only-prompted-sessions.md`.
- **Generic agents**: `Starting` until their first output, then `Working`; `Idle` after
  2 s without output, `Working` again on new output. A generic agent that prints nothing
  becomes `Idle` 2 s after it starts.
- **Not running**: an exited agent keeps its terminal, so resize and scroll requests
  still work and its last screen can be viewed and scrolled. Input requests are rejected
  with an error and hook events are ignored. A stopped agent has no terminal, so resize
  and scroll are rejected too. Subscribing works for any agent; a stopped agent sends
  frames once it is restarted. Hook events for generic agents are ignored.
- **Both**: process exit → `Exited(code)`; agents loaded from disk at daemon start →
  `Stopped`.
- The merge behaviour of `--settings` with user hooks was verified in the spike (see `docs/decisions/0003-status-via-claude-hooks.md`).
