#!/usr/bin/env bash
# Drives a TUI inside a detached session on a private tmux server for scripted end-to-end checks.
# The server is $LAZYAGENTS_HARNESS_SERVER (default lazyagents-harness); concurrent runs, such
# as `just check` in two worktrees, each set their own.
#
# Run it as a command, or source it from a bash script and call `harness <command> ...` with the
# same arguments: that saves launching bash for every call, and `harness_sleep` waits without
# launching `sleep`.

_harness_usage() {
  cat >&2 <<'USAGE'
usage: tmux-harness.sh start <name> <cols> <rows> -- <command...>
       tmux-harness.sh type <name> <text> [key...]   send literal text, then tmux key names
       tmux-harness.sh keys <name> <key>...          send tmux key names: Enter C-] M-Enter Escape
       tmux-harness.sh capture <name>                print the visible screen
       tmux-harness.sh wait-for <name> <text> [timeout_secs=10]
       tmux-harness.sh wait-gone <name> [timeout_secs=10]
       tmux-harness.sh alive <name>                  exit 0 iff the session exists
       tmux-harness.sh stop <name>
USAGE
}

_harness_tmux() { tmux -L "${LAZYAGENTS_HARNESS_SERVER:-lazyagents-harness}" -f /dev/null "$@"; }

# Bash 4 and later time out a read on a pipe from a coprocess that never writes; the
# coprocess ends when this shell does. The eval lets bash 3.2, which has no coproc, parse this.
harness_sleep() {
  if ((BASH_VERSINFO[0] < 4)); then
    sleep "$1"
    return
  fi
  if [[ -z ${_HARNESS_SLEEP_PID:-} ]]; then
    eval 'coproc _HARNESS_SLEEP { read -r; }'
  fi
  read -rt "$1" -u "${_HARNESS_SLEEP[0]}" || true
}

harness() {
  local cmd=${1:-} name=${2:-}
  if [[ -z $cmd || -z $name ]]; then
    _harness_usage
    return 2
  fi
  case $cmd in
    start)
      if [[ $# -lt 5 ]]; then
        _harness_usage
        return 2
      fi
      local cols=$3 rows=$4
      shift 4
      [[ ${1:-} == -- ]] && shift
      _harness_tmux new-session -d -s "$name" -x "$cols" -y "$rows" "$@"
      ;;
    type)
      local text=${3:?text}
      shift 3
      if [[ $# -gt 0 ]]; then
        _harness_tmux send-keys -t "$name" -l "$text" \; send-keys -t "$name" "$@"
      else
        _harness_tmux send-keys -t "$name" -l "$text"
      fi
      ;;
    keys)
      shift 2
      _harness_tmux send-keys -t "$name" "$@"
      ;;
    capture) _harness_tmux capture-pane -p -t "$name" ;;
    wait-for)
      local text=${3:?text} deadline=$((SECONDS + ${4:-10})) screen
      until screen=$(_harness_tmux capture-pane -p -t "$name") && [[ $screen == *"$text"* ]]; do
        if ((SECONDS >= deadline)); then
          echo "timed out waiting for: $text" >&2
          printf '%s\n' "$screen" >&2
          return 1
        fi
        harness_sleep 0.2
      done
      ;;
    wait-gone)
      local deadline=$((SECONDS + ${3:-10}))
      while _harness_tmux has-session -t "$name" 2>/dev/null; do
        ((SECONDS < deadline)) || return 1
        harness_sleep 0.2
      done
      ;;
    alive) _harness_tmux has-session -t "$name" 2>/dev/null ;;
    stop) _harness_tmux kill-session -t "$name" 2>/dev/null || true ;;
    *)
      _harness_usage
      return 2
      ;;
  esac
}

if [[ ${BASH_SOURCE[0]} == "$0" ]]; then
  set -euo pipefail
  harness "$@"
fi
