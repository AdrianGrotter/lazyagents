#!/usr/bin/env bash
# Release build to ~/.local/bin/lazyagents, linked only against system libraries so a
# nix garbage collection can never break it.
set -euo pipefail

root="$HOME/.local"
bin="$root/bin/lazyagents"
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
