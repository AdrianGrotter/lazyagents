# Spike report: embedded terminal (milestone 1)

Date: 2026-10-15
Plan: docs/superpowers/plans/2026-10-15-foundation-and-spike.md

## Automated results

- `just check`: exit 0. `39 tests run: 39 passed, 0 skipped`, `all hook tests passed`, `harness self-test passed`, `spike smoke passed`.
- `claude --help` on `--settings`:

  ```
  --settings <file-or-json>   Path to a settings JSON file or a JSON string to load additional settings from
  ```

## What the spike does

The spike (`target/debug/spike [cmd...]`, or `just spike [cmd...]`) runs `claude` by default in the main pane with a sidebar showing process state, Claude hook status, and the terminal modes the agent enabled. Enter focuses the pane, ctrl-] returns to the sidebar, q quits from the sidebar.

## Human checklist

Build once from the repo with `nix develop -c cargo build -p spike`. Then, from a real project directory and outside the nix dev shell, run `~/git/lazyagents/target/debug/spike`, once directly in your terminal and once inside tmux. Tick what holds; note anything off.

- [ ] Claude renders like it does natively: colors, spinner, box drawing, diff output
- [ ] Typing feels instant; enter submits
- [ ] alt+enter inserts a newline; shift+enter too where "kbd enhancement: on" shows
- [ ] ctrl-c interrupts, esc cancels, up/down history, tab completion, shift+tab cycles modes
- [ ] multi-line paste lands as one prompt
- [ ] mouse wheel scrolls back through output
- [ ] resizing the terminal reflows the pane
- [x] sidebar status moves through idle → working → needs input (trigger a permission prompt) → idle
  - The spike has no PostToolUse hook, so after you approve a permission prompt the status stays "needs input" until the next tool call or Stop. Claude's idle notification also shows as "needs input". Do not record either alone as a bug.
- [x] your own setup still applies: statusline, plugins/skills, your hooks (e.g. your Stop hooks still fire)
- [ ] `/exit` shows `process: exited (0)`
- [x] overall verdict: good enough to build the daemon on

## Known gaps (deliberately out of the spike)

Text selection and copy, kitty keyboard protocol towards the agent, multiple agents, the daemon. These land in plan 2.

## Findings

Executor notes, known spike limitations from code review:

- On quit, the temp hook directory is removed before the agent is shut down. A Stop hook firing during teardown may show an error in Claude.
- A panic would leave mouse capture and bracketed paste enabled in the shell; run `reset`.
- The app redraws about 60 times per second even when idle.
- The child inherits terminal-identifying environment variables from the parent (`TMUX`, `TMUX_PANE`, `TERM_PROGRAM`, ...), so Claude may believe it runs in tmux or Ghostty.
- Emoji with variation selectors or combining characters may render slightly differently, because zero-width characters are not drawn.
- The spike does not answer an OSC 11 background-colour query (`ColorRequest`), which could affect Claude's theme detection; check that first if the theme looks wrong.

User findings from the checklist:

- Verdict (2026-10-15): it works. The embedded pane is good enough to build the daemon on; hook status followed Claude, and the user's own Claude Code setup kept working. Items left unticked were not reported individually.
