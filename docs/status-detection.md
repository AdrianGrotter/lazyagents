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
  - `Notification` → `NeedsInput`
  - `Stop` → `Idle`
- **Session rules**: every start uses a fresh `--session-id` until a prompt has been
  submitted. `UserPromptSubmit` marks the agent resumable, and from then on restarts use
  `--resume`. A `SessionStart` carrying a different session id replaces the stored id.
  When its `source` is `resume` or `compact` (`/resume`, compaction) the agent is
  resumable afterwards; for `startup`, `clear` or a missing source it is not until the
  next prompt. See
  `docs/decisions/0006-resume-only-prompted-sessions.md`.
- **Generic agents**: `Starting` until their first output, then `Working`; `Idle` after
  2 s without output, `Working` again on new output.
- **Not running**: for exited or stopped agents, input, resize and scroll requests are
  rejected with an error and hook events are ignored. Hook events for generic agents are
  ignored too.
- **Both**: process exit → `Exited(code)`; agents loaded from disk at daemon start →
  `Stopped`.
- The merge behaviour of `--settings` with user hooks was verified in the spike (see `docs/decisions/0003-status-via-claude-hooks.md`).
