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
