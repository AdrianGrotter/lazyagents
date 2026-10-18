# 0020. Keep TUI errors where they happened and until they are read

Date: 2026-10-18
Status: Accepted

## Context

The TUI showed every error in the status line for five seconds. A refused new agent, add
project or rename closed its form first, so the typed input was lost and the reason
flashed by beside the key hints, cut off on a narrow terminal. Config warnings, a TOML
syntax error that drops the whole file among them, were joined into one such line. An
error from a slow request (a worktree create, a removal) could arrive and expire while the
user looked elsewhere.

## Decision

- A form that sends a request (new agent, add project, rename) stays open until the daemon
  answers. It ignores keys meanwhile except `esc`, which closes it and leaves the answer to
  the status line. A refusal stays in the form, word-wrapped above its hint, with the input
  kept. To tell the form's answer from another, the connection actor passes each ordered
  reply on with the request it answers.
- Startup config warnings open in the output popup, one per line, until dismissed.
- A status line notice lasts 5 s. An error lasts 5 s and after that until the next key
  press or click: the user's next action shows they were there, and an error never goes
  before anyone could have read it.
- The finish menu's two removals ask for confirmation, as `d` does, instead of the menu
  choice being the confirmation (ADR 0013), and a removal that took a worktree says so in the
  status bar once it succeeds.
- Whether a project can have a worktree is judged in the TUI by looking for `.git` in the
  project's path or above it, as git does; outside a repository the form's Worktree row
  says so and is skipped.

## Alternatives

- Close the form at once and reopen it with the error when the refusal arrives: the
  reopened form would take keys the user meant for something else, as the dirty-worktree
  prompt guards against; waiting in the open form needs no such guard.
- Errors that stay until the next key, with no minimum: a key typed in the pane just as an
  error arrives would clear it before it was drawn.
- A longer fixed time for errors (say 15 s): still lost when nobody is looking, and still
  sitting there long after it was read.
- Errors until dismissed with a dedicated key: one more key to learn for every error.
- Tell the TUI about git repositories through the protocol (a flag on `Project`): a wire
  change for something the TUI, on the same machine as the daemon, can check itself.

## Consequences

The user can fix a refused name or path and send again. While a create's worktree is made,
the form shows `waiting for the daemon`; `esc` gets the sidebar back at once. A sticky error
covers part of the key hints until the next key. The `.git` check misses a repository
that git finds only through `GIT_DIR`; the form then offers no worktree although the daemon
could make one.
