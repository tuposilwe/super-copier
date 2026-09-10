//! One-way folder sync / mirror, similar in spirit to `rsync`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crossbeam_channel::Sender;
use filetime::FileTime;
use walkdir::WalkDir;

use crate::platform::generic::chunked_copy;
use crate::{fsops, io_err, CancelToken, EngineError, EngineResult};

#[derive(Debug, Clone, Default)]
pub struct SyncOptions {
    /// If true, files present in the destination but not the source are
    /// deleted, making the destination an exact mirror. If false, the
    /// destination is only ever added to / updated.
    pub mirror: bool,
    /// Verify copied/updated files with a full hash comparison afterwards.
    pub verify: bool,
}


#[derive(Debug, Clone)]
pub enum SyncEvent {
    Planned {
        to_copy: usize,
        to_update: usize,
        to_delete: usize,
    },
    FileCopied { path: PathBuf },
    FileUpdated { path: PathBuf },
    FileDeleted { path: PathBuf },
    FileError { path: PathBuf, message: String },
    Finished(SyncSummary),
    Cancelled,
}

#[derive(Debug, Clone, Default)]
pub struct SyncSummary {
    pub copied: usize,
    pub updated: usize,
    pub deleted: usize,
    pub failed: usize,
    pub bytes_transferred: u64,
}

#[derive(Clone, Copy)]
struct Entry {
    size: u64,
    mtime: i64,
}

fn snapshot(root: &Path) -> HashMap<PathBuf, Entry> {
    let mut map = HashMap::new();
    for e in WalkDir::new(root).into_iter().filter_map(|e| e.ok()) {
        if !e.file_type().is_file() {
            continue;
        }
        let rel = match e.path().strip_prefix(root) {
            Ok(r) => r.to_path_buf(),
            Err(_) => continue,
        };
        if let Ok(meta) = e.metadata() {
            map.insert(
                rel,
                Entry {
                    size: meta.len(),
                    mtime: FileTime::from_last_modification_time(&meta).unix_seconds(),
                },
            );
        }
    }
    map
}

pub fn sync_dirs(
    src: &Path,
    dst: &Path,
    options: SyncOptions,
    cancel: CancelToken,
    tx: Sender<SyncEvent>,
) -> EngineResult<SyncSummary> {
    std::fs::create_dir_all(dst).map_err(|e| io_err(dst, e))?;

    let src_snap = snapshot(src);
    let dst_snap = snapshot(dst);

    let mut to_copy = Vec::new();
    let mut to_update = Vec::new();
    for (rel, src_entry) in &src_snap {
        match dst_snap.get(rel) {
            None => to_copy.push(rel.clone()),
            Some(dst_entry) => {
                if dst_entry.size != src_entry.size || dst_entry.mtime < src_entry.mtime {
                    to_update.push(rel.clone());
                }
            }
        }
    }
    let to_delete: Vec<PathBuf> = if options.mirror {
        dst_snap
            .keys()
            .filter(|rel| !src_snap.contains_key(*rel))
            .cloned()
            .collect()
    } else {
        Vec::new()
    };

    let _ = tx.send(SyncEvent::Planned {
        to_copy: to_copy.len(),
        to_update: to_update.len(),
        to_delete: to_delete.len(),
    });

    let mut summary = SyncSummary::default();

    for rel in to_copy.into_iter().chain(to_update) {
        if cancel.is_cancelled() {
            let _ = tx.send(SyncEvent::Cancelled);
            return Err(EngineError::Cancelled);
        }
        let from = src.join(&rel);
        let to = dst.join(&rel);
        if let Some(parent) = to.parent() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                let _ = tx.send(SyncEvent::FileError {
                    path: from.clone(),
                    message: e.to_string(),
                });
                summary.failed += 1;
                continue;
            }
        }
        let size = std::fs::metadata(&from).map(|m| m.len()).unwrap_or(0);
        let is_update = dst_snap.contains_key(&rel);
        match chunked_copy(&from, &to, size, None, &cancel, |_| {}) {
            Ok(bytes) => {
                summary.bytes_transferred += bytes;
                if options.verify {
                    let sh = crate::hash::hash_file_full(&from);
                    let dh = crate::hash::hash_file_full(&to);
                    if sh.ok() != dh.ok() {
                        let _ = tx.send(SyncEvent::FileError {
                            path: from.clone(),
                            message: "verification mismatch".into(),
                        });
                        summary.failed += 1;
                        continue;
                    }
                }
                if let Ok(meta) = std::fs::metadata(&from) {
                    let mtime = FileTime::from_last_modification_time(&meta);
                    let _ = filetime::set_file_mtime(&to, mtime);
                }
                if is_update {
                    summary.updated += 1;
                    let _ = tx.send(SyncEvent::FileUpdated { path: to });
                } else {
                    summary.copied += 1;
                    let _ = tx.send(SyncEvent::FileCopied { path: to });
                }
            }
            Err(e) => {
                summary.failed += 1;
                let _ = tx.send(SyncEvent::FileError {
                    path: from,
                    message: e.to_string(),
                });
            }
        }
    }

    if options.mirror {
        for rel in to_delete {
            if cancel.is_cancelled() {
                let _ = tx.send(SyncEvent::Cancelled);
                return Err(EngineError::Cancelled);
            }
            let path = dst.join(&rel);
            match fsops::delete_permanently(std::slice::from_ref(&path)) {
                Ok(()) => {
                    summary.deleted += 1;
                    let _ = tx.send(SyncEvent::FileDeleted { path });
                }
                Err(e) => {
                    summary.failed += 1;
                    let _ = tx.send(SyncEvent::FileError {
                        path,
                        message: e.to_string(),
                    });
                }
            }
        }
    }

    let _ = tx.send(SyncEvent::Finished(summary.clone()));
    Ok(summary)
}
