# Status detection

Status: designed; the spike shows hook events from a real claude in its sidebar.

## Detection rules

Status enum: `Starting`, `Working`, `NeedsInput`, `Idle`, `Exited(i32)`, `Stopped`.

- **Claude agents** are launched as
  `claude --session-id <uuid> --settings <state>/hooks/<agent_id>.json` (first start) or
  `claude --resume <uuid> --settings ...` (restart). The generated settings file only adds
  hooks; Claude Code merges it with the user's own settings. Each hook runs
  `lazyagents hook <event>` with `LAZYAGENTS_AGENT_ID` and `LAZYAGENTS_SOCKET` in the
  environment:
  - `SessionStart` → `Idle` (also records the session id from the payload, so `/clear`
    and similar don't break resume)
  - `UserPromptSubmit`, `PreToolUse` → `Working`
  - `Notification` → `NeedsInput`
  - `Stop` → `Idle`
- **Generic agents**: `Working` if output arrived in the last 2 s, otherwise `Idle`.
- **Both**: process exit → `Exited(code)`; agents loaded from disk at daemon start →
  `Stopped`.
- The merge behaviour of `--settings` with user hooks is verified in the spike before
  being relied on (see `docs/decisions/0003-status-via-claude-hooks.md`).
