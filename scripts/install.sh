#!/usr/bin/env bash
# Release build to ~/.local/bin/lazyagents ($LAZYAGENTS_INSTALL_ROOT/bin when set), linked
# only against system libraries so a nix garbage collection can never break it.
set -euo pipefail

# shellcheck source=SCRIPTDIR/install-linux.sh
source "$(dirname "${BASH_SOURCE[0]}")/install-linux.sh"

root="${LAZYAGENTS_INSTALL_ROOT-$HOME/.local}"
if [[ -z $root ]]; then
  echo "install: LAZYAGENTS_INSTALL_ROOT is set but empty" >&2
  exit 1
fi
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
