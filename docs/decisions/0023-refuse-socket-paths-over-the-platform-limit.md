# 0023. Refuse a state directory whose socket path exceeds the platform limit

Date: 2026-10-18
Status: Accepted

## Context

The daemon listens on `<state dir>/daemon.sock`. A unix socket path must fit `sun_path`: 104 bytes on macOS and the BSDs, 108 on Linux, both counting the terminating NUL. A longer state directory (a deep `LAZYAGENTS_STATE_DIR`, a long temp path) made `bind` fail inside the spawned daemon, and the user only saw "daemon exited during startup; see <log>".

## Decision

`Client::connect_or_spawn` checks the socket path before connecting or spawning, measuring it as the spawned daemon will bind it (the state directory made absolute) and failing with the path, its length, the limit and the advice to point `LAZYAGENTS_STATE_DIR` at a shorter path. The check lives only there: a directly run `lazyagents daemon` relies on the bind error. The limit is the platform's own (104 off Linux and Android, 108 on them), so Linux users keep the extra four bytes.

## Alternatives

- Use 104 everywhere: a state dir that works on Linux would then be refused for no reason, and tests written on macOS would be stricter than the system.
- Bind through a short relative path (`chdir` into the state dir): every client would need the same trick, and the hook's `LAZYAGENTS_SOCKET` carries an absolute path.
- Let the daemon report the bind error: it has no terminal, so the message reaches only its log.

## Consequences

A long state directory fails immediately, with a message that names the cause. A client that only connects (`shutdown`, hooks) is unchanged: it fails to connect, as before.
