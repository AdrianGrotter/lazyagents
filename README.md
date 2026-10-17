# lazyagents

A [lazygit](https://github.com/jesseduffield/lazygit)-style terminal UI for managing many coding agents at once.

Instead of a tmux tab per Claude Code session, lazyagents shows every agent in one place: projects and agents in a sidebar with live status (working, needs input, idle, exited), and the selected agent's fully interactive terminal in the main pane. A background daemon keeps agents running when you close the UI. Agents can get their own git worktree and branch so parallel work does not collide.

Your own Claude Code setup (`~/.claude`, project `.claude/`, plugins, skills, MCP servers, hooks) keeps working: lazyagents runs the real `claude` binary.

## Status

Early development. The daemon, the CLI and the TUI exist, with git worktrees per agent, a diff view, a config file (keybindings, presets, theme) and mouse selection with copy. See `docs/architecture.md` and `docs/superpowers/specs/2026-10-15-lazyagents-design.md`.

## Usage

```sh
nix develop -c just install    # release build to ~/.local/bin/lazyagents
cd ~/some-project
lazyagents                     # open the TUI
```

`~/.local/bin` must be on your `PATH`. Run `just install` again after pulling to upgrade. The install path stays the same across upgrades, which matters: Claude agents call `lazyagents hook` through that path, so agents started before an upgrade keep reporting their status.

The TUI starts the daemon if none is running. Quitting it leaves the agents running; run `lazyagents` again to reattach.

| Key | Action |
|---|---|
| `a` | add a project (defaults to the current directory) |
| `n` | new agent: name, preset (claude, shell, custom, your own), and an optional git worktree |
| `enter` | open the selected agent's terminal; all keys go to the agent |
| `ctrl-q q` or `ctrl-]` | back to the sidebar |
| `ctrl-q j` / `ctrl-q k` | next / previous agent without leaving the terminal |
| `D` | diff of the agent's changes |
| `s` | scroll the agent's terminal history |
| `x` / `d` | stop / remove the selected agent |
| `?` | all keys |
| `q` | quit the TUI; agents keep running |

The mouse works too: click to focus, drag in the terminal to copy. Keys, presets and colours are configurable in `~/.config/lazyagents/config.toml` ([docs/configuration.md](docs/configuration.md)). The full key map is in [docs/keybindings.md](docs/keybindings.md).

An agent created with the worktree option runs in its own git worktree on the branch `lazyagents/<name>`, under the state directory. Removing the agent can remove the worktree; the branch always stays.

The same daemon is scriptable without the TUI:

```sh
lazyagents new api        # start a Claude agent named api in the current project
lazyagents new fix --prompt "fix the failing parser test"   # start one with a task
lazyagents ls             # projects and agents with status
lazyagents kill api       # stop it
lazyagents shutdown       # stop the daemon and every agent
```

## Development

```sh
direnv allow        # or: nix develop
just check          # fmt, clippy, tests, hook tests, harness self-test, TUI smoke, hangup and worktree runs
just dev            # run the TUI from any directory
just dev ls         # ... or any CLI subcommand
```

Experiment with a throwaway state dir so your real agents stay untouched: `LAZYAGENTS_STATE_DIR=$(mktemp -d) just dev`.

Contributor and agent guide: [AGENTS.md](AGENTS.md).
