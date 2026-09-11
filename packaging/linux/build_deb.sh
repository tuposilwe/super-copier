#!/bin/sh
# Builds a .deb package for Super Copier on Linux (x86_64).
#
# Usage: packaging/linux/build_deb.sh
# Output: packaging/linux/super-copier_<version>_amd64.deb
#
# Runs natively on Linux, or cross-compiles from macOS/other via
# cargo-zigbuild (brew install zig && cargo install cargo-zigbuild).
set -eu

cd "$(dirname "$0")/../.."

VERSION=0.1.0
ARCH=amd64
PKG="super-copier_${VERSION}_${ARCH}"
STAGE="packaging/linux/${PKG}"

if [ "$(uname -s)" = "Linux" ]; then
  cargo build --release -p super-copier
  BIN=target/release/super-copier
else
  rustup target add x86_64-unknown-linux-gnu >/dev/null 2>&1 || true
  cargo zigbuild --release -p super-copier --target x86_64-unknown-linux-gnu
  BIN=target/x86_64-unknown-linux-gnu/release/super-copier
fi

rm -rf "$STAGE"
mkdir -p "$STAGE/DEBIAN" \
         "$STAGE/usr/bin" \
         "$STAGE/usr/share/applications" \
         "$STAGE/usr/share/pixmaps"

cp "$BIN" "$STAGE/usr/bin/super-copier"
chmod +x "$STAGE/usr/bin/super-copier"
cp packaging/linux/super-copier.desktop "$STAGE/usr/share/applications/"
cp app/assets/icon-1024.png "$STAGE/usr/share/pixmaps/super-copier.png"

cat > "$STAGE/DEBIAN/control" <<EOF
Package: super-copier
Version: ${VERSION}
Section: utils
Priority: optional
Architecture: ${ARCH}
Maintainer: Super Copier
Description: Fast file copy, search, and duplicate finder
 Multi-threaded file copy/move, whole-disk search, duplicate and
 large-file finders, folder sync, and a drag-and-drop GUI.
EOF

dpkg-deb --build --root-owner-group "$STAGE" "packaging/linux/${PKG}.deb"
rm -rf "$STAGE"

echo "Built packaging/linux/${PKG}.deb"
