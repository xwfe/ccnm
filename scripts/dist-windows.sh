#!/usr/bin/env bash
# Build the Windows release artifact: one x86_64 ccnm.exe, zipped, with its
# checksum.
#
#   scripts/dist-windows.sh    -> dist/ccnm-<version>-windows-x86_64.zip
#
# Windows is only ever the machine holding the projects (the Runtime Node,
# docs/plan/windows-runtime.md): this binary refuses the AI machine's
# commands. How far that half has been proven is docs/support-matrix.md.
#
# Runs in Git Bash on Windows -- the release job and the CI job both call
# it on windows-latest. Native build, as for the other two: nothing on macOS
# or Linux here links for Windows, and a binary nobody could run is a binary
# nobody checked; the tag-vs-version step in release.yml runs this one.
#
# A zip rather than a tarball because that is what Windows opens without
# anything installed. It is made with PowerShell's Compress-Archive, which
# every Windows has, so the same script works on a Windows machine outside CI.
set -euo pipefail
cd "$(dirname "$0")/.."

case "$(uname -s)" in
  MINGW* | MSYS*) ;;
  *)
    echo "run this in Git Bash on Windows; this is $(uname -s)" >&2
    exit 1
    ;;
esac

TARGET=x86_64-pc-windows-msvc
OUT=dist

if ! rustup target list --installed | grep -qx "$TARGET"; then
  echo "installing rust target $TARGET"
  rustup target add "$TARGET"
fi
echo "==> building $TARGET"
cargo build --release --target "$TARGET"

rm -rf "$OUT"
mkdir -p "$OUT"
cp "target/$TARGET/release/ccnm.exe" "$OUT/ccnm.exe"

version=$("$OUT/ccnm.exe" --version | awk '{print $2}' | tr -d '\r')
[ -n "$version" ] || { echo "the binary did not report a version" >&2; exit 1; }
name="ccnm-$version-windows-x86_64.zip"

powershell.exe -NoLogo -NoProfile -NonInteractive -Command \
  "Compress-Archive -LiteralPath '$(cygpath -w "$OUT/ccnm.exe")' -DestinationPath '$(cygpath -w "$OUT/$name")'"
# Two spaces and the bare name, as sha256sum writes on the other two, so
# `sha256sum -c` reads it anywhere; on Windows, compare with Get-FileHash.
hash=$(sha256sum "$OUT/$name" | cut -d' ' -f1)
printf '%s  %s\n' "$hash" "$name" > "$OUT/$name.sha256"

echo
ls -l "$OUT/$name" "$OUT/$name.sha256"
cat "$OUT/$name.sha256"
