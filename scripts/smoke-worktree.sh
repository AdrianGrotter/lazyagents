#!/usr/bin/env bash
# End-to-end run of a worktree agent on a private tmux server: create it with a worktree, dirty it, read the git stat and diff, remove it with its worktree, and check that the branch stays.
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
h="$root/scripts/tmux-harness.sh"
work=$(mktemp -d)
state="$work/state"
repo="$work/wt-smoke-repo"
name="wt-smoke-$$"
bin="$root/target/debug/lazyagents"
cleanup() {
  "$h" stop "$name"
  LAZYAGENTS_STATE_DIR="$state" "$bin" shutdown >/dev/null 2>&1 || true
  rm -rf "$work"
}
trap cleanup EXIT

cargo build --locked -q -p lazyagents --manifest-path "$root/Cargo.toml"

# The script's own git runs ignore the user's global and system config.
own_git() { GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_NOSYSTEM=1 git "$@"; }
own_git init -q -b main "$repo"
own_git -C "$repo" config user.name smoke
own_git -C "$repo" config user.email smoke@example.com
own_git -C "$repo" commit -q --allow-empty -m init

"$h" start "$name" 120 32 -- sh -c "cd '$repo' && exec env LAZYAGENTS_STATE_DIR='$state' LAZYAGENTS_CONFIG='$work/config.toml' SHELL=/bin/sh '$bin'"
"$h" wait-for "$name" 'press a to add a project' 20
"$h" keys "$name" a
"$h" wait-for "$name" ' Add project ' 5
"$h" keys "$name" Enter
"$h" wait-for "$name" '▸ wt-smoke-repo' 5

"$h" keys "$name" n
"$h" wait-for "$name" ' New agent ' 5
"$h" type "$name" wt
"$h" keys "$name" Tab Right
"$h" wait-for "$name" '‹ shell ›' 5
"$h" keys "$name" Tab Space
"$h" keys "$name" Enter
"$h" wait-for "$name" ' wt · generic ' 10
"$h" wait-for "$name" '$' 10

"$h" type "$name" pwd
"$h" keys "$name" Enter
"$h" wait-for "$name" '/worktrees/' 10
"$h" type "$name" 'echo hi > f.txt && git add f.txt'
"$h" keys "$name" Enter

"$h" keys "$name" C-q q
"$h" wait-for "$name" 'n new · x stop' 5
"$h" wait-for "$name" 'branch  lazyagents/wt' 15
"$h" wait-for "$name" '1 uncommitted' 15

"$h" keys "$name" D
"$h" wait-for "$name" '+hi' 10
"$h" keys "$name" q

"$h" keys "$name" d
"$h" wait-for "$name" 'y: agent + worktree' 5
"$h" keys "$name" y
"$h" wait-for "$name" 'Worktree has uncommitted changes' 10
"$h" keys "$name" y
"$h" wait-for "$name" 'no agents — press n' 10

if [[ -n $(find "$state/worktrees" -mindepth 2 -maxdepth 2 -name wt) ]]; then
  echo "worktree directory still exists" >&2
  exit 1
fi
if [[ $(own_git -C "$repo" branch --list lazyagents/wt) != *lazyagents/wt* ]]; then
  echo "branch lazyagents/wt was deleted" >&2
  exit 1
fi

"$h" keys "$name" q
LAZYAGENTS_STATE_DIR="$state" "$bin" shutdown >/dev/null
echo "worktree smoke passed"
