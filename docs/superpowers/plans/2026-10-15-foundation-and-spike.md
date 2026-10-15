# Foundation and Embedded-Terminal Spike Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Stand up the agents-first lazyagents repository (nix dev shell, Cargo workspace, docs, skills, hooks, CI) and build a throwaway spike that runs a real `claude` inside a ratatui pane, so the user can judge whether the embedded-terminal design is good enough before the real daemon/TUI is built.

**Architecture:** Milestones 0 and 1 of the spec. The spike is a single-process ratatui app (`spike/`): `alacritty_terminal` owns a PTY and the terminal state, a renderer copies the grid into a ratatui buffer, and an input encoder turns crossterm key/paste/mouse events into the bytes the child expects. The input encoder and renderer are written test-first because Plan 2 ports them into the daemon and TUI crates.

**Tech Stack:** Rust 2024 (stable via rust-overlay), alacritty_terminal 0.26.0, ratatui 0.30.2 (crossterm 0.29 via `ratatui::crossterm`), clap 4.6.7, anyhow 1.0.104, serde_json 1, just, cargo-nextest, jq, tmux, nix flakes + direnv.

**Spec:** `docs/superpowers/specs/2026-10-15-lazyagents-design.md`

**Scope:** This is plan 1 of 2. It ends at a **human gate** (Task 9): the user tries the spike with real Claude Code. Plan 2 (milestones 2–8) is written after the gate, using the spike report.

## Global Constraints

- All tooling comes from the nix dev shell; run commands via direnv or `nix develop -c <cmd>`. Install nothing globally.
- Rust edition 2024, workspace lints: `unsafe_code = "forbid"`, clippy `all` at warn, CI and hooks run clippy with `-D warnings`.
- Crate versions pinned as in Task 1's workspace `Cargo.toml`; `Cargo.lock` and `flake.lock` are committed; cargo runs with `--locked` in `just` recipes.
- Commits follow Conventional Commits: types `feat fix docs refactor test chore build ci perf`; scopes `protocol daemon tui cli docs nix repo spike`; first line ≤ 72 chars.
- Code comments: none by default; only non-obvious invariants or constraints. Comments never narrate changes.
- The tmux harness uses its own server (`tmux -L lazyagents-harness`); the user's tmux sessions are never touched.
- `LAZYAGENTS_STATE_DIR` and `~/.local/state/lazyagents` are not used by this plan; the spike writes only to a per-process temp dir.

## Checkpoints

Every task ends with `just check` green (from Task 1 on) and a commit. Two **checkpoints** sit between tasks:

- **Checkpoint A (after Task 4):** foundation verified end to end.
- **Checkpoint B (inside Task 9):** spike verified by tests and a harness run, then STOP for the human gate.

At a checkpoint the executor runs the listed commands, pastes their tail into the plan's checkpoint log (the `Result:` line), and proceeds only when every line matches its expectation. A red checkpoint is fixed before continuing; it is never skipped.

## Review Focus

1. **Outer terminal without keyboard enhancement** (inside tmux, Terminal.app): Enter must still submit, alt+enter must insert a newline, and `ctrl-]` must still detach even though it arrives as the legacy byte that crossterm reports as `ctrl+5`. Tests: Task 5 (`plain_enter_is_cr`, `alt_enter_is_esc_cr`, `detach_matches_legacy_ctrl_5`).
2. **Wide characters and emoji in agent output**: they must land in the right columns without shifting the rest of the row. Test: Task 7 (`wide_chars_keep_column_alignment`).
3. **Multi-line paste**: without bracketed paste, newlines become CR so no line is lost; with bracketed paste, a payload containing the end marker cannot break out early. Tests: Task 5 (`paste_without_brackets_maps_newlines`, `bracketed_paste_strips_end_marker`).
4. **Tiny pane** (terminal window shrunk to almost nothing): resize clamps and never panics. Test: Task 6 (`resize_to_zero_is_clamped`).
5. **Agent command missing or exiting immediately**: spawn fails with an error naming the command, or the exit status is reported; nothing hangs. Tests: Task 6 (`missing_command_is_an_error`, `exit_code_is_reported`).

---

## File Structure

```
.envrc                         direnv: use flake
.gitignore
.github/workflows/ci.yml       nix develop -c just check on linux + macos
.githooks/commit-msg           Conventional Commits gate
.githooks/pre-commit           fmt + clippy gate for staged Rust changes
.claude/settings.json          permissions + Claude Code hooks
.claude/hooks/rustfmt-edited.sh
.claude/hooks/clippy-on-stop.sh
.claude/skills/committing/SKILL.md
.claude/skills/verifying/SKILL.md
.claude/skills/running-dev/SKILL.md
.claude/skills/recording-decisions/SKILL.md
AGENTS.md                      canonical agent guide
CLAUDE.md                      @AGENTS.md
README.md
Cargo.toml                     workspace
rust-toolchain.toml
rustfmt.toml
flake.nix / flake.lock
justfile
crates/protocol/{Cargo.toml,src/lib.rs}
crates/daemon/{Cargo.toml,src/lib.rs}
crates/tui/{Cargo.toml,src/lib.rs}
crates/lazyagents/{Cargo.toml,src/main.rs,tests/cli.rs}
docs/architecture.md
docs/protocol.md
docs/keybindings.md
docs/status-detection.md
docs/decisions/0001-daemon-owns-agent-processes.md
docs/decisions/0002-embedded-terminal-in-main-pane.md
docs/decisions/0003-status-via-claude-hooks.md
docs/decisions/0004-commit-rules-enforced-by-git-hooks.md
docs/spike-report.md           written in Task 9
scripts/tmux-harness.sh        drive a TUI in an isolated tmux server
scripts/test-hooks.sh          tests for .githooks and .claude/hooks
scripts/test-harness.sh        self-test for tmux-harness.sh
spike/Cargo.toml
spike/src/lib.rs               module list (a lib so pub items aren't dead code mid-build)
spike/src/main.rs              app loop, layout, sidebar, key routing
spike/src/input.rs             key/paste/mouse → bytes
spike/src/session.rs           PTY + alacritty Term + event pump
spike/src/render.rs            Term grid → ratatui Buffer
spike/src/hooks.rs             Claude --settings JSON + hook-log status
```

---

### Task 1: Toolchain, workspace skeleton, CI

**Files:**
- Create: `flake.nix`, `.envrc`, `rust-toolchain.toml`, `rustfmt.toml`, `.gitignore`, `Cargo.toml`, `justfile`, `.github/workflows/ci.yml`
- Create: `crates/protocol/Cargo.toml`, `crates/protocol/src/lib.rs`, `crates/daemon/Cargo.toml`, `crates/daemon/src/lib.rs`, `crates/tui/Cargo.toml`, `crates/tui/src/lib.rs`, `crates/lazyagents/Cargo.toml`, `crates/lazyagents/src/main.rs`
- Test: `crates/lazyagents/tests/cli.rs`

**Interfaces:**
- Produces: `just check` (later tasks extend it), `protocol::PROTOCOL_VERSION: u32 = 1`, binary `lazyagents --version` printing `lazyagents 0.1.0`, workspace dependency table that later crates reference with `{ workspace = true }`.

- [ ] **Step 1: Write the nix flake and direnv files**

`flake.nix`:
```nix
{
  description = "lazyagents: a lazygit-style TUI for managing coding agents";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs =
    { nixpkgs, flake-utils, rust-overlay, ... }:
    flake-utils.lib.eachDefaultSystem (
      system:
      let
        pkgs = import nixpkgs {
          inherit system;
          overlays = [ (import rust-overlay) ];
        };
        toolchain = pkgs.rust-bin.fromRustupToolchainFile ./rust-toolchain.toml;
      in
      {
        devShells.default = pkgs.mkShell {
          packages = [
            toolchain
            pkgs.just
            pkgs.cargo-nextest
            pkgs.cargo-insta
            pkgs.cargo-deny
            pkgs.jq
            pkgs.tmux
            pkgs.git
          ];
          RUST_BACKTRACE = "1";
          shellHook = ''
            if git rev-parse --git-dir >/dev/null 2>&1; then
              git config core.hooksPath .githooks
            fi
          '';
        };
        formatter = pkgs.nixfmt-rfc-style;
      }
    );
}
```

`.envrc`:
```
use flake
```

`rust-toolchain.toml`:
```toml
[toolchain]
channel = "stable"
components = ["rustfmt", "clippy", "rust-analyzer", "rust-src"]
```

`rustfmt.toml`:
```toml
edition = "2024"
style_edition = "2024"
```

`.gitignore`:
```
/target
/result
.direnv/
*.snap.new
.DS_Store
```

- [ ] **Step 2: Enter the dev shell and confirm the toolchain**

Run: `nix develop -c sh -c 'cargo --version && just --version && cargo nextest --version && jq --version && tmux -V'`
Expected: five version lines, exit 0. This also creates `flake.lock`. Then run `direnv allow` so later commands get the shell automatically (if direnv is not hooked into the executor's shell, prefix every later command with `nix develop -c`).

- [ ] **Step 3: Write the Cargo workspace and crates**

`Cargo.toml`:
```toml
[workspace]
resolver = "3"
members = ["crates/*"]

[workspace.package]
version = "0.1.0"
edition = "2024"

[workspace.dependencies]
alacritty_terminal = "0.26.0"
anyhow = "1.0.104"
clap = { version = "4.6.7", features = ["derive"] }
ratatui = "0.30.2"
serde_json = "1"

[workspace.lints.rust]
unsafe_code = "forbid"

[workspace.lints.clippy]
all = { level = "warn", priority = -1 }
```

`crates/protocol/Cargo.toml`:
```toml
[package]
name = "protocol"
version.workspace = true
edition.workspace = true
publish = false

[lints]
workspace = true
```

`crates/protocol/src/lib.rs`:
```rust
//! Messages exchanged between the lazyagents daemon and its clients.

pub const PROTOCOL_VERSION: u32 = 1;
```

`crates/daemon/Cargo.toml` and `crates/tui/Cargo.toml`: identical to the protocol manifest with `name = "daemon"` and `name = "tui"` respectively.

`crates/daemon/src/lib.rs`:
```rust
//! The lazyagents daemon: owns agent processes, their terminals, and persisted state.
```

`crates/tui/src/lib.rs`:
```rust
//! The lazyagents terminal UI client.
```

`crates/lazyagents/Cargo.toml`:
```toml
[package]
name = "lazyagents"
version.workspace = true
edition.workspace = true
publish = false

[dependencies]
clap.workspace = true

[lints]
workspace = true
```

- [ ] **Step 4: Write the failing CLI test**

`crates/lazyagents/tests/cli.rs`:
```rust
use std::process::Command;

#[test]
fn prints_version() {
    let out = Command::new(env!("CARGO_BIN_EXE_lazyagents"))
        .arg("--version")
        .output()
        .expect("binary runs");
    assert!(out.status.success());
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        format!("lazyagents {}", env!("CARGO_PKG_VERSION"))
    );
}
```

`crates/lazyagents/src/main.rs` (deliberately without clap yet):
```rust
fn main() {}
```

Run: `cargo test -p lazyagents --test cli`
Expected: FAIL — `assert!(out.status.success())` passes but the version assertion fails with an empty left side.

- [ ] **Step 5: Implement the CLI**

`crates/lazyagents/src/main.rs`:
```rust
use clap::Parser;

/// A lazygit-style terminal UI for managing coding agents.
#[derive(Parser)]
#[command(version)]
struct Cli {}

fn main() {
    let _cli = Cli::parse();
}
```

Run: `cargo test -p lazyagents --test cli`
Expected: PASS (1 test).

- [ ] **Step 6: Write the justfile**

`justfile`:
```just
default: check

fmt:
    cargo fmt --all

fmt-check:
    cargo fmt --all -- --check

lint:
    cargo clippy --workspace --all-targets --locked -- -D warnings

test:
    cargo nextest run --workspace --locked --no-tests=pass

check: fmt-check lint test
```

Run: `just check`
Expected: exit 0; nextest reports `1 test run: 1 passed`. `Cargo.lock` now exists.

- [ ] **Step 7: Write CI**

`.github/workflows/ci.yml`:
```yaml
name: ci

on:
  push:
    branches: [main]
  pull_request:

jobs:
  check:
    strategy:
      matrix:
        os: [ubuntu-latest, macos-latest]
    runs-on: ${{ matrix.os }}
    steps:
      - uses: actions/checkout@v4
      - uses: DeterminateSystems/nix-installer-action@v16
      - uses: DeterminateSystems/magic-nix-cache-action@v8
      - run: nix develop --command just check
```

Run: `nix run nixpkgs#actionlint -- .github/workflows/ci.yml`
Expected: no output, exit 0.

- [ ] **Step 8: Commit**

```bash
git add flake.nix flake.lock .envrc rust-toolchain.toml rustfmt.toml .gitignore Cargo.toml Cargo.lock justfile .github crates
git commit -m "build(nix): add dev shell, cargo workspace skeleton and ci"
```

---

### Task 2: Commit gates and Claude Code hooks

**Files:**
- Create: `.githooks/commit-msg`, `.githooks/pre-commit`, `.claude/hooks/rustfmt-edited.sh`, `.claude/hooks/clippy-on-stop.sh`, `.claude/settings.json`
- Test: `scripts/test-hooks.sh`
- Modify: `justfile` (add `test-hooks`, extend `check`)

**Interfaces:**
- Consumes: dev shell from Task 1 (`jq`, `cargo`, `rustfmt`), `core.hooksPath=.githooks` set by the flake `shellHook`.
- Produces: `just test-hooks`; `just check` = `fmt-check lint test test-hooks`.

- [ ] **Step 1: Write the failing hook tests**

`scripts/test-hooks.sh`:
```bash
#!/usr/bin/env bash
# Exercises .githooks and .claude/hooks with canned inputs.
set -uo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
failures=0

expect() {
  local want=$1 desc=$2
  shift 2
  "$@" >"$tmp/out" 2>&1
  local got=$?
  if [[ $got == "$want" ]]; then
    echo "ok   $desc"
  else
    echo "FAIL $desc (exit $got, want $want)"
    sed 's/^/     /' "$tmp/out"
    failures=$((failures + 1))
  fi
}

msg() { printf '%s\n' "$1" >"$tmp/msg"; "$root/.githooks/commit-msg" "$tmp/msg"; }
expect 0 "commit-msg accepts typed scoped subject" msg "feat(daemon): spawn agents in a pty"
expect 0 "commit-msg accepts unscoped subject" msg "docs: describe status detection"
expect 0 "commit-msg accepts breaking marker" msg "feat(protocol)!: version the handshake"
expect 0 "commit-msg accepts merge commits" msg "Merge branch 'x'"
expect 1 "commit-msg rejects untyped subject" msg "added stuff"
expect 1 "commit-msg rejects unknown scope" msg "feat(web): add page"
expect 1 "commit-msg rejects unknown type" msg "feature: add page"
expect 1 "commit-msg rejects subject over 72 chars" msg "feat: $(printf 'x%.0s' {1..80})"

fmt_hook() { printf '%s' "$1" | "$root/.claude/hooks/rustfmt-edited.sh"; }
printf 'fn  main( ){}\n' >"$tmp/ugly.rs"
expect 0 "rustfmt hook ignores non-rust files" fmt_hook '{"tool_input":{"file_path":"README.md"}}'
expect 0 "rustfmt hook formats a rust file" fmt_hook "{\"tool_input\":{\"file_path\":\"$tmp/ugly.rs\"}}"
expect 0 "rustfmt hook output is formatted" grep -qx 'fn main() {}' "$tmp/ugly.rs"
printf 'fn main( {\n' >"$tmp/broken.rs"
expect 2 "rustfmt hook reports syntax errors" fmt_hook "{\"tool_input\":{\"file_path\":\"$tmp/broken.rs\"}}"

stop_hook() { printf '%s' "$1" | CLAUDE_PROJECT_DIR="$root" "$root/.claude/hooks/clippy-on-stop.sh"; }
expect 0 "clippy stop hook lets a re-entered stop through" stop_hook '{"stop_hook_active":true}'

if ((failures > 0)); then
  echo "$failures hook test(s) failed"
  exit 1
fi
echo "all hook tests passed"
```

Run: `chmod +x scripts/test-hooks.sh && scripts/test-hooks.sh`
Expected: FAIL lines for every case (hook files don't exist yet), final line `… hook test(s) failed`, exit 1.

- [ ] **Step 2: Write the git hooks**

`.githooks/commit-msg`:
```bash
#!/usr/bin/env bash
# Conventional Commits gate; the rules live in .claude/skills/committing/SKILL.md.
set -euo pipefail

subject=$(head -n1 "$1")
types='feat|fix|docs|refactor|test|chore|build|ci|perf'
scopes='protocol|daemon|tui|cli|docs|nix|repo|spike'
pattern="^($types)(\(($scopes)\))?!?: [^ ].*$"

if [[ $subject =~ ^(Merge|Revert|fixup!|squash!) ]]; then
  exit 0
fi
if [[ ! $subject =~ $pattern ]]; then
  echo "commit-msg: '$subject' is not a Conventional Commit." >&2
  echo "  expected <type>(<scope>): <subject>, type: ${types//|/ }, scope: ${scopes//|/ }" >&2
  exit 1
fi
if ((${#subject} > 72)); then
  echo "commit-msg: subject is ${#subject} chars; keep it at 72 or fewer." >&2
  exit 1
fi
```

`.githooks/pre-commit`:
```bash
#!/usr/bin/env bash
# Formatting and clippy gate for commits that touch Rust or Cargo files.
set -euo pipefail

git diff --cached --name-only --diff-filter=ACMR | grep -qE '(\.rs|Cargo\.(toml|lock))$' || exit 0
if ! command -v cargo >/dev/null; then
  echo "pre-commit: cargo not on PATH; commit from the nix dev shell (direnv or nix develop)." >&2
  exit 1
fi
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
```

- [ ] **Step 3: Write the Claude Code hooks and settings**

`.claude/hooks/rustfmt-edited.sh`:
```bash
#!/usr/bin/env bash
# PostToolUse: format the Rust file that was just edited; exit 2 hands syntax errors back to Claude.
set -euo pipefail

file=$(jq -r '.tool_input.file_path // empty')
[[ $file == *.rs && -f $file ]] || exit 0
command -v rustfmt >/dev/null || exit 0
rustfmt "$file" >&2 || exit 2
```

`.claude/hooks/clippy-on-stop.sh`:
```bash
#!/usr/bin/env bash
# Stop: keep Claude working while clippy fails on uncommitted Rust changes.
set -euo pipefail

input=$(cat)
[[ $(jq -r '.stop_hook_active // false' <<<"$input") == true ]] && exit 0
cd "${CLAUDE_PROJECT_DIR:-.}"
git status --porcelain -- '*.rs' '*Cargo.toml' 'Cargo.lock' | grep -q . || exit 0

run() {
  if command -v cargo >/dev/null; then "$@"; else nix develop --command "$@"; fi
}
if ! out=$(run cargo clippy --workspace --all-targets --locked --message-format short -- -D warnings 2>&1); then
  {
    echo "cargo clippy fails on the current changes; fix these before finishing:"
    tail -n 80 <<<"$out"
  } >&2
  exit 2
fi
```

`.claude/settings.json`:
```json
{
  "$schema": "https://json.schemastore.org/claude-code-settings.json",
  "permissions": {
    "allow": [
      "Bash(cargo build:*)",
      "Bash(cargo check:*)",
      "Bash(cargo clippy:*)",
      "Bash(cargo fmt:*)",
      "Bash(cargo test:*)",
      "Bash(cargo nextest:*)",
      "Bash(cargo run:*)",
      "Bash(cargo insta:*)",
      "Bash(cargo tree:*)",
      "Bash(just:*)",
      "Bash(nix develop:*)",
      "Bash(git status:*)",
      "Bash(git diff:*)",
      "Bash(git log:*)",
      "Bash(git show:*)",
      "Bash(git add:*)",
      "Bash(git commit:*)",
      "Bash(scripts/tmux-harness.sh:*)",
      "Bash(scripts/test-hooks.sh:*)",
      "Bash(scripts/test-harness.sh:*)"
    ]
  },
  "hooks": {
    "PostToolUse": [
      {
        "matcher": "Edit|Write|MultiEdit",
        "hooks": [
          {
            "type": "command",
            "command": "\"$CLAUDE_PROJECT_DIR\"/.claude/hooks/rustfmt-edited.sh"
          }
        ]
      }
    ],
    "Stop": [
      {
        "hooks": [
          {
            "type": "command",
            "command": "\"$CLAUDE_PROJECT_DIR\"/.claude/hooks/clippy-on-stop.sh",
            "timeout": 300
          }
        ]
      }
    ]
  }
}
```

- [ ] **Step 4: Make the hooks executable and run the tests**

Run: `chmod +x .githooks/* .claude/hooks/*.sh && scripts/test-hooks.sh`
Expected: 13 `ok` lines, `all hook tests passed`, exit 0.

- [ ] **Step 5: Wire hook tests into `just check`**

In `justfile`, add the recipe and extend `check`:
```just
test-hooks:
    scripts/test-hooks.sh

check: fmt-check lint test test-hooks
```

Run: `just check`
Expected: exit 0, ends with `all hook tests passed`.

- [ ] **Step 6: Prove the git hooks are live**

Run: `git config core.hooksPath` → Expected: `.githooks` (set by the flake shellHook; if empty, re-enter the dev shell).
Run: `git commit --allow-empty -m "bad message"` → Expected: rejected with `commit-msg: 'bad message' is not a Conventional Commit.`

- [ ] **Step 7: Commit**

```bash
git add .githooks .claude/hooks .claude/settings.json scripts/test-hooks.sh justfile
git commit -m "build(repo): gate commits and agent edits with fmt, clippy and hooks"
```

---

### Task 3: Agent guide, README, living docs, ADRs

**Files:**
- Create: `AGENTS.md`, `CLAUDE.md`, `README.md`, `docs/architecture.md`, `docs/protocol.md`, `docs/keybindings.md`, `docs/status-detection.md`, `docs/decisions/0001-daemon-owns-agent-processes.md`, `docs/decisions/0002-embedded-terminal-in-main-pane.md`, `docs/decisions/0003-status-via-claude-hooks.md`, `docs/decisions/0004-commit-rules-enforced-by-git-hooks.md`

**Interfaces:**
- Consumes: commands from Tasks 1–2; skill names from Task 4 (`committing`, `verifying`, `running-dev`, `recording-decisions`).
- Produces: `docs/` as the living source of truth; specs and plans become dated history.

- [ ] **Step 1: Write `AGENTS.md`**

````markdown
# lazyagents

A lazygit-style terminal UI for running many coding agents (mostly Claude Code) side by side: a sidebar of projects and agents with live status, and a main pane holding the selected agent's interactive terminal. A background daemon owns the agent processes; the TUI is a client that can quit and reattach.

`docs/` describes the system as it is now; `docs/superpowers/specs/` and `docs/superpowers/plans/` are dated history. Start with `docs/architecture.md`.

## Environment

All tooling comes from the nix dev shell: run `direnv allow` once, or prefix commands with `nix develop -c`. The shell also points git at `.githooks`.

## Commands

- `just check`: fmt check, clippy with `-D warnings`, tests, hook tests. The done gate.
- `just fmt`, `just lint`, `just test`, `just test-hooks`
- `just spike [cmd...]`: the milestone-1 embedded-terminal spike (default command `claude`).

## Layout

| Path | Purpose |
|---|---|
| `crates/protocol` | client↔daemon messages and framing |
| `crates/daemon` | agents, PTYs, terminal emulation, persistence, git worktrees |
| `crates/tui` | ratatui client |
| `crates/lazyagents` | the binary and CLI |
| `spike/` | throwaway embedded-terminal spike; removed after milestone 1 |
| `scripts/` | tmux harness and hook tests |
| `docs/` | architecture, protocol, keybindings, status detection, ADRs (`decisions/`) |

## Working rules

- Test first: write the failing test, watch it fail for the expected reason, then implement.
- Done means `just check` is green and you read its output (skill: verifying). Anything visible in a terminal also gets a harness run (skill: running-dev).
- Commit each logical change with a Conventional Commit (skill: committing). Git hooks gate message format, fmt and clippy; Claude Code hooks format edited Rust files and run clippy before you stop.
- A choice between real alternatives gets an ADR in `docs/decisions/` (skill: recording-decisions).
- A change to the protocol, keybindings or status detection updates its doc in `docs/` in the same commit.
- Comments: default to none. Write one only for a non-obvious invariant or constraint that stays true as the code evolves; change history belongs in commit messages.
- Tests and dev runs keep state in temp dirs (`LAZYAGENTS_STATE_DIR` once the daemon exists); `~/.local/state/lazyagents` belongs to the user's real agents.
- The tmux harness runs on its own server (`tmux -L lazyagents-harness`), leaving the user's tmux sessions alone.
````

- [ ] **Step 2: Write `CLAUDE.md` and `README.md`**

`CLAUDE.md`:
```markdown
@AGENTS.md
```

`README.md`:
````markdown
# lazyagents

A [lazygit](https://github.com/jesseduffield/lazygit)-style terminal UI for managing many coding agents at once.

Instead of a tmux tab per Claude Code session, lazyagents shows every agent in one place: projects and agents in a sidebar with live status (working, needs input, idle, exited), and the selected agent's fully interactive terminal in the main pane. A background daemon keeps agents running when you close the UI. Agents can get their own git worktree and branch so parallel work does not collide.

Your own Claude Code setup (`~/.claude`, project `.claude/`, plugins, skills, MCP servers, hooks) keeps working: lazyagents runs the real `claude` binary.

## Status

Early development. Milestone 1 is an embedded-terminal spike (`just spike`). See `docs/architecture.md` and `docs/superpowers/specs/2026-10-15-lazyagents-design.md`.

## Development

```sh
direnv allow        # or: nix develop
just check          # fmt, clippy, tests
just spike          # run the spike with claude in the main pane
just spike sh       # ... or any other command
```

Contributor and agent guide: [AGENTS.md](AGENTS.md).
````

- [ ] **Step 3: Write the living docs**

Each file starts with a one-line status and then carries the named spec section verbatim (headings demoted one level). The spec text is the design; these files are where it is kept current from now on.

- `docs/architecture.md`: heading `# Architecture`, line `Status: designed; the spike (milestone 1) exists, crates are skeletons until plan 2.`, then spec §1 "Purpose" (without "Success criteria" and "Non-goals"), §2 intro with the crate table and subcommands, §2.1, §2.4, §2.5, §2.6, §4.
- `docs/protocol.md`: heading `# Protocol`, line `Status: designed, not implemented. crates/protocol is the source of truth once it exists.`, then spec §2.2.
- `docs/keybindings.md`: heading `# Keybindings`, line `Status: designed, not implemented. The spike uses enter (focus pane), ctrl-] (back to sidebar) and q (quit).`, then spec §3 from "Two input modes" to the end.
- `docs/status-detection.md`: heading `# Status detection`, line `Status: designed; the spike shows hook events from a real claude in its sidebar.`, then spec §2.3.

- [ ] **Step 4: Write the ADRs**

`docs/decisions/0001-daemon-owns-agent-processes.md`:
```markdown
# 0001. A daemon owns the agent processes

Date: 2026-10-15
Status: Accepted

## Context

Agents must keep running when the UI closes, and several UI instances may attach at once. Three models were considered.

## Decision

A `lazyagents daemon` process owns every agent's PTY and terminal state and persists agent metadata. The TUI is a client over a Unix socket and is started on demand; it auto-spawns the daemon.

## Alternatives

- tmux-backed (one tmux session per agent, TUI drives tmux): least code, but the UI is bounded by what tmux exposes and focus means leaving the UI.
- PTYs inside the TUI process: simplest embedding, but agents die with the UI.

## Consequences

Closing the UI is free and reattaching is instant. A daemon crash kills running agents; metadata survives and Claude agents resume their session on restart. A versioned protocol is needed between client and daemon.
```

`docs/decisions/0002-embedded-terminal-in-main-pane.md`:
```markdown
# 0002. Agents are interactive inside the main pane

Date: 2026-10-15
Status: Accepted (pending spike verdict, milestone 1)

## Context

The user wants the sidebar visible while working with an agent, like a tmux split, rather than switching to a full-screen agent.

## Decision

The daemon runs an `alacritty_terminal` emulator per agent and encodes input according to the modes the agent enabled. The TUI renders the emulator's grid in the main pane and forwards abstract key, paste, mouse and resize events. `ctrl-]` returns focus to the sidebar.

## Alternatives

- Full-terminal passthrough (detach the UI, pipe raw bytes, like `tmux attach`): perfect fidelity, but the sidebar disappears while working.
- `vt100` crate as emulator: smaller, but less complete than alacritty's, which also powers Zed's terminal.

## Consequences

lazyagents is a small terminal multiplexer: key encoding, paste, mouse, scrollback and selection are ours to get right. A spike gates the decision before the daemon is built.
```

`docs/decisions/0003-status-via-claude-hooks.md`:
```markdown
# 0003. Claude status comes from hooks injected with `--settings`

Date: 2026-10-15
Status: Accepted (merge behaviour verified in the spike)

## Context

The sidebar must show whether each agent is working, needs input, is idle or exited. Scraping terminal output is fragile across Claude Code releases.

## Decision

Claude agents start with `--settings <generated.json>` that only adds hooks (`SessionStart`, `UserPromptSubmit`, `PreToolUse`, `Notification`, `Stop`), each calling `lazyagents hook <event>`. Claude Code merges these with the user's own settings, so `~/.claude/settings.json` is never edited. Agents get `--session-id <uuid>` at creation and `--resume <uuid>` on restart. Non-Claude agents use an output-activity heuristic.

## Alternatives

- Parse Claude's screen for spinners and prompts: breaks whenever the UI changes.
- Write hooks into the user's settings: invasive and affects sessions outside lazyagents.

## Consequences

Status is exact for Claude and approximate for other agents. Hooks must never slow Claude down: the hook subcommand times out quickly and always exits 0.
```

`docs/decisions/0004-commit-rules-enforced-by-git-hooks.md`:
```markdown
# 0004. Commit rules are enforced by git hooks

Date: 2026-10-15
Status: Accepted

## Context

The design called for a Claude Code `PreToolUse` guard that inspects `git commit` commands. Extracting a message from arbitrary shell (heredocs, `-F`, editors) is unreliable, and the guard would only apply to Claude.

## Decision

`.githooks/commit-msg` validates Conventional Commits and `.githooks/pre-commit` runs fmt and clippy on commits touching Rust. The nix dev shell sets `core.hooksPath=.githooks`.

## Alternatives

- Claude `PreToolUse` guard on `git commit`: only sees the command line, misses messages from files and editors.

## Consequences

Humans and every agent get the same gate. Commits outside the dev shell fail with a message telling the user to enter it.
```

- [ ] **Step 5: Verify links and commit**

Run: `grep -oE '\(?[a-zA-Z0-9_./-]+\.md\)?' AGENTS.md README.md | tr -d '()' | sort -u | while read -r f; do [ -e "$f" ] || [ -e "docs/$f" ] || echo "missing: $f"; done`
Expected: no `missing:` lines.
Run: `just check` → Expected: exit 0.

```bash
git add AGENTS.md CLAUDE.md README.md docs/architecture.md docs/protocol.md docs/keybindings.md docs/status-detection.md docs/decisions
git commit -m "docs: add agent guide, living docs and initial decisions"
```

---

### Task 4: Skills and tmux harness

**Files:**
- Create: `scripts/tmux-harness.sh`, `.claude/skills/committing/SKILL.md`, `.claude/skills/verifying/SKILL.md`, `.claude/skills/running-dev/SKILL.md`, `.claude/skills/recording-decisions/SKILL.md`
- Test: `scripts/test-harness.sh`
- Modify: `justfile` (add `test-harness`, extend `check`)

**Interfaces:**
- Produces: `scripts/tmux-harness.sh {start <name> <cols> <rows> -- <cmd...> | type <name> <text> | keys <name> <key...> | capture <name> | wait-for <name> <text> [secs] | stop <name>}`; exit 1 on `wait-for` timeout with the screen on stderr.

- [ ] **Step 1: Write the failing harness self-test**

`scripts/test-harness.sh`:
```bash
#!/usr/bin/env bash
# Self-test for tmux-harness.sh against a plain shell.
set -euo pipefail

h="$(dirname "$0")/tmux-harness.sh"
name="harness-selftest-$$"
trap '"$h" stop "$name"' EXIT

"$h" start "$name" 80 24 -- sh
"$h" type "$name" 'echo harness-$((20+22))'
"$h" keys "$name" Enter
"$h" wait-for "$name" 'harness-42' 5
if "$h" wait-for "$name" 'never-printed' 1 2>/dev/null; then
  echo "wait-for should time out on absent text" >&2
  exit 1
fi
echo "harness self-test passed"
```

Run: `chmod +x scripts/test-harness.sh && scripts/test-harness.sh`
Expected: FAIL — `tmux-harness.sh: No such file or directory`.

- [ ] **Step 2: Write the harness**

`scripts/tmux-harness.sh`:
```bash
#!/usr/bin/env bash
# Drives a TUI inside a detached session on a private tmux server for scripted end-to-end checks.
set -euo pipefail

usage() {
  cat >&2 <<'EOF'
usage: tmux-harness.sh start <name> <cols> <rows> -- <command...>
       tmux-harness.sh type <name> <text>        send literal text
       tmux-harness.sh keys <name> <key>...      send tmux key names: Enter C-] M-Enter Escape
       tmux-harness.sh capture <name>            print the visible screen
       tmux-harness.sh wait-for <name> <text> [timeout_secs=10]
       tmux-harness.sh stop <name>
EOF
  exit 2
}

t() { tmux -L lazyagents-harness "$@"; }

cmd=${1:-}
name=${2:-}
[[ -n $cmd && -n $name ]] || usage

case $cmd in
  start)
    [[ $# -ge 5 ]] || usage
    cols=$3 rows=$4
    shift 4
    [[ ${1:-} == -- ]] && shift
    t new-session -d -s "$name" -x "$cols" -y "$rows" "$@"
    ;;
  type) t send-keys -t "$name" -l "${3:?text}" ;;
  keys)
    shift 2
    t send-keys -t "$name" "$@"
    ;;
  capture) t capture-pane -p -t "$name" ;;
  wait-for)
    text=${3:?text}
    deadline=$((SECONDS + ${4:-10}))
    while ((SECONDS < deadline)); do
      if t capture-pane -p -t "$name" | grep -qF -- "$text"; then exit 0; fi
      sleep 0.2
    done
    echo "timed out waiting for: $text" >&2
    t capture-pane -p -t "$name" >&2
    exit 1
    ;;
  stop) t kill-session -t "$name" 2>/dev/null || true ;;
  *) usage ;;
esac
```

Run: `chmod +x scripts/tmux-harness.sh && scripts/test-harness.sh`
Expected: `harness self-test passed`, exit 0.

- [ ] **Step 3: Wire the harness test into `just check`**

In `justfile`:
```just
test-harness:
    scripts/test-harness.sh

check: fmt-check lint test test-hooks test-harness
```

Run: `just check` → Expected: exit 0.

- [ ] **Step 4: Write the skills**

`.claude/skills/committing/SKILL.md`:
````markdown
---
name: committing
description: Conventional Commits rules for this repo: types, scopes, subject format, commit granularity. Load before every git commit.
---

# Committing

1. Run `just check` and read the result. Commit only on green.
2. Stage one logical change: a feature with its tests and docs, a fix with its regression test. Unrelated changes become separate commits.
3. Write the message:

   ```
   <type>(<scope>): <subject>

   <body: why the change was made, wrapped at 72>
   ```

   - type: `feat` `fix` `docs` `refactor` `test` `chore` `build` `ci` `perf`
   - scope (optional): `protocol` `daemon` `tui` `cli` `docs` `nix` `repo` `spike`, naming the crate or area touched; leave it out when the change spans several.
   - subject: imperative mood, lower case, no trailing period; the whole first line is at most 72 characters.
   - breaking change to the protocol or the on-disk state format: `!` after the scope and a `BREAKING CHANGE:` footer.
4. Commit with a heredoc so the body keeps its line breaks:

   ```bash
   git commit -F - <<'EOF'
   feat(daemon): persist agents atomically

   Write state.toml through a temp file and rename so a crash mid-write
   never leaves a truncated file.
   EOF
   ```

5. Done when `git log -1` shows the commit. When a git hook rejects it, fix what it reports and commit again; every commit passes through the hooks.

Examples: `feat(tui): add attention sorting`, `fix(daemon): clamp pane size to 2x2`, `test(protocol): round-trip every request`, `docs: document scroll mode`, `build(nix): add cargo-insta`.
````

`.claude/skills/verifying/SKILL.md`:
```markdown
---
name: verifying
description: The done gate for lazyagents. Load before claiming a step, task or milestone is complete, and before committing.
---

# Verifying

1. Run `just check`. Read the whole output: test counts, warnings, hook and harness results. It is green only when the command exits 0.
2. For anything visible in a terminal (the spike, the TUI), run the scenario through the tmux harness (skill: running-dev) and quote the captured screen lines that prove it.
3. For a bug fix, show the regression test failing before the fix and passing after.
4. Report what you ran and what it printed. Red is reported as red, with the failing output; every claim of done carries its evidence.
```

`.claude/skills/running-dev/SKILL.md`:
````markdown
---
name: running-dev
description: Drive the spike or the lazyagents TUI in an isolated tmux session: start it, send keys, assert on the screen. Load for any end-to-end or terminal-behaviour check.
---

# Running dev

`scripts/tmux-harness.sh` runs a TUI in a detached session on a private tmux server (`-L lazyagents-harness`); the user's own tmux is untouched.

1. Build first so startup is instant: `cargo build -p spike`.
2. Start: `scripts/tmux-harness.sh start <name> 120 40 -- target/debug/spike sh`
3. Drive: `type <name> '<text>'` sends literal text; `keys <name> Enter C-] M-Enter Escape` sends named keys.
4. Assert: `wait-for <name> '<text>' [secs]` exits 1 and prints the screen on timeout; `capture <name>` prints the screen at any time.
5. Finish with `stop <name>`, also after a failure.

Use `sh` as the agent in automated runs. Real `claude` needs the user's login and belongs to manual checks.

Gotchas:
- tmux delivers `C-]` as the legacy byte 0x1d, which crossterm reports as ctrl+5. Detach handles both.
- Inside tmux, shift+enter arrives as plain enter unless tmux `extended-keys` is on; check newline insertion with `M-Enter` (alt+enter).
````

`.claude/skills/recording-decisions/SKILL.md`:
````markdown
---
name: recording-decisions
description: When and how to write an ADR in docs/decisions. Load when choosing between real alternatives (dependency, protocol or storage format, process model, UX convention) or reversing an earlier decision.
---

# Recording decisions

An ADR is due when a choice had real alternatives and a future reader would ask "why this?".

1. Number: highest `NNNN` in `docs/decisions/` plus one.
2. Create `docs/decisions/NNNN-kebab-title.md` from the template.
3. Reversing an earlier ADR: set its status to `Superseded by NNNN` and reference it from the new one.
4. Commit the ADR together with the change it explains.

```markdown
# NNNN. Title stating the decision

Date: YYYY-MM-DD
Status: Accepted

## Context

The forces and constraints that made this a decision.

## Decision

What we do, in one paragraph.

## Alternatives

- Option: why it lost.

## Consequences

What gets easier and what gets harder.
```
````

- [ ] **Step 5: Commit**

```bash
git add scripts/tmux-harness.sh scripts/test-harness.sh .claude/skills justfile
git commit -m "build(repo): add agent skills and tmux end-to-end harness"
```

- [ ] **Checkpoint A: foundation verified**

Run each and compare:
1. `nix develop -c just check` → exit 0; output contains `1 passed`, `all hook tests passed`, `harness self-test passed`.
2. `git log --oneline` → 5 commits, all Conventional (spec, nix, repo hooks, docs, repo skills).
3. `git config core.hooksPath` → `.githooks`.
4. `ls .claude/skills` → `committing recording-decisions running-dev verifying`.

Result: _(executor pastes the tail of command 1 and the output of 2–4 here)_

---

### Task 5: Spike crate and input encoder

**Files:**
- Create: `spike/Cargo.toml`, `spike/src/lib.rs`, `spike/src/main.rs` (empty `fn main` until Task 8), `spike/src/input.rs`
- Modify: `Cargo.toml` (members), `justfile` (add `spike`)

**Interfaces:**
- Produces (in `spike/src/input.rs`):
  - `pub fn encode_key(key: KeyEvent, mode: TermMode) -> Option<Vec<u8>>`
  - `pub fn encode_paste(text: &str, mode: TermMode) -> Vec<u8>`
  - `pub fn encode_wheel(up: bool, col: u16, row: u16, mode: TermMode) -> Option<Vec<u8>>` (None = the app has no mouse/alternate-scroll mode; caller scrolls scrollback)
  - `pub fn is_detach(key: &KeyEvent) -> bool`
  - Types: `KeyEvent`, `KeyCode`, `KeyModifiers` from `ratatui::crossterm::event`; `TermMode` from `alacritty_terminal::term::TermMode`.

- [ ] **Step 1: Create the crate**

In `Cargo.toml`: `members = ["crates/*", "spike"]`.

`spike/Cargo.toml`:
```toml
[package]
name = "spike"
version.workspace = true
edition.workspace = true
publish = false

[dependencies]
alacritty_terminal.workspace = true
anyhow.workspace = true
ratatui.workspace = true
serde_json.workspace = true

[lints]
workspace = true
```

`spike/src/lib.rs`:
```rust
//! Throwaway spike: a real agent in an embedded terminal pane beside a sidebar.

pub mod input;
```

`spike/src/main.rs`:
```rust
fn main() {}
```

In `justfile`:
```just
spike *args:
    cargo run -p spike -- {{args}}
```

- [ ] **Step 2: Write the failing encoder tests**

`spike/src/input.rs` (tests first; the functions are stubs that return wrong values):
```rust
use alacritty_terminal::term::TermMode;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

pub fn encode_key(_key: KeyEvent, _mode: TermMode) -> Option<Vec<u8>> {
    None
}

pub fn encode_paste(_text: &str, _mode: TermMode) -> Vec<u8> {
    Vec::new()
}

pub fn encode_wheel(_up: bool, _col: u16, _row: u16, _mode: TermMode) -> Option<Vec<u8>> {
    None
}

pub fn is_detach(_key: &KeyEvent) -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode, mods: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, mods)
    }

    fn enc(code: KeyCode, mods: KeyModifiers) -> Vec<u8> {
        encode_key(key(code, mods), TermMode::empty()).expect("encodable")
    }

    #[test]
    fn plain_chars_are_utf8() {
        assert_eq!(enc(KeyCode::Char('a'), KeyModifiers::NONE), b"a");
        assert_eq!(enc(KeyCode::Char('A'), KeyModifiers::SHIFT), b"A");
        assert_eq!(enc(KeyCode::Char('é'), KeyModifiers::NONE), "é".as_bytes());
    }

    #[test]
    fn ctrl_letters_are_control_bytes() {
        assert_eq!(enc(KeyCode::Char('c'), KeyModifiers::CONTROL), [0x03]);
        assert_eq!(enc(KeyCode::Char('r'), KeyModifiers::CONTROL), [0x12]);
        assert_eq!(enc(KeyCode::Char(' '), KeyModifiers::CONTROL), [0x00]);
    }

    #[test]
    fn alt_prefixes_escape() {
        assert_eq!(enc(KeyCode::Char('b'), KeyModifiers::ALT), b"\x1bb");
        assert_eq!(enc(KeyCode::Backspace, KeyModifiers::ALT), b"\x1b\x7f");
    }

    #[test]
    fn plain_enter_is_cr() {
        assert_eq!(enc(KeyCode::Enter, KeyModifiers::NONE), b"\r");
    }

    #[test]
    fn alt_enter_is_esc_cr() {
        assert_eq!(enc(KeyCode::Enter, KeyModifiers::ALT), b"\x1b\r");
        assert_eq!(enc(KeyCode::Enter, KeyModifiers::SHIFT), b"\x1b\r");
    }

    #[test]
    fn editing_keys() {
        assert_eq!(enc(KeyCode::Tab, KeyModifiers::NONE), b"\t");
        assert_eq!(enc(KeyCode::BackTab, KeyModifiers::SHIFT), b"\x1b[Z");
        assert_eq!(enc(KeyCode::Backspace, KeyModifiers::NONE), [0x7f]);
        assert_eq!(enc(KeyCode::Esc, KeyModifiers::NONE), [0x1b]);
        assert_eq!(enc(KeyCode::Delete, KeyModifiers::NONE), b"\x1b[3~");
        assert_eq!(enc(KeyCode::PageUp, KeyModifiers::NONE), b"\x1b[5~");
    }

    #[test]
    fn arrows_follow_cursor_mode() {
        assert_eq!(enc(KeyCode::Up, KeyModifiers::NONE), b"\x1b[A");
        let app = encode_key(key(KeyCode::Up, KeyModifiers::NONE), TermMode::APP_CURSOR);
        assert_eq!(app.as_deref(), Some(b"\x1bOA".as_slice()));
    }

    #[test]
    fn modified_arrows_use_csi_params() {
        assert_eq!(enc(KeyCode::Left, KeyModifiers::CONTROL), b"\x1b[1;5D");
        assert_eq!(enc(KeyCode::Right, KeyModifiers::SHIFT), b"\x1b[1;2C");
        assert_eq!(enc(KeyCode::Delete, KeyModifiers::ALT), b"\x1b[3;3~");
    }

    #[test]
    fn function_keys() {
        assert_eq!(enc(KeyCode::F(1), KeyModifiers::NONE), b"\x1bOP");
        assert_eq!(enc(KeyCode::F(5), KeyModifiers::NONE), b"\x1b[15~");
        assert_eq!(enc(KeyCode::F(12), KeyModifiers::NONE), b"\x1b[24~");
        assert_eq!(encode_key(key(KeyCode::F(13), KeyModifiers::NONE), TermMode::empty()), None);
    }

    #[test]
    fn paste_without_brackets_maps_newlines() {
        assert_eq!(encode_paste("a\nb\r\nc", TermMode::empty()), b"a\rb\rc");
    }

    #[test]
    fn bracketed_paste_wraps_text() {
        assert_eq!(
            encode_paste("hi\n", TermMode::BRACKETED_PASTE),
            b"\x1b[200~hi\n\x1b[201~"
        );
    }

    #[test]
    fn bracketed_paste_strips_end_marker() {
        assert_eq!(
            encode_paste("a\x1b[201~b", TermMode::BRACKETED_PASTE),
            b"\x1b[200~ab\x1b[201~"
        );
    }

    #[test]
    fn wheel_scrolls_scrollback_without_mouse_mode() {
        assert_eq!(encode_wheel(true, 0, 0, TermMode::empty()), None);
    }

    #[test]
    fn wheel_reports_sgr_when_requested() {
        let mode = TermMode::MOUSE_REPORT_CLICK | TermMode::SGR_MOUSE;
        assert_eq!(encode_wheel(true, 4, 9, mode).as_deref(), Some(b"\x1b[<64;5;10M".as_slice()));
        assert_eq!(encode_wheel(false, 0, 0, mode).as_deref(), Some(b"\x1b[<65;1;1M".as_slice()));
    }

    #[test]
    fn wheel_sends_arrows_on_alternate_screen() {
        let mode = TermMode::ALT_SCREEN | TermMode::ALTERNATE_SCROLL;
        assert_eq!(encode_wheel(true, 0, 0, mode).as_deref(), Some(b"\x1b[A\x1b[A\x1b[A".as_slice()));
    }

    #[test]
    fn detach_matches_ctrl_bracket() {
        assert!(is_detach(&key(KeyCode::Char(']'), KeyModifiers::CONTROL)));
    }

    #[test]
    fn detach_matches_legacy_ctrl_5() {
        assert!(is_detach(&key(KeyCode::Char('5'), KeyModifiers::CONTROL)));
    }

    #[test]
    fn detach_ignores_plain_bracket() {
        assert!(!is_detach(&key(KeyCode::Char(']'), KeyModifiers::NONE)));
    }
}
```

Run: `cargo nextest run -p spike`
Expected: FAIL — every test except `detach_ignores_plain_bracket` and `wheel_scrolls_scrollback_without_mouse_mode` fails.

- [ ] **Step 3: Implement the encoder**

Replace the four stub functions in `spike/src/input.rs` with:
```rust
pub fn encode_key(key: KeyEvent, mode: TermMode) -> Option<Vec<u8>> {
    let shift = key.modifiers.contains(KeyModifiers::SHIFT);
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let param = 1 + u8::from(shift) + 2 * u8::from(alt) + 4 * u8::from(ctrl);
    let app_cursor = mode.contains(TermMode::APP_CURSOR);

    let bytes = match key.code {
        KeyCode::Char(c) => encode_char(c, ctrl, alt),
        KeyCode::Enter if shift || alt => b"\x1b\r".to_vec(),
        KeyCode::Enter => b"\r".to_vec(),
        KeyCode::Tab if shift => b"\x1b[Z".to_vec(),
        KeyCode::Tab => b"\t".to_vec(),
        KeyCode::BackTab => b"\x1b[Z".to_vec(),
        KeyCode::Backspace if ctrl => vec![0x08],
        KeyCode::Backspace if alt => b"\x1b\x7f".to_vec(),
        KeyCode::Backspace => vec![0x7f],
        KeyCode::Esc => vec![0x1b],
        KeyCode::Up => cursor_key(b'A', param, app_cursor),
        KeyCode::Down => cursor_key(b'B', param, app_cursor),
        KeyCode::Right => cursor_key(b'C', param, app_cursor),
        KeyCode::Left => cursor_key(b'D', param, app_cursor),
        KeyCode::Home => cursor_key(b'H', param, app_cursor),
        KeyCode::End => cursor_key(b'F', param, app_cursor),
        KeyCode::Insert => tilde_key(2, param),
        KeyCode::Delete => tilde_key(3, param),
        KeyCode::PageUp => tilde_key(5, param),
        KeyCode::PageDown => tilde_key(6, param),
        KeyCode::F(n) => function_key(n, param)?,
        _ => return None,
    };
    Some(bytes)
}

pub fn encode_paste(text: &str, mode: TermMode) -> Vec<u8> {
    if mode.contains(TermMode::BRACKETED_PASTE) {
        let body = text.replace("\x1b[201~", "");
        [b"\x1b[200~".as_slice(), body.as_bytes(), b"\x1b[201~"].concat()
    } else {
        text.replace("\r\n", "\r").replace('\n', "\r").into_bytes()
    }
}

pub fn encode_wheel(up: bool, col: u16, row: u16, mode: TermMode) -> Option<Vec<u8>> {
    if mode.intersects(TermMode::MOUSE_MODE) {
        let button: u8 = if up { 64 } else { 65 };
        if mode.contains(TermMode::SGR_MOUSE) {
            Some(format!("\x1b[<{button};{};{}M", col + 1, row + 1).into_bytes())
        } else {
            let coord = |v: u16| u8::try_from(v + 33).unwrap_or(u8::MAX);
            Some(vec![0x1b, b'[', b'M', 32 + button, coord(col), coord(row)])
        }
    } else if mode.contains(TermMode::ALT_SCREEN | TermMode::ALTERNATE_SCROLL) {
        let arrow = cursor_key(if up { b'A' } else { b'B' }, 1, mode.contains(TermMode::APP_CURSOR));
        Some(arrow.repeat(3))
    } else {
        None
    }
}

pub fn is_detach(key: &KeyEvent) -> bool {
    key.modifiers.contains(KeyModifiers::CONTROL)
        && matches!(key.code, KeyCode::Char(']') | KeyCode::Char('5'))
}

fn encode_char(c: char, ctrl: bool, alt: bool) -> Vec<u8> {
    let mut out = Vec::with_capacity(5);
    if alt {
        out.push(0x1b);
    }
    match ctrl.then(|| ctrl_byte(c)).flatten() {
        Some(byte) => out.push(byte),
        None => out.extend_from_slice(c.encode_utf8(&mut [0; 4]).as_bytes()),
    }
    out
}

fn ctrl_byte(c: char) -> Option<u8> {
    match c {
        'a'..='z' => Some(c as u8 - b'a' + 1),
        'A'..='Z' => Some(c as u8 - b'A' + 1),
        ' ' | '@' | '2' => Some(0x00),
        '[' | '3' => Some(0x1b),
        '\\' | '4' => Some(0x1c),
        ']' | '5' => Some(0x1d),
        '^' | '6' => Some(0x1e),
        '_' | '/' | '7' => Some(0x1f),
        '?' | '8' => Some(0x7f),
        _ => None,
    }
}

fn cursor_key(final_byte: u8, param: u8, app_cursor: bool) -> Vec<u8> {
    if param > 1 {
        format!("\x1b[1;{param}{}", final_byte as char).into_bytes()
    } else if app_cursor {
        vec![0x1b, b'O', final_byte]
    } else {
        vec![0x1b, b'[', final_byte]
    }
}

fn tilde_key(code: u8, param: u8) -> Vec<u8> {
    if param > 1 {
        format!("\x1b[{code};{param}~").into_bytes()
    } else {
        format!("\x1b[{code}~").into_bytes()
    }
}

fn function_key(n: u8, param: u8) -> Option<Vec<u8>> {
    match n {
        1..=4 => {
            let final_byte = b"PQRS"[usize::from(n - 1)];
            Some(if param > 1 {
                format!("\x1b[1;{param}{}", final_byte as char).into_bytes()
            } else {
                vec![0x1b, b'O', final_byte]
            })
        }
        5..=12 => {
            let code = [15, 17, 18, 19, 20, 21, 23, 24][usize::from(n - 5)];
            Some(tilde_key(code, param))
        }
        _ => None,
    }
}
```

Run: `cargo nextest run -p spike`
Expected: PASS, 18 tests.

- [ ] **Step 4: Full check and commit**

Run: `just check` → Expected: exit 0.

```bash
git add Cargo.toml Cargo.lock justfile spike
git commit -m "feat(spike): encode keys, paste and wheel for the embedded terminal"
```

---

### Task 6: Spike terminal session

**Files:**
- Create: `spike/src/session.rs`
- Modify: `spike/src/lib.rs` (add `pub mod session;`)

**Interfaces:**
- Consumes: nothing from Task 5.
- Produces (in `spike/src/session.rs`):
  - `#[derive(Clone, Copy, Debug, PartialEq, Eq)] pub struct PaneSize { pub cols: u16, pub rows: u16 }` with `pub fn new(cols: u16, rows: u16) -> Self` (clamps each to ≥ 2) and `impl Dimensions`.
  - `#[derive(Clone)] pub struct Proxy` implementing `EventListener`.
  - `pub struct Session` with:
    - `pub fn spawn(program: &str, args: &[String], cwd: &Path, env: HashMap<String, String>, size: PaneSize) -> anyhow::Result<Session>`
    - `pub fn write(&self, bytes: Vec<u8>)`
    - `pub fn resize(&mut self, size: PaneSize)`
    - `pub fn size(&self) -> PaneSize`
    - `pub fn pump(&mut self) -> bool` (handles queued terminal events; true when the screen may have changed)
    - `pub fn exit_status(&self) -> Option<ExitStatus>`
    - `pub fn mode(&self) -> TermMode`
    - `pub fn scroll(&self, lines: i32)` (positive = back in history)
    - `pub fn scroll_to_bottom(&self)`
    - `pub fn term(&self) -> &Arc<FairMutex<Term<Proxy>>>`
    - `pub fn screen_text(&self) -> String` (visible lines, right-trimmed, trailing blank lines dropped)
    - `pub fn wait_for_exit(&mut self, timeout: Duration) -> Option<ExitStatus>`

- [ ] **Step 1: Write the failing session tests**

Add `pub mod session;` to `spike/src/lib.rs`. `spike/src/session.rs` starts with the types and the tests; `PaneSize`'s methods are `todo!()` and `impl Session` does not exist yet:
```rust
use std::collections::HashMap;
use std::path::Path;
use std::process::ExitStatus;
use std::sync::Arc;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use alacritty_terminal::event::{Event, EventListener, WindowSize};
use alacritty_terminal::event_loop::{EventLoop, EventLoopSender, Msg};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::{Config, Term, TermMode};
use alacritty_terminal::tty::{self, Options, Shell};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PaneSize {
    pub cols: u16,
    pub rows: u16,
}

impl PaneSize {
    pub fn new(cols: u16, rows: u16) -> Self {
        todo!()
    }

    fn window_size(self) -> WindowSize {
        todo!()
    }
}

impl Dimensions for PaneSize {
    fn total_lines(&self) -> usize {
        usize::from(self.rows)
    }

    fn screen_lines(&self) -> usize {
        usize::from(self.rows)
    }

    fn columns(&self) -> usize {
        usize::from(self.cols)
    }
}

#[derive(Clone)]
pub struct Proxy(mpsc::Sender<Event>);

impl EventListener for Proxy {
    fn send_event(&self, event: Event) {
        let _ = self.0.send(event);
    }
}

pub struct Session {
    term: Arc<FairMutex<Term<Proxy>>>,
    sender: EventLoopSender,
    events: mpsc::Receiver<Event>,
    size: PaneSize,
    exit_status: Option<ExitStatus>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sh(script: &str) -> Session {
        Session::spawn(
            "sh",
            &["-c".to_string(), script.to_string()],
            Path::new("."),
            HashMap::new(),
            PaneSize::new(40, 10),
        )
        .expect("spawns")
    }

    #[test]
    fn output_reaches_the_screen() {
        let mut s = sh("printf 'hello from pty'");
        s.wait_for_exit(Duration::from_secs(5)).expect("exits");
        assert!(s.screen_text().contains("hello from pty"), "{}", s.screen_text());
    }

    #[test]
    fn exit_code_is_reported() {
        let mut s = sh("exit 3");
        let status = s.wait_for_exit(Duration::from_secs(5)).expect("exits");
        assert_eq!(status.code(), Some(3));
    }

    #[test]
    fn input_reaches_the_child() {
        let mut s = sh("read -r line; printf 'got:%s' \"$line\"");
        s.write(b"ping\r".to_vec());
        s.wait_for_exit(Duration::from_secs(5)).expect("exits");
        assert!(s.screen_text().contains("got:ping"), "{}", s.screen_text());
    }

    #[test]
    fn terminal_replies_reach_the_child() {
        let mut s = Session::spawn(
            "bash",
            &[
                "-c".to_string(),
                r#"printf '\033[6n'; IFS= read -rs -d R reply; printf '\ncursor:%s' "${reply:1}""#.to_string(),
            ],
            Path::new("."),
            HashMap::new(),
            PaneSize::new(40, 10),
        )
        .expect("spawns");
        s.wait_for_exit(Duration::from_secs(5)).expect("exits");
        assert!(s.screen_text().contains("cursor:[1;1"), "{}", s.screen_text());
    }

    #[test]
    fn env_is_passed_through() {
        let mut env = HashMap::new();
        env.insert("LAZYAGENTS_SPIKE_PROBE".to_string(), "probe-ok".to_string());
        let mut s = Session::spawn(
            "sh",
            &["-c".to_string(), "printf '%s' \"$LAZYAGENTS_SPIKE_PROBE\"".to_string()],
            Path::new("."),
            env,
            PaneSize::new(40, 10),
        )
        .expect("spawns");
        s.wait_for_exit(Duration::from_secs(5)).expect("exits");
        assert!(s.screen_text().contains("probe-ok"), "{}", s.screen_text());
    }

    #[test]
    fn missing_command_is_an_error() {
        let err = Session::spawn(
            "/nonexistent/lazyagents-spike-cmd",
            &[],
            Path::new("."),
            HashMap::new(),
            PaneSize::new(40, 10),
        )
        .err()
        .expect("spawn fails");
        assert!(err.to_string().contains("lazyagents-spike-cmd"), "{err}");
    }

    #[test]
    fn resize_to_zero_is_clamped() {
        let mut s = sh("sleep 5");
        s.resize(PaneSize::new(0, 0));
        assert_eq!(s.size(), PaneSize { cols: 2, rows: 2 });
        s.pump();
    }

    #[test]
    fn resize_reaches_the_child() {
        let mut s = sh("sleep 0.3; stty size");
        s.resize(PaneSize::new(33, 7));
        s.wait_for_exit(Duration::from_secs(5)).expect("exits");
        assert!(s.screen_text().contains("7 33"), "{}", s.screen_text());
    }
}
```

Run: `cargo nextest run -p spike session`
Expected: FAIL to compile with `no function or associated item named 'spawn' found for struct 'Session'`.

- [ ] **Step 2: Implement the session**

Replace the `todo!()` bodies and add `impl Session`:
```rust
impl PaneSize {
    pub fn new(cols: u16, rows: u16) -> Self {
        Self { cols: cols.max(2), rows: rows.max(2) }
    }

    fn window_size(self) -> WindowSize {
        WindowSize { num_lines: self.rows, num_cols: self.cols, cell_width: 1, cell_height: 1 }
    }
}

impl Session {
    pub fn spawn(
        program: &str,
        args: &[String],
        cwd: &Path,
        mut env: HashMap<String, String>,
        size: PaneSize,
    ) -> anyhow::Result<Self> {
        env.entry("TERM".into()).or_insert_with(|| "xterm-256color".into());
        env.entry("COLORTERM".into()).or_insert_with(|| "truecolor".into());
        let options = Options {
            shell: Some(Shell::new(program.to_string(), args.to_vec())),
            working_directory: Some(cwd.to_path_buf()),
            drain_on_exit: true,
            env,
        };
        let pty = tty::new(&options, size.window_size(), 0)
            .map_err(|e| anyhow::anyhow!("failed to start `{program}`: {e}"))?;

        let (tx, events) = mpsc::channel();
        let proxy = Proxy(tx);
        let term = Arc::new(FairMutex::new(Term::new(Config::default(), &size, proxy.clone())));
        let event_loop = EventLoop::new(Arc::clone(&term), proxy, pty, true, false)?;
        let sender = event_loop.channel();
        event_loop.spawn();

        Ok(Self { term, sender, events, size, exit_status: None })
    }

    pub fn write(&self, bytes: Vec<u8>) {
        let _ = self.sender.send(Msg::Input(bytes.into()));
    }

    pub fn resize(&mut self, size: PaneSize) {
        let size = PaneSize::new(size.cols, size.rows);
        if size == self.size {
            return;
        }
        self.size = size;
        self.term.lock().resize(size);
        let _ = self.sender.send(Msg::Resize(size.window_size()));
    }

    pub fn size(&self) -> PaneSize {
        self.size
    }

    pub fn pump(&mut self) -> bool {
        let mut changed = false;
        while let Ok(event) = self.events.try_recv() {
            match event {
                Event::Wakeup => changed = true,
                Event::PtyWrite(text) => self.write(text.into_bytes()),
                Event::TextAreaSizeRequest(format) => {
                    self.write(format(self.size.window_size()).into_bytes());
                }
                Event::ChildExit(status) => {
                    self.exit_status = Some(status);
                    changed = true;
                }
                _ => {}
            }
        }
        changed
    }

    pub fn exit_status(&self) -> Option<ExitStatus> {
        self.exit_status
    }

    pub fn mode(&self) -> TermMode {
        *self.term.lock().mode()
    }

    pub fn scroll(&self, lines: i32) {
        self.term.lock().scroll_display(Scroll::Delta(lines));
    }

    pub fn scroll_to_bottom(&self) {
        self.term.lock().scroll_display(Scroll::Bottom);
    }

    pub fn term(&self) -> &Arc<FairMutex<Term<Proxy>>> {
        &self.term
    }

    pub fn screen_text(&self) -> String {
        let term = self.term.lock();
        let grid = term.grid();
        let lines: Vec<String> = (0..grid.screen_lines())
            .map(|line| {
                let row = &grid[Line(line as i32)];
                let text: String = (0..grid.columns()).map(|col| row[Column(col)].c).collect();
                text.trim_end().to_string()
            })
            .collect();
        lines.join("\n").trim_end().to_string()
    }

    pub fn wait_for_exit(&mut self, timeout: Duration) -> Option<ExitStatus> {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            self.pump();
            if self.exit_status.is_some() {
                std::thread::sleep(Duration::from_millis(50));
                self.pump();
                return self.exit_status;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        None
    }
}
```

Run: `cargo nextest run -p spike session`
Expected: PASS, 8 tests. If `terminal_replies_reach_the_child` fails, print `s.screen_text()` and confirm `Event::PtyWrite` is being forwarded in `pump`; `wait_for_exit` must pump while the child is still running for replies to flow.

- [ ] **Step 3: Full check and commit**

Run: `just check` → Expected: exit 0.

```bash
git add spike
git commit -m "feat(spike): run agents in a pty backed by alacritty_terminal"
```

---

### Task 7: Spike renderer

**Files:**
- Create: `spike/src/render.rs`
- Modify: `spike/src/lib.rs` (add `pub mod render;`)

**Interfaces:**
- Consumes: nothing from Tasks 5–6 (works on any `Term<T>`).
- Produces: `pub fn render_term<T>(term: &Term<T>, area: Rect, buf: &mut Buffer) -> Option<Position>` — draws the visible grid (respecting scrollback offset) into `area`, returns the cursor position in buffer coordinates when it should be shown.

- [ ] **Step 1: Write the failing renderer tests**

Add `pub mod render;` to `spike/src/lib.rs`. `spike/src/render.rs`:
```rust
use alacritty_terminal::Term;
use alacritty_terminal::term::TermMode;
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::vte::ansi::{Color as AnsiColor, CursorShape, NamedColor};
use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect};
use ratatui::style::{Color, Modifier, Style};

pub fn render_term<T>(_term: &Term<T>, _area: Rect, _buf: &mut Buffer) -> Option<Position> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use alacritty_terminal::event::VoidListener;
    use alacritty_terminal::grid::Dimensions;
    use alacritty_terminal::term::Config;
    use alacritty_terminal::vte::ansi::Processor;

    struct Size(usize, usize);

    impl Dimensions for Size {
        fn total_lines(&self) -> usize {
            self.1
        }
        fn screen_lines(&self) -> usize {
            self.1
        }
        fn columns(&self) -> usize {
            self.0
        }
    }

    fn term_with(bytes: &[u8]) -> Term<VoidListener> {
        let mut term = Term::new(Config::default(), &Size(20, 4), VoidListener);
        let mut parser: Processor = Processor::new();
        parser.advance(&mut term, bytes);
        term
    }

    fn draw(term: &Term<VoidListener>, area: Rect) -> (Buffer, Option<Position>) {
        let mut buf = Buffer::empty(Rect::new(0, 0, 30, 8));
        let cursor = render_term(term, area, &mut buf);
        (buf, cursor)
    }

    #[test]
    fn text_lands_at_the_area_offset() {
        let term = term_with(b"hi");
        let (buf, _) = draw(&term, Rect::new(5, 2, 20, 4));
        assert_eq!(buf[(5, 2)].symbol(), "h");
        assert_eq!(buf[(6, 2)].symbol(), "i");
    }

    #[test]
    fn sgr_colors_and_modifiers_map() {
        let term = term_with(b"\x1b[1;31mR\x1b[0m\x1b[38;2;1;2;3;48;5;200mX");
        let (buf, _) = draw(&term, Rect::new(0, 0, 20, 4));
        let r = &buf[(0, 0)];
        assert_eq!(r.fg, Color::Indexed(1));
        assert!(r.modifier.contains(Modifier::BOLD));
        let x = &buf[(1, 0)];
        assert_eq!(x.fg, Color::Rgb(1, 2, 3));
        assert_eq!(x.bg, Color::Indexed(200));
    }

    #[test]
    fn default_colors_are_reset() {
        let term = term_with(b"d");
        let (buf, _) = draw(&term, Rect::new(0, 0, 20, 4));
        assert_eq!(buf[(0, 0)].fg, Color::Reset);
        assert_eq!(buf[(0, 0)].bg, Color::Reset);
    }

    #[test]
    fn wide_chars_keep_column_alignment() {
        let term = term_with("a日b".as_bytes());
        let (buf, _) = draw(&term, Rect::new(0, 0, 20, 4));
        assert_eq!(buf[(0, 0)].symbol(), "a");
        assert_eq!(buf[(1, 0)].symbol(), "日");
        assert_eq!(buf[(3, 0)].symbol(), "b");
    }

    #[test]
    fn cursor_position_is_reported() {
        let term = term_with(b"abc\r\nde");
        let (_, cursor) = draw(&term, Rect::new(2, 1, 20, 4));
        assert_eq!(cursor, Some(Position::new(4, 2)));
    }

    #[test]
    fn hidden_cursor_is_not_reported() {
        let term = term_with(b"abc\x1b[?25l");
        let (_, cursor) = draw(&term, Rect::new(0, 0, 20, 4));
        assert_eq!(cursor, None);
    }

    #[test]
    fn area_smaller_than_grid_clips() {
        let term = term_with(b"0123456789");
        let mut buf = Buffer::empty(Rect::new(0, 0, 4, 1));
        render_term(&term, Rect::new(0, 0, 4, 1), &mut buf);
        assert_eq!(buf[(3, 0)].symbol(), "3");
    }
}
```

Run: `cargo nextest run -p spike render`
Expected: FAIL — symbol and color assertions fail (empty buffer), cursor tests return `None` where `Some` is expected; `hidden_cursor_is_not_reported` and `area_smaller_than_grid_clips` may pass or fail.

- [ ] **Step 2: Implement the renderer**

Replace `render_term` and add helpers:
```rust
pub fn render_term<T>(term: &Term<T>, area: Rect, buf: &mut Buffer) -> Option<Position> {
    let content = term.renderable_content();
    let offset = content.display_offset as i32;
    let to_screen = |line: i32, col: usize| -> Option<Position> {
        let row = u16::try_from(line + offset).ok()?;
        let col = u16::try_from(col).ok()?;
        (row < area.height && col < area.width).then(|| Position::new(area.x + col, area.y + row))
    };

    for indexed in content.display_iter {
        let cell = indexed.cell;
        if cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
            continue;
        }
        let Some(pos) = to_screen(indexed.point.line.0, indexed.point.column.0) else {
            continue;
        };
        let Some(target) = buf.cell_mut(pos) else {
            continue;
        };
        let symbol = if cell.flags.contains(Flags::HIDDEN) { ' ' } else { cell.c };
        target.set_char(symbol).set_style(style_for(cell.fg, cell.bg, cell.flags));
    }

    let cursor = content.cursor;
    let visible = content.mode.contains(TermMode::SHOW_CURSOR) && cursor.shape != CursorShape::Hidden;
    visible.then(|| to_screen(cursor.point.line.0, cursor.point.column.0)).flatten()
}

fn style_for(fg: AnsiColor, bg: AnsiColor, flags: Flags) -> Style {
    let mut modifier = Modifier::empty();
    for (flag, m) in [
        (Flags::BOLD, Modifier::BOLD),
        (Flags::DIM, Modifier::DIM),
        (Flags::ITALIC, Modifier::ITALIC),
        (Flags::ALL_UNDERLINES, Modifier::UNDERLINED),
        (Flags::INVERSE, Modifier::REVERSED),
        (Flags::STRIKEOUT, Modifier::CROSSED_OUT),
    ] {
        if flags.intersects(flag) {
            modifier |= m;
        }
    }
    Style::new().fg(map_color(fg)).bg(map_color(bg)).add_modifier(modifier)
}

fn map_color(color: AnsiColor) -> Color {
    match color {
        AnsiColor::Spec(rgb) => Color::Rgb(rgb.r, rgb.g, rgb.b),
        AnsiColor::Indexed(i) => Color::Indexed(i),
        AnsiColor::Named(named) => {
            let index = named as usize;
            let dim_black = NamedColor::DimBlack as usize;
            match index {
                0..=15 => Color::Indexed(index as u8),
                i if (dim_black..dim_black + 8).contains(&i) => Color::Indexed((i - dim_black) as u8),
                _ => Color::Reset,
            }
        }
    }
}
```

Run: `cargo nextest run -p spike render`
Expected: PASS, 7 tests. If `sgr_colors_and_modifiers_map` fails on `Color::Indexed(1)`, check that `NamedColor::Red as usize == 1` (it is defined `Black = 0, Red, ...`).

- [ ] **Step 3: Full check and commit**

Run: `just check` → Expected: exit 0.

```bash
git add spike
git commit -m "feat(spike): render the terminal grid into a ratatui buffer"
```

---

### Task 8: Spike hooks and app loop

**Files:**
- Create: `spike/src/hooks.rs`
- Modify: `spike/src/lib.rs` (add `pub mod hooks;`), `spike/src/main.rs` (replace with the app)

**Interfaces:**
- Consumes: `input::{encode_key, encode_paste, encode_wheel, is_detach}` (Task 5), `session::{Session, PaneSize}` (Task 6), `render::render_term` (Task 7).
- Produces (in `spike/src/hooks.rs`):
  - `pub fn settings_json(log: &Path) -> serde_json::Value` — Claude `--settings` payload whose hooks append the event name to `log`.
  - `pub fn status_from_log(log: &str) -> &'static str` — `"starting"` for an empty log, else the status mapped from the last event: `SessionStart`/`Stop` → `"idle"`, `UserPromptSubmit`/`PreToolUse` → `"working"`, `Notification` → `"needs input"`, anything else → `"unknown"`.
  - Binary: `spike [command [args...]]`, default `claude`. When the program's file name is `claude`, it appends `--settings <tmp>/hooks.json`.

- [ ] **Step 1: Write the failing hooks tests**

`spike/src/hooks.rs`:
```rust
use std::path::Path;

use serde_json::{Value, json};

const EVENTS: [&str; 5] = ["SessionStart", "UserPromptSubmit", "PreToolUse", "Notification", "Stop"];

pub fn settings_json(_log: &Path) -> Value {
    json!({})
}

pub fn status_from_log(_log: &str) -> &'static str {
    ""
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_register_every_event() {
        let value = settings_json(Path::new("/tmp/x/hooks.log"));
        for event in EVENTS {
            let command = value["hooks"][event][0]["hooks"][0]["command"]
                .as_str()
                .unwrap_or_else(|| panic!("{event} has a command"));
            assert!(command.contains(event), "{command}");
            assert!(command.contains("'/tmp/x/hooks.log'"), "{command}");
            assert_eq!(value["hooks"][event][0]["hooks"][0]["type"], "command");
        }
    }

    #[test]
    fn hook_command_appends_event_to_log() {
        let dir = std::env::temp_dir().join(format!("lazyagents-spike-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let log = dir.join("hooks.log");
        let value = settings_json(&log);
        let command = value["hooks"]["Notification"][0]["hooks"][0]["command"].as_str().expect("command");
        let status = std::process::Command::new("sh")
            .args(["-c", command])
            .stdin(std::process::Stdio::null())
            .status()
            .expect("runs");
        assert!(status.success());
        assert_eq!(std::fs::read_to_string(&log).expect("log written"), "Notification\n");
        std::fs::remove_dir_all(dir).expect("cleanup");
    }

    #[test]
    fn status_follows_last_event() {
        assert_eq!(status_from_log(""), "starting");
        assert_eq!(status_from_log("SessionStart\n"), "idle");
        assert_eq!(status_from_log("SessionStart\nUserPromptSubmit\n"), "working");
        assert_eq!(status_from_log("PreToolUse\nNotification\n"), "needs input");
        assert_eq!(status_from_log("PreToolUse\nStop\n"), "idle");
        assert_eq!(status_from_log("Mystery\n"), "unknown");
    }
}
```

Run: `cargo nextest run -p spike hooks` (after adding `pub mod hooks;` to `lib.rs`)
Expected: FAIL — all 3 tests.

- [ ] **Step 2: Implement hooks**

Replace the two stub functions:
```rust
pub fn settings_json(log: &Path) -> Value {
    let quoted = format!("'{}'", log.display().to_string().replace('\'', r"'\''"));
    let mut hooks = serde_json::Map::new();
    for event in EVENTS {
        let command = format!("cat >/dev/null; printf '%s\\n' {event} >> {quoted}");
        hooks.insert(
            event.to_string(),
            json!([{ "hooks": [{ "type": "command", "command": command }] }]),
        );
    }
    json!({ "hooks": hooks })
}

pub fn status_from_log(log: &str) -> &'static str {
    match log.lines().last() {
        None => "starting",
        Some("SessionStart" | "Stop") => "idle",
        Some("UserPromptSubmit" | "PreToolUse") => "working",
        Some("Notification") => "needs input",
        Some(_) => "unknown",
    }
}
```

Run: `cargo nextest run -p spike hooks`
Expected: PASS, 3 tests.

- [ ] **Step 3: Write the app**

Replace `spike/src/main.rs`:
```rust
use std::collections::HashMap;
use std::io::stdout;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::Context;
use ratatui::crossterm::event::{
    self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
    Event, KeyCode, KeyEventKind, KeyboardEnhancementFlags, MouseEventKind,
    PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use ratatui::crossterm::{execute, terminal};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Style, Stylize};
use ratatui::text::Line;
use ratatui::widgets::{Block, Paragraph};
use ratatui::{DefaultTerminal, Frame};

use spike::session::{PaneSize, Session};
use spike::{hooks, input, render};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Focus {
    Sidebar,
    Pane,
}

struct App {
    session: Session,
    label: String,
    focus: Focus,
    hook_log: Option<PathBuf>,
    status: &'static str,
    keyboard_enhanced: bool,
    pane: Rect,
    quit: bool,
}

fn main() -> anyhow::Result<()> {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() {
        args.push("claude".to_string());
    }
    let program = args.remove(0);

    let scratch = std::env::temp_dir().join(format!("lazyagents-spike-{}", std::process::id()));
    std::fs::create_dir_all(&scratch).context("creating spike temp dir")?;
    let is_claude = Path::new(&program).file_name().is_some_and(|name| name == "claude");
    let hook_log = if is_claude {
        let log = scratch.join("hooks.log");
        let settings = scratch.join("hooks.json");
        std::fs::write(&settings, hooks::settings_json(&log).to_string())?;
        args.push("--settings".to_string());
        args.push(settings.display().to_string());
        Some(log)
    } else {
        None
    };

    let (cols, rows) = terminal::size()?;
    let pane = pane_area(Rect::new(0, 0, cols, rows));
    let session = Session::spawn(
        &program,
        &args,
        &std::env::current_dir()?,
        HashMap::new(),
        PaneSize::new(pane.width, pane.height),
    )?;

    let mut terminal = ratatui::init();
    let keyboard_enhanced = terminal::supports_keyboard_enhancement().unwrap_or(false);
    execute!(stdout(), EnableBracketedPaste, EnableMouseCapture)?;
    if keyboard_enhanced {
        execute!(
            stdout(),
            PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
        )?;
    }

    let mut app = App {
        session,
        label: std::iter::once(program).chain(args).collect::<Vec<_>>().join(" "),
        focus: Focus::Pane,
        hook_log,
        status: "starting",
        keyboard_enhanced,
        pane,
        quit: false,
    };
    let result = run(&mut terminal, &mut app);

    if keyboard_enhanced {
        let _ = execute!(stdout(), PopKeyboardEnhancementFlags);
    }
    let _ = execute!(stdout(), DisableMouseCapture, DisableBracketedPaste);
    ratatui::restore();
    let _ = std::fs::remove_dir_all(&scratch);
    result
}

fn pane_area(screen: Rect) -> Rect {
    let [_, main] = Layout::horizontal([Constraint::Length(30), Constraint::Fill(1)]).areas(screen);
    Block::bordered().inner(main)
}

fn run(terminal: &mut DefaultTerminal, app: &mut App) -> anyhow::Result<()> {
    let mut last_status_read = Instant::now() - Duration::from_secs(1);
    while !app.quit {
        app.session.pump();
        if let Some(log) = &app.hook_log
            && last_status_read.elapsed() > Duration::from_millis(250)
        {
            app.status = hooks::status_from_log(&std::fs::read_to_string(log).unwrap_or_default());
            last_status_read = Instant::now();
        }
        terminal.draw(|frame| draw(frame, app))?;
        if event::poll(Duration::from_millis(16))? {
            handle(app, event::read()?);
        }
    }
    Ok(())
}

fn draw(frame: &mut Frame, app: &mut App) {
    let [sidebar, main] =
        Layout::horizontal([Constraint::Length(30), Constraint::Fill(1)]).areas(frame.area());

    let pane_border = if app.focus == Focus::Pane { Color::Green } else { Color::DarkGray };
    let sidebar_border = if app.focus == Focus::Sidebar { Color::Green } else { Color::DarkGray };

    let mode = app.session.mode();
    let flag = |on: bool| if on { "on" } else { "off" };
    let exit = match app.session.exit_status() {
        Some(status) => format!("exited ({})", status.code().map_or("signal".into(), |c| c.to_string())),
        None => "running".to_string(),
    };
    let focus_name = if app.focus == Focus::Pane { "PANE" } else { "SIDEBAR" };
    let lines = vec![
        Line::from(app.label.clone().bold()),
        Line::from(format!("process: {exit}")),
        Line::from(format!("status: {}", if app.hook_log.is_some() { app.status } else { "n/a (not claude)" })),
        Line::from(""),
        Line::from(format!("mode: {focus_name}")),
        Line::from(format!("kbd enhancement: {}", flag(app.keyboard_enhanced))),
        Line::from(format!("app cursor: {}", flag(mode.contains(alacritty_terminal::term::TermMode::APP_CURSOR)))),
        Line::from(format!("bracketed paste: {}", flag(mode.contains(alacritty_terminal::term::TermMode::BRACKETED_PASTE)))),
        Line::from(format!("mouse report: {}", flag(mode.intersects(alacritty_terminal::term::TermMode::MOUSE_MODE)))),
        Line::from(format!("alt screen: {}", flag(mode.contains(alacritty_terminal::term::TermMode::ALT_SCREEN)))),
        Line::from(""),
        Line::from("enter  focus pane".dark_gray()),
        Line::from("ctrl-] back to sidebar".dark_gray()),
        Line::from("q      quit (sidebar)".dark_gray()),
    ];
    frame.render_widget(
        Paragraph::new(lines).block(
            Block::bordered().title(" lazyagents spike ").border_style(Style::new().fg(sidebar_border)),
        ),
        sidebar,
    );

    let block = Block::bordered().title(" agent ").border_style(Style::new().fg(pane_border));
    let inner = block.inner(main);
    frame.render_widget(block, main);
    if inner != app.pane {
        app.pane = inner;
        app.session.resize(PaneSize::new(inner.width, inner.height));
    }
    let cursor = render::render_term(&app.session.term().lock(), inner, frame.buffer_mut());
    if app.focus == Focus::Pane
        && let Some(position) = cursor
    {
        frame.set_cursor_position(position);
    }
}

fn handle(app: &mut App, event: Event) {
    match event {
        Event::Key(key) if key.kind != KeyEventKind::Release => match app.focus {
            Focus::Sidebar => match key.code {
                KeyCode::Enter => app.focus = Focus::Pane,
                KeyCode::Char('q') => app.quit = true,
                _ => {}
            },
            Focus::Pane if input::is_detach(&key) => app.focus = Focus::Sidebar,
            Focus::Pane => {
                if app.session.exit_status().is_none()
                    && let Some(bytes) = input::encode_key(key, app.session.mode())
                {
                    app.session.scroll_to_bottom();
                    app.session.write(bytes);
                }
            }
        },
        Event::Paste(text) if app.focus == Focus::Pane => {
            app.session.scroll_to_bottom();
            app.session.write(input::encode_paste(&text, app.session.mode()));
        }
        Event::Mouse(mouse) if app.pane.contains((mouse.column, mouse.row).into()) => {
            let up = match mouse.kind {
                MouseEventKind::ScrollUp => true,
                MouseEventKind::ScrollDown => false,
                _ => return,
            };
            let (col, row) = (mouse.column - app.pane.x, mouse.row - app.pane.y);
            match input::encode_wheel(up, col, row, app.session.mode()) {
                Some(bytes) => app.session.write(bytes),
                None => app.session.scroll(if up { 3 } else { -3 }),
            }
        }
        _ => {}
    }
}
```

Run: `cargo build -p spike`
Expected: compiles with no warnings. Fix compile errors against the actual crate APIs (the task's tests pin behaviour; signatures may need small adjustments such as the `Stylize` import or `let`-chain syntax, which requires edition 2024).

- [ ] **Step 4: Full check and commit**

Run: `just check` → Expected: exit 0; spike tests: 18 input + 8 session + 7 render + 3 hooks = 36 passed, plus 1 cli test.

```bash
git add spike
git commit -m "feat(spike): host an agent beside a sidebar with focus switching"
```

---

### Task 9: End-to-end smoke, spike report, human gate

**Files:**
- Create: `scripts/smoke-spike.sh`, `docs/spike-report.md`
- Modify: `justfile` (add `smoke-spike`, extend `check`)

**Interfaces:**
- Consumes: `scripts/tmux-harness.sh` (Task 4), `target/debug/spike` (Task 8).

- [ ] **Step 1: Write the smoke script**

`scripts/smoke-spike.sh`:
```bash
#!/usr/bin/env bash
# End-to-end check of the spike: shell in the pane, typing, colours, detach, exit status, quit.
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
h="$root/scripts/tmux-harness.sh"
name="spike-smoke-$$"
trap '"$h" stop "$name"' EXIT

cargo build -q -p spike --manifest-path "$root/Cargo.toml"
"$h" start "$name" 120 30 -- "$root/target/debug/spike" sh
"$h" wait-for "$name" 'mode: PANE' 10
"$h" wait-for "$name" '$' 10

"$h" type "$name" 'echo lazy-$((40+2))'
"$h" keys "$name" Enter
"$h" wait-for "$name" 'lazy-42' 5

"$h" type "$name" 'printf "\033[1;31m%s\033[0m\n" colour-$((1+1))'
"$h" keys "$name" Enter
"$h" wait-for "$name" 'colour-2' 5

"$h" keys "$name" C-]
"$h" wait-for "$name" 'mode: SIDEBAR' 5
"$h" keys "$name" Enter
"$h" wait-for "$name" 'mode: PANE' 5

"$h" type "$name" 'exit 7'
"$h" keys "$name" Enter
"$h" wait-for "$name" 'exited (7)' 5

"$h" keys "$name" C-]
"$h" keys "$name" q
sleep 0.5
if tmux -L lazyagents-harness has-session -t "$name" 2>/dev/null \
  && tmux -L lazyagents-harness capture-pane -p -t "$name" | grep -q 'lazyagents spike'; then
  echo "spike did not quit on q" >&2
  exit 1
fi
echo "spike smoke passed"
```

Note: tmux's `new-session` runs the command directly; when the spike exits, the session closes, which is what the final check detects.

Run: `chmod +x scripts/smoke-spike.sh && scripts/smoke-spike.sh`
Expected: `spike smoke passed`. On a failed `wait-for`, the harness prints the screen; diagnose from it (skill: superpowers:systematic-debugging) before changing code.

- [ ] **Step 2: Wire it into `just check`**

In `justfile`:
```just
smoke-spike:
    scripts/smoke-spike.sh

check: fmt-check lint test test-hooks test-harness smoke-spike
```

Run: `just check` → Expected: exit 0, ending with `spike smoke passed`.

- [ ] **Step 3: Verify the `--settings` hook merge without a login**

Run: `claude --help | grep -A2 -- '--settings'`
Expected: the flag is documented as loading additional settings from a file or JSON string. Record the exact help text in the report. (The behavioural check with a live session is part of the human checklist below; automated runs never start a real `claude`.)

- [ ] **Step 4: Write `docs/spike-report.md`**

```markdown
# Spike report: embedded terminal (milestone 1)

Date: <today>
Plan: docs/superpowers/plans/2026-10-15-foundation-and-spike.md

## Automated results

- `just check`: <paste final lines: test counts, hook tests, harness, smoke>
- `claude --help` on `--settings`: <paste>

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

<executor: anything surprising from the automated runs; user: findings from the checklist>
```

Fill in the two `<paste>` fields and the date from Steps 2–3; leave the checklist unticked and the Findings section for the user beyond executor notes.

- [ ] **Step 5: Commit**

```bash
git add scripts/smoke-spike.sh justfile docs/spike-report.md
git commit -m "test(spike): add end-to-end smoke run and spike report"
```

- [ ] **Checkpoint B: spike verified, then STOP**

Run each and compare:
1. `just check` → exit 0; 37 tests passed (36 spike + 1 cli), `all hook tests passed`, `harness self-test passed`, `spike smoke passed`.
2. `git status --short` → empty.
3. `git log --oneline | head -5` → the four spike commits on top.

Result: _(executor pastes the tail of 1 and the output of 2–3 here)_

**Human gate.** Stop here. Report to the user: the checkpoint results, how to run `just spike`, and the checklist in `docs/spike-report.md`. Plan 2 (milestones 2–8) is written only after the user returns the checklist and a verdict. If the verdict is "not good enough", the fallback is full-terminal passthrough (ADR 0002 alternatives); that decision gets its own ADR.
