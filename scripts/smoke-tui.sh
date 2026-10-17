#!/usr/bin/env bash
# End-to-end run of the TUI on a private tmux server: add project, start a shell agent, type, stop, remove, quit; then stop the daemon underneath a running TUI.
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
h="$root/scripts/tmux-harness.sh"
work=$(mktemp -d)
state="$work/state"
project="$work/tui-smoke-project"
mkdir -p "$project"
name="tui-smoke-$$"
bin="$root/target/debug/lazyagents"
cleanup() {
  "$h" stop "$name"
  "$h" stop "$name-2"
  LAZYAGENTS_STATE_DIR="$state" "$bin" shutdown >/dev/null 2>&1 || true
  rm -rf "$work"
}
trap cleanup EXIT

cargo build --locked -q -p lazyagents --manifest-path "$root/Cargo.toml"
launch() {
  "$h" start "$1" 120 32 -- sh -c "cd '$project' && exec env LAZYAGENTS_STATE_DIR='$state' LAZYAGENTS_CONFIG='$work/config.toml' SHELL=/bin/sh '$bin'"
}

launch "$name"
"$h" wait-for "$name" 'press a to add a project' 20

"$h" keys "$name" a
"$h" wait-for "$name" ' Add project ' 5
"$h" keys "$name" Enter
"$h" wait-for "$name" '▸ tui-smoke-project' 5

"$h" keys "$name" n
"$h" wait-for "$name" ' New agent ' 5
"$h" type "$name" demo
"$h" keys "$name" Tab Right
"$h" wait-for "$name" '‹ shell ›' 5
"$h" keys "$name" Enter
"$h" wait-for "$name" ' demo · generic ' 10
"$h" wait-for "$name" '$' 10

"$h" type "$name" 'echo tui-$((40+2))'
"$h" keys "$name" Enter
"$h" wait-for "$name" 'tui-42' 10

"$h" type "$name" 'exit 3'
"$h" keys "$name" Enter
"$h" wait-for "$name" 'exited (3) · enter restart · d remove' 10
"$h" keys "$name" Enter
"$h" wait-for "$name" 'ctrl-q q/esc sidebar · ctrl-q j next agent' 10
"$h" type "$name" 'echo back-$((40+3))'
"$h" keys "$name" Enter
"$h" wait-for "$name" 'back-43' 10

"$h" keys "$name" C-q q
"$h" wait-for "$name" 'n new · x stop' 5
"$h" keys "$name" Enter
"$h" wait-for "$name" ' demo · generic · ' 5
"$h" keys "$name" C-]
"$h" wait-for "$name" 'n new · x stop' 5
"$h" keys "$name" x
"$h" wait-for "$name" 'Stop demo?' 5
"$h" keys "$name" y
"$h" wait-for "$name" 'stopped' 10

"$h" keys "$name" d
"$h" wait-for "$name" 'Remove demo?' 5
"$h" keys "$name" y
"$h" wait-for "$name" 'no agents — press n' 10

"$h" keys "$name" q
for _ in $(seq 25); do
  "$h" alive "$name" || break
  sleep 0.2
done
if "$h" alive "$name"; then
  echo "TUI did not quit on q" >&2
  "$h" capture "$name" >&2
  exit 1
fi

launch "$name-2"
"$h" wait-for "$name-2" 'tui-smoke-project' 20
LAZYAGENTS_STATE_DIR="$state" "$bin" shutdown >/dev/null
for _ in $(seq 25); do
  "$h" alive "$name-2" || break
  sleep 0.2
done
if "$h" alive "$name-2"; then
  echo "TUI kept running after the daemon stopped" >&2
  "$h" capture "$name-2" >&2
  exit 1
fi
echo "tui smoke passed"
