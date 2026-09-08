#!/usr/bin/env bash
# Build the release artifact: one universal macOS binary, tarred with its
# checksum.
#
#   scripts/dist.sh            -> dist/ccnm-<version>-macos-universal.tar.gz
#
# Universal because the two machines can be different Macs -- an M-series
# work machine and an Intel one at home is a normal pair, and telling
# people to pick the right download is a support question waiting to
# happen. The cost is 16 MB instead of 8.
#
# The version comes from the binary itself (`ccnm --version`), not from
# Cargo.toml, so the name of the file can never disagree with what is
# inside it.
set -euo pipefail
cd "$(dirname "$0")/.."

TARGETS=(aarch64-apple-darwin x86_64-apple-darwin)
OUT=dist

for target in "${TARGETS[@]}"; do
  if ! rustup target list --installed | grep -qx "$target"; then
    echo "installing rust target $target"
    rustup target add "$target"
  fi
  echo "==> building $target"
  cargo build --release --target "$target"
done

rm -rf "$OUT"
mkdir -p "$OUT"
lipo -create -output "$OUT/ccnm" \
  "target/aarch64-apple-darwin/release/ccnm" \
  "target/x86_64-apple-darwin/release/ccnm"

version=$("$OUT/ccnm" --version | awk '{print $2}')
[ -n "$version" ] || { echo "the binary did not report a version"; exit 1; }
name="ccnm-$version-macos-universal.tar.gz"

tar -czf "$OUT/$name" -C "$OUT" ccnm
( cd "$OUT" && shasum -a 256 "$name" > "$name.sha256" )

echo
lipo -info "$OUT/ccnm"
ls -lh "$OUT/$name" "$OUT/$name.sha256"
cat "$OUT/$name.sha256"
