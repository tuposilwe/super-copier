//! Basic disk-usage reporting — total/used/free space per volume, the way
//! Disk Utility (macOS) or a drive's Properties dialog (Windows) show it.
//! This is capacity accounting only, not a folder-size breakdown.

use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy)]
pub struct DiskSpace {
    pub total: u64,
    pub free: u64,
}

impl DiskSpace {
    pub fn used(&self) -> u64 {
        self.total.saturating_sub(self.free)
    }

    pub fn used_fraction(&self) -> f32 {
        if self.total == 0 {
            0.0
        } else {
            self.used() as f32 / self.total as f32
        }
    }
}

pub struct DriveUsage {
    pub path: PathBuf,
    pub space: Option<DiskSpace>,
}

/// Reports usage for every drive/volume `drives::list_drives` finds.
pub fn drive_usage() -> Vec<DriveUsage> {
    crate::drives::list_drives()
        .into_iter()
        .map(|path| {
            let space = disk_space(&path).ok();
            DriveUsage { path, space }
        })
        .collect()
}

pub fn disk_space(path: &Path) -> std::io::Result<DiskSpace> {
    #[cfg(windows)]
    {
        windows_disk_space(path)
    }
    #[cfg(unix)]
    {
        unix_disk_space(path)
    }
    #[cfg(not(any(windows, unix)))]
    {
        let _ = path;
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "disk usage isn't supported on this platform",
        ))
    }
}

#[cfg(windows)]
fn windows_disk_space(path: &Path) -> std::io::Result<DiskSpace> {
    use std::os::windows::ffi::OsStrExt;

    use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;

    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
    let mut free_to_caller: u64 = 0;
    let mut total: u64 = 0;
    let mut total_free: u64 = 0;

    let ok = unsafe {
        GetDiskFreeSpaceExW(
            wide.as_ptr(),
            &mut free_to_caller,
            &mut total,
            &mut total_free,
        )
    };

    if ok == 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(DiskSpace {
            total,
            free: total_free,
        })
    }
}

#[cfg(unix)]
fn unix_disk_space(path: &Path) -> std::io::Result<DiskSpace> {
    use std::ffi::CString;
    use std::mem::MaybeUninit;
    use std::os::unix::ffi::OsStrExt;

    let c_path = CString::new(path.as_os_str().as_bytes())?;
    let mut stat = MaybeUninit::<libc::statvfs>::uninit();
    let ret = unsafe { libc::statvfs(c_path.as_ptr(), stat.as_mut_ptr()) };
    if ret != 0 {
        return Err(std::io::Error::last_os_error());
    }
    let stat = unsafe { stat.assume_init() };

    // The exact field width varies by libc (e.g. narrower on some 32-bit
    // targets), so the cast isn't a no-op everywhere even though clippy
    // flags it as one on this machine.
    #[allow(clippy::unnecessary_cast)]
    let block_size = stat.f_frsize as u64;
    Ok(DiskSpace {
        total: stat.f_blocks as u64 * block_size,
        free: stat.f_bavail as u64 * block_size,
    })
}
