//! macOS fast path: `clonefile(2)` gives an instant, copy-on-write clone on
//! APFS (same volume) — no data is actually duplicated on disk until one
//! side is modified. This is dramatically faster than a byte-for-byte copy
//! for same-volume copies and is what Finder itself uses.

use std::ffi::CString;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

extern "C" {
    fn clonefile(src: *const libc::c_char, dst: *const libc::c_char, flags: u32) -> libc::c_int;
}

/// Attempts a copy-on-write clone of `src` to `dst`. `dst` must not already
/// exist. Returns `Ok(true)` if the clone succeeded, `Ok(false)` if cloning
/// isn't possible here (different volume, non-APFS, destination exists, ...)
/// so the caller should fall back to a regular copy, or `Err` for a genuine
/// I/O error worth surfacing.
pub fn try_clone_file(src: &Path, dst: &Path) -> std::io::Result<bool> {
    let src_c = CString::new(src.as_os_str().as_bytes())?;
    let dst_c = CString::new(dst.as_os_str().as_bytes())?;

    let ret = unsafe { clonefile(src_c.as_ptr(), dst_c.as_ptr(), 0) };
    if ret == 0 {
        return Ok(true);
    }

    let err = std::io::Error::last_os_error();
    match err.raw_os_error() {
        // Not supported on this filesystem, cross-device, or dst exists:
        // all just mean "can't clone here", not a real failure.
        Some(libc::ENOTSUP) | Some(libc::EXDEV) | Some(libc::EEXIST) | Some(libc::ENOSYS) => {
            Ok(false)
        }
        _ => Err(err),
    }
}
