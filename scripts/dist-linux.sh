#!/usr/bin/env bash
# Build the Linux release artifact: one x86_64 binary, tarred with its
# checksum.
#
#   scripts/dist-linux.sh      -> dist/ccnm-<version>-linux-x86_64.tar.gz
#
# This artifact is for the **Runtime** half of ccnm -- `internal mcp-serve`
# and the seven tools, which is what a Linux machine actually runs. The
# Agent half (Controller, sessions) is a launchd LaunchAgent and does not
# run here at all; shipping a Linux binary is not a claim that it does.
# See docs/support-matrix.md.
#
# Native build, no cross-compiling. Linking against another host's glibc
# needs that host's toolchain, and a binary nobody on this machine can run
# is a binary nobody checked -- the tag-vs-version step in release.yml
# runs it.
#
# The glibc floor is **measured from the binary**, not claimed from the
# distro. Get it wrong and the user sees `version GLIBC_2.39 not found`
# on a machine that looked supported; the number lands in dist/glibc-floor.txt
# so the release notes quote something that was actually read off the file.
set -euo pipefail
cd "$(dirname "$0")/.."

[ "$(uname -s)" = Linux ] || {
  echo "run this on the Linux machine that builds the artifact; this is $(uname -s)" >&2
  exit 1
}
arch=$(uname -m)
[ "$arch" = x86_64 ] || {
  echo "the only verified Linux Runtime is x86_64; this machine is $arch" >&2
  echo "arm64 Linux has no evidence yet -- see docs/support-matrix.md" >&2
  exit 1
}

TARGET=x86_64-unknown-linux-gnu
OUT=dist

if ! rustup target list --installed | grep -qx "$TARGET"; then
  echo "installing rust target $TARGET"
  rustup target add "$TARGET"
fi
echo "==> building $TARGET"
cargo build --release --target "$TARGET"

rm -rf "$OUT"
mkdir -p "$OUT"
cp "target/$TARGET/release/ccnm" "$OUT/ccnm"

version=$("$OUT/ccnm" --version | awk '{print $2}')
[ -n "$version" ] || { echo "the binary did not report a version" >&2; exit 1; }
name="ccnm-$version-linux-x86_64.tar.gz"

tar -czf "$OUT/$name" -C "$OUT" ccnm
(cd "$OUT" && sha256sum "$name" > "$name.sha256")

# The highest GLIBC_x.y symbol version the binary asks for is the oldest
# glibc it will start on.
floor=$(objdump -T "$OUT/ccnm" | sed -n 's/.*GLIBC_\([0-9][0-9.]*\).*/\1/p' | sort -V | tail -1)
[ -n "$floor" ] || { echo "could not read the glibc floor out of the binary" >&2; exit 1; }
printf '%s\n' "$floor" > "$OUT/glibc-floor.txt"

echo
file "$OUT/ccnm"
echo "needs glibc >= $floor"
ls -lh "$OUT/$name" "$OUT/$name.sha256"
cat "$OUT/$name.sha256"
