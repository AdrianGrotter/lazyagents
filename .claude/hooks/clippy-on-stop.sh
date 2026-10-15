#!/usr/bin/env bash
# Stop: keep Claude working while clippy fails on uncommitted Rust changes.
set -euo pipefail

input=$(cat)
[[ $(jq -r '.stop_hook_active // false' <<<"$input") == true ]] && exit 0
cd "${CLAUDE_PROJECT_DIR:-.}"
changed=$(git status --porcelain -- '*.rs' '*Cargo.toml' 'Cargo.lock')
[[ -n $changed ]] || exit 0

run() {
  if command -v cargo >/dev/null; then "$@"; else nix develop --command "$@"; fi
}
if ! out=$(run cargo clippy --workspace --all-targets --locked --message-format short -- -D warnings 2>&1); then
  {
    echo "cargo clippy fails on the current changes; fix these before finishing:"
    tail -n 80 <<<"$out"
  } >&2
  exit 2
fi
