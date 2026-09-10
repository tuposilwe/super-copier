//! Move, rename, delete and auto-organize operations.

use std::path::{Path, PathBuf};

use chrono::{DateTime, Datelike};
use crossbeam_channel::Sender;
use filetime::FileTime;

use crate::{io_err, CancelToken, EngineError, EngineResult};

/// Moves `src` to `dst`. Tries a cheap rename first; if that fails because
/// the paths are on different volumes, falls back to copy-then-delete.
pub fn move_path(src: &Path, dst: &Path) -> EngineResult<()> {
    if let Some(parent) = dst.parent() {
        std::fs::create_dir_all(parent).map_err(|e| io_err(parent, e))?;
    }
    match std::fs::rename(src, dst) {
        Ok(()) => Ok(()),
        Err(e) if is_cross_device(&e) => {
            copy_recursive(src, dst)?;
            if src.is_dir() {
                std::fs::remove_dir_all(src).map_err(|e| io_err(src, e))?;
            } else {
                std::fs::remove_file(src).map_err(|e| io_err(src, e))?;
            }
            Ok(())
        }
        Err(e) => Err(io_err(src, e)),
    }
}

fn is_cross_device(e: &std::io::Error) -> bool {
    #[cfg(unix)]
    {
        e.raw_os_error() == Some(libc_exdev())
    }
    #[cfg(windows)]
    {
        // ERROR_NOT_SAME_DEVICE
        e.raw_os_error() == Some(17)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = e;
        false
    }
}

#[cfg(unix)]
fn libc_exdev() -> i32 {
    #[cfg(target_os = "macos")]
    {
        libc::EXDEV
    }
    #[cfg(not(target_os = "macos"))]
    {
        18 // EXDEV on Linux
    }
}

fn copy_recursive(src: &Path, dst: &Path) -> EngineResult<()> {
    let meta = std::fs::metadata(src).map_err(|e| io_err(src, e))?;
    if meta.is_dir() {
        std::fs::create_dir_all(dst).map_err(|e| io_err(dst, e))?;
        for entry in std::fs::read_dir(src).map_err(|e| io_err(src, e))? {
            let entry = entry.map_err(|e| io_err(src, e))?;
            let name = entry.file_name();
            copy_recursive(&entry.path(), &dst.join(name))?;
        }
    } else {
        std::fs::copy(src, dst).map_err(|e| io_err(src, e))?;
    }
    Ok(())
}

pub fn rename_path(path: &Path, new_name: &str) -> EngineResult<PathBuf> {
    let parent = path
        .parent()
        .ok_or_else(|| EngineError::Other(format!("no parent for {}", path.display())))?;
    let dst = parent.join(new_name);
    std::fs::rename(path, &dst).map_err(|e| io_err(path, e))?;
    Ok(dst)
}

/// Expands a rename pattern for one item. Recognised tokens:
/// - `{name}` — original file stem (name without extension)
/// - `{ext}` — original extension (without the dot)
/// - `{n}`, `{nn}`, `{nnn}`, ... — the 1-based `index`, zero-padded to the
///   number of `n`s (`{n}` = no padding, `{nnn}` = at least 3 digits)
///
/// Unrecognised `{...}` tokens are left untouched.
pub fn apply_rename_pattern(pattern: &str, stem: &str, ext: &str, index: usize) -> String {
    let mut result = pattern.replace("{name}", stem).replace("{ext}", ext);

    let mut search_from = 0;
    while let Some(rel_start) = result[search_from..].find('{') {
        let start = search_from + rel_start;
        let Some(rel_end) = result[start..].find('}') else {
            break;
        };
        let end = start + rel_end;
        let token = &result[start + 1..end];
        if !token.is_empty() && token.chars().all(|c| c == 'n') {
            let width = token.len();
            let replacement = format!("{index:0width$}");
            result.replace_range(start..=end, &replacement);
            search_from = start + replacement.len();
        } else {
            search_from = end + 1;
        }
    }

    result
}

/// Renames `paths` in place using `pattern` (see [`apply_rename_pattern`]),
/// numbering items `1..=paths.len()` in the order given. Returns the new
/// paths in the same order. Stops at the first failure; items already
/// renamed are left renamed (not rolled back).
pub fn rename_batch(paths: &[PathBuf], pattern: &str) -> EngineResult<Vec<PathBuf>> {
    let mut results = Vec::with_capacity(paths.len());
    for (i, path) in paths.iter().enumerate() {
        let parent = path
            .parent()
            .ok_or_else(|| EngineError::Other(format!("no parent for {}", path.display())))?;
        let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
        let ext = path.extension().and_then(|s| s.to_str()).unwrap_or("");
        let new_name = apply_rename_pattern(pattern, stem, ext, i + 1);
        let dst = parent.join(&new_name);
        std::fs::rename(path, &dst).map_err(|e| io_err(path, e))?;
        results.push(dst);
    }
    Ok(results)
}

/// Sends files to the OS trash / Recycle Bin (recoverable).
pub fn delete_to_trash(paths: &[PathBuf]) -> EngineResult<()> {
    trash::delete_all(paths).map_err(|e| EngineError::Other(e.to_string()))
}

/// Permanently deletes files/directories. Not recoverable.
pub fn delete_permanently(paths: &[PathBuf]) -> EngineResult<()> {
    for p in paths {
        let meta = std::fs::metadata(p).map_err(|e| io_err(p, e))?;
        if meta.is_dir() {
            std::fs::remove_dir_all(p).map_err(|e| io_err(p, e))?;
        } else {
            std::fs::remove_file(p).map_err(|e| io_err(p, e))?;
        }
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrganizeStrategy {
    /// `dir/Images/photo.jpg`, `dir/Documents/report.pdf`, ...
    ByExtension,
    /// `dir/2026-09/photo.jpg`, based on modification time.
    ByDate,
    /// `dir/Images/2026-09/photo.jpg`
    ByExtensionAndDate,
}

#[derive(Debug, Clone)]
pub enum OrganizeEvent {
    FileMoved { from: PathBuf, to: PathBuf },
    FileError { path: PathBuf, message: String },
    Finished { moved: usize },
}

#[derive(Debug, Clone, Default)]
pub struct OrganizeSummary {
    pub moved: usize,
}

const EXTENSION_CATEGORIES: &[(&[&str], &str)] = &[
    (&["jpg", "jpeg", "png", "gif", "heic", "webp", "bmp", "tiff", "raw"], "Images"),
    (&["mp4", "mov", "avi", "mkv", "webm", "m4v"], "Videos"),
    (&["mp3", "wav", "flac", "aac", "m4a", "ogg"], "Audio"),
    (&["pdf", "doc", "docx", "txt", "rtf", "odt", "pages"], "Documents"),
    (&["xls", "xlsx", "csv", "numbers"], "Spreadsheets"),
    (&["ppt", "pptx", "key"], "Presentations"),
    (&["zip", "rar", "7z", "tar", "gz", "dmg"], "Archives"),
    (&["exe", "msi", "app", "pkg", "apk"], "Installers"),
];

fn category_for_extension(ext: &str) -> &'static str {
    let ext_lower = ext.to_lowercase();
    for (exts, category) in EXTENSION_CATEGORIES {
        if exts.contains(&ext_lower.as_str()) {
            return category;
        }
    }
    "Other"
}

/// Moves each file directly inside `dir` (non-recursive) into a subfolder
/// determined by `strategy`. Files already sitting in a correctly named
/// destination subfolder are left alone.
pub fn organize_dir(
    dir: &Path,
    strategy: OrganizeStrategy,
    cancel: CancelToken,
    tx: Sender<OrganizeEvent>,
) -> EngineResult<OrganizeSummary> {
    let mut moved = 0usize;

    let entries: Vec<PathBuf> = std::fs::read_dir(dir)
        .map_err(|e| io_err(dir, e))?
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().map(|t| t.is_file()).unwrap_or(false))
        .map(|e| e.path())
        .collect();

    for path in entries {
        if cancel.is_cancelled() {
            return Err(EngineError::Cancelled);
        }

        let subfolder = match strategy {
            OrganizeStrategy::ByExtension => {
                let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
                category_for_extension(ext).to_string()
            }
            OrganizeStrategy::ByDate => date_folder_name(&path)?,
            OrganizeStrategy::ByExtensionAndDate => {
                let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
                format!("{}/{}", category_for_extension(ext), date_folder_name(&path)?)
            }
        };

        let dest_dir = dir.join(&subfolder);
        let file_name = match path.file_name() {
            Some(n) => n,
            None => continue,
        };
        let dest_path = dest_dir.join(file_name);

        if dest_path == path {
            continue;
        }

        match move_path(&path, &dest_path) {
            Ok(()) => {
                moved += 1;
                let _ = tx.send(OrganizeEvent::FileMoved {
                    from: path,
                    to: dest_path,
                });
            }
            Err(e) => {
                let _ = tx.send(OrganizeEvent::FileError {
                    path,
                    message: e.to_string(),
                });
            }
        }
    }

    let _ = tx.send(OrganizeEvent::Finished { moved });
    Ok(OrganizeSummary { moved })
}

fn date_folder_name(path: &Path) -> EngineResult<String> {
    let meta = std::fs::metadata(path).map_err(|e| io_err(path, e))?;
    let mtime = FileTime::from_last_modification_time(&meta);
    let dt = DateTime::from_timestamp(mtime.unix_seconds(), 0)
        .ok_or_else(|| EngineError::Other("invalid mtime".into()))?;
    Ok(format!("{:04}-{:02}", dt.year(), dt.month()))
}
