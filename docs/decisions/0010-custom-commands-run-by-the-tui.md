# 0010. Custom commands run by the TUI, with shell-quoted template values

Date: 2026-10-17
Status: Accepted

## Context

Users want to run their own tools against an agent: lazygit or an editor in its worktree,
`gh pr create`, a test watcher, a commit with a typed message. Building each into
lazyagents would turn it into a git client and a PR tool. lazygit solves the same problem
with `customCommands`: templated shell commands bound to keys, with prompts and a choice of
where the output goes.

Three choices shape such a feature here. Who runs the command: the TUI, or the daemon that
owns the agents. How values get into the shell command: agent titles come from Claude's
terminal title, which the model writes, so a title can contain `$(...)`. And how a command
gets the real terminal, which the TUI holds in raw mode with a thread blocked on reading it.

## Decision

`[[commands]]` in the TUI's `config.toml`, with `key`, `description`, `command`, `context`
(`agent` or `project`), `output` (`terminal`, `agent`, `popup`, `none`) and `prompts`
(`input`, `menu`, `confirm`). The TUI runs them with `sh -c`; `output = "agent"` is the
exception, which creates a generic agent through the daemon as `n` would. Every `{{name}}`
in the template is replaced by its value shell-quoted, always. For `output = "terminal"` the
TUI pauses its terminal reader (which polls in 50 ms slices so a pause takes effect),
leaves raw mode and the alternate screen, runs the command in the foreground, then restores
everything and drops the SIGINT a ctrl-c in the command also delivered to the TUI.

## Alternatives

- The daemon runs commands: it would outlive the TUI and serve the CLI too, but a command
  that needs the user's terminal (lazygit, `$EDITOR`) can only run where the terminal is,
  and the daemon would need the TUI's config (see ADR 0009 for keeping user state in the
  TUI).
- lazygit's model, with values inserted raw and a `quote` filter to opt in: familiar, but
  one forgotten filter on a model-written title is a shell injection. Always quoting still
  lets a value sit inside a word: `--name={{agent.name}}` becomes `--name='api'`, which the
  shell reads as one word. It only rules out a value expanding into several words or into
  code, which is the point.
- Blocking reads with the reader thread left running during a command: the thread would
  take the command's keystrokes. Killing and respawning the thread is impossible while it
  is blocked in `read`.
- Running the command in its own process group as the terminal's foreground job: the
  cleanest isolation from ctrl-c and ctrl-z, but it needs `tcsetpgrp` through a libc
  dependency the TUI does not have. Catching the stray SIGINT covers ctrl-c; ctrl-z and
  ctrl-\ still reach lazyagents.

## Consequences

Most integrations (editor, lazygit, PR flow, issue-to-agent, test runners) become config
instead of code, and the docs can ship them as examples. Commands are TUI-only: the CLI
cannot run them, and a background command dies with the TUI. A value can never expand into
several arguments; a command that needs that must split it itself. The terminal reader now
wakes every 50 ms instead of blocking.
