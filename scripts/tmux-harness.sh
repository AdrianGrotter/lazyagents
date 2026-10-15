#!/usr/bin/env bash
# Drives a TUI inside a detached session on a private tmux server for scripted end-to-end checks.
set -euo pipefail

usage() {
  cat >&2 <<'USAGE'
usage: tmux-harness.sh start <name> <cols> <rows> -- <command...>
       tmux-harness.sh type <name> <text>        send literal text
       tmux-harness.sh keys <name> <key>...      send tmux key names: Enter C-] M-Enter Escape
       tmux-harness.sh capture <name>            print the visible screen
       tmux-harness.sh wait-for <name> <text> [timeout_secs=10]
       tmux-harness.sh stop <name>
USAGE
  exit 2
}

t() { tmux -L lazyagents-harness "$@"; }

cmd=${1:-}
name=${2:-}
[[ -n $cmd && -n $name ]] || usage

case $cmd in
  start)
    [[ $# -ge 5 ]] || usage
    cols=$3 rows=$4
    shift 4
    [[ ${1:-} == -- ]] && shift
    t new-session -d -s "$name" -x "$cols" -y "$rows" "$@"
    ;;
  type) t send-keys -t "$name" -l "${3:?text}" ;;
  keys)
    shift 2
    t send-keys -t "$name" "$@"
    ;;
  capture) t capture-pane -p -t "$name" ;;
  wait-for)
    text=${3:?text}
    deadline=$((SECONDS + ${4:-10}))
    while ((SECONDS < deadline)); do
      if t capture-pane -p -t "$name" | grep -qF -- "$text"; then exit 0; fi
      sleep 0.2
    done
    echo "timed out waiting for: $text" >&2
    t capture-pane -p -t "$name" >&2
    exit 1
    ;;
  stop) t kill-session -t "$name" 2>/dev/null || true ;;
  *) usage ;;
esac
