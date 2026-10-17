# Review batch 2 plan

The remaining findings of the 2026-10-17 whole-app review, after the six "fix first" items
landed (plan 2026-10-17-review-fixes). No separate spec: each task states its required
behaviour. Owner decisions: the repo stays unlicensed; CI is left alone; the agent
allowlist gets `ask` entries; the big refactors run last.

## Global constraints

- Tooling comes from the nix dev shell: prefix commands with `nix develop -c`.
- Follow AGENTS.md: test first (watch the test fail for the expected reason, then
  implement); one Conventional Commit per logical change (load the `committing` skill);
  a change to the protocol, keybindings, configuration or status detection updates its
  doc in `docs/` in the same commit; a choice between real alternatives gets an ADR
  (`recording-decisions` skill; check `docs/decisions/` for the next free number at
  commit time, since other branches add ADRs too), and an ADR a change overtakes gets
  an "Amended by" note.
- Comments: default to none; never narrate a change.
- Done for a task means `nix develop -c just check` is green and you read its output
  (3–6 minutes; run it with a 10-minute timeout). Anything visible in the TUI also gets
  a harness run (skill: running-dev).
- Safety: never run `just install`. Never touch `~/.local/state/lazyagents`. Every
  manual run sets `LAZYAGENTS_STATE_DIR` to a short temp dir and shuts its daemon down.
  Never kill processes by name or pattern (`pkill`, `killall`): the user's real daemon
  and agents, including the controller of this work, run the same binary name. Never
  start real `claude` agents. Clean up temp dirs you create.
- The wire protocol is pinned by `crates/protocol/tests/wire.rs`. Version 6 has not
  been installed anywhere yet, so a task that must change the wire regenerates the v6
  fixture in place (delete `crates/protocol/tests/fixtures/wire-v6.txt`, run with
  `UPDATE_GOLDEN=1`) rather than bumping, and says so in its commit body. Prefer
  designs that don't change the wire.

## Task 1: Test infrastructure

- Make the tmux harness server name configurable: `scripts/tmux-harness.sh` uses
  `${LAZYAGENTS_HARNESS_SERVER:-lazyagents-harness}`; every script that drives it
  (smoke-tui, smoke-worktree, test-harness, test-tui-hangup) honours the same variable,
  so parallel `just check` runs in different worktrees can't collide. Document it in the
  `running-dev` skill.
- Add `.config/nextest.toml`: a test group that limits concurrency for the process-heavy
  binaries (`daemon` integration tests `server` and `git`, `lazyagents` `cli`, `client`
  `client`) — pick a limit (e.g. 4) and justify it in the commit body — plus a
  `slow-timeout` with `terminate-after` so a hung test fails clearly instead of hanging
  the run. Keep `just check`'s command working (`--locked --no-tests=pass`).
- Isolate tests from the developer's git config: the daemon's own git calls in tests
  must not read `~/.gitconfig` or system config (`commit.gpgsign`, `core.hooksPath`,
  `merge.ff=only`). Set `GIT_CONFIG_GLOBAL=/dev/null` and `GIT_CONFIG_NOSYSTEM=1` for
  the whole test process (nextest `[env]`/setup, or the test helpers that spawn the
  daemon), and set any identity the tests need explicitly. Prove it with a test that
  fails under a hostile global config (e.g. point `GIT_CONFIG_GLOBAL` at a temp file
  with `merge.ff = only` / `commit.gpgsign = true` and show the isolation wins).
- Loosen load-sensitive timing in tests without losing what they check: e.g.
  `crates/lazyagents/tests/cli.rs` `no_subcommand_without_a_terminal_fails_cleanly`
  asserts under 5 s and flaked at 9.6 s under load — assert it doesn't start a daemon
  or hang (a generous bound, e.g. 30 s) instead. Replace fixed sleeps before assertions
  with polling for the real condition: `cli.rs` (~203, ~285: sleep then read a file that
  may be half-written) and `crates/daemon/tests/server.rs` (~2747: a negative "setup ran
  again" check after 300 ms — wait for a definite marker instead). Grep for other
  `sleep` before asserts and fix those too.
- The tmux smoke scripts start daemons inside the developer's shell; unset `DIRENV_*`
  in them so they don't depend on the developer's direnv state.
- Deduplicate the DIRENV variable list in `crates/lazyagents/tests/cli.rs` (two copies).
- Add tests for the TUI runtime's untested flows in `crates/tui/src/run.rs`: the
  version-mismatch restart (ask, stop the old daemon, spawn, reconnect — use a fake
  listener that announces another protocol version, like the fake-daemon pattern in
  `crates/client/tests/client.rs`), and running a configured command in the terminal
  (reader paused/resumed). A harness/smoke scenario is acceptable where a unit test
  can't reach.

## Task 2: Agent setup and docs

- `.claude/settings.json`: add `permissions.ask` entries for `Bash(just install:*)`,
  `Bash(just dev:*)`, `Bash(nix develop -c just install:*)`,
  `Bash(nix develop -c just dev:*)` and the `--command` spellings, so they prompt even
  though `Bash(just:*)` is allowed. Check Claude Code's precedence rules (ask vs allow)
  so the entries actually take effect; document the result in the commit body.
- AGENTS.md: say `just check` takes 3–6 minutes (use a 10-minute timeout or run it in
  the background); say a clippy stop-hook or pre-commit failure in files you didn't
  touch is reported, not fixed, and that parallel agents should use worktree agents;
  list the CLI's full command set (`send`, `rename`, `project add|rm` included); include
  `docs/configuration.md` in the docs-in-the-same-commit rule; name
  `docs/superpowers/notes/` alongside specs and plans as dated history; mention
  `LAZYAGENTS_HARNESS_SERVER` if Task 1 added it (it has, by the time this runs).
- `.claude/skills/running-dev/SKILL.md`: the template uses a relative
  `target/debug/lazyagents` inside `cd <project> && …` and the next line says to use an
  absolute path — make the template absolute.
- `.claude/skills/committing/SKILL.md` step 1 repeats `verifying`; point it at the
  verifying skill instead, and say one green run on the combined tree covers several
  commits made from it.
- Move `docs/spike-report.md` under `docs/superpowers/notes/` (keep ADR 0002's
  reference working); drop the stale `spike` scope from `.githooks/commit-msg`, the
  committing skill and `scripts/test-hooks.sh` if it tests it.
- ADR 0003: add an "Amended" note — status also comes from reading the screen (trust
  dialog, `awaits_trust`) and from silence timers, and the hook list is now
  `HookKind::ALL`. ADR 0006: amended — agents whose hooks never fire become Idle after
  30 s of startup silence, not stuck in `starting`.
- ADR 0016 (direnv revert): add the rejected alternative "the daemon re-execs itself
  under `direnv exec /` at startup" with why it lost, and note that an allowed
  `/.envrc` would be evaluated by the startup probe.
- `docs/architecture.md`: refresh the TUI section (Effect list incl. `Notify`,
  `ApproveSetup`, `RunInTerminal`, `RunCommand` and any newer ones; the scopes of
  keys incl. `direct_next`/`direct_previous`; `Config` contents incl. notifications and
  commands) and the Status line at the top. Check every claim against the code.
- Do not touch `docs/superpowers/notes/2026-10-17-agent-apps-feature-research.md`
  (untracked, the owner's).

## Task 3: Dependencies

- Workspace `Cargo.toml`: `postcard = { version = …, default-features = false,
  features = ["use-std"] }` (keep the version), removing heapless/atomic-polyfill
  (RUSTSEC-2023-0089). Confirm with `cargo tree` and that the wire test still passes
  unchanged.
- Add a `deny.toml` (licenses allow-list matching what's compiled, advisories, sources:
  crates.io only plus the vendored path) and a `just deny` recipe running
  `cargo deny check`. Not part of `just check` (it needs the network). Run it once and
  fix or explicitly ignore (with reason) anything it reports.
- Remove `cargo-insta` from `flake.nix` and `*.snap.new` from `.gitignore` (unused).
- `crates/protocol/Cargo.toml`: drop the redundant `tokio` dev-dependency if it's
  already a normal dependency with the needed features. Pin `serde_json` like the other
  workspace deps.
- AGENTS.md / README if they mention removed tools.

## Task 4: TUI error handling

Findings from the hands-on UX review:
- A failed submit closes the form and loses the input. New-agent form, add-project form
  and rename (and any other form popup) must stay open on a daemon error, keep what was
  typed, and show the error inside the popup. The pending-request plumbing (e.g.
  `pending_select`, `request_failed`) is where replies are matched.
- Raw git errors are shown verbatim (`Preparing worktree (new branch 'x')fatal: a branch
  named 'x' already exists`, `fatal: not a git repository …`). Translate the common
  worktree-creation failures into short messages (branch exists, not a git repo, base
  not found, path exists) — prefer doing it in the daemon where the git call is, so the
  CLI benefits too — and keep the raw text available (e.g. after a colon or in the
  log).
- The Worktree option is offered for projects that aren't git repos. Hide or disable it
  (with a reason) for non-git projects. Find out where the TUI can learn that (project
  info, a git stat, or a cheap check) without a new string contract.
- Config warnings (including a TOML syntax error that makes the whole file ignored) show
  only in a 5-second status line that may be cut off. Show startup config warnings in a
  popup (or a sticky message) that stays until dismissed and lists every warning on its
  own line. Daemon errors from requests also deserve to stay readable: keep error
  messages visible longer than info messages or until the next key — pick and justify.
- Finish menu → "remove agent, worktree and branch" runs silently; show a confirmation
  message ("removed <name>, its worktree and branch").
- An empty filter result looks like an empty project ("press n to start an agent");
  say "no agents match /<filter> · esc clears".
- Update docs/keybindings.md / docs/configuration.md where behaviour they describe
  changes.

## Task 5: TUI discoverability

- The footer is cut off even at 200 columns (at 120: ends at `A all · g next input ·`),
  and it shows actions that don't apply (`x stop`, `r restart`, `R rename` with Projects
  focused or nothing selected; both `enter start` and `enter open` on a stopped agent).
  Make footer hints context-aware (only what applies to the current focus and
  selection), ranked by importance, and dropped from the end when they don't fit
  (never cut mid-hint). Help (`?`) stays the complete list.
- The help popup can't scroll: at 60x20 it cuts off with no "more" marker and the
  "Agent pane" section is unreachable; Down does nothing and PageDown closes it.
  Make it scroll (j/k, arrows, page keys, g/G) with above/below markers; don't let
  page keys close it.
- Long agent names hide status: the list row shows `▸ ● a-very-long-agent-name-f` with
  no status word, and the pane title loses `needs input` at 80 columns. Truncate the
  name (with an ellipsis) and keep the status visible in both places.
- First run: with no projects, every empty-state hint must say `a` (add a project), not
  `n`. The empty Projects box is drawn without side borders — fix the drawing.
- Help text says "remove / rename agent (in Projects: the project)" but `R` only renames
  agents — fix the wording. Hints mix `r` and `enter` for starting/restarting a stopped
  agent (app.rs ~1795, ~2856, ui.rs ~667) — pick one wording consistent with the
  status-bar hint (`enter start/restart`).
- `ctrl-q +` (full-screen) is missing from the help popup's Agent pane section and from
  the prefix hint after a stray key (`prefix_help`). Full-screen mode has no hint for
  getting back — add one.
- The Rename popup has no key hints, unlike every other popup. New-agent form labels
  don't line up. `terminal too small` doesn't say the minimum size. Once Claude sets a
  session title, the agent's name (which the CLI needs) appears nowhere — show it in
  the Info box.
- The diff cursor highlight replaces the line's green/red colour; keep the line colour
  visible on the cursor row.
- Fixed keys are written twice (handler match arms and hint strings, e.g. diff view,
  scroll mode, popups — app.rs ~1264-1306, ~1555-1577, ~2261-2287 vs ui.rs ~25-35,
  ~101, ~780-792). Where you touch hints in this task, drive them from one table per
  mode so they can't drift.
- Harness-check at 60x20, 80x24, 120x40 and 200x50. Update docs/keybindings.md.

## Task 6: Daemon correctness

- A setup script using `set -e` or `exit` stops the agent from starting
  (`crates/daemon/src/spawn.rs` ~55 pastes it into the wrapper `sh -c`). Run the setup
  in a subshell so the wrapper always reaches `status=$?` and `exec "$@"`. Test with a
  script that does `set -e; false`.
- `store::save` (`crates/daemon/src/store.rs` ~49) never fsyncs: `sync_all` the temp
  file before the rename and sync the directory after it. Also refuse to load an empty
  `state.toml` as an empty state (treat it like a corrupt file) — decide and test.
- The lock probe race: `lock_holder` (`crates/client/src/lib.rs` ~237) briefly takes the
  exclusive lock with `try_lock`; `InstanceLock::acquire` (`crates/daemon/src/lock.rs`
  ~25) tries only once, so a daemon starting during a probe exits "already running".
  Retry acquisition briefly (e.g. ~200 ms with short sleeps) or make the probe not take
  the lock. Test.
- A merge in flight doesn't mark the agent busy, so a remove can run during it. Mark it
  busy like the other lifecycle jobs (ADR 0019's policy) and test remove-during-merge
  is refused.
- Shutdown waits for in-flight creates with no time limit (`drain_jobs`); a hung
  checkout hook blocks the daemon forever and the test
  `shutdown_fits_in_the_clients_wait` (server.rs ~1981) no longer reflects the real
  worst case (stop → drain → second stop). Bound the drain so shutdown fits the
  client's `SHUTDOWN_WAIT`, and update the invariant test to the new worst case.
- A crash (or bounded shutdown) in the middle of a create leaves an unrecorded worktree
  and branch that block re-creating that name. Persist in-flight creates (path and
  branch) and discard their leftovers at the next daemon start. Test.
- No backpressure: each client's outgoing queue is an unbounded channel
  (`server.rs` ~130) fed at 30 Hz. Bound it; when full, drop frames for that client and
  mark its subscription so the next frame that goes through is a full frame. Never drop
  replies or non-frame events silently (decide: disconnect a client whose queue of
  non-frame messages overflows). Test with a client that stops reading.
- `.worktreeinclude` listing: individually ignored files outside wholly ignored
  directories are re-listed in batches of 500 by both passes (`git.rs` ~216); skip the
  second pass for them.
- Update docs/architecture.md and the relevant ADRs.

## Task 7: CLI polish

- `<AGENT>` arguments of kill/restart/rm/rename/send have no help text; document how an
  agent is named (id, `project/name`, a name unique across projects, or an id prefix of
  at least 4 hex characters; see `resolve_agent` in `crates/lazyagents/src/commands.rs`).
- Top-level `--help` should say running `lazyagents` with no command opens the TUI.
- `lazyagents new x` when the command (e.g. `claude`) is missing exits 1 with "failed
  to start `claude`…" without saying the agent was kept; retrying the same name then
  fails with "already exists". Say the agent was kept (and how to remove or restart it),
  and make `ls` print `last_error` for agents that have one.
- A state dir path too long for a unix socket fails only with "daemon exited during
  startup; see <log>". Check the socket path length in the client before spawning and
  say so directly. Test.
- `hook.rs` (~19-43) and `statusline.rs` (~89-110) duplicate reading the agent/socket
  environment and building the runtime; extract a shared helper.
- Update docs (README/architecture/protocol) where CLI behaviour is described.

## Task 8: Investigations

Research-first; implement only what the evidence supports, and write findings to
`docs/superpowers/notes/2026-10-18-<topic>.md`.
- Linux `just install`: `scripts/install.sh` only fixes nix-store linking on Darwin. On
  Linux, rustc in a nix shell typically sets the ELF interpreter (and rpath) to
  `/nix/store/...`. No Linux machine is available: determine from the nix toolchain and
  docs what the binary would link against, and make `install.sh` on Linux at least
  detect a `/nix/store` interpreter or rpath (`readelf`/`patchelf` from the dev shell)
  and fail with a clear message, or fix it with `patchelf` if that's reliable. Keep the
  Darwin path unchanged; test the detection logic in a way that runs on macOS (e.g. a
  function fed fixture `readelf` output).
- Status-line chaining before workspace trust: `lazyagents statusline` runs the repo's
  own `.claude/settings.json` `statusLine` command. Find out (Claude Code docs,
  changelog, behaviour of the installed `claude` without starting a real session if
  possible — e.g. its bundled code) whether Claude Code invokes a `--settings`
  statusLine before the user accepts the trust dialog. If it can, a freshly cloned repo
  gets code execution without approval: then only chain a project's status line once
  the folder is trusted (Claude records trust in `~/.claude.json` per project), or
  require the same approval as setup scripts. Implement the fix if the risk is real;
  otherwise record the evidence.

## Task 9: Refactor — selection by id and typed errors

- `crates/tui/src/app.rs`: the selected agent is stored as an index into the filtered,
  sorted `visible_agents()` (~440, ~579-602, ~1038), with `reselect` at ~8 call sites
  and `selected_agent()` re-sorting on each of ~86 calls. Make `selected:
  Option<AgentId>` the source of truth; derive the row only for rendering and j/k
  movement. Keep every existing selection test green and add one for a reorder that
  previously needed `reselect`.
- Typed errors instead of matching error text:
  - `crates/client`: a typed `ConnectionClosed` error (downcast like `VersionMismatch`)
    instead of `connection.rs` matching "closed the connection".
  - Dirty worktree: the daemon formats "worktree has uncommitted changes:\n…"
    (server.rs ~1015) and the TUI parses the file list from its own copy of the prefix
    (app.rs ~28, ~677). Replace with a typed reply (e.g. a `Response` variant or a
    structured error carrying the files). This changes the wire: regenerate the v6
    fixture per the Global constraints and update docs/protocol.md.
- `Focus::Diff`/`Focus::Scroll` beside `diff: Option<DiffView>` / `scroll:
  Option<ScrollMode>` (app.rs ~32, ~445-446) can disagree; make them
  `Focus::Diff(DiffView)` / `Focus::Scroll(ScrollMode)`. `help_filter` moves into
  `Popup::Help`; `message_kind` joins `message`.
- `app.rs` calls `crate::ui::setup_scroll_limits` (model depends on view); move the
  layout computation so the model doesn't depend on `ui`. `ui::label` is a model
  function used by app.rs — move it to the model (e.g. a method on `AgentInfo` or in
  app). `App::apply_diff` is used only by tests — remove or make it real. Replace
  positional-bool calls like `remove(*id, true, true)` with a small enum or named
  struct.

## Task 10: Refactor — split app.rs and run.rs

Mechanical moves only, after Task 9; behaviour must not change.
- Split `crates/tui/src/app.rs` into an `app/` module: `mod.rs` (App, server sync,
  selection), `diff.rs` (DiffView, diff and comment handlers), `commands.rs` (custom
  commands), `finish.rs` (finish menu), `forms.rs` (Preset, NewAgentForm, ProjectForm,
  picker, form keys, submit), `input.rs` (Selection, mouse and paste) — adjust to the
  code as it is by then. Each file carries its own `#[cfg(test)]` module with that
  area's tests.
- `crates/tui/src/run.rs` `run` (~242 lines): extract the version-mismatch recovery and
  an `apply_effect(ctx, effect) -> ControlFlow` so the effect handling is findable.
- Split the sidebar/pane `Action` enum into `SidebarAction` and `PaneAction` only if it
  falls out naturally; otherwise skip and say why.
- Update AGENTS.md's layout table and docs/architecture.md for the new module layout.
