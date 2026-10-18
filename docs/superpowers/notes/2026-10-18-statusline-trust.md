# Does Claude run a `--settings` status line before workspace trust?

Date: 2026-10-18. Question: `lazyagents statusline` runs the status line command from the
agent directory's `.claude/settings.local.json` or `.claude/settings.json` (ADR 0012). If
Claude Code ran the `statusLine` lazyagents passes in `--settings` before the user accepted
the workspace-trust dialog, a freshly cloned repo would get code execution without approval.

## Answer: no, not in interactive mode since Claude Code 2.1.51

Changelog (github.com/anthropics/claude-code, CHANGELOG.md):

- 2.1.51: "Fixed a security issue where `statusLine` and `fileSuggestion` hook commands could
  execute without workspace trust acceptance in interactive mode."
- 2.1.79: "Fixed custom status line showing nothing when workspace trust is blocking it."

The installed 2.1.302 (also 2.1.300 and 2.1.301 on disk) shows the gate in its bundled code,
read with `strings` without starting a session:

- The status line runner returns early with the debug message "Skipping StatusLine command
  execution - workspace trust not accepted" when trust is not accepted, before it reads
  `statusLine` from the merged settings. The merged settings include the `--settings` flag
  file, so the gate covers lazyagents' line as well as the user's own.
- The status line component checks the same condition on mount and logs "Status line command
  skipped: workspace trust not accepted"; `subagentStatusLine` has the same gate.
- The trust check passes when the session is non-interactive (`-p`), when trust was accepted
  in this session, when `CLAUDE_CODE_SANDBOXED` is set, or when `~/.claude.json` has
  `projects[<path>].hasTrustDialogAccepted: true` for the project path or, walking up, a
  parent within the same git repository.

lazyagents starts Claude interactively, without `-p` or any trust-bypassing flag, so Claude
never invokes `lazyagents statusline` in an untrusted folder, and lazyagents adds no code
execution that Claude itself would not do.

## Could `lazyagents statusline` check trust itself?

- The daemon's `at_trust_dialog` flag (ADR 0017) only records that the agent showed the dialog
  while starting; it does not say whether the user accepted it, and the status line process
  cannot see it without a new request on the wire.
- Reading `~/.claude.json` would mean copying Claude's rules: the parent walk bounded by the
  git root, session-only trust (for example the home directory), the sandbox override. A copy
  that drifts would hide the status line in folders Claude trusts.
- The call itself is the evidence: Claude only runs the status line after its own trust check.

So no code change. If a future Claude version dropped the gate, the safe fallback is to chain
only when `projects[<project dir>].hasTrustDialogAccepted` is true, accepting that
session-only trust then shows no line.
