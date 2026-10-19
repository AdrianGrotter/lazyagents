# 0031. Read the process table natively instead of launching `ps`

Date: 2026-10-19
Status: Accepted

## Context

The daemon read the process table by launching `ps -A -o pid=,ppid=,lstart=` once a second for process tracking and every 25 ms while stopping an agent, and the lock-file check launched `ps -o lstart= -p <pid>`. On a Mac with an endpoint security agent every exec is held for about 256 ms and concurrent launches of one binary queue behind each other, so a reading cost about 300 ms under load and a full test run launched `ps` hundreds of times. `lstart` also only has second resolution, needed `LC_ALL=C TZ=UTC0` to compare across clients, and on Linux is derived from the boot time, which moves when the clock is stepped. The workspace forbids `unsafe` (ADR 0016 relies on it), so the reading has to come from a crate rather than our own FFI. Both macOS and Linux are supported.

## Decision

`protocol::process` reads pid, parent pid and start time without launching anything: on macOS with the `libproc` crate (`proc_listpids` plus `proc_pidinfo(PROC_PIDTBSDINFO)`), on Linux by parsing `/proc/<pid>/stat`. A start time is an opaque `Start(u64)`: microseconds since the epoch on macOS, clock ticks since boot on Linux; neither depends on locale or time zone or moves when the clock is set. The reaper's tracked processes hold a `Start` (they live only in memory), and the lock file records it in decimal. Lock files written by older daemons hold the `lstart` text; a client parses it as UTC and compares it to the pid's start time to the second, so a client after this change still recognises, and can stop, a daemon from before it. The housekeeping cadence stays at one reading a second.

## Alternatives

- `sysinfo`: one crate for both systems and widely used, but a refresh with nothing requested still reads thread info and process names (about 5 ms against 0.4 ms for libproc on the same machine), it reports start times only in whole seconds, on Linux derived from the boot time, and on macOS it pulls in the objc2 crates.
- `procfs` on Linux: well maintained, but `/proc/<pid>/stat` needs two fields, and parsing it in a pure function lets the Linux logic be unit-tested on macOS, where the crate's code would not even compile.
- `darwin-libproc`: no `bindgen` build dependency, but unmaintained since 2020.
- Reading `kern.proc` through `sysctl`: sees every process, but returns raw `kinfo_proc` structs that would have to be decoded by byte offset.
- Keep `ps` and launch it less often: every reading would still cost an exec, and the stop loop needs fresh readings.

## Consequences

A reading takes about half a millisecond on macOS instead of tens to hundreds of milliseconds, so the window in which a detaching descendant can escape tracking shrinks to the snapshot interval, and tests no longer queue behind `ps`. Start times compare exactly instead of to the second. `libproc` builds its bindings with `bindgen`, so a macOS build needs libclang (the Xcode command line tools or the nix shell provide it) and adds `bindgen`'s build dependencies, licensed BSD-3-Clause and ISC, which `deny.toml` now allows. On macOS `proc_pidinfo` refuses other users' processes and zombies, so they are missing from the table: the daemon could not signal them anyway, but a user-owned descendant whose only link to the agent runs through another user's process (a command under `sudo -u`) is no longer found. An older client reading a newer daemon's lock file finds a start time that never equals the `ps` text it compares it with, so it does not trust the pid. `$LAZYAGENTS_PS`, the debug hook for substituting a slow `ps`, is gone.
