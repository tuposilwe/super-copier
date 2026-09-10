//! Windows fast path: `CopyFileExW`, which lets the OS pick the most
//! efficient copy strategy (including server-side/remote-share copy and
//! Copy Offload on supported storage) and reports progress via a callback,
//! which we use both to stream progress to the UI and to support
//! cooperative cancellation.

use std::ffi::c_void;
use std::os::windows::ffi::OsStrExt;
use std::path::Path;

use windows_sys::Win32::Foundation::{BOOL, FALSE, HANDLE, TRUE};
use windows_sys::Win32::Storage::FileSystem::CopyFileExW;

use crate::CancelToken;

struct CallbackCtx<'a> {
    on_progress: &'a mut dyn FnMut(u64),
    cancel: &'a CancelToken,
}

unsafe extern "system" fn progress_routine(
    _total_file_size: i64,
    total_bytes_transferred: i64,
    _stream_size: i64,
    _stream_bytes_transferred: i64,
    _stream_number: u32,
    _callback_reason: u32,
    _src_handle: HANDLE,
    _dst_handle: HANDLE,
    data: *const c_void,
) -> u32 {
    // PROGRESS_CONTINUE / PROGRESS_CANCEL
    const PROGRESS_CONTINUE: u32 = 0;
    const PROGRESS_CANCEL: u32 = 1;

    let ctx = &mut *(data as *mut CallbackCtx);
    (ctx.on_progress)(total_bytes_transferred.max(0) as u64);

    if ctx.cancel.is_cancelled() {
        PROGRESS_CANCEL
    } else {
        PROGRESS_CONTINUE
    }
}

fn to_wide(path: &Path) -> Vec<u16> {
    path.as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

/// Copies `src` to `dst` using `CopyFileExW`, invoking `on_progress(bytes_so_far)`
/// as the OS reports progress. Returns the number of bytes copied, or an
/// error (including a cancellation, surfaced as an `Interrupted` io error)
/// if the copy did not complete.
pub fn copy_file_with_progress<F>(
    src: &Path,
    dst: &Path,
    mut on_progress: F,
    cancel: &CancelToken,
) -> std::io::Result<u64>
where
    F: FnMut(u64),
{
    let src_w = to_wide(src);
    let dst_w = to_wide(dst);

    let mut ctx = CallbackCtx {
        on_progress: &mut on_progress,
        cancel,
    };

    let mut cancel_flag: BOOL = FALSE;
    let ok = unsafe {
        CopyFileExW(
            src_w.as_ptr(),
            dst_w.as_ptr(),
            Some(progress_routine),
            &mut ctx as *mut CallbackCtx as *const c_void,
            &mut cancel_flag as *mut BOOL,
            0u32,
        )
    };

    if ok == TRUE {
        std::fs::metadata(dst).map(|m| m.len())
    } else {
        let err = std::io::Error::last_os_error();
        if cancel.is_cancelled() {
            Err(std::io::Error::new(
                std::io::ErrorKind::Interrupted,
                "copy cancelled",
            ))
        } else {
            Err(err)
        }
    }
}
