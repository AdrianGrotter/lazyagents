#!/usr/bin/env bash
# Exercises .githooks and .claude/hooks with canned inputs.
set -uo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
failures=0

expect() {
  local want=$1 desc=$2
  shift 2
  "$@" >"$tmp/out" 2>&1
  local got=$?
  if [[ $got == "$want" ]]; then
    echo "ok   $desc"
  else
    echo "FAIL $desc (exit $got, want $want)"
    sed 's/^/     /' "$tmp/out"
    failures=$((failures + 1))
  fi
}

msg() { printf '%s\n' "$1" >"$tmp/msg"; "$root/.githooks/commit-msg" "$tmp/msg"; }
expect 0 "commit-msg accepts typed scoped subject" msg "feat(daemon): spawn agents in a pty"
expect 0 "commit-msg accepts unscoped subject" msg "docs: describe status detection"
bare_msg() { printf '%s' "$1" >"$tmp/msg"; "$root/.githooks/commit-msg" "$tmp/msg"; }
expect 0 "commit-msg accepts a subject without a trailing newline" bare_msg "docs: describe status detection"
expect 0 "commit-msg accepts breaking marker" msg "feat(protocol)!: version the handshake"
expect 0 "commit-msg accepts merge commits" msg "Merge branch 'x'"
expect 0 "commit-msg accepts merged pull requests" msg "Merge pull request #1 from a/b"
expect 0 "commit-msg accepts fixup commits" msg "fixup! feat: a"
expect 0 "commit-msg accepts squash commits" msg "squash! feat: a"
expect 1 "commit-msg rejects untyped subject" msg "added stuff"
expect 1 "commit-msg rejects unknown scope" msg "feat(web): add page"
expect 1 "commit-msg rejects unknown type" msg "feature: add page"
expect 1 "commit-msg rejects subject over 72 chars" msg "feat: $(printf 'x%.0s' {1..80})"
expect 1 "commit-msg rejects Reverting without a colon form" msg "Reverting stuff"
expect 1 "commit-msg rejects Merged prefix" msg "Merged things"
expect 0 "commit-msg accepts git revert subject" msg 'Revert "feat: x"'

fmt_hook() { printf '%s' "$1" | "$root/.claude/hooks/rustfmt-edited.sh"; }
printf 'fn  main( ){}\n' >"$tmp/ugly.rs"
expect 0 "rustfmt hook ignores non-rust files" fmt_hook '{"tool_input":{"file_path":"README.md"}}'
expect 0 "rustfmt hook formats a rust file" fmt_hook "{\"tool_input\":{\"file_path\":\"$tmp/ugly.rs\"}}"
expect 0 "rustfmt hook output is formatted" grep -qx 'fn main() {}' "$tmp/ugly.rs"
printf 'fn main( {\n' >"$tmp/broken.rs"
expect 2 "rustfmt hook reports syntax errors" fmt_hook "{\"tool_input\":{\"file_path\":\"$tmp/broken.rs\"}}"

stop_hook() { printf '%s' "$1" | CLAUDE_PROJECT_DIR="$root" "$root/.claude/hooks/clippy-on-stop.sh"; }
expect 0 "clippy stop hook lets a re-entered stop through" stop_hook '{"stop_hook_active":true}'

clean_repo="$tmp/clean-repo"
git init -q "$clean_repo"
stop_hook_in() { printf '%s' '{}' | CLAUDE_PROJECT_DIR="$1" "$root/.claude/hooks/clippy-on-stop.sh"; }
expect 0 "clippy stop hook skips when no Rust files changed" stop_hook_in "$clean_repo"

mkdir -p "$tmp/bin"
cat >"$tmp/bin/cargo" <<'EOF'
#!/usr/bin/env bash
echo run >>"$CARGO_STUB_LOG"
exit "${CARGO_STUB_EXIT:-0}"
EOF
chmod +x "$tmp/bin/cargo"
export CARGO_STUB_LOG="$tmp/cargo.log"
stub_repo="$tmp/stub-repo"
git init -q "$stub_repo"
printf 'pub fn a() {}\n' >"$stub_repo/lib.rs"
git -C "$stub_repo" add lib.rs
git -C "$stub_repo" -c user.name=t -c user.email=t@t commit -qm init
stub_stop() { PATH="$tmp/bin:$PATH" CARGO_STUB_EXIT=${1:-0} stop_hook_in "$stub_repo"; }
clippy_runs() {
  local runs=0 line
  if [[ -f $CARGO_STUB_LOG ]]; then
    while IFS= read -r line; do runs=$((runs + 1)); done <"$CARGO_STUB_LOG"
  fi
  ((runs == $1))
}

printf 'pub fn a() { }\n' >"$stub_repo/lib.rs"
expect 0 "clippy stop hook runs clippy on a changed file" stub_stop
expect 0 "clippy stop hook ran clippy once" clippy_runs 1
expect 0 "clippy stop hook skips an unchanged diff after a pass" stub_stop
expect 0 "clippy stop hook did not rerun clippy" clippy_runs 1
printf 'pub fn a() {  }\n' >"$stub_repo/lib.rs"
expect 0 "clippy stop hook reruns after an edit" stub_stop
expect 0 "clippy stop hook ran clippy again" clippy_runs 2
printf 'pub fn b() {}\n' >"$stub_repo/new.rs"
expect 0 "clippy stop hook reruns for a new untracked file" stub_stop
expect 0 "clippy stop hook ran clippy for the new file" clippy_runs 3
printf 'pub fn b() { }\n' >"$stub_repo/new.rs"
expect 2 "clippy stop hook blocks when clippy fails" stub_stop 1
expect 2 "clippy stop hook reruns after a failure" stub_stop 1
expect 0 "clippy stop hook ran clippy for each failure" clippy_runs 5

precommit_in() { (cd "$1" && "$root/.githooks/pre-commit"); }
expect 0 "pre-commit skips when nothing Rust is staged" precommit_in "$clean_repo"

if ((failures > 0)); then
  echo "$failures hook test(s) failed"
  exit 1
fi
echo "all hook tests passed"
