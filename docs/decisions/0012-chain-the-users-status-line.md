# 0012. Read usage by chaining the user's own Claude status line

Date: 2026-10-17
Status: Accepted

## Context

lazyagents wants each Claude agent's model, cost, context use and plan limits. Claude Code
gives these to exactly one documented consumer outside the session: the `statusLine`
command, which gets them as JSON on stdin on every refresh. The other sources are worse:
transcripts change format between versions, and OpenTelemetry needs an exporter set up.

Two facts constrain using it. A `statusLine` in the `--settings` lazyagents passes replaces
the user's rather than merging with it. And any custom status line makes Claude hide most of
its footer hints (`esc to interrupt`, `? for shortcuts`), so giving one to a user who has
none changes how Claude looks.

## Decision

Only when the user already has a `command` status line, found where Claude would find it
(the agent directory's `.claude/settings.local.json`, then `.claude/settings.json`, then the
user's `settings.json`), the generated settings carry a copy of it with the command set to
`lazyagents statusline`. That command starts the user's command with the same JSON on stdin
and its stdout untouched, forwards the figures to the daemon within the hook budget while it
runs, and exits with its exit code. A user without a status line gets none, and lazyagents
shows no usage for their agents.

## Alternatives

- Inject a status line for everyone, with a lazyagents-designed line for users who have
  none: usage for all, but Claude loses its footer hints for users who never asked for a
  status line. Left for later, as an opt-in, if it is wanted.
- Write into the user's `~/.claude/settings.json`: lazyagents never edits the user's
  Claude settings (ADR 0003).
- Parse the session transcript: undocumented and versioned format.
- OpenTelemetry: accurate cost by session, but needs the user to configure an exporter and
  a collector that lazyagents would have to run.

## Consequences

For a user with a status line, nothing visible changes in Claude, and lazyagents shows the
figures in its Info box and `lazyagents ls`. The user's status line now depends on the
lazyagents binary in agents it starts: a broken install would blank it. Changes to the
status line's padding or refresh interval apply when the agent restarts; changes to the
script behind it apply at once. Rate limits appear only for Pro and Max plans, as Claude
sends them.
