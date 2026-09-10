#!/bin/sh
# Builds Super Copier.app and packages it into a distributable .dmg.
#
# Usage: packaging/macos/build_dmg.sh
# Output: packaging/macos/SuperCopier.dmg
set -eu

cd "$(dirname "$0")/../.."

cargo build --release -p super-copier

APP="target/release/Super Copier.app"
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp target/release/super-copier "$APP/Contents/MacOS/super-copier"
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

OUT="packaging/macos/SuperCopier.dmg"
rm -f "$OUT"
hdiutil create -volname "Super Copier" -srcfolder "$STAGE" -ov -format UDZO "$OUT"

echo "Built $OUT"
