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

## Project layout

- `engine/` — the core library: copy engine, hashing, duplicate finder,
  file ops (move/rename/delete/organize), sync. Has no UI dependencies,
  so it's also usable from a CLI or tests.
- `app/` — the `super-copier` binary: an [egui]/[eframe] desktop UI with
  four tabs (Copy, Duplicates, Organize, Sync).

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

This was developed and tested on macOS. The Windows-specific code (the
`CopyFileExW` fast path in `engine/src/platform/windows.rs`) has been
type-checked against the real `windows-sys` bindings via
`cargo check --target x86_64-pc-windows-msvc`, but **has not been run on
real Windows yet**. Before relying on it:

```sh
# On a Windows machine, or via a Windows CI runner:
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
