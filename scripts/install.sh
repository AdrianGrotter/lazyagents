#!/usr/bin/env bash
# Release build to ~/.local/bin/lazyagents ($LAZYAGENTS_INSTALL_ROOT/bin when set), linked
# only against system libraries so a nix garbage collection can never break it.

interpreter_pattern='^.*\[Requesting program interpreter: (.*)\]$'

# The ELF interpreter in `readelf -lW` output.
elf_interpreter() {
  sed -nE "s/$interpreter_pattern/\1/p"
}

# The /nix/store paths in `readelf -lW -dW` output: the interpreter, then every RUNPATH or
# RPATH entry.
nix_store_links() {
  sed -nE -e "s/$interpreter_pattern/\1/p" -e 's/^.*\((RUNPATH|RPATH)\).*\[(.*)\]$/\2/p' |
    tr ':' '\n' | grep '^/nix/store/' || true
}

# The loader named like the one given, from the first of the given directories that has it.
system_loader() {
  local name=${1##*/} dir
  shift
  for dir in "$@"; do
    if [[ -e $dir/$name ]]; then
      echo "$dir/$name"
      return 0
    fi
  done
  return 1
}

# Installs a binary built in the nix shell as bin once it runs against the system: its nix
# store loader swapped for the one of the same name in the given directories, its nix store
# runpath dropped.
install_linux() {
  local built=$1 bin=$2
  shift 2
  local elf interp loader out
  elf=$(readelf -lW -dW "$built") || return 1
  if [[ -n $(nix_store_links <<<"$elf") ]]; then
    interp=$(elf_interpreter <<<"$elf")
    if [[ $interp == /nix/store/* ]]; then
      if ! loader=$(system_loader "$interp" "$@"); then
        echo "install: lazyagents uses the nix store loader $interp, and this system has no ${interp##*/} in $*" >&2
        install_linux_advice "$bin"
        return 1
      fi
      patchelf --set-interpreter "$loader" --remove-rpath "$built" || return 1
    else
      patchelf --remove-rpath "$built" || return 1
    fi
  fi
  if ! out=$(env -u LD_LIBRARY_PATH "$built" --version 2>&1); then
    echo "install: lazyagents does not run against the system's loader and libraries:" >&2
    printf '%s\n' "$out" | sed 's/^/  /' >&2
    install_linux_advice "$bin"
    return 1
  fi
  mkdir -p "${bin%/*}" && cp "$built" "$bin.new" && mv -f "$bin.new" "$bin"
}

install_linux_advice() {
  cat >&2 <<EOF
install: left $1 as it was.
A build in the nix shell links against the nix glibc, which may be newer than this
system's. Build outside the nix shell with rustup instead:
  cargo install --locked --path crates/lazyagents --root ${1%/bin/*}
EOF
}

[[ ${BASH_SOURCE[0]} == "$0" ]] || return 0
set -euo pipefail

root="${LAZYAGENTS_INSTALL_ROOT:-$HOME/.local}"
bin="$root/bin/lazyagents"

if [[ $(uname) == Linux ]]; then
  stage=$(mktemp -d)
  trap 'rm -rf "$stage"' EXIT
  cargo install --locked --path crates/lazyagents --root "$stage"
  install_linux "$stage/bin/lazyagents" "$bin" /lib64 /lib
  "$bin" --version
  exit
fi

cargo install --locked --path crates/lazyagents --root "$root"

if [[ $(uname) == Darwin ]]; then
  while read -r lib; do
    case $lib in
      */libiconv.2.dylib) install_name_tool -change "$lib" /usr/lib/libiconv.2.dylib "$bin" ;;
      *) echo "install: $bin links $lib from the nix store; no system replacement known" >&2; exit 1 ;;
    esac
  done < <(otool -L "$bin" | awk 'NR > 1 && $1 ~ /^\/nix\/store\// { print $1 }')
  codesign --force --sign - "$bin"
fi

"$bin" --version
