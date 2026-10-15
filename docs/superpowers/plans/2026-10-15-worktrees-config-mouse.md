# Worktrees, Config, Keys and Mouse Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Finish the spec's interface. That covers git worktrees per agent, with branch and diffstat in Info and a `D` diff view. It also covers:
- a `ctrl-q` prefix for pane keys, chosen for Norwegian Mac layouts
- `config.toml` for keybindings, presets and theme
- keyboard scroll mode
- mouse click-to-focus and drag-select with copy on release
- the version-mismatch popup
- spawn errors shown in the pane
- the minors parked from plan 3

**Architecture:**
- **Protocol:** moves to version 2, with worktree and git-stat messages.
- **Daemon:** gains a small `git` module that shells out to the git CLI. Read-only git queries answer asynchronously, so the loop never blocks on a large diff.
- **TUI:**
  - A configurable keymap: an action enum with key parsing and defaults, plus a prefix state machine for the pane.
  - A config loader.
  - Mouse handling built on the existing `App` model and effects pattern. Clicks and drags become model events; copy becomes an `Effect`.
  - Focus still moves into a pane only on an explicit user action. A click on the pane counts as one.

**Tech Stack:** Rust 2024; the existing workspace crates; `toml` 1.1.8 (now also a tui dependency); `base64` 0.23.1 (new, for OSC 52); git 2.55 via the CLI; rustix 1.1.5 for SIGTERM to a mismatched daemon.

**Spec:** `docs/superpowers/specs/2026-10-15-lazyagents-design.md` (§2.5 worktrees, §2.6 config, §3 UI, §4 errors). User decisions recorded during planning (ADR 0007 in Task 10):
- **Pane keys:** a `ctrl-q` prefix replaces `[`/`]` keys, which are hard to reach on a Norwegian Mac layout:
  - `ctrl-q j`/`k`: next or previous agent, staying in the pane
  - `ctrl-q q`/`esc`: back to the sidebar
  - `ctrl-q ctrl-q`: sends `ctrl-q` to the agent
  - `ctrl-q s`: scroll mode
- **Worktrees:** off by default in the new-agent form.
- **Copy:** mouse drag selects, and release copies through OSC 52 with `pbcopy` as a fallback.
- **Clicks move focus:** on a project row, an agent row or the pane.
- **Target terminals:** Ghostty, kitty or WezTerm, often inside tmux.

## Global Constraints

- All tooling comes from the nix dev shell: run `nix develop -c <cmd>` for every cargo, just or git-commit command.
- Rust edition 2024. `unsafe_code = "forbid"`, and clippy runs with `-D warnings`.
- Pinned workspace dependencies only. `Cargo.lock` is committed, and `just` recipes use `--locked`.
- Conventional Commits. Types: `feat fix docs refactor test chore build ci perf`. Scopes: `protocol daemon tui cli client docs nix repo`. The first line is at most 72 characters; count it.
- Comments: none by default. Docs describe the system as it is.
- Tests and dev runs use a short `mktemp -d` `LAZYAGENTS_STATE_DIR`, because the socket path limit is about 104 bytes. Tests never touch `~/.local/state/lazyagents` or `~/.config/lazyagents`; set `LAZYAGENTS_CONFIG` to a temp file.
- No test leaves a daemon, agent or TUI running. Kill only exact pids; never kill by pattern.
- Git tests create their own temp repos with `git init` and set `user.name`/`user.email` locally. They never touch the user's repos or global git config.
- The daemon never deletes a branch. It removes a worktree only on an explicit request, and refuses when the worktree has uncommitted changes unless the request sets `force`.
- Focus enters a pane only through an explicit user action: enter, a click on the pane, the just-submitted create, or `ctrl-q j`/`k`. It never enters a non-running agent's pane.
- The terminal must be restored on every exit, and the TUI must exit when its terminal disappears. The existing hangup script must keep passing.
- If plan code fails against a real API, make the minimal correction and record it. Test expectations do not change.

## Checkpoints

- **A** (after Task 3): protocol v2 and daemon worktrees are green, against real temp git repos.
- **B** (after Task 7): keys, config, worktree UI, diff view and scroll mode are unit-green.
- **C** (after Task 10): mouse, mismatch popup, docs and end-to-end scripts (smoke, hangup, worktree smoke) are green, with a leak scan clean.

## Review Focus

1. **Destroying user work.** Removing a worktree with uncommitted changes must refuse without `force`, and the refusal must list the files. Branches are never deleted. Tests: Task 3 `remove_refuses_a_dirty_worktree_and_lists_files`, `branches_survive_worktree_removal`.
2. **Git ref rules.** Agent names that are valid for lazyagents but invalid as git refs (`foo.lock`, `a..b`) must be refused before anything is created. Test: Task 3 `invalid_branch_names_create_nothing`.
3. **The `ctrl-q` prefix never leaks or traps keys.** An unknown key after `ctrl-q` exits the prefix without sending anything. `ctrl-q ctrl-q` sends exactly one `ctrl-q`. A prefix pending while the agent exits does not keep focus. Tests: Task 4 `prefix_*`.
4. **A click is an explicit action, but only for the pane under the cursor.** A click on a row selects and focuses that list and never focuses the pane. A click on a non-running agent's pane does not focus it. Tests: Task 9 `click_*`.
5. **Large diffs must not freeze the daemon or the TUI.** Git queries run off the daemon loop, and the diff view caps its text. Tests: Task 3 `git_queries_answer_while_the_loop_keeps_serving`, Task 6 `diff_view_caps_huge_diffs`.

---

## File Structure

```
crates/protocol/src/{types.rs,message.rs,lib.rs}   v2: worktree, last_error, GitStat, RemoveAgent flags
crates/daemon/src/git.rs                           git CLI: current_branch, check_branch, add_worktree, dirty_files, remove_worktree, stat, diff
crates/daemon/src/{server.rs,store.rs,reap.rs,status.rs}
crates/daemon/tests/{server.rs,git.rs}
crates/client/src/lib.rs                           stop_daemon (SIGTERM the lock holder)
crates/tui/src/keymap.rs                           Action, KeySpec parse, defaults, lookup
crates/tui/src/config.rs                           Config { keys, presets, theme }, load with warnings
crates/tui/src/{app.rs,ui.rs,run.rs,keys.rs}
crates/tui/src/clipboard.rs                        OSC 52 encoding + pbcopy fallback
crates/lazyagents/src/main.rs
scripts/smoke-tui.sh, scripts/smoke-worktree.sh
docs/keybindings.md, docs/architecture.md, docs/protocol.md, README.md, AGENTS.md, docs/decisions/0007-ctrl-q-pane-prefix.md
```

---

### Task 1: Parked minors from plan 3

**Files:** `crates/daemon/src/{server.rs,status.rs,reap.rs}`, `crates/tui/src/{app.rs,ui.rs,run.rs}`, with tests in those modules and `crates/daemon/tests/server.rs`

**Interfaces:** no new public API.

Fix each item below test-first.
- **Daemon:**
  1. Reaping when an agent exits on its own runs off the event loop. Apply `terminate` on a `std::thread::Builder::spawn` thread. If spawning fails, log the error and reap inline. The snapshot thread also uses `Builder::spawn`, with the same inline fallback. Test `exit_reaping_does_not_block_requests`: an agent whose `nohup`'d child ignores TERM exits; while it is reaped, a `ListAgents` request answers in under 300 ms.
  2. A hook that leaves the status unchanged still refreshes `status_since` for Claude agents, because it counts as activity. Unit test in `status.rs`, or a server test: `PreToolUse` while already Working, 9 s apart, keeps Working.
  3. A Claude agent with no hook events and no output for 30 s settles `Starting` → `Idle`. Add `status::CLAUDE_STARTUP_SILENCE = 30 s` and a `settle` unit test.
- **TUI:**
  4. In the All view, when `ProjectsChanged` removes the highlighted project, the highlight snaps to the selected agent's project. If no agent is selected, it clamps. Test.
  5. While "terminal too small" is shown, keys are ignored except `q`, `ctrl-c` and `ctrl-q q`. Test with `set_pane_size` below the floor, then `on_key('x')` gives no effect and no popup.
  6. A transient message no longer hides the hint line. The status bar shows `<message> │ <hint>`, truncated from the right. The message is in red. ui test.

Run `nix develop -c just check`, then commit:

```bash
nix develop -c git commit -m "fix: settle parked lifecycle, status and tui edge cases"
```

---

### Task 2: Protocol v2 and stored worktrees

**Files:** `crates/protocol/src/{types.rs,message.rs,lib.rs}`, `crates/daemon/src/store.rs`, plus every crate that constructs these types (fix the call sites)

**Interfaces (protocol, re-exported):**
- `PROTOCOL_VERSION = 2`
- `WorktreeInfo { path: PathBuf, branch: String, base: String }`
- `AgentInfo` gains `worktree: Option<WorktreeInfo>` and `last_error: Option<String>`.
- `CreateAgent` gains `worktree: Option<WorktreeRequest>`, where `WorktreeRequest { base: Option<String> }`. `None` base means the project's current branch.
- `Request::RemoveAgent { id, remove_worktree: bool, force: bool }`
- `Request::GitStat { id }` → `Response::GitStat(GitStat)`, where `GitStat { insertions: u32, deletions: u32, files: u32, uncommitted: u32 }`
- `Request::GitDiff { id }` → `Response::Diff(String)`
- `AgentRecord` (store) gains `#[serde(default, skip_serializing_if = "Option::is_none")] worktree: Option<WorktreeInfo>`. An old `state.toml` without it loads unchanged.

- [ ] **Step 1: Failing tests**
  - Extend `message.rs` round-trip tests to cover every new and changed variant.
  - `store.rs` test `state_without_worktrees_still_loads`: a literal TOML string in the plan-2 format parses, and `worktree` is `None`.
  - `store.rs` test `worktree_round_trips`.
  - Run: `nix develop -c cargo nextest run -p protocol -p daemon`. Expected: compile errors, which are the RED here.
- [ ] **Step 2: Implement**
  - Add the types and fields. Update every constructor in daemon, client, tui and lazyagents.
  - The daemon fills `worktree` and `last_error`, using `None` for now. Task 3 sets them.
  - Answer `GitStat` and `GitDiff` with `Err("not implemented")` until Task 3.
  - `RemoveAgent` keeps its current behaviour; Task 3 wires `remove_worktree` and `force`.
  - The CLI `rm` sends `remove_worktree: false, force: false`.
  - The TUI sends the same until Task 5.
  - Make `last_error` hold the spawn error string when a start fails (Exited(127)), and clear it on a successful start. Test `spawn_failure_reports_last_error`.
- [ ] **Step 3:** Run `nix develop -c just check`, then commit: `feat(protocol): add worktrees, git stats and diffs as version 2`.

---

### Task 3: Daemon worktrees and git queries

**Files:** create `crates/daemon/src/git.rs`; modify `crates/daemon/src/{lib.rs,server.rs}`; create `crates/daemon/tests/git.rs`; extend `crates/daemon/tests/server.rs`

**Interfaces (`daemon::git`, all synchronous; the server runs the read-only ones off-loop):**

```rust
pub fn current_branch(repo: &Path) -> anyhow::Result<String>;            // git -C repo rev-parse --abbrev-ref HEAD
pub fn check_branch(name: &str) -> anyhow::Result<()>;                   // git check-ref-format --branch name
pub fn add_worktree(repo: &Path, path: &Path, branch: &str, base: &str) -> anyhow::Result<()>; // git -C repo worktree add -b branch path base
pub fn dirty_files(worktree: &Path) -> anyhow::Result<Vec<String>>;     // git -C wt status --porcelain → paths
pub fn remove_worktree(repo: &Path, path: &Path, force: bool) -> anyhow::Result<()>; // git -C repo worktree remove [--force] path
pub fn stat(dir: &Path, base: Option<&str>) -> anyhow::Result<protocol::GitStat>;
pub fn diff(dir: &Path, base: Option<&str>) -> anyhow::Result<String>;
```

- **`stat` and `diff`:**
  - With a base, they compare the working tree against `git merge-base <base> HEAD`, which covers committed and uncommitted changes. They use `git diff --numstat <mb>` for stat and `git diff --no-color <mb>` for diff.
  - Without a base (non-worktree agents), they compare against `HEAD`, which shows uncommitted changes only.
  - `uncommitted` is `dirty_files(dir).len()`.
  - If `dir` is not a git repo, both return an error such as `not a git repository`.
- **Errors:** every function returns git's stderr in the error when the command fails. A spawn error mentions git.
- **Server behaviour:**
  - **`CreateAgent` with `worktree: Some(req)`:**
    - It requires the project to be a git repo.
    - branch = `lazyagents/<name>`, checked with `check_branch`.
    - base = `req.base` or `current_branch(project)`.
    - path = `<state>/worktrees/<project-name>-<first 8 of project id>/<agent name>`.
    - After `add_worktree`, it sets the record's cwd to the path and stores `WorktreeInfo`.
    - Any failure returns `Err` and leaves no record, branch or directory behind. If a step after `add_worktree` fails, remove the worktree again.
  - **`RemoveAgent`:**
    - If `remove_worktree` is set and the agent has a worktree, call `dirty_files` first.
    - If it is non-empty and `force` is false, return `Err("worktree has uncommitted changes:\n<one path per line>")`. Nothing is stopped or removed.
    - Otherwise stop the agent, call `remove_worktree(project, path, force)`, then remove the record.
    - If `remove_worktree` is not set, the worktree stays on disk.
    - The branch is never deleted.
  - **`RenameAgent`:** keeps the agent's branch and worktree path unchanged. This is documented, and the info still shows the original branch.
  - **`GitStat` and `GitDiff`:** run on a blocking thread with the client's sender and the request id, and send `ServerMessage::Response` when done. This needs a deferred-response path in `handle_command`: the request handler returns `Deferred`, and no immediate response is sent. A daemon shutdown while a query runs simply drops the late answer. Unknown agents still get an immediate `Err`.

- [ ] **Step 1: Failing tests**
  - `tests/git.rs`, using a helper `repo()`: tempdir, `git init -b main`, local user config, one committed file.
    - `current_branch_reports_main`
    - `check_branch_rejects_lock_and_double_dot` (`foo.lock`, `a..b`)
    - `worktree_lifecycle` (add, dirty after a write, stat counts, diff contains the line, remove refused without force while dirty, removed with force, branch still listed in `git branch --list`)
    - `stat_without_base_counts_uncommitted_only`
    - `non_repo_errors_mention_git_repository`
  - `tests/server.rs`:
    - `worktree_agent_runs_in_its_worktree`: the agent `sh -c 'pwd; sleep 30'` shows the worktree path, and `AgentInfo.worktree` has branch `lazyagents/<name>` and base `main`.
    - `invalid_branch_names_create_nothing`: the name `foo.lock`; afterwards no record, no branch and no directory.
    - `worktree_in_non_git_project_is_an_error`
    - `remove_refuses_a_dirty_worktree_and_lists_files`
    - `remove_with_force_removes_the_worktree`
    - `branches_survive_worktree_removal`
    - `remove_without_remove_worktree_keeps_it`
    - `git_queries_answer_while_the_loop_keeps_serving`: a `GitDiff` for a worktree with a 5 MB file change, plus a concurrent `ListAgents` that answers first or within 300 ms.
    - `git_stat_and_diff_for_plain_agents_show_uncommitted_changes`
  - Run: `nix develop -c cargo nextest run -p daemon`. Expected: RED (compile errors, then failures).
- [ ] **Step 2: Implement** as specified. Run every git command through `std::process::Command::new("git")` with `.env("GIT_TERMINAL_PROMPT", "0")`.
- [ ] **Step 3:** Run `nix develop -c just check`, and run the server tests 2 more times with `--retries 0`. Then commit: `feat(daemon): create, inspect and remove git worktrees for agents`.

- [ ] **Checkpoint A:**
  - `nix develop -c just check` is green; paste the summary.
  - `pgrep -fl "lazyagents daemon|sleep 30"` shows nothing from these runs.
  - `git worktree list` in the repo shows no stray worktrees.

---

### Task 4: Keymap with the `ctrl-q` prefix

**Files:** create `crates/tui/src/keymap.rs`; modify `crates/tui/src/{lib.rs,app.rs,keys.rs}`

**Interfaces:**

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Deserialize)] #[serde(rename_all = "snake_case")]
pub enum Action {
    // sidebar
    Quit, StopDaemon, Help, Down, Up, FocusProjects, FocusAgents, ToggleFocus, Open, NewAgent, AddProject,
    Stop, Restart, Remove, Rename, NextAttention, ToggleAllAgents, Diff, Scroll,
    // pane (after the prefix)
    PaneNext, PanePrevious, PaneLeave, PaneScroll, PaneSendPrefix,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct KeySpec { pub code: ratatui::crossterm::event::KeyCode, pub ctrl: bool, pub alt: bool, pub shift: bool }
impl KeySpec { pub fn parse(s: &str) -> Result<KeySpec, String>; pub fn matches(&self, key: &KeyEvent) -> bool; }
pub struct Keymap { /* sidebar: Vec<(KeySpec, Action)>, prefix: KeySpec, pane: Vec<(KeySpec, Action)>, leave: Vec<KeySpec> */ }
impl Keymap {
    pub fn defaults() -> Keymap;
    pub fn sidebar(&self, key: &KeyEvent) -> Option<Action>;
    pub fn is_prefix(&self, key: &KeyEvent) -> bool;
    pub fn after_prefix(&self, key: &KeyEvent) -> Option<Action>;
    pub fn is_direct_leave(&self, key: &KeyEvent) -> bool;   // ctrl-] by default (kept as an alias)
    pub fn describe(&self, action: Action) -> String;        // e.g. "ctrl-q j", used by help and hints
}
```

- **Key strings:**
  - Modifiers are joined with `-`: `ctrl-q`, `alt-j`, `shift-tab`.
  - Named keys: `enter`, `esc`, `tab`, `backtab`, `backspace`, `up`, `down`, `left`, `right`, `home`, `end`, `pageup`, `pagedown`, `space`, `f1`–`f12`.
  - Otherwise a single character, which is case-sensitive: `A` means shift+a. Crossterm reports `Char('A')` with or without SHIFT, so `matches` ignores SHIFT for character keys.
- **Defaults:**
  - Sidebar: exactly today's bindings, plus `D` (Diff) and `s` (Scroll).
  - Prefix: `ctrl-q`.
  - After the prefix: `j` PaneNext, `k` PanePrevious, `q`/`esc` PaneLeave, `s` PaneScroll, `ctrl-q` PaneSendPrefix.
  - Direct leave: `ctrl-]`, with legacy `ctrl-5`.
  - `alt-[`/`alt-]` are not bound by default.
- **`App`:**
  - Gains `keymap: Keymap` (passed into `App::new`) and `prefix_pending: bool`.
  - Pane key handling:
    - With the prefix pending, the next key resolves through `after_prefix`. An unknown key clears the prefix with the message `ctrl-q: j/k switch · q sidebar · s scroll · ctrl-q send` and sends nothing.
    - A prefix key sets `prefix_pending` and sends nothing.
    - A direct leave goes to the sidebar.
    - Anything else is input to the agent.
  - `PaneSendPrefix` sends exactly one `ctrl-q` to the agent.
  - `PaneNext`/`PanePrevious` cycle within the visible list and stay in the pane, but only onto a running agent. A non-running one drops focus to the sidebar, as `sync` already does.
  - `prefix_pending` clears whenever focus leaves the pane.
  - The status bar shows `ctrl-q …` while the prefix is pending.
- `keys.rs::is_detach` and `cycle_step` are replaced by the keymap. Delete them and their tests, and port the tests' intent into keymap tests.

- [ ] **Step 1: Failing tests**
  - keymap: parse table (valid and invalid strings), `matches` with case and shift, defaults contain the decided bindings, `describe`.
  - app:
    - `prefix_then_j_switches_and_stays_in_the_pane`
    - `prefix_then_q_and_esc_leave`
    - `prefix_twice_sends_one_ctrl_q`
    - `prefix_then_unknown_sends_nothing`
    - `prefix_clears_when_the_agent_exits`
    - `ctrl_bracket_still_leaves`
    - `alt_brackets_are_plain_input_by_default`
    (`ctrl-q s` → scroll mode is tested in Task 7, where `Focus::Scroll` exists.)
- [ ] **Step 2: Implement.** Update the existing app tests that used `alt-[`/`alt-]` or `keys::is_detach` so they use the prefix. The expectations change only where the keys themselves changed.
- [ ] **Step 3:** Run `nix develop -c just check`; the smoke script uses `C-]`, which stays valid. Commit: `feat(tui): add a configurable keymap with a ctrl-q pane prefix`.

---

### Task 5: Config file

**Files:** create `crates/tui/src/config.rs`; modify `crates/tui/{Cargo.toml,src/lib.rs,src/app.rs,src/ui.rs,src/run.rs}`

**Interfaces:**

```rust
pub struct Config { pub keymap: Keymap, pub presets: Vec<PresetDef>, pub theme: Theme, pub warnings: Vec<String> }
pub struct PresetDef { pub name: String, pub kind: AgentKind, pub command: Vec<String> }
pub struct Theme { pub accent: Color, pub attention: Color, pub working: Color, pub idle: Color, pub dim: Color, pub border: Color }
pub fn load(path: Option<&Path>) -> Config; // never fails: missing file → defaults; bad entries → defaults + warnings
pub fn default_path() -> Option<PathBuf>;   // $LAZYAGENTS_CONFIG, else $XDG_CONFIG_HOME/lazyagents/config.toml, else ~/.config/lazyagents/config.toml
```

The file format, documented in `docs/configuration.md` (Task 10):

```toml
[keys.sidebar]          # action = "key" or ["key", "key"]
new_agent = "n"
toggle_all_agents = "A"

[keys.pane]
prefix = "ctrl-q"
next = "j"
previous = "k"
leave = ["q", "esc"]
scroll = "s"
send_prefix = "ctrl-q"
direct_leave = ["ctrl-]"]

[[presets]]
name = "codex"
command = ["codex"]

[theme]
accent = "green"        # named colors, "#rrggbb", or 0-255
attention = "red"
```

- **Rules:**
  - Each configured action replaces that action's default keys; actions not mentioned keep their defaults.
  - Unknown tables, actions or keys produce a warning naming the item, and are ignored.
  - Duplicate bindings within a scope produce a warning; the last one wins.
  - Presets are appended to the built-in Claude, Shell and Custom.
  - The new-agent form's preset row cycles over all of them.
- **The TUI:**
  - Loads config at startup.
  - Shows the warnings, joined with `; `, as the startup message.
  - Uses `theme` for every colour `ui.rs` hard-codes: the focus border (accent), the needs-input row and badge (attention), the spinner (working), the idle checkmark, and dim.
  - Uses `keymap.describe` in the help popup and the hints, so they reflect the configured keys.

- [ ] **Step 1: Failing tests** in `config.rs`:
  - `missing_file_gives_defaults`
  - `keys_override_only_named_actions`
  - `unknown_entries_warn`
  - `bad_key_strings_warn_and_keep_defaults`
  - `presets_append_after_builtins`
  - `theme_parses_names_hex_and_indexed`
  - `default_path_prefers_env_then_xdg`, a pure function over passed-in env values, as `StatePaths::resolve` does.

  ui tests:
  - `help_reflects_configured_keys`
  - `theme_colours_apply`

  app test:
  - `custom_preset_creates_with_its_command`
- [ ] **Step 2: Implement.** Add `toml.workspace = true` and `serde.workspace = true` to tui.
- [ ] **Step 3:** Run `nix develop -c just check`, then commit: `feat(tui): load keybindings, presets and theme from config.toml`.

---

### Task 6: Worktree UI, Info stats, spawn errors and the diff view

**Files:** `crates/tui/src/{app.rs,ui.rs}`

**Behaviour:**
- **New-agent form:**
  - It gains a `Worktree: [ ]` field after Preset; space, ← and → toggle it. When the toggle is on, a `Base: <input>` field appears, and an empty base means the project's current branch.
  - The field order cycles over the visible fields.
  - When the toggle is on, `CreateAgent.worktree` is `Some(WorktreeRequest { base })`.
- **Info panel:**
  - For a worktree agent it shows `branch: lazyagents/<name>`, then `+I −D · F files`, then `U uncommitted` once a `GitStat` has arrived.
  - For a plain agent it shows the stat line only when the cwd is a git repo, which means the `GitStat` succeeded.
  - The app sends `GitStat { id }` when the selection changes and every 5 s while the agent stays selected. The runtime tick drives this through an `App::on_tick(now) -> Vec<Effect>`.
  - `GitStat` errors are silent; no message is shown.
- **Remove:**
  - For a worktree agent, `d` opens `Remove <name>? y: agent + worktree · k: keep worktree · n: cancel`.
  - `y` sends `remove_worktree: true, force: false`, and `k` sends `remove_worktree: false`.
  - If the reply is `Err` starting with `worktree has uncommitted changes`, a second Confirm shows `Worktree has uncommitted changes:` plus the file list (at most 8 lines, then `… and N more`) and `Remove anyway? y/n`. `y` resends with `force: true`.
  - This needs the app to remember the last remove request. Clear it on any other response.
- **Spawn errors:** a selected agent with no terminal and a `last_error` shows the pane text `failed to start: <error>` and `press r to retry`, in place of the empty screen.
- **Diff view (`D`):**
  - New `Focus::Diff`. `D` on a selected agent sends `GitDiff` and shows `loading diff…`; on `Response::Diff(text)` it renders the text in the main pane.
  - Lines starting with `+` are green, `-` red, `@@` cyan, and `diff`/`index`/`---`/`+++` bold.
  - Keys:
    - `j`/`k`: ±1 line
    - `ctrl-d`/`ctrl-u`: ±half a page
    - `pagedown`/`pageup`: ±1 page
    - `g`/`G`: top and bottom
    - `D`, `q` or `esc`: close, back to Agents
  - The text is capped at 20 000 lines, followed by a final line `… diff truncated (N more lines)`.
  - `Err` from `GitDiff` closes the view with the message.

- [ ] **Step 1: Failing tests** in app and ui:
  - `form_worktree_toggle_and_base_field`
  - `form_sends_worktree_request_when_on`
  - `form_worktree_is_off_by_default`
  - `stat_requested_on_select_and_every_5s`
  - `info_shows_branch_and_stats`
  - `remove_worktree_agent_offers_keep_or_remove`
  - `dirty_worktree_error_asks_to_force`
  - `spawn_error_shows_in_pane`
  - `diff_view_opens_scrolls_and_closes`
  - `diff_view_colours_lines`
  - `diff_view_caps_huge_diffs`
  - `diff_error_closes_with_message`
- [ ] **Step 2: Implement.**
- [ ] **Step 3:** Run `nix develop -c just check`, then commit: `feat(tui): manage worktrees and show git stats and diffs`.

---

### Task 7: Keyboard scroll mode

**Files:** `crates/tui/src/{app.rs,ui.rs}`

**Behaviour:**
- `Focus::Scroll` is entered with `s` in the sidebar or `ctrl-q s` in the pane, on a selected agent that has a terminal (running or exited).
- Keys:
  - `j`/`k`: `Scroll { lines: -1 / +1 }`, where positive means back in history, as the daemon defines it.
  - `ctrl-u`/`ctrl-d`: ±half the pane height.
  - `pageup`/`pagedown`: ±a full page.
  - `g`: `Scroll { lines: i32::MAX / 2 }`, to the top.
  - `G`: `Scroll { lines: i32::MIN / 2 }`, to the bottom.
  - `q` or `esc`: sends `Scroll { lines: i32::MIN / 2 }` and returns to where you came from (pane or Agents).
- While in scroll mode, the pane border is yellow and the status shows `SCROLL · j/k ctrl-u/d g/G · q exit`.
- Mouse wheel behaviour is unchanged.

- [ ] **Step 1: Failing tests:**
  - `scroll_mode_from_sidebar_and_prefix`
  - `scroll_keys_send_scroll_requests`
  - `scroll_exit_returns_to_origin_and_bottoms`
  - `scroll_needs_a_terminal`: a stopped agent gives the message `nothing to scroll`.
- [ ] **Step 2: Implement.**
- [ ] **Step 3:** Run `nix develop -c just check`, then commit: `feat(tui): scroll agent history from the keyboard`.

- [ ] **Checkpoint B:** `nix develop -c cargo nextest run -p tui`; paste the summary.

---

### Task 8: Version-mismatch popup

**Files:** `crates/client/src/lib.rs`, `crates/tui/src/run.rs`, `crates/lazyagents/tests/cli.rs`

**Interfaces:**
- `client::stop_daemon(paths: &StatePaths, timeout: Duration) -> anyhow::Result<()>`:
  - Reads the pid with `daemon_pid`, which only trusts a held lock, and SIGTERMs it with rustix.
  - Waits until the socket file is gone, or errors after `timeout`.
  - No daemon running → `Ok(())`.
- **TUI startup:** on `VersionMismatch` from `connect_or_spawn`:
  1. Enter the terminal guard.
  2. Draw a full-screen popup: `This lazyagents (protocol {ours}) can't talk to the running daemon {daemon_version} (protocol {theirs}).` with `Restart the daemon? This stops every running agent.` and `y restart · n quit`.
  3. `y` → `stop_daemon(paths, 5 s)`, then `connect_or_spawn` again, then continue normally.
  4. `n` / `q` / `esc` → leave and return the original error.

  This is a small loop in `run.rs` before the main loop. It reuses the input thread and draws with a dedicated `ui::draw_mismatch`.
- `lazyagents shutdown` on a mismatch uses `stop_daemon`. On success it prints `daemon stopped`, replacing the old `kill <pid>` hint.

- [ ] **Step 1: Failing tests:**
  - client `stop_daemon_without_a_daemon_is_ok`
  - CLI `stop_daemon_stops_a_running_daemon`: start a real `lazyagents daemon` child process in a temp state dir, wait for its socket, call `client::stop_daemon(&paths, 5 s)`, and assert that the socket is gone and the child exited, using `try_wait` within 5 s. On failure, kill the child by its pid.
  - ui `mismatch_popup_text`
- [ ] **Step 2: Implement.**
- [ ] **Step 3:** Run `nix develop -c just check`, then commit: `feat(tui): offer to restart a daemon that speaks another protocol`.

---

### Task 9: Mouse: click to focus, drag to select, copy on release

**Files:** create `crates/tui/src/clipboard.rs`; modify `crates/tui/{Cargo.toml,src/app.rs,src/ui.rs,src/run.rs}`; add `base64 = "0.23.1"` to the workspace deps.

**Interfaces:**
- `ui::row_at(area: Rect, selected: usize, len: usize, y: u16) -> Option<usize>`: maps a click on a bordered list to an item index. ratatui scrolls a list so the selected item is visible from offset 0, so `offset = selected.saturating_sub(inner_height - 1)`.
- `App::on_mouse(event: MouseInput, areas: &Areas) -> Vec<Effect>`, where `MouseInput { kind: Down | Drag | Up | ScrollUp | ScrollDown, col, row }` uses absolute terminal coordinates. `run.rs` forwards Left Down, Drag and Up as well as wheel events.
- `Effect::Copy(String)`.
- `clipboard::osc52(text: &str) -> String` returns `"\x1b]52;c;<base64>\x07"`.
- `clipboard::copy(text: &str)` writes `osc52` to stdout. If `pbcopy` (macOS) or `wl-copy` (Wayland) is on `PATH`, it also pipes the text to it, ignoring errors. Inside tmux, OSC 52 needs tmux `set-clipboard on`; note this in the docs.
- **Behaviour:**
  - **Down on a Projects row:** select that project and focus Projects.
  - **Down on an Agents row:** select that agent and focus Agents. It never focuses the pane.
  - **Down inside `pane_inner` with an agent selected:** start a selection anchored at that cell. If the agent is running, focus the pane, since a click is an explicit action. If not, keep focus and show the message `<name> is not running — r restarts`.
  - **Drag:** extend the selection, clamped to `pane_inner`.
  - **Up:**
    - If the anchor equals the end, it was a plain click: clear the selection.
    - Otherwise emit `Effect::Copy(selected text)`, set the message `copied N characters`, and clear the selection.
  - **Down outside an open popup's rect:** close the popup.
  - **Wheel:** unchanged.
- **Selected text:**
  - Rows are read from the app's mirrored `screen`, line-wise from anchor to end, in reading order.
  - The first row starts at the anchor column and the last row ends at the end column.
  - Wide-character spacer cells (empty text) are skipped, and trailing spaces on each line are trimmed.
  - Lines are joined with `\n`.
- **Rendering:** the selection is drawn reversed on top of the screen.

- [ ] **Step 1: Failing tests:**
  - ui: `row_at_maps_rows_with_scrolling`
  - clipboard: `osc52_encodes`
  - app:
    - `click_project_row_selects_and_focuses_projects`
    - `click_agent_row_selects_and_focuses_agents_not_pane`
    - `click_pane_focuses_running_agent`
    - `click_pane_on_stopped_agent_keeps_focus`
    - `drag_selects_and_release_copies_text`, including a multi-line selection with a wide character and trailing spaces
    - `plain_click_copies_nothing`
    - `click_outside_popup_closes_it`
    - `drag_is_clamped_to_the_pane`
- [ ] **Step 2: Implement.** Run each test RED first.
- [ ] **Step 3:** Run `nix develop -c just check`, then commit: `feat(tui): focus by click and copy pane text by dragging`.

---

### Task 10: Docs, ADR, end-to-end scripts

**Files:**
- Create `scripts/smoke-worktree.sh`, `docs/configuration.md` and `docs/decisions/0007-ctrl-q-pane-prefix.md`.
- Modify `scripts/smoke-tui.sh` (use `C-q q` to leave the pane, alongside the existing `C-]` step), `justfile` (add `smoke-worktree` to `check`), `docs/keybindings.md`, `docs/architecture.md`, `docs/protocol.md` (v2), `README.md`, `AGENTS.md` and `.claude/skills/running-dev/SKILL.md`.

- **`smoke-worktree.sh`**, through the private tmux harness:
  1. Make a temp git repo (`git init -b main`, local user config, one commit) as the project.
  2. Launch the TUI there with a temp state dir and a temp `LAZYAGENTS_CONFIG` file. Use `SHELL=/bin/sh`.
  3. Press `a` and enter, then wait for `▸ <repo name>`.
  4. Press `n`, type `wt`, then tab, → (Shell), tab, space (Worktree on), enter. Wait for ` wt · generic `.
  5. Type `pwd` and enter, then wait for `/worktrees/`.
  6. Type `echo hi > f.txt` and enter.
  7. Press `C-q q` and wait for the sidebar hint. Within 10 s, wait for `branch: lazyagents/wt` and `1 uncommitted`.
  8. Press `D` and wait for `+hi`, then `q`.
  9. Press `d`, wait for `y: agent + worktree`, press `y`, and wait for `Worktree has uncommitted changes`. Press `y` and wait for `no agents — press n`.
  10. Assert the worktree dir is gone and `git -C repo branch --list lazyagents/wt` still prints the branch.
  11. Quit, then run `lazyagents shutdown`.
  12. Clean everything up in an `EXIT` trap. Print `worktree smoke passed`.
- **ADR 0007:** context (Norwegian Mac layouts make `[`/`]` hard, and legacy terminals swallow the key after `alt-[`), decision (the `ctrl-q` prefix and its sub-keys, `ctrl-]` kept as an alias), alternatives (alt-j/k, ctrl-, and ctrl-., F-keys), consequences (agents never see `ctrl-q` unless it is pressed twice).
- **`docs/configuration.md`:** the full format, defaults, the colour syntax, warnings, and the tmux `set-clipboard on` note.
- **Other docs:** keybindings (prefix, `D`, `s`, mouse), architecture (worktrees, deferred git queries, keymap and config, mouse), protocol v2, README usage, AGENTS commands. The running-dev skill gains `C-q` usage.

Run `nix develop -c just check`, which now includes `smoke-tui`, `test-tui-hangup` and `smoke-worktree`. Then commit: `docs: document worktrees, config, keys and mouse` (or split into a `test(repo)` commit for the scripts).

- [ ] **Checkpoint C:**
  1. `nix develop -c just check` is green: paste the nextest summary and all script lines.
  2. Run `smoke-worktree.sh` and `test-tui-hangup.sh` 2 more times each.
  3. `pgrep -fl "lazyagents|sleep 30"` shows nothing from these runs.
  4. `git status --short` is empty.
  5. `git worktree list` is clean.
  6. The log shows all tasks as Conventional commits.
