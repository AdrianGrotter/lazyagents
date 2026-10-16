# direnv Auto-Allow and Process Hardening Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Worktree agents start in direnv repos without a manual `direnv allow`. The
direnv hint appears only for real start failures. Stopping an agent never stalls the
daemon on an unrelated process check. A failed `ps` or a stale lock file can no longer
leak a process or signal the wrong one.

**Architecture:** The four fixes come from decisions 1–4 in
`docs/superpowers/notes/2026-10-16-plan-4-overnight.md`.
- **direnv:** a new `daemon::direnv` module decides and performs the auto-allow when a
  worktree agent is created. The daemon gets its direnv binary from `Config`, so tests
  inject a fake one.
- **Snapshots:** the pending snapshot remembers which agents it covers.
- **Process leftovers:** `reap::terminate` returns the tracked processes it could not
  verify, and the daemon retries them.
- **Lock file:** the lock file records the daemon's start time next to its pid, and the
  shared format lives in `protocol::lockfile`.

**Tech Stack:** Rust 2024 workspace (`crates/{protocol,daemon,client,tui,lazyagents}`),
rustix, serde_json, direnv 2.37+, nix dev shell.

**Spec:** `docs/superpowers/specs/2026-10-15-lazyagents-design.md` (spec section 1, goal
5: optional worktree per agent). The decisions behind this plan are in
`docs/superpowers/notes/2026-10-16-plan-4-overnight.md`, "Decide together" 1–4.

## Global Constraints

- **Tooling.** Every cargo, just and git-commit command runs as `nix develop -c …`. Rust is
  not installed globally.
- **The done gate.** `nix develop -c just check` must be green. It runs fmt-check, clippy
  `-D warnings`, nextest, test-hooks, test-harness, smoke-tui, test-tui-hangup and
  smoke-worktree.
- **Commits.** Conventional Commits: types feat fix docs refactor test chore build ci perf;
  scopes protocol daemon tui cli client docs nix repo; first line at most 72 characters.
- **Test first.** Write the failing test, watch it fail for the expected reason, then
  implement.
- **Comments.** Write none by default. Only a non-obvious invariant or constraint gets
  one, and never write a comment that narrates a change.
- **Docs.** `docs/` describes the system as it is now. A change to the protocol, keys or
  status detection updates its doc in the same commit.
- **Process safety.**
  - Never kill by broad pattern; the user runs their own copies.
  - No test leaves a process behind. Tests kill only children they started, by pid, and
    reap them.
  - A process is signalled only when its pid and start time are verified.
- **User state.** Tests never touch `~/.local/state/lazyagents`, `~/.config/lazyagents`,
  the user's git config, or the user's direnv data (`~/.local/share/direnv`,
  `~/.config/direnv`). Unix socket paths are about 104 bytes at most, so use short
  `mktemp -d` dirs.
- **direnv trust.** lazyagents runs `direnv allow` only for a worktree whose `.envrc` is
  byte-identical to the project's `.envrc`, and only when direnv already reports the
  project's `.envrc` as allowed. Anything else leaves direnv untouched.

## Review Focus

1. **direnv is missing, too old for `status --json`, or prints something unexpected.** The
   agent is still created, nothing is allowed, and the existing hint shows if the start
   fails. Covered by the Task 1 tests `status_failure_allows_nothing` and
   `unparsable_status_allows_nothing`.
2. **The project's `.envrc` is allowed, but the worktree's differs because the chosen base
   branch changed it.** Nothing is allowed. Covered by the Task 1 test
   `different_envrc_is_not_allowed`.
3. **The user's real daemon wrote a pid-only lock file.** That is today's format.
   `stop_daemon` still stops it. Covered by the Task 5 test
   `pid_only_lock_file_is_still_trusted`.
4. **`ps` is down for the whole shutdown.** The daemon logs the unverified pids and
   signals none of them. Covered by the Task 4 test
   `unverifiable_leftovers_are_never_signalled`.
5. **An agent runs normally for minutes, and direnv's "is blocked" text happens to be on
   its last screen when it exits.** No "failed to start". Covered by the Task 2 test
   `blocked_text_after_a_long_run_is_not_a_start_failure`.

---

## File Structure

- **Create `crates/daemon/src/direnv.rs`.** This holds the auto-allow decision, which is a
  pure function, plus the two direnv commands.
- **Modify `crates/daemon/src/server.rs`.**
  - `Config.direnv`.
  - Auto-allow at create.
  - The early-exit rule for the hint.
  - The pending-snapshot coverage.
  - A leftovers list.
- **Modify `crates/daemon/src/reap.rs`.** `terminate` returns unverified leftovers, through
  an injectable table reader.
- **Create `crates/protocol/src/lockfile.rs`.** This holds the lock file format and a
  process start-time reader. Both sides of the lock file use it.
- **Modify `crates/daemon/src/lock.rs` and `crates/client/src/lib.rs`.** The daemon writes
  the start time and the client verifies it.
- **Modify `crates/lazyagents/src/daemon_cmd.rs`.** It passes the real direnv into
  `Config`.
- **Modify `flake.nix`.** `pkgs.direnv` joins the dev shell, so the real-direnv test runs
  everywhere.
- **Docs.** Create `docs/decisions/0008-auto-allow-identical-envrc.md`, and update
  `docs/architecture.md`.

---

### Task 1: Auto-allow an identical `.envrc` in new worktrees

**Files:**
- Create `crates/daemon/src/direnv.rs`, `crates/daemon/tests/direnv.rs` and
  `docs/decisions/0008-auto-allow-identical-envrc.md`.
- Modify:
  - `crates/daemon/src/lib.rs`
  - `crates/daemon/src/server.rs`: `Config`, `Daemon::new` (around line 337), and
    `create_agent` after `add_worktree`
  - `crates/lazyagents/src/daemon_cmd.rs:45`
  - `crates/daemon/tests/server.rs:72-78`
  - `flake.nix`
  - `docs/architecture.md`, the worktree section

**Interfaces:**
- Produces:
  - `daemon::Config { …, pub direnv: Option<PathBuf> }`. `daemon_cmd` passes
    `daemon::find_in_path("direnv")`, and test configs pass `None` unless a test injects a
    fake.
  - In `daemon::direnv`:
    - `pub fn should_allow(project_rc: Option<&[u8]>, worktree_rc: Option<&[u8]>, project_allowed: bool) -> bool`
    - `pub type Env<'a> = &'a [(&'a str, &'a Path)];`, extra environment for direnv.
      Production passes `&[]`; tests pass temp `HOME` and `XDG_*` dirs.
    - `pub fn is_allowed(direnv: &Path, dir: &Path, env: Env) -> anyhow::Result<bool>`
    - `pub fn allow(direnv: &Path, dir: &Path, env: Env) -> anyhow::Result<()>`
    - `pub fn auto_allow(direnv: &Path, project: &Path, worktree: &Path, env: Env) -> anyhow::Result<bool>`.
      It returns true when it ran `direnv allow`.

- [ ] **Step 1: Add direnv to the dev shell.** Add `pkgs.direnv` to
  `devShells.default.packages` in `flake.nix`. Check it with
  `nix develop -c direnv version`, which should print 2.37 or later. Commit:
  `build(nix): add direnv to the dev shell`.

- [ ] **Step 2: Write the failing unit tests** in `crates/daemon/src/direnv.rs`.

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identical_allowed_envrc_is_allowed() {
        assert!(should_allow(Some(b"use flake\n"), Some(b"use flake\n"), true));
    }

    #[test]
    fn different_envrc_is_not_allowed() {
        assert!(!should_allow(Some(b"use flake\n"), Some(b"use flake\nexport X=1\n"), true));
    }

    #[test]
    fn unallowed_project_envrc_is_not_copied() {
        assert!(!should_allow(Some(b"use flake\n"), Some(b"use flake\n"), false));
    }

    #[test]
    fn a_missing_envrc_on_either_side_allows_nothing() {
        assert!(!should_allow(None, Some(b"use flake\n"), true));
        assert!(!should_allow(Some(b"use flake\n"), None, true));
        assert!(!should_allow(None, None, true));
    }

    #[test]
    fn status_json_reports_allowed_only_for_that_envrc() {
        let json = br#"{"state":{"foundRC":{"allowed":0,"path":"/p/.envrc"},"loadedRC":null}}"#;
        assert!(parse_allowed(json, Path::new("/p")).unwrap());
        let other = br#"{"state":{"foundRC":{"allowed":0,"path":"/elsewhere/.envrc"}}}"#;
        assert!(!parse_allowed(other, Path::new("/p")).unwrap());
        let blocked = br#"{"state":{"foundRC":{"allowed":1,"path":"/p/.envrc"}}}"#;
        assert!(!parse_allowed(blocked, Path::new("/p")).unwrap());
        let none = br#"{"state":{"foundRC":null}}"#;
        assert!(!parse_allowed(none, Path::new("/p")).unwrap());
    }

    #[test]
    fn unparsable_status_is_an_error() {
        assert!(parse_allowed(b"direnv: unknown flag --json", Path::new("/p")).is_err());
    }
}
```

- [ ] **Step 3: Run the unit tests and watch them fail.**
  `nix develop -c cargo nextest run -p daemon direnv::`. Expected: they fail to compile,
  because `should_allow` and `parse_allowed` do not exist yet.

- [ ] **Step 4: Implement `crates/daemon/src/direnv.rs`** and add `pub mod direnv;` to
  `lib.rs`.

```rust
//! Allows a new worktree's `.envrc` when the user already allowed the same content in
//! the project.

use std::path::Path;
use std::process::Command;

use anyhow::Context;
use serde_json::Value;

pub fn should_allow(project_rc: Option<&[u8]>, worktree_rc: Option<&[u8]>, project_allowed: bool) -> bool {
    matches!((project_rc, worktree_rc), (Some(p), Some(w)) if p == w) && project_allowed
}

/// direnv reports `allowed: 0` for an allowed rc; any other value or rc means no.
fn parse_allowed(stdout: &[u8], dir: &Path) -> anyhow::Result<bool> {
    let status: Value = serde_json::from_slice(stdout).context("reading direnv status --json")?;
    let found = &status["state"]["foundRC"];
    let path_matches = found["path"].as_str().is_some_and(|p| Path::new(p) == dir.join(".envrc"));
    Ok(path_matches && found["allowed"].as_i64() == Some(0))
}

pub type Env<'a> = &'a [(&'a str, &'a Path)];

pub fn is_allowed(direnv: &Path, dir: &Path, env: Env) -> anyhow::Result<bool> {
    let output = Command::new(direnv)
        .args(["status", "--json"])
        .envs(env.iter().copied())
        .current_dir(dir)
        .output()
        .with_context(|| format!("running {} status", direnv.display()))?;
    anyhow::ensure!(output.status.success(), "direnv status failed: {}", String::from_utf8_lossy(&output.stderr).trim());
    parse_allowed(&output.stdout, dir)
}

pub fn allow(direnv: &Path, dir: &Path, env: Env) -> anyhow::Result<()> {
    let output = Command::new(direnv)
        .arg("allow")
        .envs(env.iter().copied())
        .arg(dir)
        .output()
        .with_context(|| format!("running {} allow", direnv.display()))?;
    anyhow::ensure!(output.status.success(), "direnv allow failed: {}", String::from_utf8_lossy(&output.stderr).trim());
    Ok(())
}

pub fn auto_allow(direnv: &Path, project: &Path, worktree: &Path, env: Env) -> anyhow::Result<bool> {
    let project_rc = std::fs::read(project.join(".envrc")).ok();
    let worktree_rc = std::fs::read(worktree.join(".envrc")).ok();
    if project_rc.is_none() || project_rc != worktree_rc {
        return Ok(false);
    }
    if !should_allow(project_rc.as_deref(), worktree_rc.as_deref(), is_allowed(direnv, project, env)?) {
        return Ok(false);
    }
    allow(direnv, worktree, env)?;
    Ok(true)
}
```

  Compare paths after canonicalising both sides: macOS `/var` resolves to `/private/var`.
  In `parse_allowed`, compare `std::fs::canonicalize(p)` with
  `canonicalize(dir.join(".envrc"))`, falling back to the raw paths when canonicalising
  fails. Add a unit test that uses a symlinked temp dir.

- [ ] **Step 5: Run the unit tests.** Expected: PASS.

- [ ] **Step 6: Write the real-direnv tests** in `crates/daemon/tests/direnv.rs`.
  - They run real direnv with
    `env = [("HOME", t), ("XDG_DATA_HOME", t/data), ("XDG_CONFIG_HOME", t/config), ("XDG_CACHE_HOME", t/cache)]`
    for one tempdir `t`, so the user's allow list is never read or written. The setup's
    own `direnv allow <project>` uses the same env.
  - Find direnv with
    `daemon::find_in_path("direnv").expect("direnv in the nix dev shell")`.
  - `real_direnv_allows_an_identical_worktree_envrc`:
    1. The project and worktree dirs both hold an `.envrc` containing
       `export LAZYAGENTS_DIRENV_TEST=1\n`.
    2. Allow the project.
    3. Assert `auto_allow` returns true and `is_allowed(worktree)` is now true.
  - `real_direnv_leaves_a_different_worktree_envrc_blocked`: the same setup, with
    different worktree content. `auto_allow` returns false, and `is_allowed(worktree)`
    stays false.

- [ ] **Step 7: Run them.** `nix develop -c cargo nextest run -p daemon --test direnv`.
  Expected: PASS. These tests pin real direnv's behaviour that the unit tests assume: the
  `allowed: 0` encoding and the path it reports. If either fails, fix `parse_allowed` to
  match what real direnv prints, not the test.

- [ ] **Step 8: Write the failing server tests** in `crates/daemon/tests/server.rs`.
  - `config(dir)` gains `direnv: None`.
  - A helper `fake_direnv(dir, allowed: bool) -> PathBuf` writes an executable `sh` script.
    For `status --json` in any directory, it prints
    `{"state":{"foundRC":{"allowed":<0|1>,"path":"$PWD/.envrc"}}}`. For `allow <dir>`, it
    appends `allow <dir>` to `<dir-of-script>/calls`. For `exec <dir> <cmd…>`, it runs
    `cd <dir> && exec <cmd…>`.
  - Tests, each creating a worktree agent `sh -c 'pwd; sleep 30'` in a temp git repo that
    commits `.envrc`, then killing it:
    - `identical_allowed_envrc_is_allowed_for_a_new_worktree`: `calls` contains
      `allow <worktree path>`.
    - `unallowed_project_envrc_is_not_copied_to_the_worktree`: `allowed: false`, and
      `calls` has no `allow`.
    - `status_failure_allows_nothing`: the script exits 1 on `status`. The agent is still
      created, and there is no `allow`.
    - `unparsable_status_allows_nothing`: the script prints `nope` on `status`. The agent
      is created, and there is no `allow`.

- [ ] **Step 9: Run them and watch them fail.**
  `nix develop -c cargo nextest run -p daemon --test server -E 'test(/envrc|status_failure|unparsable_status/)'`.
  Expected: the first fails because no `allow` call is recorded.

- [ ] **Step 10: Wire it in.**
  - **Config.** Add `pub direnv: Option<PathBuf>` to `Config`, and store `config.direnv`
    in `Daemon::new` instead of calling `find_in_path`. `daemon_cmd.rs` passes
    `daemon::find_in_path("direnv")`; export `find_in_path` from `daemon` if it is not
    exported yet.
  - **Create.** In `create_agent`, right after `add_worktree` succeeds and before
    `self.start`, run:

```rust
if let (Some(direnv), Some(worktree)) = (self.direnv.as_deref(), worktree.as_ref()) {
    match direnv::auto_allow(direnv, &project.path, &worktree.path, &[]) {
        Ok(true) => tracing::info!(path = %worktree.path.display(), "allowed the worktree's .envrc"),
        Ok(false) => {}
        Err(e) => tracing::warn!(error = %e, "checking the worktree's .envrc failed"),
    }
}
```

  - **Restarts.** Keep the existing blocked-`.envrc` hint unchanged; it still covers every
    case where nothing was allowed.

- [ ] **Step 11: Run all daemon tests.** `nix develop -c cargo nextest run -p daemon`.
  Expected: all pass.

- [ ] **Step 12: Write ADR 0008** with `.claude/skills/recording-decisions/SKILL.md`.
  - **Context:** direnv allows by path plus content, so every new worktree starts blocked.
  - **Decision:** auto-allow only byte-identical content whose project copy direnv already
    reports as allowed.
  - **Alternatives:** a manual hint only; skipping direnv in worktrees, which loses the
    environment; allowing every worktree, which trusts unreviewed content.
  - **Consequences:** lazyagents writes to the user's direnv allow list. The trust extends
    to files the `.envrc` loads from the worktree, for example `use flake` evaluating the
    worktree's `flake.nix`, which come from the chosen base branch. A base branch that
    changed those files therefore runs them.
- [ ] **Step 13: Update `docs/architecture.md`.** In the worktree section, replace the
  sentence saying a worktree with an `.envrc` needs `direnv allow` once. The new text says
  which case lazyagents allows automatically and that every other case shows the hint.

- [ ] **Step 14: Run the harness check.** Use `.claude/skills/running-dev/SKILL.md`.
  1. Make a temp git repo with a committed `.envrc` containing
     `export LAZYAGENTS_DIRENV_TEST=1`.
  2. Start the TUI with temp `LAZYAGENTS_STATE_DIR`, `LAZYAGENTS_CONFIG`, `HOME`,
     `XDG_DATA_HOME`, `XDG_CONFIG_HOME` and `XDG_CACHE_HOME`. Run
     `direnv allow <repo>` with that env first.
  3. Create a Shell worktree agent and type `echo $LAZYAGENTS_DIRENV_TEST`. Expect `1`,
     and no "blocked".
  4. Shut down, and confirm with `pgrep -fl "lazyagents daemon"` that nothing is left.

- [ ] **Step 15: Commit.** Run `nix develop -c just check`, then commit:
  `feat(daemon): allow a worktree's .envrc when the project's identical one is`.

---

### Task 2: Show the direnv hint only for a real start failure

**Files:**
- Modify `crates/daemon/src/server.rs`: `note_blocked_envrc` (around line 1168) and
  `handle_terminal_event` (around line 990).
- Test: `crates/daemon/src/server.rs` `mod tests`.

**Interfaces:**
- Produces `const EARLY_EXIT: Duration = Duration::from_secs(10);` and
  `fn failed_early(agent: &Agent) -> bool`.

- [ ] **Step 1: Write the failing tests** in `server.rs` `mod tests`. Build an `Agent` for
  a worktree record whose terminal screen contains
  `direnv: error /w/.envrc is blocked. Run \`direnv allow\` to approve its content`. Reuse
  the existing helpers that build agents with terminals; if there are none, test
  `failed_early` and `note_blocked_envrc` through a constructed `Agent` with `terminal:
  None` and a separate `blocked_envrc_hint` call.

```rust
#[test]
fn blocked_text_after_a_long_run_is_not_a_start_failure() {
    let mut agent = worktree_agent();
    agent.started = Instant::now() - Duration::from_secs(120);
    agent.set_status(Status::Exited(1));
    assert!(!failed_early(&agent));
}

#[test]
fn a_clean_exit_is_never_a_start_failure() {
    let mut agent = worktree_agent();
    agent.started = Instant::now() - Duration::from_secs(1);
    agent.set_status(Status::Exited(0));
    assert!(!failed_early(&agent));
}

#[test]
fn a_quick_error_exit_is_a_start_failure() {
    let mut agent = worktree_agent();
    agent.started = Instant::now() - Duration::from_secs(1);
    agent.set_status(Status::Exited(1));
    assert!(failed_early(&agent));
}
```

- [ ] **Step 2: Run them and watch them fail.**
  `nix develop -c cargo nextest run -p daemon -E 'test(/start_failure/)'`. Expected:
  compile error, because `failed_early` does not exist yet.
- [ ] **Step 3: Implement.**

```rust
const EARLY_EXIT: Duration = Duration::from_secs(10);

fn failed_early(agent: &Agent) -> bool {
    matches!(agent.status, Status::Exited(code) if code != 0)
        && agent.status_since.duration_since(agent.started) < EARLY_EXIT
}
```

  In `note_blocked_envrc`, replace `agent.status.is_running()` with `!failed_early(agent)`
  in the early return.

- [ ] **Step 4: Run all daemon tests.** The existing server test for the blocked hint must
  still pass: its agent exits non-zero at once. Expected: PASS.
- [ ] **Step 5: Commit.** Run `nix develop -c just check`, then commit:
  `fix(daemon): show the direnv hint only when an agent fails to start`.

---

### Task 3: Wait for a running snapshot only when it covers a stopped agent

**Files:**
- Modify `crates/daemon/src/server.rs`:
  - the `snapshot` field (line 312)
  - `refresh_tracked` (around line 1016)
  - `stop` (around line 763)
  - `stop_all` (around line 795)
  - `finish_snapshot` (around line 800)
- Test: `crates/daemon/src/server.rs` `mod tests`.

**Interfaces:**
- Produces:
  - `struct PendingSnapshot { rx: Receiver<Snapshot>, agents: Vec<AgentId> }`
  - `snapshot: Option<PendingSnapshot>`
  - `fn finish_snapshot(&mut self, covering: Option<&[AgentId]>)`. `None` means wait
    whatever the snapshot covers.

- [ ] **Step 1: Write the failing tests** in `mod tests`. Construct a `Daemon` over a temp
  `StatePaths`, the same way the existing F1 tests in this module do. Install a pending
  snapshot whose sender is held by the test and never sends, covering agent `a` only.

```rust
#[test]
fn stopping_an_uncovered_agent_does_not_wait_for_the_snapshot() {
    let (mut daemon, a, b) = daemon_with_two_agents();
    let (_tx, rx) = std::sync::mpsc::channel();
    daemon.snapshot = Some(PendingSnapshot { rx, agents: vec![a] });
    let started = Instant::now();
    daemon.stop(&[b], STOP_GRACE);
    assert!(started.elapsed() < Duration::from_millis(500), "{:?}", started.elapsed());
    assert!(daemon.snapshot.is_some(), "the snapshot for {a} must stay pending");
}

#[test]
fn stopping_a_covered_agent_waits_for_the_snapshot() {
    let (mut daemon, a, _b) = daemon_with_two_agents();
    let (tx, rx) = std::sync::mpsc::channel();
    daemon.snapshot = Some(PendingSnapshot { rx, agents: vec![a] });
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(300));
        let _ = tx.send(Vec::new());
    });
    let started = Instant::now();
    daemon.stop(&[a], STOP_GRACE);
    assert!(started.elapsed() >= Duration::from_millis(250));
    assert!(daemon.snapshot.is_none());
}

#[test]
fn shutdown_waits_for_any_snapshot() {
    let (mut daemon, a, _b) = daemon_with_two_agents();
    let (tx, rx) = std::sync::mpsc::channel();
    daemon.snapshot = Some(PendingSnapshot { rx, agents: vec![a] });
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(300));
        let _ = tx.send(Vec::new());
    });
    daemon.stop_all();
    assert!(daemon.snapshot.is_none());
}
```

  `daemon_with_two_agents` inserts two `Agent::new(record)` values with no terminals.

- [ ] **Step 2: Run them and watch them fail.**
  `nix develop -c cargo nextest run -p daemon -E 'test(/snapshot/)'`. Expected: compile
  error, because `PendingSnapshot` does not exist yet.
- [ ] **Step 3: Implement.**
  - `refresh_tracked` stores
    `PendingSnapshot { rx, agents: requests.iter().map(|r| r.0).collect() }`.
  - `stop(ids, grace)` calls `self.finish_snapshot(Some(ids))`.
  - `stop_all` calls `self.finish_snapshot(None)` before computing its ids, then
    `self.stop(...)`.
  - `finish_snapshot(covering)`:
    1. If the pending snapshot covers none of `covering`, leave it in place and return.
    2. Otherwise take it and `recv_timeout(SNAPSHOT_WAIT)`.
    3. Apply the result, or warn on a timeout.
  - Update the doc comment on `stop`. It waits for a running snapshot only when that
    snapshot covers an agent being stopped; results for other agents are applied, or
    reaped as orphans, when they arrive.
- [ ] **Step 4: Run all daemon tests, including the F1 tests and the slow-`ps` check.**
  1. Write `sleep 1.2; exec /bin/ps "$@"` to `<tmp>/ps` and make it executable.
  2. Run
     `PATH=<tmp>:$PATH nix develop -c cargo nextest run -p daemon --test server -E 'test(/kill_|exited_agent_leaves|shutdown_during|remove_stops|restart_leaves/)'`.

  Expected: all pass.
- [ ] **Step 5: Update `docs/architecture.md`.** Edit the stop and snapshot paragraph to
  match.
- [ ] **Step 6: Commit.** Run `nix develop -c just check`, then commit:
  `fix(daemon): wait for a snapshot only when it covers a stopped agent`.

---

### Task 4: Keep unverified processes until they are really stopped

**Files:**
- Modify `crates/daemon/src/reap.rs`: `terminate`, around line 147.
- Modify `crates/daemon/src/server.rs`:
  - `Reaper`
  - `reap_off_loop`
  - `tick`
  - `stop`
  - `stop_all`
  - a new `leftovers` field
- Test: `reap.rs` `mod tests` and `server.rs` `mod tests`.

**Interfaces:**
- Produces:
  - In `reap`:
    - `pub fn terminate(trees: &[u32], groups: &[u32], tracked: &[Tracked], grace: Duration) -> Vec<Tracked>`.
      It returns the `tracked` entries it could not verify because no `ps` reading
      succeeded. It never signals those.
    - `fn terminate_with(read: &dyn Fn() -> Option<ProcessTable>, trees, groups, tracked, grace) -> Vec<Tracked>`,
      for tests.
  - In `Daemon`: `leftovers: Vec<(AgentId, Tracked)>`. `Reaper.thread` becomes
    `JoinHandle<Vec<Tracked>>`.

- [ ] **Step 1: Write the failing `reap.rs` tests.** Spawn a real child,
  `sh -c "trap '' TERM HUP; exec sleep 300"`, and record it as `Tracked` with its start
  time from `snapshot(std::process::id())` or a one-off `ProcessTable::read()`.

```rust
#[test]
fn unverifiable_leftovers_are_never_signalled() {
    let (mut child, tracked) = tracked_child();
    let left = terminate_with(&|| None, &[], &[], std::slice::from_ref(&tracked), Duration::from_millis(200));
    assert_eq!(left, vec![tracked.clone()]);
    assert!(child.try_wait().expect("try_wait").is_none(), "an unverified process was signalled");
    let left = terminate_with(&ProcessTable::read, &[], &[], &[tracked], Duration::from_millis(200));
    assert!(left.is_empty());
    let deadline = Instant::now() + Duration::from_secs(3);
    while child.try_wait().expect("try_wait").is_none() {
        assert!(Instant::now() < deadline, "the verified process survived");
        std::thread::sleep(POLL);
    }
}
```

  Make sure the child is killed by its pid if the test fails, using a guard struct with
  `Drop`.

- [ ] **Step 2: Run it and watch it fail.**
  `nix develop -c cargo nextest run -p daemon -E 'test(unverifiable_leftovers)'`. Expected:
  compile error, because `terminate_with` does not exist yet.
- [ ] **Step 3: Implement in `reap.rs`.**
  - `terminate` calls `terminate_with(&ProcessTable::read, …)`.
  - In `terminate_with`, when both readings fail, keep today's behaviour for trees and
    groups, and set `let unverified = tracked.to_vec();`.
  - Every return path returns `unverified`, which is empty when a reading succeeded.
- [ ] **Step 4: Run it.** Expected: PASS.
- [ ] **Step 5: Write the failing server test.** `leftovers_are_retried_until_verified`:
  1. Push `(agent, tracked_child)` into `daemon.leftovers` for an agent with no terminal.
  2. Call `daemon.tick()`, then wait up to 3 s for the reaper thread to finish, calling
     `tick()` again to collect it.
  3. Assert the child is dead and `leftovers` is empty.
- [ ] **Step 6: Implement in `server.rs`.**
  - `Reaper.thread: JoinHandle<Vec<Tracked>>`, and the reaper closure returns
    `terminate`'s result.
  - **`tick`.** Replace `retain(!is_finished)` with code that drains the finished reapers,
    `join`s each, and pushes the returned leftovers into `self.leftovers` tagged with
    `reaper.agent`. Then, once per `SNAPSHOT`, move every leftover into
    `reap_off_loop(agent, vec![], tracked)`, grouped by agent.
  - **`stop(ids)`.** Also drain the `leftovers` for `ids` into `tracked`. After
    `reap::terminate`, push the returned unverified entries back onto `leftovers`, under
    the first id of `ids`, as each `Tracked` keeps its own pid and start time.
  - **`stop_all`.** It includes every leftover. After its `terminate`, if anything is still
    unverified, log `tracing::error!(?pids, "could not verify these processes before
    exiting; they were not signalled")`.
  - The inline-reap fallback in `reap_off_loop` pushes its result onto `leftovers` too.
- [ ] **Step 7: Run all daemon tests.** Run the full `-p daemon` suite, then the slow-`ps`
  run from Task 3 Step 4. Expected: all pass.
- [ ] **Step 8: Update `docs/architecture.md`.** In the reaping paragraph, say that
  processes that cannot be verified are kept and retried, and are never signalled
  unverified.
- [ ] **Step 9: Commit.** Run `nix develop -c just check`, then commit:
  `fix(daemon): keep unverified processes until a ps reading confirms them`.

---

### Task 5: Record the daemon's start time in its lock file

**Files:**
- Create `crates/protocol/src/lockfile.rs`.
- Modify:
  - `crates/protocol/src/lib.rs`
  - `crates/daemon/src/lock.rs`
  - `crates/client/src/lib.rs`: `lock_holder`, `daemon_pid` and `stop_daemon`, around
    lines 230-310
  - `docs/architecture.md`, the instance lock paragraph
- Test: `crates/protocol/src/lockfile.rs` `mod tests` and `crates/client/tests/client.rs`.

**Interfaces:**
- Produces:
  - `protocol::lockfile::Holder { pub pid: u32, pub start: Option<String> }`
  - `protocol::lockfile::format(pid: u32, start: Option<&str>) -> String`. It writes
    `"<pid>\n<start>\n"`, or just `"<pid>\n"` without a start.
  - `protocol::lockfile::parse(text: &str) -> Option<Holder>`. It accepts the pid-only
    format that today's daemons write.
  - `protocol::lockfile::process_start(pid: u32) -> Option<String>`. It runs
    `ps -o lstart= -p <pid>`, trimmed, and returns `None` on any failure. This is the same
    `lstart` text that `reap.rs` uses.
  - `protocol::lockfile::is_current(holder: &Holder) -> bool`. It is true when `start` is
    `None` (the legacy format) or equals `process_start(pid)`.

The overnight notes suggested writing the pid before taking the lock. That cannot be done
in a shared file without clobbering a live holder's pid. Recording the start time closes
the same window: a stale `pid start` pair left by a SIGKILLed daemon never matches a
recycled pid.

- [ ] **Step 1: Write the failing protocol tests.**

```rust
#[test]
fn round_trips_with_and_without_a_start() {
    assert_eq!(parse(&format(42, Some("Thu Oct 16 09:00:00 2026"))),
        Some(Holder { pid: 42, start: Some("Thu Oct 16 09:00:00 2026".into()) }));
    assert_eq!(parse("42"), Some(Holder { pid: 42, start: None }));
    assert_eq!(parse(""), None);
    assert_eq!(parse("x"), None);
}

#[test]
fn the_current_process_is_current_and_a_wrong_start_is_not() {
    let pid = std::process::id();
    let start = process_start(pid).expect("ps reads our own start");
    assert!(is_current(&Holder { pid, start: Some(start) }));
    assert!(!is_current(&Holder { pid, start: Some("Mon Jan  1 00:00:00 1990".into()) }));
    assert!(is_current(&Holder { pid, start: None }));
}
```

- [ ] **Step 2: Run them and watch them fail.**
  `nix develop -c cargo nextest run -p protocol lockfile`. Expected: compile error.
- [ ] **Step 3: Implement `lockfile.rs`** and `pub mod lockfile;`. Use
  `std::process::Command` with no extra dependencies.
- [ ] **Step 4: Write the failing client tests** in `crates/client/tests/client.rs`, next
  to `stop_daemon_waits_for_a_pid_written_after_the_lock`. Reuse its lock-holding pattern
  and its sleep stand-in.
  - `stale_start_time_is_never_signalled`:
    1. The test holds the lock.
    2. It writes `format(<sleep child pid>, Some("Mon Jan  1 00:00:00 1990"))`.
    3. `stop_daemon(&paths, 1 s)` returns `Err` naming the pid.
    4. The sleep child is still alive. Kill it by its pid afterwards.
  - `matching_start_time_is_stopped`: the same, but with the child's real
    `process_start`. `stop_daemon` stops it.
  - `pid_only_lock_file_is_still_trusted`: the lock holds just the child's pid.
    `stop_daemon` stops it. This is today's format, which the user's running daemon
    writes.
- [ ] **Step 5: Run them and watch them fail.** Expected:
  `stale_start_time_is_never_signalled` fails because the child gets signalled.
- [ ] **Step 6: Implement.**
  - **`lock.rs`.** After `file.try_lock()` succeeds, call `file.set_len(0)` and write
    `lockfile::format(std::process::id(), lockfile::process_start(std::process::id()).as_deref())`.
  - **`client::lock_holder`.** Return `Option<Option<Holder>>` via `lockfile::parse`.
  - **`daemon_pid`.** Treat a holder that is not `is_current` like an unreadable pid: keep
    re-reading for up to `PID_WAIT`, because a starting daemon is about to overwrite it,
    then return `None`.
  - **`stop_daemon`.** Re-read the holder right before SIGTERM, and require the same pid
    and `is_current`. Otherwise bail with
    `"{lock} names pid {pid}, which is no longer the daemon; try again"`.
- [ ] **Step 7: Run the protocol, client and daemon tests and the CLI tests.**
  `nix develop -c cargo nextest run -p protocol -p client -p daemon -p lazyagents`.
  Expected: all pass.
- [ ] **Step 8: Update `docs/architecture.md`.** In the instance lock paragraph, describe
  the `pid` and `start` lines and the verification.
- [ ] **Step 9: Commit.** Run `nix develop -c just check`, then commit:
  `fix: verify the daemon's start time before signalling it`.

---

### Task 6: Close the overnight notes

**Files:** modify `docs/superpowers/notes/2026-10-16-plan-4-overnight.md`.

- [ ] **Step 1: Close decisions 1–4.** Under "Decide together", mark items 1–4 as done in
  plan 5 and name their commits. Item 5 stays accepted.
- [ ] **Step 2: Run the final checks.**
  1. `nix develop -c just check`, three times.
  2. `pgrep -fl "lazyagents|sleep 300|sleep 30"` shows nothing from these runs.
  3. `git status --short` is empty.
  4. `git worktree list` is clean.
- [ ] **Step 3: Commit.** `docs: close the plan 4 overnight decisions`.
