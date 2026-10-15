# 0006. Restart resumes only sessions that received a prompt

Date: 2026-10-15
Status: Accepted

## Context

ADR 0003 starts Claude agents with `--session-id <uuid>` and restarts them with `--resume <uuid>`. A session that never received a prompt may not exist on disk, so `--resume` on it can fail. `/clear` also starts a new session id inside the same Claude process.

## Decision

The daemon marks an agent `resumable` when its `UserPromptSubmit` hook fires. Restart passes `--resume` only for resumable agents; otherwise it starts a fresh session id. A `SessionStart` hook carrying a different session id replaces the stored one; it clears `resumable` when its `source` is `startup` or `clear` (or missing) and sets it when the source is `resume` or `compact`, since those continue a session that already has history. `PostToolUse` maps to Working, so the status leaves "needs input" once an approved tool finishes.

## Alternatives

- Always `--resume`: fails for agents restarted before their first prompt.
- `--continue`: picks the most recent session in the directory, which is wrong when two agents share a directory.

## Consequences

Restarting an agent that never got a prompt gives a fresh session, which loses nothing. Status depends on hooks firing; agents whose Claude Code ignores `--settings` hooks stay in `starting`.
