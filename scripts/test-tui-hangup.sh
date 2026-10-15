#!/usr/bin/env bash
# The TUI must exit cleanly (status 0) when its terminal disappears: once with hangup,
# termination and interrupt blocked so only the dead tty can end it, and once through
# the signal path.
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
h="$root/scripts/tmux-harness.sh"
bin="$root/target/debug/lazyagents"
work=$(mktemp -d)
state="$work/state"
project="$work/project"
link="$work/lazyagents-hangup-$$"
name="tui-hangup-$$"
block="use POSIX; sigprocmask(SIG_BLOCK, POSIX::SigSet->new(SIGHUP, SIGTERM, SIGINT)); exec @ARGV"
tui_pid=""

cleanup() {
  "$h" stop "$name"
  if [[ -n $tui_pid ]] && kill -0 "$tui_pid" 2>/dev/null; then kill -9 "$tui_pid" 2>/dev/null || true; fi
  LAZYAGENTS_STATE_DIR="$state" "$bin" shutdown >/dev/null 2>&1 || true
  rm -rf "$work"
}
trap cleanup EXIT

cargo build --locked -q -p lazyagents --manifest-path "$root/Cargo.toml"
mkdir -p "$project"
ln -s "$bin" "$link"

scenario() {
  local label=$1 wrapper=$2
  rm -f "$work/status"
  "$h" start "$name" 100 30 -- sh -c \
    "trap '' HUP; cd '$project' && env LAZYAGENTS_STATE_DIR='$state' LAZYAGENTS_CONFIG='$work/config.toml' SHELL=/bin/sh $wrapper '$link'; echo \$? > '$work/status'"
  "$h" wait-for "$name" "press a to add a project" 10
  tui_pid=$(pgrep -fx "$link")
  "$h" stop "$name"

  for _ in $(seq 25); do
    if ! kill -0 "$tui_pid" 2>/dev/null && [[ -s "$work/status" ]]; then break; fi
    sleep 0.2
  done
  if kill -0 "$tui_pid" 2>/dev/null; then
    echo "$label: the TUI kept running after its terminal closed" >&2
    exit 1
  fi
  local status
  status=$(cat "$work/status" 2>/dev/null || echo missing)
  if [[ $status != 0 ]]; then
    echo "$label: the TUI exited with status $status" >&2
    exit 1
  fi
  tui_pid=""
}

scenario "signals blocked" "perl -e '$block' --"
scenario "signals delivered" ""
echo "tui hangup test passed"
