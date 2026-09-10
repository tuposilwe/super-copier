# Super Copier

A fast, cross-platform (Windows + macOS) file-copy and file-management tool
written in Rust, with a native GUI.

## Features

- **Fast copy** — multi-threaded, with OS-accelerated whole-file paths:
  `clonefile` (instant copy-on-write) on macOS/APFS, `CopyFileExW` on
  Windows. Falls back to a large-buffer chunked copy elsewhere or when
  resume/verify is requested.
- **Resume** — re-running a copy skips files already copied (same size),
  so an interrupted job picks up where it left off.
- **Verify** — optional post-copy BLAKE3 hash comparison for
  byte-for-byte integrity.
- **Duplicate finder** — size → partial-hash → full-hash funnel so it only
  fully hashes files that are already very likely duplicates.
- **Organize** — sorts a folder's files into subfolders by type, by
  month, or both.
- **Sync / mirror** — one-way folder sync, optionally deleting files in
  the destination that no longer exist in the source (like `rsync
  --delete`).
- **Delete to Trash / Recycle Bin** — duplicate cleanup is recoverable by
  default.
- **Whole-disk search** — find files by name, defaulting to every
  attached drive when no folder is chosen; Duplicates and Big Files also
  get a one-click "Scan Entire Disk".
- **Drag-and-drop** everywhere, including a dedicated destination drop
  zone on Copy/Move and Sync.
- **Reveal in Finder/Explorer** and **desktop notifications** when a job
  finishes.

## Project layout

- `engine/` — the core library: copy engine, hashing, duplicate finder,
  large-file finder, whole-disk search, drive listing, file ops
  (move/rename/delete/organize), sync. Has no UI dependencies, so it's
  also usable from a CLI or tests.
- `app/` — the `super-copier` binary: an [egui]/[eframe] desktop UI with
  six tabs (Copy/Move/Rename, Search, Duplicates, Big Files, Organize,
  Sync).

[egui]: https://github.com/emilk/egui
[eframe]: https://github.com/emilk/egui/tree/master/crates/eframe

## Building

Requires a stable Rust toolchain (install via [rustup](https://rustup.rs)).

```sh
cargo build --release
```

The binary is written to `target/release/super-copier` (`.exe` on Windows).

Run the tests:

```sh
cargo test -p engine
```

Run in development:

```sh
cargo run -p super-copier
```

### Building the Windows binary

This was developed on macOS. Two ways to get a `.exe`:

**Cross-compile from macOS** (verified working — this is how the project
was actually built and linked for Windows during development):

```sh
brew install mingw-w64
rustup target add x86_64-pc-windows-gnu
cargo build --release -p super-copier --target x86_64-pc-windows-gnu
# -> target/x86_64-pc-windows-gnu/release/super-copier.exe
```

This repo's `.cargo/config.toml` points the `x86_64-pc-windows-gnu`
target at the MinGW linker so the command above works without extra
flags. The whole dependency graph — including wgpu/winit/egui, the
`CopyFileExW` fast path, and the WinRT notification backend — compiles
and links cleanly this way. What this *can't* verify is runtime behavior
(no Wine is installed here), so treat a fresh Windows machine as the
final check, especially for the OS-specific paths (`CopyFileExW`,
Explorer's `/select,` reveal, toast notifications).

**Build natively on Windows** (MSVC, the more common target for
distribution):

```sh
cargo build --release
cargo test -p engine
```

If anything in the Windows fast path misbehaves, the safe fallback is to
turn off "Use OS fast-copy" in the Copy tab (or set
`CopyOptions::use_fast_path = false`), which uses the portable chunked
copy on every platform.

## Icon

`app/assets/icon-1024.png` is the master icon (1024x1024, generated once
with Pillow — see the git history of that file for the script). Every
other icon asset is derived from it and already checked in, so you don't
need to regenerate them unless you change the artwork:

- `icon_256.rgba` — raw 256x256 RGBA8 pixels, `include_bytes!`'d into
  `main.rs` and set via `ViewportBuilder::with_icon` for the *running*
  window/Dock/taskbar icon (both platforms).
- `icon.icns` — macOS bundle icon (see below).
- `icon.ico` + `icon.rc` — compiled into the `.exe` itself as its file
  icon via `build.rs` (using the [`embed-resource`] crate, which no-ops
  on non-Windows targets).

If you change `icon-1024.png`, regenerate the rest:

```sh
cd app/assets
python3 - <<'EOF'
from PIL import Image
img = Image.open("icon-1024.png").convert("RGBA")
img.save("icon.ico", sizes=[(16,16),(32,32),(48,48),(64,64),(128,128),(256,256)])
img.resize((256, 256), Image.LANCZOS).tobytes()
with open("icon_256.rgba", "wb") as f:
    f.write(img.resize((256, 256), Image.LANCZOS).tobytes())
EOF

mkdir -p AppIcon.iconset
for size in 16 32 128 256 512; do
  sips -z $size $size icon-1024.png --out "AppIcon.iconset/icon_${size}x${size}.png"
  sips -z $((size*2)) $((size*2)) icon-1024.png --out "AppIcon.iconset/icon_${size}x${size}@2x.png"
done
sips -z 1024 1024 icon-1024.png --out AppIcon.iconset/icon_512x512@2x.png
iconutil -c icns AppIcon.iconset -o icon.icns
rm -rf AppIcon.iconset
```

[`embed-resource`]: https://crates.io/crates/embed-resource

## Packaging

`cargo build --release` alone gives you a bare executable — it runs
(with the right icon in its own window/Dock/taskbar, per above), but
it's not something Finder/Explorer treats as an installable app (no
Finder icon, no proper Dock entry, and on macOS it's just a Unix binary
rather than a `.app`).

### macOS: build a `.app` bundle

```sh
cargo build --release -p super-copier

APP="target/release/Super Copier.app"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp target/release/super-copier "$APP/Contents/MacOS/super-copier"
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
# flagging it as damaged/unsigned.
codesign --force --deep -s - "$APP"
```

Then drag `target/release/Super Copier.app` into `/Applications` (or
just double-click it in place — it runs fine either way). If the Finder
icon looks stale after rebuilding, that's the icon cache, not the app —
`touch` the `.app` and/or relaunch Finder.

This ad-hoc signature is only good for **this Mac**. If you copy the
`.app` to another machine or hand it to someone else, Gatekeeper will
block it there until they right-click → Open once (no real Apple
Developer certificate is involved). For real distribution you'd need to
sign with a Developer ID and notarize via `xcrun notarytool` — out of
scope here.

### Windows

The `.exe` from `cargo build --release` (see above) is already a normal
double-clickable Windows application, icon included — no bundling step
needed to just run it.

## Installers

For actually distributing the app (rather than handing someone a bare
binary), `packaging/` has a real installer for each platform.

### macOS: `.dmg`

```sh
./packaging/macos/build_dmg.sh
# -> packaging/macos/SuperCopier.dmg
```

Builds the release binary, assembles `Super Copier.app` (see
[Packaging](#packaging) above), ad-hoc signs it, and wraps it in a
`.dmg` with an `Applications` symlink alongside it — the standard
"drag the app onto Applications" flow. Verified: the script runs
end-to-end, the resulting `.dmg` mounts, and the app inside launches.

The same ad-hoc-signature caveat from the `.app` section applies: this
`.dmg` will trigger Gatekeeper's "unidentified developer" prompt on any
Mac other than the one that built it.

### Windows: `SuperCopierSetup.exe`

Built with [NSIS] (`brew install makensis` — a real Windows installer
compiler that happens to also run on macOS/Linux):

```sh
cargo build --release -p super-copier --target x86_64-pc-windows-gnu
cd packaging/windows
makensis -DVERSION=0.1.0 installer.nsi
# -> packaging/windows/SuperCopierSetup.exe
```

`installer.nsi` installs to `Program Files\Super Copier`, adds Start
Menu and Desktop shortcuts, and registers a proper uninstaller under
Add/Remove Programs. It compiles cleanly to a real NSIS installer `.exe`
(confirmed with `file`) — what's *not* verified is running the install
wizard itself, since that needs an actual Windows machine (no Wine
here). If you build the underlying `.exe` on Windows instead of
cross-compiling, just point `EXE_PATH` at it:

```sh
makensis -DVERSION=0.1.0 -DEXE_PATH=..\..\target\release\super-copier.exe installer.nsi
```

[NSIS]: https://nsis.sourceforge.io/

## Notes on the fast-copy design

- On **macOS/APFS**, `clonefile(2)` makes same-volume copies effectively
  instant (copy-on-write — no data is duplicated on disk until one side
  is later modified). This is what Finder itself uses.
- On **Windows**, `CopyFileExW` lets the OS choose the most efficient
  strategy (including offloaded copy on supported storage) and reports
  progress via a callback, which also drives cancellation.
- Both fast paths only apply to **whole-file** copies without resume or
  in-flight verification; those modes use the portable chunked copier
  instead, since the OS calls don't expose enough control for either.
- Multiple files are copied concurrently (engine copies the largest
  files first to keep every worker thread busy for longer).
