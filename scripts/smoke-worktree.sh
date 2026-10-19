#!/usr/bin/env bash
# End-to-end run of a worktree agent on a private tmux server: create it with a worktree, dirty it, read the git stat and diff, remove it with its worktree, and check that the branch stays.
set -euo pipefail

# The daemons started here must not depend on the direnv state of the shell running this.
unset "${!DIRENV_@}"

root=$(cd "$(dirname "$0")/.." && pwd)
# shellcheck source=SCRIPTDIR/tmux-harness.sh
source "$root/scripts/tmux-harness.sh"
work=$(mktemp -d)
state="$work/state"
repo="$work/wt-smoke-repo"
name="wt-smoke-$$"
bin="$root/target/debug/lazyagents"
cleanup() {
  harness stop "$name"
  LAZYAGENTS_STATE_DIR="$state" "$bin" shutdown >/dev/null 2>&1 || true
  rm -rf "$work"
}
trap cleanup EXIT

cargo build --locked -q -p lazyagents --manifest-path "$root/Cargo.toml"

# Git, here and in the daemon and agent below, ignores the global and system config.
own_git() { GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_NOSYSTEM=1 git "$@"; }
own_git init -q -b main "$repo"
printf '[user]\n\tname = smoke\n\temail = smoke@example.com\n' >>"$repo/.git/config"
own_git -C "$repo" commit -q --allow-empty -m init

harness start "$name" 120 32 -- sh -c "cd '$repo' && exec env GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_NOSYSTEM=1 LAZYAGENTS_STATE_DIR='$state' LAZYAGENTS_CONFIG='$work/config.toml' SHELL=/bin/sh '$bin'"
harness wait-for "$name" 'press a to add a project' 20
harness keys "$name" a
harness wait-for "$name" ' Add project ' 5
harness keys "$name" Enter
harness wait-for "$name" '│wt-smoke-repo' 5

harness keys "$name" n
harness wait-for "$name" ' New agent · project ' 5
harness keys "$name" Enter
harness wait-for "$name" ' New agent in ' 5
harness type "$name" wt
harness keys "$name" Tab Right
harness wait-for "$name" '‹ shell ›' 5
harness keys "$name" Tab Space
harness keys "$name" Enter
harness wait-for "$name" ' wt · generic ' 10
harness wait-for "$name" '$' 10

harness type "$name" pwd Enter
harness wait-for "$name" '/worktrees/' 10
harness type "$name" 'echo hi > f.txt && git add f.txt' Enter

harness keys "$name" C-q q
harness wait-for "$name" 'n new agent · x stop' 5
harness wait-for "$name" 'branch  lazyagents/wt' 15
harness wait-for "$name" '1 uncommitted' 15

harness keys "$name" D
harness wait-for "$name" '+hi' 10
harness keys "$name" q

harness keys "$name" d
harness wait-for "$name" 'y: agent + worktree' 5
harness keys "$name" y
harness wait-for "$name" 'Worktree has uncommitted changes' 10
harness keys "$name" y
harness wait-for "$name" 'no agents — press n' 10

for dir in "$state"/worktrees/*/wt; do
  if [[ -e $dir ]]; then
    echo "worktree directory still exists" >&2
    exit 1
  fi
done
if [[ $(own_git -C "$repo" branch --list lazyagents/wt) != *lazyagents/wt* ]]; then
  echo "branch lazyagents/wt was deleted" >&2
  exit 1
fi

harness keys "$name" q
LAZYAGENTS_STATE_DIR="$state" "$bin" shutdown >/dev/null
echo "worktree smoke passed"
