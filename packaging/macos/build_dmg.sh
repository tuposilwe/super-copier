#!/bin/sh
# Builds Super Copier.app for one or both macOS architectures and packages
# each into a distributable .dmg.
#
# Usage: packaging/macos/build_dmg.sh [intel|applesilicon]
#   (no argument builds both)
# Output: packaging/macos/SuperCopier-macOS-Intel.dmg
#         packaging/macos/SuperCopier-macOS-AppleSilicon.dmg
#
# Signing (all optional; with none set you get an ad-hoc signed build that
# only opens cleanly on the Mac that built it — downloads get Gatekeeper's
# "Apple could not verify..." block):
#   SIGN_IDENTITY   a "Developer ID Application: ..." identity from
#                   `security find-identity -v -p codesigning`. Signs the app
#                   and dmg with the hardened runtime and a secure timestamp.
#   NOTARY_PROFILE  a keychain profile created once with
#                   `xcrun notarytool store-credentials <name> --apple-id ...
#                   --team-id ...`. Needs SIGN_IDENTITY. Submits the app and
#                   the dmg to Apple, waits, and staples the tickets so the
#                   result opens with no warning, even offline.
# Example:
#   SIGN_IDENTITY="Developer ID Application: Your Name (TEAMID)" \
#   NOTARY_PROFILE=super-copier-notary sh packaging/macos/build_dmg.sh
set -eu

SIGN_IDENTITY="${SIGN_IDENTITY:-}"
NOTARY_PROFILE="${NOTARY_PROFILE:-}"
if [ -n "$NOTARY_PROFILE" ] && [ -z "$SIGN_IDENTITY" ]; then
    echo "NOTARY_PROFILE needs SIGN_IDENTITY: Apple only notarizes Developer ID-signed code." >&2
    exit 1
fi

# Sends $1 to Apple's notary service and waits for the verdict. A rejection
# prints Apple's log (which names the offending file) and fails the build.
notarize() {
    submission="$(xcrun notarytool submit "$1" --keychain-profile "$NOTARY_PROFILE" --wait 2>&1)" || {
        echo "$submission" >&2
        return 1
    }
    echo "$submission" | tail -4
    id="$(echo "$submission" | awk '/^ *id:/ {print $2; exit}')"
    if ! echo "$submission" | grep -q "status: Accepted"; then
        echo "Notarization of $1 was not accepted:" >&2
        xcrun notarytool log "$id" --keychain-profile "$NOTARY_PROFILE" >&2 || true
        return 1
    fi
}

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
	<string>0.3.1</string>
	<key>CFBundleShortVersionString</key>
	<string>0.3.1</string>
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

    if [ -n "$SIGN_IDENTITY" ]; then
        # Hardened runtime + secure timestamp are both required for
        # notarization. The bundle holds a single executable, so no --deep.
        codesign --force --options runtime --timestamp -s "$SIGN_IDENTITY" "$APP"
        codesign --verify --strict --verbose=2 "$APP"
        if [ -n "$NOTARY_PROFILE" ]; then
            ZIP="$(mktemp -d)/SuperCopier.zip"
            ditto -c -k --keepParent "$APP" "$ZIP"
            notarize "$ZIP"
            rm -rf "$(dirname "$ZIP")"
            # Stapled before the dmg is built, so the app carries its own
            # ticket and still verifies once dragged out of the dmg offline.
            xcrun stapler staple "$APP"
        fi
    else
        # Ad-hoc signature: enough for the Mac that built it, not for a
        # download. See the notes at the top of this file.
        codesign --force --deep -s - "$APP"
    fi

    STAGE="$(mktemp -d)"
    trap 'rm -rf "$STAGE"' EXIT
    cp -R "$APP" "$STAGE/Super Copier.app"
    ln -s /Applications "$STAGE/Applications"

    OUT="packaging/macos/SuperCopier-macOS-$arch_label.dmg"
    rm -f "$OUT"
    hdiutil create -volname "Super Copier" -srcfolder "$STAGE" -ov -format UDZO "$OUT"
    rm -rf "$STAGE"

    if [ -n "$SIGN_IDENTITY" ]; then
        codesign --force --timestamp -s "$SIGN_IDENTITY" "$OUT"
        if [ -n "$NOTARY_PROFILE" ]; then
            notarize "$OUT"
            xcrun stapler staple "$OUT"
        fi
    fi

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
