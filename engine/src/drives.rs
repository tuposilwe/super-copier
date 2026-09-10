//! Lists locally attached drives/volumes so the UI can offer them as
//! one-click scan roots (e.g. `C:\`, `D:\` on Windows; mounted volumes on
//! macOS) instead of requiring the user to browse to them manually.

use std::path::PathBuf;

pub fn list_drives() -> Vec<PathBuf> {
    #[cfg(windows)]
    {
        windows_drives()
    }
    #[cfg(target_os = "macos")]
    {
        macos_volumes()
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        vec![PathBuf::from("/")]
    }
}

#[cfg(windows)]
fn windows_drives() -> Vec<PathBuf> {
    (b'A'..=b'Z')
        .filter_map(|letter| {
            let path = PathBuf::from(format!("{}:\\", letter as char));
            path.exists().then_some(path)
        })
        .collect()
}

#[cfg(target_os = "macos")]
fn macos_volumes() -> Vec<PathBuf> {
    let mut drives = vec![PathBuf::from("/")];
    if let Ok(entries) = std::fs::read_dir("/Volumes") {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                drives.push(path);
            }
        }
    }
    drives
}
