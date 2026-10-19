# 0030. Run the tests with cargo test, one process per test binary

Date: 2026-10-19
Status: Accepted

## Context

`just test` ran `cargo nextest run`, which starts a process for every test: about 730 per
run. On the development Mac a security agent can hold each `exec` for about 256 ms, and
launches of the same binary queue behind one another, so the run time follows the number
of processes, not the work. Measured under that hold, `cargo nextest run -p tui --lib` took
88.5 s for 345 tests and `cargo test -p tui --lib` took 1.8 s.

nextest also gave us three things the switch has to replace or give up:

- a setup script that pointed git at no global or system config and a fixed identity for
  every test, the daemons it starts and their agents;
- a `process-heavy` test group running at most four tests at once from `daemon::server`,
  `daemon::git`, `lazyagents::cli` and `client::client`, which otherwise starve one another
  past their own timeouts;
- a slow timeout: a test was reported after 60 s and killed after 180 s.

## Decision

`just test` runs `scripts/test.sh`, which builds every test target once, then runs two
`cargo test` invocations side by side: the library unit tests on libtest's default
threads, and the binary's unit tests plus every integration test file (`--bins --test
'*'`) with `--test-threads=4`. Cargo runs the binaries of one invocation one after
another, so at most four process-heavy tests run at a time, as before, and every test
binary runs exactly once. The script exports the git isolation itself. nextest, its
config and its setup script are gone, and so is `cargo-nextest` from the dev shell. There
are no doctests, so none are run.

Tests that shared a process for the first time exposed one reliance on process
isolation: tests waited for "two more process-table readings" on a counter global to the
test process, which every in-process daemon bumped. Each daemon now counts its own
readings (`daemon::run_with_readings`).

## Alternatives

- Keep nextest beside libtest as `just test-nextest`: two runners to keep in step on the
  git isolation and the concurrency cap, for a per-test process model the suite no longer
  needs. nextest stays one `cargo install` away for anyone chasing a hang.
- Git isolation in `.cargo/config.toml` `[env]`: it would also apply to `cargo run` and
  `just dev`, whose git must use the developer's config on real repos.
- One invocation capped at four threads: the 345 TUI and 150 daemon unit tests would wait
  behind the cap for no reason.
- Listing the light and heavy binaries by name: a new test file would be silently left
  out; `--lib` and `--bins --test '*'` cover every target between them.

## Consequences

Measured on 2026-10-19 on the same machine, while the exec hold was not in effect (a
fresh binary's first exec took 0.6 s, later ones none) and other agents loaded it: the
nextest run took 39, 40 and 43 s; `just test` now takes 41 to 46 s (six runs). With the
hold in effect the nextest run pays for about 730 queued execs and `just test` for about
twelve, so the saving is then most of the 88 s the TUI unit tests alone spent launching.

No test is timed out any more: libtest only reports a test still running after 60 s, and a
hung test hangs the run. Ctrl-C does not stop a test binary whose tests started daemons,
because each daemon handles SIGINT; that binary runs on until its tests finish.

Tests share their process with the others in their binary: a test must not depend on
process-wide state such as the environment, the working directory, a global counter or a
signal disposition. The crates forbid `unsafe`, which already rules out changing the
environment from a test.

`cargo test` alone does not isolate git from the developer's config; `just test` does, and
`hostile_global_and_system_git_config_do_not_reach_the_tests` fails with a pointer to it
otherwise.
