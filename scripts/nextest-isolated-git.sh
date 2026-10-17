#!/bin/sh
# nextest setup script: git run by the tests, the daemons they start and those daemons'
# agents reads neither the developer's global nor the system config (signing, hooks,
# merge.ff), and commits under a fixed identity.
set -eu
cat >>"$NEXTEST_ENV" <<'ENV'
GIT_CONFIG_GLOBAL=/dev/null
GIT_CONFIG_NOSYSTEM=1
GIT_AUTHOR_NAME=lazyagents test
GIT_AUTHOR_EMAIL=test@lazyagents.invalid
GIT_COMMITTER_NAME=lazyagents test
GIT_COMMITTER_EMAIL=test@lazyagents.invalid
ENV
