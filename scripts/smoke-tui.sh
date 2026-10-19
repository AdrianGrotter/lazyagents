#!/usr/bin/env bash
# End-to-end run of the TUI on a private tmux server: add project, start a shell agent, type, stop, remove, quit; then stop the daemon underneath a running TUI.
set -euo pipefail

# The daemons started here must not depend on the direnv state of the shell running this.
unset "${!DIRENV_@}"

root=$(cd "$(dirname "$0")/.." && pwd)
# shellcheck source=SCRIPTDIR/tmux-harness.sh
source "$root/scripts/tmux-harness.sh"
work=$(mktemp -d)
state="$work/state"
project="$work/tui-smoke-project"
mkdir -p "$project"
name="tui-smoke-$$"
bin="$root/target/debug/lazyagents"
cleanup() {
  harness stop "$name"
  harness stop "$name-2"
  LAZYAGENTS_STATE_DIR="$state" "$bin" shutdown >/dev/null 2>&1 || true
  rm -rf "$work"
}
trap cleanup EXIT

cargo build --locked -q -p lazyagents --manifest-path "$root/Cargo.toml"
launch() {
  harness start "$1" 120 32 -- sh -c "cd '$project' && exec env LAZYAGENTS_STATE_DIR='$state' LAZYAGENTS_CONFIG='$work/config.toml' SHELL=/bin/sh '$bin'"
}

launch "$name"
harness wait-for "$name" 'press a to add a project' 20

harness keys "$name" a
harness wait-for "$name" ' Add project ' 5
harness keys "$name" Enter
harness wait-for "$name" '▸ tui-smoke-project' 5

harness keys "$name" n
harness wait-for "$name" ' New agent ' 5
harness type "$name" demo
harness keys "$name" Tab Right
harness wait-for "$name" '‹ shell ›' 5
harness keys "$name" Enter
harness wait-for "$name" ' demo · generic ' 10
harness wait-for "$name" '$' 10

harness type "$name" 'echo tui-$((40+2))' Enter
harness wait-for "$name" 'tui-42' 10

harness type "$name" 'exit 3' Enter
harness wait-for "$name" 'exited (3) · enter restart · d remove' 10
harness keys "$name" Enter
harness wait-for "$name" 'ctrl-q q/esc sidebar · ctrl-q j next agent' 10
harness type "$name" 'echo back-$((40+3))' Enter
harness wait-for "$name" 'back-43' 10

harness keys "$name" C-q q
harness wait-for "$name" 'n new agent · x stop' 5
harness keys "$name" Enter
harness wait-for "$name" ' demo · generic · ' 5
harness keys "$name" C-]
harness wait-for "$name" 'n new agent · x stop' 5
harness keys "$name" x
harness wait-for "$name" 'Stop demo?' 5
harness keys "$name" y
harness wait-for "$name" 'stopped' 10

harness keys "$name" d
harness wait-for "$name" 'Remove demo?' 5
harness keys "$name" y
harness wait-for "$name" 'no agents — press n' 10

harness keys "$name" q
if ! harness wait-gone "$name"; then
  echo "TUI did not quit on q" >&2
  harness capture "$name" >&2
  exit 1
fi

launch "$name-2"
harness wait-for "$name-2" 'tui-smoke-project' 20
LAZYAGENTS_STATE_DIR="$state" "$bin" shutdown >/dev/null
if ! harness wait-gone "$name-2"; then
  echo "TUI kept running after the daemon stopped" >&2
  harness capture "$name-2" >&2
  exit 1
fi
echo "tui smoke passed"
