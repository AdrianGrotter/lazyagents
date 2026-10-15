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

`just spike` runs `claude` (or `just spike <cmd...>`) in the main pane with a sidebar showing process state, Claude hook status, and the terminal modes the agent enabled. Enter focuses the pane, ctrl-] returns to the sidebar, q quits from the sidebar.

## Human checklist

Run `just spike` in a project directory, once directly in your terminal and once inside tmux. Tick what holds; note anything off.

- [ ] Claude renders like it does natively: colors, spinner, box drawing, diff output
- [ ] Typing feels instant; enter submits
- [ ] alt+enter inserts a newline; shift+enter too where "kbd enhancement: on" shows
- [ ] ctrl-c interrupts, esc cancels, up/down history, tab completion, shift+tab cycles modes
- [ ] multi-line paste lands as one prompt
- [ ] mouse wheel scrolls back through output
- [ ] resizing the terminal reflows the pane
- [ ] sidebar status moves through idle → working → needs input (trigger a permission prompt) → idle
- [ ] your own setup still applies: statusline, plugins/skills, your hooks (e.g. your Stop hooks still fire)
- [ ] `/exit` shows `process: exited (0)`
- [ ] overall verdict: good enough to build the daemon on

## Known gaps (deliberately out of the spike)

Text selection and copy, kitty keyboard protocol towards the agent, multiple agents, the daemon. These land in plan 2.

## Findings

Executor notes, known spike limitations from code review:

- On quit, the temp hook directory is removed before the agent is shut down. A Stop hook firing during teardown may show an error in Claude.
- A panic would leave mouse capture and bracketed paste enabled in the shell; run `reset`.
- The app redraws about 60 times per second even when idle.

User findings from the checklist:

