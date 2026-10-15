# lazyagents

A [lazygit](https://github.com/jesseduffield/lazygit)-style terminal UI for managing many coding agents at once.

Instead of a tmux tab per Claude Code session, lazyagents shows every agent in one place: projects and agents in a sidebar with live status (working, needs input, idle, exited), and the selected agent's fully interactive terminal in the main pane. A background daemon keeps agents running when you close the UI. Agents can get their own git worktree and branch so parallel work does not collide.

Your own Claude Code setup (`~/.claude`, project `.claude/`, plugins, skills, MCP servers, hooks) keeps working: lazyagents runs the real `claude` binary.

## Status

Early development. The daemon, the CLI and the TUI exist. Git worktrees per agent, the diff view, the config file (keybindings, presets) and text selection come next. See `docs/architecture.md` and `docs/superpowers/specs/2026-10-15-lazyagents-design.md`.

## Usage

```sh
nix develop -c cargo build -p lazyagents
cd ~/some-project
~/git/lazyagents/target/debug/lazyagents    # open the TUI
```

The TUI starts the daemon if none is running. Quitting it leaves the agents running; run `lazyagents` again to reattach.

| Key | Action |
|---|---|
| `a` | add a project (defaults to the current directory) |
| `n` | new agent: name and preset (claude, shell, custom command) |
| `enter` | open the selected agent's terminal; all keys go to the agent |
| `ctrl-]` | back to the sidebar |
| `x` / `d` | stop / remove the selected agent |
| `?` | all keys |
| `q` | quit the TUI; agents keep running |

The full key map is in [docs/keybindings.md](docs/keybindings.md).

The same daemon is scriptable without the TUI:

```sh
lazyagents new api        # start a Claude agent named api in the current project
lazyagents ls             # projects and agents with status
lazyagents kill api       # stop it
lazyagents shutdown       # stop the daemon and every agent
```

## Development

```sh
direnv allow        # or: nix develop
just check          # fmt, clippy, tests, hook tests, harness self-test, TUI smoke and hangup runs
just dev            # run the TUI from any directory
just dev ls         # ... or any CLI subcommand
```

Experiment with a throwaway state dir so your real agents stay untouched: `LAZYAGENTS_STATE_DIR=$(mktemp -d) just dev`.

Contributor and agent guide: [AGENTS.md](AGENTS.md).
