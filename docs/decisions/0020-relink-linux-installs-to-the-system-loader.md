# 0020. Relink Linux installs to the system loader, and install only what runs

Date: 2026-10-18
Status: Accepted

## Context

`just install` builds in the nix dev shell and puts the binary at `~/.local/bin`, where it
must outlive any nix garbage collection (running agents' hook settings name that path). On
Darwin the only nix store link is libiconv, which `install.sh` points at the system copy. On
Linux the nix bintools wrapper gives every dynamically linked build the nix glibc's loader as
its ELF interpreter and a RUNPATH of the nix store library directories it linked against
(glibc, gcc's libgcc_s). A garbage collection that drops that glibc leaves a binary that no
longer starts. The nix glibc is also new (2.44 in the pinned nixpkgs), and Rust's std links
versioned symbols such as `pidfd_spawnp@GLIBC_2.39`, so the same binary pointed at an older
system glibc fails to load.

## Decision

On Linux, `install.sh` builds into a temp root, reads the interpreter and RUNPATH with
`readelf`, and when either is in `/nix/store` sets the interpreter to the loader of the same
name in `/lib64` or `/lib` and removes the runpath with `patchelf`. It then runs the binary
with `--version`; only if that succeeds does it replace `$root/bin/lazyagents`, by copying
next to it and renaming. Otherwise it stops, leaves the installed binary alone, and says how
to build with rustup outside the nix shell. The Darwin path is unchanged.

## Alternatives

- Detect the nix store links and always fail: safe, but `just install` would never work on
  Linux, since every build in the shell has them.
- Relink without running the result: installs a binary that cannot load on a system whose
  glibc is older than the symbols it needs.
- A static musl build: needs the musl target in the toolchain and a musl C toolchain for the
  crates that compile C or assembly (`psm`), and changes the dev shell for every platform.
- Build with the system toolchain from the start: the script would no longer be runnable
  from the dev shell that `just` comes from.

## Consequences

On distributions with glibc 2.39 or newer the install works as on Darwin. On older ones it
fails before touching the installed binary, with the reason and the rustup command. On NixOS
the `/lib64` loader is a stub that refuses to run the binary, so the install fails there too;
NixOS users should install through nix instead. None of this has run on a Linux machine yet:
the detection is tested on fixture `readelf` output and the flow with stub tools
(`scripts/test-install.sh`).
