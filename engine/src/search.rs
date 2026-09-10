//! Name search across one or more folders — typically every attached
//! drive, for a whole-disk search. Filtering happens during the walk so a
//! disk-wide search doesn't have to hold every file in memory before it can
//! narrow down to matches.

use std::path::PathBuf;

use crossbeam_channel::Sender;
use walkdir::WalkDir;

use crate::{CancelToken, EngineError, EngineResult};

#[derive(Debug, Clone)]
pub struct SearchOptions {
    /// Substring to match against each entry's file name.
    pub query: String,
    pub case_sensitive: bool,
    /// Match folders too, not just files.
    pub include_dirs: bool,
}

#[derive(Debug, Clone)]
pub struct SearchMatch {
    pub path: PathBuf,
    pub size: u64,
    pub is_dir: bool,
}

#[derive(Debug, Clone)]
pub enum SearchEvent {
    Scanning { files_scanned: usize },
    Found(SearchMatch),
    Finished { count: usize },
    Cancelled,
}

pub fn search_files(
    roots: &[PathBuf],
    options: SearchOptions,
    cancel: CancelToken,
    tx: Sender<SearchEvent>,
) -> EngineResult<Vec<SearchMatch>> {
    let needle = if options.case_sensitive {
        options.query.clone()
    } else {
        options.query.to_lowercase()
    };

    let mut results = Vec::new();
    let mut files_scanned = 0usize;

    for root in roots {
        for entry in WalkDir::new(root).into_iter().filter_map(|e| e.ok()) {
            if cancel.is_cancelled() {
                let _ = tx.send(SearchEvent::Cancelled);
                return Err(EngineError::Cancelled);
            }

            let is_dir = entry.file_type().is_dir();
            if is_dir && !options.include_dirs {
                continue;
            }
            if !is_dir && !entry.file_type().is_file() {
                continue;
            }

            files_scanned += 1;
            if files_scanned.is_multiple_of(2000) {
                let _ = tx.send(SearchEvent::Scanning { files_scanned });
            }

            let name = entry.file_name().to_string_lossy();
            let matched = if needle.is_empty() {
                false
            } else if options.case_sensitive {
                name.contains(&needle)
            } else {
                name.to_lowercase().contains(&needle)
            };

            if matched {
                let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
                let m = SearchMatch {
                    path: entry.path().to_path_buf(),
                    size,
                    is_dir,
                };
                let _ = tx.send(SearchEvent::Found(m.clone()));
                results.push(m);
            }
        }
    }

    let _ = tx.send(SearchEvent::Finished { count: results.len() });
    Ok(results)
}
