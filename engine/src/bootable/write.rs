//! Raw image writing: stream the image onto the device in large,
//! sector-aligned chunks, flush, then read everything back and compare.

use std::fs::File;
use std::io::{Read, Write};
use std::path::Path;
use std::time::{Duration, Instant};

use super::{Device, Event};
use crate::{io_err, CancelToken, EngineError, EngineResult};

const CHUNK: usize = 4 * 1024 * 1024;
/// Raw devices only accept reads/writes in whole sectors (512 or 4096
/// bytes depending on the drive) — 4096 satisfies both.
const SECTOR: usize = 4096;

fn round_up(n: usize) -> usize {
    n.div_ceil(SECTOR) * SECTOR
}

fn fill<R: Read>(src: &mut R, buf: &mut [u8]) -> EngineResult<usize> {
    let mut got = 0;
    while got < buf.len() {
        match src.read(&mut buf[got..]) {
            Ok(0) => break,
            Ok(n) => got += n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(EngineError::Other(format!("read failed: {e}"))),
        }
    }
    Ok(got)
}

/// Copies exactly `total` bytes from `src` to `dst`, zero-padding the final
/// chunk up to a whole sector.
pub fn write_stream<R: Read, W: Write>(
    mut src: R,
    mut dst: W,
    total: u64,
    cancel: &CancelToken,
    mut progress: impl FnMut(u64),
) -> EngineResult<()> {
    let mut buf = vec![0u8; CHUNK];
    let mut done = 0u64;
    while done < total {
        if cancel.is_cancelled() {
            return Err(EngineError::Cancelled);
        }
        let want = ((total - done) as usize).min(CHUNK);
        let got = fill(&mut src, &mut buf[..want])?;
        if got < want {
            return Err(EngineError::Other("image ended earlier than expected".into()));
        }
        let padded = round_up(want);
        buf[want..padded].fill(0);
        dst.write_all(&buf[..padded])
            .map_err(|e| EngineError::Other(format!("write to device failed at {done} bytes: {e}")))?;
        done += want as u64;
        progress(done);
    }
    Ok(())
}

/// Reads `total` bytes back from both sides and fails on the first
/// difference.
pub fn verify_stream<A: Read, B: Read>(
    mut image: A,
    mut device: B,
    total: u64,
    cancel: &CancelToken,
    mut progress: impl FnMut(u64),
) -> EngineResult<()> {
    let mut a = vec![0u8; CHUNK];
    let mut b = vec![0u8; CHUNK];
    let mut done = 0u64;
    while done < total {
        if cancel.is_cancelled() {
            return Err(EngineError::Cancelled);
        }
        let want = ((total - done) as usize).min(CHUNK);
        if fill(&mut image, &mut a[..want])? < want {
            return Err(EngineError::Other("image ended earlier than expected".into()));
        }
        let padded = round_up(want);
        if fill(&mut device, &mut b[..padded])? < padded {
            return Err(EngineError::Other(format!("device returned too little data at {done} bytes")));
        }
        if let Some(i) = a[..want].iter().zip(&b[..want]).position(|(x, y)| x != y) {
            return Err(EngineError::Other(format!(
                "verification failed: byte {} on the drive doesn't match the image",
                done + i as u64
            )));
        }
        done += want as u64;
        progress(done);
    }
    Ok(())
}

/// Throttles progress events so a fast write doesn't flood the progress file.
pub(crate) struct Throttle {
    last: Instant,
}

impl Throttle {
    pub(crate) fn new() -> Self {
        Self { last: Instant::now() - Duration::from_secs(1) }
    }
    pub(crate) fn ready(&mut self) -> bool {
        if self.last.elapsed() >= Duration::from_millis(150) {
            self.last = Instant::now();
            true
        } else {
            false
        }
    }
}

pub fn flash_raw(
    image: &Path,
    device: &Device,
    verify: bool,
    cancel: &CancelToken,
    emit: &mut dyn FnMut(Event),
) -> EngineResult<()> {
    let total = std::fs::metadata(image).map_err(|e| io_err(image, e))?.len();
    if round_up(total as usize) as u64 > device.size {
        return Err(EngineError::Other(format!(
            "the image ({}) is larger than the drive ({})",
            total, device.size
        )));
    }

    emit(Event::Phase { name: "Unmounting drive".into() });
    prepare_device(device, emit)?;

    emit(Event::Phase { name: "Writing image".into() });
    let mut throttle = Throttle::new();
    {
        let src = File::open(image).map_err(|e| io_err(image, e))?;
        let mut dst = open_for_write(device)?;
        write_stream(src, &mut dst, total, cancel, |done| {
            if throttle.ready() || done == total {
                emit(Event::Progress { done, total });
            }
        })?;
        emit(Event::Phase { name: "Flushing to the drive".into() });
        flush_device(&mut dst)?;
    }

    if verify {
        emit(Event::Phase { name: "Verifying".into() });
        let src = File::open(image).map_err(|e| io_err(image, e))?;
        let dev = open_for_read(device)?;
        verify_stream(src, dev, total, cancel, |done| {
            if throttle.ready() || done == total {
                emit(Event::Progress { done, total });
            }
        })?;
    }

    finish_device(device, emit);
    Ok(())
}

// ------------------------------------------------------- per-OS plumbing

fn command_ok(cmd: &mut std::process::Command, what: &str) -> EngineResult<()> {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    let out = cmd.output().map_err(|e| EngineError::Other(format!("{what}: {e}")))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(EngineError::Other(format!(
            "{what} failed: {}{}",
            String::from_utf8_lossy(&out.stdout).trim(),
            String::from_utf8_lossy(&out.stderr).trim()
        )))
    }
}

/// Makes the OS let go of the drive so it can be written raw.
pub(crate) fn prepare_device(device: &Device, emit: &mut dyn FnMut(Event)) -> EngineResult<()> {
    #[cfg(target_os = "macos")]
    {
        let _ = emit;
        command_ok(
            std::process::Command::new("diskutil").args(["unmountDisk", "force", &device.id]),
            "unmounting the drive",
        )
    }
    #[cfg(target_os = "linux")]
    {
        let _ = emit;
        for m in &device.mounted {
            command_ok(std::process::Command::new("umount").arg(m), &format!("unmounting {m}"))?;
        }
        Ok(())
    }
    #[cfg(windows)]
    {
        // Wiping the partition table releases every volume on the disk, so
        // Windows lets us open the physical drive for writing.
        emit(Event::Log { line: "Clearing existing partitions".into() });
        diskpart(&format!("select disk {}\nclean\n", windows_disk_number(device)?))
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
    {
        let _ = (device, emit);
        Err(EngineError::Other("unsupported platform".into()))
    }
}

#[cfg(windows)]
pub(crate) fn windows_disk_number(device: &Device) -> EngineResult<u32> {
    device
        .id
        .trim_start_matches("Disk ")
        .parse()
        .map_err(|_| EngineError::Other(format!("unexpected disk id {}", device.id)))
}

#[cfg(windows)]
pub(crate) fn diskpart(script: &str) -> EngineResult<()> {
    let path = std::env::temp_dir().join(format!("supercopier-diskpart-{}.txt", std::process::id()));
    std::fs::write(&path, script).map_err(|e| io_err(&path, e))?;
    let r = command_ok(std::process::Command::new("diskpart").arg("/s").arg(&path), "diskpart");
    let _ = std::fs::remove_file(&path);
    r
}

fn open_for_write(device: &Device) -> EngineResult<File> {
    let mut opts = std::fs::OpenOptions::new();
    opts.read(true).write(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        opts.share_mode(3); // FILE_SHARE_READ | FILE_SHARE_WRITE
    }
    opts.open(&device.node)
        .map_err(|e| EngineError::Other(format!("couldn't open {} for writing: {e} (administrator rights are required)", device.node)))
}

fn open_for_read(device: &Device) -> EngineResult<File> {
    let mut opts = std::fs::OpenOptions::new();
    opts.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        opts.share_mode(3);
    }
    opts.open(&device.node)
        .map_err(|e| EngineError::Other(format!("couldn't reopen {} to verify: {e}", device.node)))
}

fn flush_device(f: &mut File) -> EngineResult<()> {
    f.flush().map_err(|e| EngineError::Other(e.to_string()))?;
    // Some raw devices (macOS character devices, disk images) are already
    // unbuffered and answer fsync with ENOTTY/EINVAL/ENOTSUP — that means
    // "nothing to flush", not failure.
    if let Err(e) = f.sync_all() {
        #[cfg(unix)]
        let unsupported = matches!(e.raw_os_error(), Some(libc::ENOTTY | libc::EINVAL | libc::ENOTSUP));
        #[cfg(not(unix))]
        let unsupported = false;
        if !unsupported {
            return Err(EngineError::Other(format!("flushing the drive failed: {e}")));
        }
    }
    #[cfg(target_os = "macos")]
    {
        use std::os::fd::AsRawFd;
        // sync_all is fsync, which macOS doesn't guarantee reaches the media.
        unsafe { libc::fcntl(f.as_raw_fd(), libc::F_FULLFSYNC) };
    }
    #[cfg(target_os = "linux")]
    {
        use std::os::fd::AsRawFd;
        // Drop the kernel's cached copy so verification reads the real drive.
        const BLKFLSBUF: libc::c_ulong = 0x1261;
        unsafe { libc::ioctl(f.as_raw_fd(), BLKFLSBUF, 0) };
    }
    Ok(())
}

/// Best effort: a failed eject shouldn't fail an otherwise good flash.
pub(crate) fn finish_device(device: &Device, emit: &mut dyn FnMut(Event)) {
    #[cfg(target_os = "macos")]
    {
        if command_ok(std::process::Command::new("diskutil").args(["eject", &device.id]), "ejecting").is_err() {
            emit(Event::Log { line: "Couldn't eject automatically — eject the drive before unplugging.".into() });
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (device, emit);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn data(len: usize) -> Vec<u8> {
        (0..len).map(|i| (i * 31 % 251) as u8).collect()
    }

    #[test]
    fn write_pads_the_last_chunk_to_a_whole_sector_and_reports_progress() {
        let src = data(CHUNK + 1000);
        let mut out = Vec::new();
        let mut last = 0;
        write_stream(Cursor::new(&src), &mut out, src.len() as u64, &CancelToken::new(), |d| last = d).unwrap();
        assert_eq!(last, src.len() as u64);
        assert_eq!(out.len(), CHUNK + SECTOR, "1000 leftover bytes round up to one 4096 sector");
        assert_eq!(&out[..src.len()], &src[..]);
        assert!(out[src.len()..].iter().all(|&b| b == 0));
    }

    #[test]
    fn verify_accepts_a_faithful_copy_and_pinpoints_corruption() {
        let src = data(CHUNK * 2 + 777);
        let mut good = Vec::new();
        write_stream(Cursor::new(&src), &mut good, src.len() as u64, &CancelToken::new(), |_| {}).unwrap();
        verify_stream(Cursor::new(&src), Cursor::new(&good), src.len() as u64, &CancelToken::new(), |_| {}).unwrap();

        let bad_at = CHUNK + 12345;
        good[bad_at] ^= 0xFF;
        let err = verify_stream(Cursor::new(&src), Cursor::new(&good), src.len() as u64, &CancelToken::new(), |_| {})
            .unwrap_err()
            .to_string();
        assert!(err.contains(&bad_at.to_string()), "error should name the bad byte: {err}");
    }

    #[test]
    fn cancelling_stops_the_write_early() {
        let src = data(CHUNK * 3);
        let cancel = CancelToken::new();
        let mut out = Vec::new();
        let r = write_stream(Cursor::new(&src), &mut out, src.len() as u64, &cancel, |_| cancel.cancel());
        assert!(matches!(r, Err(EngineError::Cancelled)));
        assert_eq!(out.len(), CHUNK, "only the first chunk should have been written");
    }

    #[test]
    fn a_truncated_image_is_an_error_not_silent_padding() {
        let src = data(1000);
        let r = write_stream(Cursor::new(&src), Vec::new(), 5000, &CancelToken::new(), |_| {});
        assert!(r.is_err());
    }
}
