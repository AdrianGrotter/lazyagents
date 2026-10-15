#!/usr/bin/env bash
# PostToolUse: format the Rust file that was just edited; exit 2 hands syntax errors back to Claude.
set -euo pipefail

file=$(jq -r '.tool_input.file_path // empty')
[[ $file == *.rs && -f $file ]] || exit 0
command -v rustfmt >/dev/null || exit 0
rustfmt "$file" >&2 || exit 2
