#!/usr/bin/env bash
# End-to-end check of the spike: shell in the pane, typing, SGR output text, detach, exit status, quit.
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
h="$root/scripts/tmux-harness.sh"
name="spike-smoke-$$"
trap '"$h" stop "$name"' EXIT

cargo build --locked -q -p spike --manifest-path "$root/Cargo.toml"
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
"$h" wait-for "$name" 'mode: SIDEBAR' 5
"$h" keys "$name" q
for _ in $(seq 25); do
  "$h" alive "$name" || { echo "spike smoke passed"; exit 0; }
  sleep 0.2
done
echo "spike did not quit on q" >&2
"$h" capture "$name" >&2
exit 1
