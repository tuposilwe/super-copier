#!/bin/sh
# Builds Super Copier.app for one or both macOS architectures and packages
# each into a distributable .dmg.
#
# Usage: packaging/macos/build_dmg.sh [intel|applesilicon]
#   (no argument builds both)
# Output: packaging/macos/SuperCopier-macOS-Intel.dmg
#         packaging/macos/SuperCopier-macOS-AppleSilicon.dmg
set -eu

cd "$(dirname "$0")/../.."

build_one() {
    arch_label="$1"   # Intel | AppleSilicon
    rust_target="$2"  # x86_64-apple-darwin | aarch64-apple-darwin

    cargo build --release --target "$rust_target" -p super-copier

    APP="target/$rust_target/release/Super Copier.app"
    rm -rf "$APP"
    mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
    cp "target/$rust_target/release/super-copier" "$APP/Contents/MacOS/super-copier"
    chmod +x "$APP/Contents/MacOS/super-copier"
    cp app/assets/icon.icns "$APP/Contents/Resources/AppIcon.icns"

    cat > "$APP/Contents/Info.plist" <<'EOF'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>CFBundleName</key>
	<string>Super Copier</string>
	<key>CFBundleDisplayName</key>
	<string>Super Copier</string>
	<key>CFBundleIdentifier</key>
	<string>dev.tuposilwe.supercopier</string>
	<key>CFBundleVersion</key>
	<string>0.1.0</string>
	<key>CFBundleShortVersionString</key>
	<string>0.1.0</string>
	<key>CFBundleExecutable</key>
	<string>super-copier</string>
	<key>CFBundleIconFile</key>
	<string>AppIcon</string>
	<key>CFBundlePackageType</key>
	<string>APPL</string>
	<key>CFBundleSignature</key>
	<string>????</string>
	<key>LSMinimumSystemVersion</key>
	<string>11.0</string>
	<key>NSHighResolutionCapable</key>
	<true/>
</dict>
</plist>
EOF

    # Ad-hoc sign so Gatekeeper treats it as a normal local app instead of
    # flagging it as damaged/unsigned. Only valid on this machine — see the
    # README for what real distribution would require.
    codesign --force --deep -s - "$APP"

    STAGE="$(mktemp -d)"
    trap 'rm -rf "$STAGE"' EXIT
    cp -R "$APP" "$STAGE/Super Copier.app"
    ln -s /Applications "$STAGE/Applications"

    OUT="packaging/macos/SuperCopier-macOS-$arch_label.dmg"
    rm -f "$OUT"
    hdiutil create -volname "Super Copier" -srcfolder "$STAGE" -ov -format UDZO "$OUT"
    rm -rf "$STAGE"

    echo "Built $OUT"
}

case "${1:-}" in
    intel) build_one Intel x86_64-apple-darwin ;;
    applesilicon) build_one AppleSilicon aarch64-apple-darwin ;;
    "")
        build_one Intel x86_64-apple-darwin
        build_one AppleSilicon aarch64-apple-darwin
        ;;
    *)
        echo "Usage: $0 [intel|applesilicon]" >&2
        exit 1
        ;;
esac
