#!/bin/sh
# Builds a portable Super Copier tarball for Linux (x86_64) — extract and
# run, no package manager or installation needed.
#
# Usage: packaging/linux/build_tarball.sh
# Output: packaging/linux/super-copier-linux-x86_64.tar.gz
#
# Runs natively on Linux, or cross-compiles from macOS/other via
# cargo-zigbuild (brew install zig && cargo install cargo-zigbuild).
set -eu

cd "$(dirname "$0")/../.."

if [ "$(uname -s)" = "Linux" ]; then
  cargo build --release -p super-copier
  BIN=target/release/super-copier
else
  rustup target add x86_64-unknown-linux-gnu >/dev/null 2>&1 || true
  cargo zigbuild --release -p super-copier --target x86_64-unknown-linux-gnu
  BIN=target/x86_64-unknown-linux-gnu/release/super-copier
fi

NAME="super-copier-linux-x86_64"
STAGE="packaging/linux/${NAME}"
rm -rf "$STAGE"
mkdir -p "$STAGE"
cp "$BIN" "$STAGE/super-copier"
chmod +x "$STAGE/super-copier"
cp packaging/linux/super-copier.desktop "$STAGE/"
cp app/assets/icon-1024.png "$STAGE/super-copier.png"

tar -C packaging/linux -czf "packaging/linux/${NAME}.tar.gz" "$NAME"
rm -rf "$STAGE"

echo "Built packaging/linux/${NAME}.tar.gz"
