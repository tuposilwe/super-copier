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
