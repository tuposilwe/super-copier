//! Finds files above a size threshold under one or more folders. Search and
//! sort are left to the caller (the UI) — this just does the one expensive
//! part, walking the filesystem, and streams matches back as they're found.

use std::path::PathBuf;

use crossbeam_channel::Sender;
use walkdir::WalkDir;

use crate::{CancelToken, EngineError, EngineResult};

#[derive(Debug, Clone)]
pub struct LargeFilesOptions {
    pub min_size: u64,
}

impl Default for LargeFilesOptions {
    fn default() -> Self {
        Self {
            min_size: 100 * 1024 * 1024, // 100 MB
        }
    }
}

#[derive(Debug, Clone)]
pub struct FileEntry {
    pub path: PathBuf,
    pub size: u64,
}

#[derive(Debug, Clone)]
pub enum LargeFileEvent {
    Scanning { files_scanned: usize },
    Found(FileEntry),
    Finished { count: usize, total_bytes: u64 },
    Cancelled,
}

pub fn find_large_files(
    roots: &[PathBuf],
    options: LargeFilesOptions,
    cancel: CancelToken,
    tx: Sender<LargeFileEvent>,
) -> EngineResult<Vec<FileEntry>> {
    let mut results = Vec::new();
    let mut files_scanned = 0usize;
    let mut total_bytes = 0u64;

    for root in roots {
        for entry in WalkDir::new(root).into_iter().filter_map(|e| e.ok()) {
            if cancel.is_cancelled() {
                let _ = tx.send(LargeFileEvent::Cancelled);
                return Err(EngineError::Cancelled);
            }
            if !entry.file_type().is_file() {
                continue;
            }
            files_scanned += 1;
            if files_scanned.is_multiple_of(1000) {
                let _ = tx.send(LargeFileEvent::Scanning { files_scanned });
            }

            let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
            if size >= options.min_size {
                let file_entry = FileEntry {
                    path: entry.path().to_path_buf(),
                    size,
                };
                total_bytes += size;
                let _ = tx.send(LargeFileEvent::Found(file_entry.clone()));
                results.push(file_entry);
            }
        }
    }

    results.sort_by_key(|f| std::cmp::Reverse(f.size));

    let _ = tx.send(LargeFileEvent::Finished {
        count: results.len(),
        total_bytes,
    });

    Ok(results)
}
