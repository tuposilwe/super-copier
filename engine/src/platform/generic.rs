//! Portable chunked copy used as the fallback on every platform, and as the
//! only strategy whenever resume or in-flight verification is requested
//! (the OS-accelerated whole-file paths can't report mid-file progress in a
//! resumable way).

use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;

use crate::{io_err, CancelToken, EngineError, EngineResult};

/// 8 MiB strikes a good balance between syscall overhead and memory use for
/// both spinning disks and SSDs/NVMe.
pub const BUF_SIZE: usize = 8 * 1024 * 1024;

/// Copies `src` to `dst`, calling `on_progress(bytes_copied_this_call, total_bytes_so_far)`
/// after every chunk. If `resume_from` is `Some(offset)`, `dst` is assumed to
/// already contain `offset` valid bytes and the copy continues from there.
pub fn chunked_copy<F>(
    src: &Path,
    dst: &Path,
    total_size: u64,
    resume_from: Option<u64>,
    cancel: &CancelToken,
    mut on_progress: F,
) -> EngineResult<u64>
where
    F: FnMut(u64),
{
    let mut in_file = File::open(src).map_err(|e| io_err(src, e))?;
    let start_offset = resume_from.unwrap_or(0);

    let mut out_file = if start_offset > 0 {
        let mut f = OpenOptions::new()
            .write(true)
            .open(dst)
            .map_err(|e| io_err(dst, e))?;
        f.seek(SeekFrom::Start(start_offset))
            .map_err(|e| io_err(dst, e))?;
        f
    } else {
        let f = File::create(dst).map_err(|e| io_err(dst, e))?;
        // Best-effort preallocation to reduce fragmentation; ignore failure
        // (not all filesystems support it, and it's not required for correctness).
        let _ = f.set_len(total_size);
        f
    };

    if start_offset > 0 {
        in_file
            .seek(SeekFrom::Start(start_offset))
            .map_err(|e| io_err(src, e))?;
    }

    let mut buf = vec![0u8; BUF_SIZE];
    let mut copied = start_offset;

    loop {
        if cancel.is_cancelled() {
            return Err(EngineError::Cancelled);
        }
        let n = in_file.read(&mut buf).map_err(|e| io_err(src, e))?;
        if n == 0 {
            break;
        }
        out_file
            .write_all(&buf[..n])
            .map_err(|e| io_err(dst, e))?;
        copied += n as u64;
        on_progress(copied);
    }

    out_file.flush().map_err(|e| io_err(dst, e))?;
    Ok(copied)
}
