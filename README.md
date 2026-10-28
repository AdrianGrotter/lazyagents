# lazyagents

A [lazygit](https://github.com/jesseduffield/lazygit)-style terminal UI for managing many coding agents at once.

Instead of a tmux tab per Claude Code session, lazyagents shows every agent in one place: projects and agents in a sidebar with live status (working, needs input, idle, exited), and the selected agent's fully interactive terminal in the main pane. A background daemon keeps agents running when you close the UI. Agents can get their own git worktree and branch so parallel work does not collide.

Your own Claude Code setup (`~/.claude`, project `.claude/`, plugins, skills, MCP servers, hooks) keeps working: lazyagents runs the real `claude` binary.

## Status

A personal project, shared as is. It is not maintained as a product: there is no release schedule and no support, and anything (config, CLI, on-disk state) can change without notice. Issues and pull requests may go unanswered.

What exists: a daemon that keeps agents running, a CLI and the TUI, git worktrees per agent, a diff view, a config file (keybindings, presets, theme) and mouse selection with copy. How it fits together: [docs/architecture.md](docs/architecture.md).

## Install

On macOS, with [Homebrew](https://brew.sh):

```sh
brew install AdrianGrotter/tap/lazyagents
brew upgrade lazyagents        # later, to upgrade
```

Claude agents need [Claude Code](https://docs.claude.com/en/docs/claude-code) (`claude` on your `PATH`). [direnv](https://direnv.net) is used when present.

Run it as `lazyagents` from your `PATH`, not through the versioned path under Homebrew's `Cellar`: agents call `lazyagents hook` through the path the daemon was started from, and only the `PATH` one survives an upgrade.

Linux works too, built from source (see [Building from source](#building-from-source)).

## Usage

```sh
cd ~/some-project
lazyagents                     # open the TUI
```

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

An agent created with the worktree option runs in its own git worktree on the branch `lazyagents/<name>`, under the state directory. Removing the agent can remove the worktree, and the finish menu (`F`) can merge the branch, open a pull request, or delete the branch once it is merged.

The same daemon is scriptable without the TUI. An agent argument is its id, `<project>/<name>`, a name that is unique across projects, or an id prefix of at least 4 hex characters:

```sh
lazyagents new api        # start a Claude agent named api in the current project
lazyagents new fix --prompt "fix the failing parser test"   # start one with a task
lazyagents ls             # projects and agents with status
lazyagents kill api       # stop it
lazyagents shutdown       # stop the daemon and every agent
```

## Building from source

With [Nix](https://nixos.org):

```sh
nix develop -c just install    # release build to ~/.local/bin/lazyagents
```

On Linux the build is relinked to the system's loader and libraries and installed only if it runs that way; when the system's glibc is older than the nix shell's, the script stops and says how to build with rustup instead.

Without Nix, with a [rustup](https://rustup.rs) toolchain:

```sh
cargo install --locked --path crates/lazyagents --root ~/.local
```

Either way `~/.local/bin` must be on your `PATH`; `LAZYAGENTS_INSTALL_ROOT` makes `just install` use `$LAZYAGENTS_INSTALL_ROOT/bin` instead. Install again after pulling to upgrade, to the same path, so agents started before an upgrade keep reporting their status.

## Development

```sh
direnv allow        # or: nix develop
just check          # fmt, clippy, tests, hook tests, harness self-test, TUI smoke, hangup and worktree runs
just dev            # run the TUI from any directory
just dev ls         # ... or any CLI subcommand
```

Experiment with a throwaway state dir so your real agents stay untouched: `LAZYAGENTS_STATE_DIR=$(mktemp -d) just dev`.

Contributor and agent guide: [AGENTS.md](AGENTS.md).

## License

[MIT](LICENSE). `vendor/crossterm` is a patched copy of [crossterm](https://github.com/crossterm-rs/crossterm), MIT licensed by its authors.
