//! Duplicate file finder. Uses a three-stage funnel so we only ever pay for
//! a full-file hash on files that are already extremely likely to be
//! duplicates: group by size -> group by partial hash (first 64 KiB) ->
//! confirm with a full hash.

use std::collections::HashMap;
use std::path::PathBuf;

use crossbeam_channel::Sender;
use rayon::prelude::*;
use walkdir::WalkDir;

use crate::{hash, CancelToken, EngineError, EngineResult};

const PARTIAL_HASH_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone)]
pub struct DupOptions {
    /// Ignore files smaller than this (tiny files produce a lot of noise
    /// and are rarely worth deduplicating).
    pub min_size: u64,
}

impl Default for DupOptions {
    fn default() -> Self {
        Self { min_size: 4096 }
    }
}

#[derive(Debug, Clone)]
pub enum DupEvent {
    Scanning { files_found: usize },
    Hashing { done: usize, total: usize },
    GroupFound(DuplicateGroup),
    Finished { groups: usize, wasted_bytes: u64 },
    Cancelled,
}

#[derive(Debug, Clone)]
pub struct DuplicateGroup {
    pub hash: String,
    pub size: u64,
    pub paths: Vec<PathBuf>,
}

pub fn find_duplicates(
    roots: &[PathBuf],
    options: DupOptions,
    cancel: CancelToken,
    tx: Sender<DupEvent>,
) -> EngineResult<Vec<DuplicateGroup>> {
    let mut by_size: HashMap<u64, Vec<PathBuf>> = HashMap::new();
    let mut files_found = 0usize;

    for root in roots {
        for entry in WalkDir::new(root).into_iter().filter_map(|e| e.ok()) {
            if cancel.is_cancelled() {
                let _ = tx.send(DupEvent::Cancelled);
                return Err(EngineError::Cancelled);
            }
            if !entry.file_type().is_file() {
                continue;
            }
            let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
            if size < options.min_size {
                continue;
            }
            by_size.entry(size).or_default().push(entry.path().to_path_buf());
            files_found += 1;
            if files_found.is_multiple_of(500) {
                let _ = tx.send(DupEvent::Scanning { files_found });
            }
        }
    }
    let _ = tx.send(DupEvent::Scanning { files_found });

    let candidates: Vec<(u64, Vec<PathBuf>)> = by_size
        .into_iter()
        .filter(|(_, paths)| paths.len() > 1)
        .collect();

    let total_candidates: usize = candidates.iter().map(|(_, p)| p.len()).sum();
    let hashed = std::sync::atomic::AtomicUsize::new(0);

    let mut groups = Vec::new();
    let mut wasted_bytes: u64 = 0;

    for (size, paths) in candidates {
        if cancel.is_cancelled() {
            let _ = tx.send(DupEvent::Cancelled);
            return Err(EngineError::Cancelled);
        }

        // Stage 2: partial hash pre-filter.
        let partials: Vec<(PathBuf, Option<blake3::Hash>)> = paths
            .par_iter()
            .map(|p| {
                let h = hash::hash_file_partial(p, PARTIAL_HASH_BYTES).ok();
                let n = hashed.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
                if n.is_multiple_of(50) {
                    let _ = tx.send(DupEvent::Hashing {
                        done: n,
                        total: total_candidates,
                    });
                }
                (p.clone(), h)
            })
            .collect();

        let mut by_partial: HashMap<blake3::Hash, Vec<PathBuf>> = HashMap::new();
        for (path, h) in partials {
            if let Some(h) = h {
                by_partial.entry(h).or_default().push(path);
            }
        }

        // Stage 3: full hash confirmation for anything that still collides.
        for (_partial_hash, paths) in by_partial.into_iter().filter(|(_, p)| p.len() > 1) {
            let fulls: Vec<(PathBuf, Option<blake3::Hash>)> = paths
                .par_iter()
                .map(|p| (p.clone(), hash::hash_file_full(p).ok()))
                .collect();

            let mut by_full: HashMap<blake3::Hash, Vec<PathBuf>> = HashMap::new();
            for (path, h) in fulls {
                if let Some(h) = h {
                    by_full.entry(h).or_default().push(path);
                }
            }

            for (full_hash, dup_paths) in by_full.into_iter().filter(|(_, p)| p.len() > 1) {
                wasted_bytes += size * (dup_paths.len() as u64 - 1);
                let group = DuplicateGroup {
                    hash: full_hash.to_hex().to_string(),
                    size,
                    paths: dup_paths,
                };
                let _ = tx.send(DupEvent::GroupFound(group.clone()));
                groups.push(group);
            }
        }
    }

    let _ = tx.send(DupEvent::Finished {
        groups: groups.len(),
        wasted_bytes,
    });

    Ok(groups)
}
