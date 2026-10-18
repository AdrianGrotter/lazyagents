# What `just install` produces on Linux

Date: 2026-10-18. No Linux machine was available; everything below comes from the pinned
nixpkgs source, nix-built Linux binaries fetched from cache.nixos.org and inspected with GNU
readelf 2.46 on macOS.

## What a build in the dev shell links against

- The dev shell is `pkgs.mkShell` with a rust-overlay toolchain. rustc links through `cc`,
  which on Linux is the nixpkgs cc wrapper around the bintools wrapper.
- `pkgs/build-support/bintools-wrapper/ld-wrapper.sh` adds
  `-dynamic-linker $NIX_DYNAMIC_LINKER` to every dynamic link (the wrapper sets
  `nix-support/ld-set-dynamic-linker` on Linux), and adds `-rpath <dir>` for each `-L`
  directory in the nix store that holds a library named by a `-l` flag, unless
  `NIX_DONT_SET_RPATH=1`.
- The pinned nixpkgs gives `/nix/store/…-glibc-2.44-25/lib/ld-linux-x86-64.so.2` on
  x86_64-linux and `/nix/store/…-glibc-2.44-25/lib/ld-linux-aarch64.so.1` on aarch64-linux
  (`stdenv.cc.bintools.dynamicLinker`).
- So a release build of lazyagents gets a PT_INTERP in the nix store and a RUNPATH with at
  least the glibc and gcc-lib (libgcc_s) store directories. ripgrep from the same nixpkgs,
  a Rust binary built the same way, shows exactly that (fixture
  `scripts/fixtures/readelf/nix-x86_64.txt`).
- A plain `mkShell` package build would run `patchelf --shrink-rpath` in fixup; a
  `cargo install` in the shell gets no fixup, so the runpath stays as linked.

A nix garbage collection that removes that glibc makes the installed binary fail to start,
and with it every running agent's hooks.

## Can patchelf fix it?

`patchelf --set-interpreter /lib64/ld-linux-x86-64.so.2 --remove-rpath` makes the binary
load the system's glibc and libgcc_s. Whether it then runs depends on the system glibc:

- The nix glibc is 2.44. ripgrep's version needs include `GLIBC_2.39`
  (`pidfd_spawnp`, `pidfd_getpid`, used by Rust std's process spawning) with flags `none`,
  so the loader refuses it on glibc < 2.39 (Ubuntu 22.04: 2.35, Debian 12: 2.36). lazyagents
  spawns processes through std too, so it will need the same.
- On glibc ≥ 2.39 (Ubuntu 24.04, Fedora 40+, Arch) it loads.
- NixOS has a stub `/lib64/ld-linux-x86-64.so.2` that refuses generic binaries.

Running the relinked binary once tells which case applies, because the loader checks every
needed library and symbol version at startup.

## What install.sh now does (ADR 0020)

On Linux: build into a temp root, detect nix store interpreter or runpath with `readelf`,
relink with `patchelf`, run `--version` with `LD_LIBRARY_PATH` unset, and only then replace
`$root/bin/lazyagents` (copy beside it, rename). Any failure leaves the installed binary
alone and prints the rustup command. Both tools are already in the Linux dev shell:
`readelf` from the bintools wrapper, `patchelf` from the Linux stdenv's
`extraNativeBuildInputs`. `LAZYAGENTS_INSTALL_ROOT` points the install somewhere other than
`~/.local`. `scripts/test-install.sh` (part of `just check`) covers the detection with fixture
output and the flow with stub `cargo`, `uname`, `readelf` and `patchelf`.

## Still unverified

- A real run on Linux, including that `readelf -lW -dW` and `patchelf` behave as the stubs
  assume.
- Other ways out if relinking is not enough: a static musl target, or `zig cc` / a glibc
  version pin to link against an older glibc.
