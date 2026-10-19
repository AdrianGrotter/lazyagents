#!/usr/bin/env bash
# Stop: keep Claude working while clippy fails on uncommitted Rust changes.
set -euo pipefail

input=$(</dev/stdin)
[[ $(jq -r '.stop_hook_active // false' <<<"$input") == true ]] && exit 0
cd "${CLAUDE_PROJECT_DIR:-.}"
paths=('*.rs' '*Cargo.toml' 'Cargo.lock')
changed=$(git status --porcelain -- "${paths[@]}")
[[ -n $changed ]] || exit 0

fingerprint() {
  {
    git rev-parse HEAD 2>/dev/null || true
    git diff HEAD --binary -- "${paths[@]}" 2>/dev/null || true
    git ls-files --others --exclude-standard -z -- "${paths[@]}" |
      while IFS= read -r -d '' f; do printf '%s %s\n' "$(git hash-object -- "$f")" "$f"; done
  } | git hash-object --stdin
}
stamp=$(git rev-parse --git-path lazyagents-clippy-passed)
current=$(fingerprint)
[[ -f $stamp && $(<"$stamp") == "$current" ]] && exit 0

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
printf '%s\n' "$current" >"$stamp"
