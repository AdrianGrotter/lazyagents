#!/usr/bin/env bash
# The spike must exit when its terminal disappears, even when no hangup or termination signal reaches it.
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
h="$root/scripts/tmux-harness.sh"
name="spike-hangup-$$"
marker="hangup-marker-$$"
trap '"$h" stop "$name"; pkill -9 -f "$marker" 2>/dev/null || true' EXIT

cargo build --locked -q -p spike --manifest-path "$root/Cargo.toml"
"$h" start "$name" 100 30 -- sh -c "trap '' HUP TERM; exec '$root/target/debug/spike' sh -c 'echo $marker; sleep 300'"
"$h" wait-for "$name" "$marker" 10
"$h" stop "$name"

for _ in $(seq 25); do
  if ! pgrep -f "spike sh -c echo $marker" >/dev/null; then
    echo "spike hangup test passed"
    exit 0
  fi
  sleep 0.2
done
echo "spike kept running after its terminal closed:" >&2
ps -o pid,%cpu,command -p "$(pgrep -f "spike sh -c echo $marker" | head -1)" >&2
exit 1
