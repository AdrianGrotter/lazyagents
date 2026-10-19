# 0033. Daemon timers come from its Config

Date: 2026-10-19
Status: Accepted

## Context

The daemon's integration tests start real daemons that run real processes, and much of
what they check is timed: a process table read once a second, a generic agent turning
idle after 2 s without output, shutdown waiting 13 s for requests in flight. Every timer
was a constant, so the tests sat through the production values: one test waited out the
whole 13 s drain, and `tests/server.rs` took about 32 s of the roughly 66 s `just check`
spends when nothing needs compiling.

## Decision

`daemon::Config` carries a `Timings` struct: the housekeeping interval, the process table
reading interval, the generic status windows (`status::GenericWindows`) and shutdown's
grace period, snapshot wait and drain. `Timings::default()` holds the production values,
and the binary always uses it; nothing in the environment or the config file changes
them. The daemon's integration tests run with one shorter profile (`TIMINGS` in
`tests/server.rs`). `Timings::shutdown_fits` states how shutdown's timers relate, and a
test checks it for both profiles. Timers that cost the tests little stay constants: the
Claude silences and the tick, which no test waits on, and the 1 s stop grace, which a few
kill and exit-reaping tests wait out once each when a child ignores the hangup.

## Alternatives

- Paused tokio time: the daemon's timing reads `Instant::now()` and waits on real
  processes and threads, which a paused tokio clock does not move.
- An environment variable or a `cfg(test)` switch: a variable could change a user's
  daemon, and integration tests build the library without `cfg(test)`.
- Scaling every timer by one factor: shutdown's timers must keep their order and sum, and
  a factor small enough to help the slow timers makes the short ones too short for a busy
  machine.

## Consequences

`tests/server.rs` drops from about 32 s to about 16 s, and its slowest test from 15 s to
4 s. Tests that check timed behaviour state it relative to `TIMINGS`, not as fixed
seconds. A new timer that tests wait on belongs in `Timings`. The test profile trades
margin for speed: a machine slow enough to miss a 500 ms status window would make the
status tests flaky, which is when to lengthen it.
