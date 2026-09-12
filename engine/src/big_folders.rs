//! Finds which folders are actually taking up space, the way WinDirStat or
//! Disk Utility's folder breakdown does — distinct from `large_files`, which
//! finds individual large *files* rather than folders. Every directory
//! under a root gets a cumulative size (itself plus everything nested
//! inside it), not just its immediate children, so a deeply nested culprit
//! like `node_modules` shows up even if nothing at the top level looks
//! big.
//!
//! Folder totals aren't known until the whole subtree under them has been
//! walked, so unlike `large_files`/`search`, results can't be streamed as
//! they're found — they're all sent at once, right before `Finished`.

use std::collections::HashMap;
use std::path::PathBuf;

use crossbeam_channel::Sender;
use walkdir::WalkDir;

use crate::{CancelToken, EngineError, EngineResult};

#[derive(Debug, Clone)]
pub struct BigFoldersOptions {
    pub min_size: u64,
}

impl Default for BigFoldersOptions {
    fn default() -> Self {
        Self {
            min_size: 500 * 1024 * 1024, // 500 MB
        }
    }
}

#[derive(Debug, Clone)]
pub struct FolderEntry {
    pub path: PathBuf,
    pub size: u64,
    pub file_count: usize,
}

#[derive(Debug, Clone)]
pub enum BigFolderEvent {
    Scanning { files_scanned: usize },
    Found(FolderEntry),
    Finished { count: usize },
    Cancelled,
}

pub fn find_big_folders(
    roots: &[PathBuf],
    options: BigFoldersOptions,
    cancel: CancelToken,
    tx: Sender<BigFolderEvent>,
) -> EngineResult<Vec<FolderEntry>> {
    let mut totals: HashMap<PathBuf, (u64, usize)> = HashMap::new();
    let mut files_scanned = 0usize;

    for root in roots {
        for entry in WalkDir::new(root).into_iter().filter_map(|e| e.ok()) {
            if cancel.is_cancelled() {
                let _ = tx.send(BigFolderEvent::Cancelled);
                return Err(EngineError::Cancelled);
            }
            if !entry.file_type().is_file() {
                continue;
            }
            files_scanned += 1;
            if files_scanned.is_multiple_of(1000) {
                let _ = tx.send(BigFolderEvent::Scanning { files_scanned });
            }

            let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
            let Some(parent) = entry.path().parent() else { continue };
            for ancestor in parent.ancestors() {
                if !ancestor.starts_with(root) {
                    break;
                }
                let totals_entry = totals.entry(ancestor.to_path_buf()).or_insert((0, 0));
                totals_entry.0 += size;
                totals_entry.1 += 1;
                if ancestor == root.as_path() {
                    break;
                }
            }
        }
    }

    let mut results: Vec<FolderEntry> = totals
        .into_iter()
        .filter(|(_, (size, _))| *size >= options.min_size)
        .map(|(path, (size, file_count))| FolderEntry { path, size, file_count })
        .collect();
    results.sort_by_key(|f| std::cmp::Reverse(f.size));

    for f in &results {
        let _ = tx.send(BigFolderEvent::Found(f.clone()));
    }
    let _ = tx.send(BigFolderEvent::Finished { count: results.len() });

    Ok(results)
}
