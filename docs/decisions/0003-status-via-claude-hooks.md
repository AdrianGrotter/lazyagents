# 0003. Claude status comes from hooks injected with `--settings`

Date: 2026-10-15
Status: Accepted (merge behaviour verified in the milestone 1 spike)

## Context

The sidebar must show whether each agent is working, needs input, is idle or exited. Scraping terminal output is fragile across Claude Code releases.

## Decision

Claude agents start with `--settings <generated.json>` that only adds hooks (`SessionStart`, `UserPromptSubmit`, `PreToolUse`, `Notification`, `Stop`), each calling `lazyagents hook <event>`. Claude Code merges these with the user's own settings, so `~/.claude/settings.json` is never edited. Agents get `--session-id <uuid>` at creation and `--resume <uuid>` on restart. Non-Claude agents use an output-activity heuristic.

## Alternatives

- Parse Claude's screen for spinners and prompts: breaks whenever the UI changes.
- Write hooks into the user's settings: invasive and affects sessions outside lazyagents.

## Consequences

Status is exact for Claude and approximate for other agents. Hooks must never slow Claude down: the hook subcommand times out quickly and always exits 0.
