#!/bin/sh
here="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"
sys="$(uname -s)"
case "$sys" in
MINGW* | MSYS* | CYGWIN*) rel="x86_64-pc-windows-msvc/mediagen.exe" ;;
Darwin*)
    case "$(uname -m)" in
    arm64) rel="aarch64-apple-darwin/mediagen" ;;
    *) rel="x86_64-apple-darwin/mediagen" ;;
    esac ;;
*)
    case "$(uname -m)" in
    aarch64 | arm64) rel="aarch64-unknown-linux-musl/mediagen" ;;
    *) rel="x86_64-unknown-linux-musl/mediagen" ;;
    esac ;;
esac
if [ -x "$here/$rel" ]; then exec "$here/$rel" "$@"; fi
for cand in "$here/target/release/mediagen.exe" "$here/target/release/mediagen" "$here/target/debug/mediagen.exe" "$here/target/debug/mediagen"; do
    if [ -x "$cand" ]; then exec "$cand" "$@"; fi
done
echo "mediagen binary not found in $here" >&2
exit 1
