#!/usr/bin/env bash
# Exercises the Linux half of the install on any host: scripts/install-linux.sh with fixture
# readelf output, and scripts/install.sh with stub cargo, uname, readelf and patchelf and every
# path it writes inside a temp dir.
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
fixtures="$root/scripts/fixtures/readelf"
tmp=$(mktemp -d) || exit 1
if [[ $tmp != /* || ! -d $tmp || $(cd "$tmp" && pwd -P) == / ]]; then
  echo "test-install: mktemp gave an unusable directory: '$tmp'" >&2
  exit 1
fi
trap 'rm -rf "$tmp"' EXIT
failures=0

# shellcheck source=SCRIPTDIR/install-linux.sh
source "$root/scripts/install-linux.sh"

# The output of the last command run through expect, for `says` in the next one.
out=""
expect() {
  local want=$1 desc=$2 got
  shift 2
  if "$@" >"$tmp/out" 2>&1; then got=0; else got=$?; fi
  out=$(<"$tmp/out")
  if [[ $got == "$want" ]]; then
    echo "ok   $desc"
  else
    echo "FAIL $desc (exit $got, want $want)"
    sed 's/^/     /' "$tmp/out"
    failures=$((failures + 1))
  fi
}

same() { [[ $1 == "$2" ]] || diff <(printf '%s\n' "$2") <(printf '%s\n' "$1"); }
says() {
  local text
  for text in "$@"; do [[ $out == *"$text"* ]] || return 1; done
}
says_line() { [[ $'\n'$out$'\n' == *$'\n'"$1"$'\n'* ]]; }
links_of() { nix_store_links <"$fixtures/$1"; }

expect 0 "finds the interpreter and every runpath entry in the nix store" same "$(links_of nix-x86_64.txt)" \
  "/nix/store/h4wfwic161kxrr74jlzla5lsm28hgary-glibc-2.44-25/lib/ld-linux-x86-64.so.2
/nix/store/7v8b5idfzwfl06hx6n0cvd5ikd5p0sbg-pcre2-10.48/lib
/nix/store/h4wfwic161kxrr74jlzla5lsm28hgary-glibc-2.44-25/lib
/nix/store/j7qx4s4mr17j1wqgvqdzj33lmrnzb387-gcc-16.2.0-lib/lib"
expect 0 "finds the aarch64 loader in the nix store" same "$(links_of nix-aarch64.txt)" \
  "/nix/store/hx9wvdx2l59iahwf9nmrx7rwkm6cymgs-glibc-2.44-25/lib/ld-linux-aarch64.so.1
/nix/store/hx9wvdx2l59iahwf9nmrx7rwkm6cymgs-glibc-2.44-25/lib"
expect 0 "finds nothing in a binary linked against the system" same "$(links_of system-x86_64.txt)" ""
sed 's/(RUNPATH)  /(RPATH)    /; s/Library runpath/Library rpath/' "$fixtures/nix-x86_64.txt" >"$tmp/rpath.txt"
expect 0 "finds an old-style rpath too" same "$(nix_store_links <"$tmp/rpath.txt" | sed -n 2p)" \
  "/nix/store/7v8b5idfzwfl06hx6n0cvd5ikd5p0sbg-pcre2-10.48/lib"
expect 0 "reads the interpreter" same "$(elf_interpreter <"$fixtures/system-x86_64.txt")" \
  "/lib64/ld-linux-x86-64.so.2"

mkdir -p "$tmp/sys/lib64" "$tmp/sys/lib"
touch "$tmp/sys/lib/ld-linux-aarch64.so.1" "$tmp/sys/lib64/ld-linux-x86-64.so.2"
nix_loader=/nix/store/h4wfwic161kxrr74jlzla5lsm28hgary-glibc-2.44-25/lib/ld-linux-x86-64.so.2
expect 0 "picks the system loader of the same name" same \
  "$(system_loader "$nix_loader" "$tmp/sys/lib64" "$tmp/sys/lib")" "$tmp/sys/lib64/ld-linux-x86-64.so.2"
expect 0 "looks in every directory given" same \
  "$(system_loader /nix/store/x/lib/ld-linux-aarch64.so.1 "$tmp/sys/lib64" "$tmp/sys/lib")" \
  "$tmp/sys/lib/ld-linux-aarch64.so.1"
expect 1 "fails when no system loader has the name" system_loader /nix/store/x/lib/ld-musl.so.1 "$tmp/sys/lib64"

# Stub tools: readelf prints a fixture until patchelf has run, then the system one. The Darwin
# tools refuse to run, so a run that takes the Darwin branch fails instead of touching a binary.
stubs="$tmp/stubs"
mkdir -p "$stubs"
cat >"$stubs/readelf" <<'EOF'
#!/usr/bin/env bash
if [[ -e $STUB_STATE/patched ]]; then fixture=system-x86_64.txt; else fixture=$STUB_ELF; fi
printf '%s\n' "$(<"$STUB_FIXTURES/$fixture")"
EOF
cat >"$stubs/patchelf" <<'EOF'
#!/usr/bin/env bash
echo "$*" >>"$STUB_STATE/patchelf.log"
: >"$STUB_STATE/patched"
EOF
cat >"$stubs/uname" <<'EOF'
#!/usr/bin/env bash
echo Linux
EOF
cat >"$stubs/cargo" <<'EOF'
#!/usr/bin/env bash
while [[ $# -gt 0 && $1 != --root ]]; do shift; done
case ${2:-} in
  "$STUB_TMP"/*) ;;
  *) echo "cargo stub: refusing --root ${2:-} outside $STUB_TMP" >&2; exit 1 ;;
esac
mkdir -p "$2/bin"
printf '#!/usr/bin/env bash\n[[ -e $STUB_STATE/broken ]] && { echo "version GLIBC_2.39 not found" >&2; exit 1; }\necho "lazyagents built"\n' >"$2/bin/lazyagents"
chmod +x "$2/bin/lazyagents"
EOF
for tool in codesign install_name_tool otool; do
  printf '#!/usr/bin/env bash\necho "%s stub: refusing to run" >&2\nexit 1\n' "$tool" >"$stubs/$tool"
done
chmod +x "$stubs"/*

export STUB_FIXTURES=$fixtures STUB_TMP=$tmp
fresh() {
  export STUB_STATE="$tmp/state-$1" STUB_ELF=$2
  if [[ -e $STUB_STATE ]]; then rm -rf "$STUB_STATE"; fi
  mkdir -p "$STUB_STATE/root/bin" "$STUB_STATE/build/bin"
  printf '#!/usr/bin/env bash\necho "lazyagents old"\n' >"$STUB_STATE/root/bin/lazyagents"
  printf '#!/usr/bin/env bash\n[[ -e $STUB_STATE/broken ]] && exit 1\necho "lazyagents built"\n' >"$STUB_STATE/build/bin/lazyagents"
  chmod +x "$STUB_STATE"/*/bin/lazyagents
}
relink_with() {
  local path=$1
  shift
  PATH="$path" install_linux "$STUB_STATE/build/bin/lazyagents" "$STUB_STATE/root/bin/lazyagents" "$@"
}
relink() { relink_with "$stubs:$PATH" "$@"; }
installed() { same "$("$STUB_STATE/root/bin/lazyagents")" "$1"; }

fresh relink nix-x86_64.txt
expect 0 "relinks a nix-linked binary to the system loader" relink "$tmp/sys/lib64" "$tmp/sys/lib"
expect 0 "patchelf sets the system loader and drops the runpath" same "$(<"$STUB_STATE/patchelf.log")" \
  "--set-interpreter $tmp/sys/lib64/ld-linux-x86-64.so.2 --remove-rpath $STUB_STATE/build/bin/lazyagents"
expect 0 "installs the relinked binary" installed "lazyagents built"

fresh clean system-x86_64.txt
expect 0 "installs a system-linked binary as built" relink "$tmp/sys/lib64"
expect 1 "leaves a system-linked binary unpatched" test -e "$STUB_STATE/patchelf.log"
expect 0 "installs the system-linked binary" installed "lazyagents built"

fresh noloader nix-aarch64.txt
expect 1 "fails when the system has no loader for it" relink "$tmp/sys/lib64"
expect 0 "says which loader it looked for" says "ld-linux-aarch64.so.1"
expect 0 "keeps the installed binary when it has no loader" installed "lazyagents old"

fresh broken nix-x86_64.txt
: >"$STUB_STATE/broken"
expect 1 "fails when the relinked binary does not run" relink "$tmp/sys/lib64"
expect 0 "shows why it does not run" says "does not run"
expect 0 "keeps the installed binary when the relinked one does not run" installed "lazyagents old"

bare="$tmp/bare"
mkdir -p "$bare"
ln -s "$(command -v cat)" "$bare/cat"
fresh notools nix-x86_64.txt
expect 1 "fails without readelf" relink_with "$bare" "$tmp/sys/lib64"
expect 0 "names the missing readelf and gives the rustup advice" \
  says "readelf is not on PATH" "cargo install --locked"
ln -s "$stubs/readelf" "$bare/readelf"
expect 1 "fails without patchelf" relink_with "$bare" "$tmp/sys/lib64"
expect 0 "names the missing patchelf" says "patchelf is not on PATH"
expect 0 "keeps the installed binary when a tool is missing" installed "lazyagents old"

# The full run: only with uname and cargo resolving to the stubs and the install root in $tmp.
dest="$tmp/install-root"
refuse() {
  echo "test-install: refusing to run scripts/install.sh: $1" >&2
  exit 1
}
[[ $(PATH="$stubs:$PATH" && command -v uname) == "$stubs/uname" ]] || refuse "uname is not the stub"
[[ $(PATH="$stubs:$PATH" && command -v cargo) == "$stubs/cargo" ]] || refuse "cargo is not the stub"
[[ $dest == "$tmp"/?* ]] || refuse "the install root $dest is outside $tmp"
mkdir -p "$tmp/home"
fresh script system-x86_64.txt
script() {
  (cd "$root" && HOME="$tmp/home" TMPDIR="$tmp" PATH="$stubs:$PATH" LAZYAGENTS_INSTALL_ROOT="$dest" scripts/install.sh)
}
expect 0 "install.sh on Linux installs into LAZYAGENTS_INSTALL_ROOT" script
expect 0 "install.sh runs the installed binary" says_line "lazyagents built"
expect 0 "install.sh installs the binary it built" test -x "$dest/bin/lazyagents"

if [[ $failures -gt 0 ]]; then
  echo "$failures failure(s)"
  exit 1
fi
echo "all install checks passed"
