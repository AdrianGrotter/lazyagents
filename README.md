# lazyagents

A [lazygit](https://github.com/jesseduffield/lazygit)-style terminal UI for managing many coding agents at once.

Instead of a tmux tab per Claude Code session, lazyagents shows every agent in one place: projects and agents in a sidebar with live status (working, needs input, idle, exited), and the selected agent's fully interactive terminal in the main pane. A background daemon keeps agents running when you close the UI. Agents can get their own git worktree and branch so parallel work does not collide.

Your own Claude Code setup (`~/.claude`, project `.claude/`, plugins, skills, MCP servers, hooks) keeps working: lazyagents runs the real `claude` binary.

## Status

Early development. The daemon, protocol, client and a headless CLI exist; the TUI is next, and the embedded-terminal spike (`just spike`) is the interim interactive UI. See `docs/architecture.md` and `docs/superpowers/specs/2026-10-15-lazyagents-design.md`.

## Usage (headless, until the TUI lands)

```sh
nix develop -c cargo build -p lazyagents
cd ~/some-project
~/git/lazyagents/target/debug/lazyagents new api    # start a Claude agent named api
~/git/lazyagents/target/debug/lazyagents ls         # projects and agents with status
~/git/lazyagents/target/debug/lazyagents kill api   # stop it
~/git/lazyagents/target/debug/lazyagents shutdown   # stop the daemon and every agent
```

## Development

```sh
direnv allow        # or: nix develop
just check          # fmt, clippy, tests, hook tests, harness and spike smoke runs
just spike          # run the spike with claude in the main pane
just spike sh       # ... or any other command

# or run the built binary from a project directory, outside the dev shell
nix develop -c cargo build -p spike
cd ~/some-project && ~/git/lazyagents/target/debug/spike
```

Contributor and agent guide: [AGENTS.md](AGENTS.md).
