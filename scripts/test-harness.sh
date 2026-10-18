#!/usr/bin/env bash
# Self-test for tmux-harness.sh against a plain shell, on a server named through
# LAZYAGENTS_HARNESS_SERVER whose socket lives in a temp dir of its own.
set -euo pipefail

h="$(dirname "$0")/tmux-harness.sh"
name="harness-selftest"
export TMUX_TMPDIR
TMUX_TMPDIR=$(mktemp -d)
export LAZYAGENTS_HARNESS_SERVER="lazyagents-harness-selftest"
trap '"$h" stop "$name"; rm -rf "$TMUX_TMPDIR"' EXIT

"$h" start "$name" 80 24 -- sh
if ! tmux -L "$LAZYAGENTS_HARNESS_SERVER" -f /dev/null has-session -t "$name" 2>/dev/null; then
  echo "the session is not on server $LAZYAGENTS_HARNESS_SERVER" >&2
  exit 1
fi
"$h" type "$name" 'echo harness-$((20+22))'
"$h" keys "$name" Enter
"$h" wait-for "$name" 'harness-42' 5
if "$h" wait-for "$name" 'never-printed' 1 2>/dev/null; then
  echo "wait-for should time out on absent text" >&2
  exit 1
fi
echo "harness self-test passed"
