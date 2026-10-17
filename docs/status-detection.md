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
  - `PermissionRequest` → `NeedsInput`, the moment Claude is about to ask
  - `Notification` → `NeedsInput` for a question: `notification_type`
    `permission_prompt`, `elicitation_dialog`, `elicitation_url_dialog`,
    `agent_needs_input`, or a type lazyagents does not know. The hook forwards an
    idle-prompt notification (`idle_prompt`, or a `message` containing "waiting for your
    input", case-insensitive) as `Stop`, and drops the informational ones
    (`auth_success`, `elicitation_complete`, `elicitation_response`, `agent_completed`
    and every `quota_*`) without telling the daemon
  - `Stop`, `StopFailure` (a turn ended by an API error) → `Idle`
- **Claude silence**: a `Working` Claude agent with no output for 10 s becomes `Idle`.
  The 10 s count from the latest of its start, its last output and its last hook (every
  hook counts, even one that leaves the status unchanged), so a prompt submitted after a
  long quiet spell is not cut short. This covers an Esc interrupt, which fires no `Stop`
  hook.
- **Claude trust dialog**: Claude runs no hook until the user trusts the directory, which
  every new worktree asks for. A `Starting` Claude agent whose screen shows the trust
  dialog ("Yes, I trust this folder", or the older "Do you trust the files in this
  folder") becomes `NeedsInput`; the `SessionStart` that follows the answer makes it
  `Idle`.
- **Claude startup silence**: a `Starting` Claude agent that has fired no hook and
  printed nothing for 30 s becomes `Idle`, counted from the later of its start and its
  last output. Other Claude statuses are left to the hooks.
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
