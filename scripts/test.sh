#!/usr/bin/env bash
# Runs every crate's tests: each test binary once, its tests on threads.
set -euo pipefail

# Git run by the tests, the daemons they start and those daemons' agents reads neither the
# global nor the system config (signing, hooks, merge.ff), and commits under a fixed
# identity.
export GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_NOSYSTEM=1
export GIT_AUTHOR_NAME='lazyagents test' GIT_AUTHOR_EMAIL=test@lazyagents.invalid
export GIT_COMMITTER_NAME='lazyagents test' GIT_COMMITTER_EMAIL=test@lazyagents.invalid

cd "$(dirname "$0")/.."
cargo test --workspace --locked --no-run --lib --bins --tests

# Library unit tests run beside the rest; their output is held back and printed afterwards
# so the two never interleave.
lib_log=$(mktemp)
cargo test --workspace --locked --lib >"$lib_log" 2>&1 &
lib=$!
trap 'kill "$lib" 2>/dev/null || true; rm -f "$lib_log"' EXIT

# Integration tests and the binary's tests start daemons, agents, git and ps; more than
# four at once starve one another past their own timeouts.
status=0
cargo test --workspace --locked --bins --test '*' -- --test-threads=4 || status=$?
wait "$lib" || status=$?
cat "$lib_log"
exit "$status"
