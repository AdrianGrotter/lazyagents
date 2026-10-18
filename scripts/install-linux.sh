# shellcheck shell=bash
# Functions scripts/install.sh uses on Linux to install a nix shell build that runs without
# the nix store. Sourced, never run.

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
  local tool elf interp loader out
  for tool in readelf patchelf; do
    if ! command -v "$tool" >/dev/null 2>&1; then
      echo "install: $tool is not on PATH; run the install from the nix dev shell" >&2
      install_linux_advice "$bin"
      return 1
    fi
  done
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
