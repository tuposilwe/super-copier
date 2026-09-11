#!/bin/sh
# Builds every platform's installer and stages them into a Releases/
# folder organized by OS, using the naming convention uploaded to
# GitHub Releases.
#
# Usage: packaging/make_releases.sh
# Output: Releases/Windows/SuperCopier-Setup.exe
#         Releases/macOS/SuperCopier-macOS-Intel.dmg
#         Releases/macOS/SuperCopier-macOS-AppleSilicon.dmg
#         Releases/Linux/SuperCopier-Linux.tar.gz
#         Releases/Linux/SuperCopier-Linux.deb
set -eu

cd "$(dirname "$0")/.."

sh packaging/macos/build_dmg.sh
sh packaging/linux/build_tarball.sh
sh packaging/linux/build_deb.sh

rustup target add x86_64-pc-windows-gnu >/dev/null 2>&1 || true
cargo build --release --target x86_64-pc-windows-gnu -p super-copier
makensis -DVERSION=0.1.0 -DEXE_PATH=../../target/x86_64-pc-windows-gnu/release/super-copier.exe packaging/windows/installer.nsi

rm -rf Releases
mkdir -p Releases/Windows Releases/macOS Releases/Linux

cp packaging/windows/SuperCopierSetup.exe Releases/Windows/SuperCopier-Setup.exe
cp packaging/macos/SuperCopier-macOS-Intel.dmg Releases/macOS/
cp packaging/macos/SuperCopier-macOS-AppleSilicon.dmg Releases/macOS/
cp packaging/linux/super-copier-linux-x86_64.tar.gz Releases/Linux/SuperCopier-Linux.tar.gz
cp packaging/linux/super-copier_0.1.0_amd64.deb Releases/Linux/SuperCopier-Linux.deb

echo "Staged release files under Releases/"
