#!/usr/bin/env bash
# Self-test for tmux-harness.sh, run as a command and sourced, against a plain shell on a server
# named through LAZYAGENTS_HARNESS_SERVER whose socket lives in a temp dir of its own.
set -euo pipefail

h="$(dirname "$0")/tmux-harness.sh"
name="harness-selftest"
export TMUX_TMPDIR
TMUX_TMPDIR=$(mktemp -d)
export LAZYAGENTS_HARNESS_SERVER="lazyagents-harness-selftest"
trap '"$h" stop "$name"; rm -rf "$TMUX_TMPDIR"' EXIT

fail() {
  echo "$1" >&2
  exit 1
}

"$h" start "$name" 80 24 -- sh
if ! tmux -L "$LAZYAGENTS_HARNESS_SERVER" -f /dev/null has-session -t "$name" 2>/dev/null; then
  fail "the session is not on server $LAZYAGENTS_HARNESS_SERVER"
fi
"$h" type "$name" 'echo harness-$((20+22))'
"$h" keys "$name" Enter
"$h" wait-for "$name" 'harness-42' 5
if "$h" wait-for "$name" 'never-printed' 1 2>/dev/null; then
  fail "wait-for should time out on absent text"
fi

# shellcheck source=SCRIPTDIR/tmux-harness.sh
source "$h"
harness type "$name" 'echo sourced-$((40+3))' Enter
harness wait-for "$name" 'sourced-43' 5
if harness wait-for "$name" 'never-printed' 0 2>/dev/null; then
  fail "sourced wait-for should return 1 on absent text"
fi
if harness wait-gone "$name" 0; then
  fail "wait-gone should time out while the session runs"
fi
harness type "$name" exit Enter
harness wait-gone "$name" 5 || fail "wait-gone should see the session end"
if harness alive "$name"; then
  fail "the session outlived its shell"
fi
echo "harness self-test passed"
