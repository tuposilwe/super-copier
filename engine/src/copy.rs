//! Multi-threaded file/tree copy engine with resume, verification and
//! platform-accelerated fast paths (APFS `clonefile` on macOS,
//! `CopyFileExW` on Windows).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Instant;

use crossbeam_channel::Sender;
use filetime::FileTime;
use walkdir::WalkDir;

use crate::platform::generic::chunked_copy;
use crate::{hash, io_err, CancelToken, EngineError, EngineResult};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OverwritePolicy {
    /// Overwrite the destination unconditionally.
    Always,
    /// Leave the destination untouched if it already exists.
    Skip,
    /// Skip if the destination exists with the same size (treated as
    /// "already copied" — the default resume-friendly behaviour).
    SkipIfSameSize,
}

#[derive(Debug, Clone)]
pub struct CopyOptions {
    pub overwrite: OverwritePolicy,
    /// Re-read the destination after copying and compare a BLAKE3 hash
    /// against the source to guarantee byte-for-byte integrity.
    pub verify: bool,
    /// Copy modification time from source to destination.
    pub preserve_times: bool,
    /// Number of files copied concurrently. 0 = pick automatically.
    pub threads: usize,
    /// Allow the macOS `clonefile` / Windows `CopyFileExW` fast paths. When
    /// `verify` is true the engine still uses the fast path but performs a
    /// separate verification pass afterwards.
    pub use_fast_path: bool,
}

impl Default for CopyOptions {
    fn default() -> Self {
        Self {
            overwrite: OverwritePolicy::SkipIfSameSize,
            verify: false,
            preserve_times: true,
            threads: 0,
            use_fast_path: true,
        }
    }
}

#[derive(Debug, Clone)]
pub enum CopyEvent {
    Started { total_files: usize, total_bytes: u64 },
    FileStarted { path: PathBuf, size: u64 },
    FileProgress { path: PathBuf, bytes_done: u64, size: u64 },
    FileVerified { path: PathBuf },
    FileDone { path: PathBuf },
    FileSkipped { path: PathBuf, reason: String },
    FileError { path: PathBuf, message: String },
    Finished(CopySummary),
    Cancelled,
}

#[derive(Debug, Clone, Default)]
pub struct CopySummary {
    pub files_copied: usize,
    pub files_skipped: usize,
    pub files_failed: usize,
    pub bytes_copied: u64,
    pub elapsed_secs: f64,
}

#[derive(Clone)]
struct PlannedFile {
    src: PathBuf,
    dst: PathBuf,
    size: u64,
}

/// Recursively copies `sources` (files or directories) into `dest_dir`.
pub fn copy_tree(
    sources: &[PathBuf],
    dest_dir: &Path,
    options: CopyOptions,
    cancel: CancelToken,
    tx: Sender<CopyEvent>,
) -> EngineResult<CopySummary> {
    let started = Instant::now();
    let plan = plan_copy(sources, dest_dir)?;

    let total_files = plan.len();
    let total_bytes: u64 = plan.iter().map(|f| f.size).sum();
    let _ = tx.send(CopyEvent::Started {
        total_files,
        total_bytes,
    });

    let result = execute_plan(&plan, &options, &cancel, &tx);
    let cancelled = result.cancelled;
    let summary = result.into_summary(started);

    if cancelled {
        let _ = tx.send(CopyEvent::Cancelled);
    } else {
        let _ = tx.send(CopyEvent::Finished(summary.clone()));
    }

    Ok(summary)
}

/// Moves `sources` (files or directories) into `dest_dir`. For each
/// top-level source this first tries a plain rename, which is atomic and
/// effectively instant on the same volume. When that isn't possible
/// (different volume, or a same-named destination already exists) it falls
/// back to a full copy — using the same fast paths, options and progress
/// events as [`copy_tree`] — followed by deleting the original, but only
/// once every file in that fallback copy has succeeded.
pub fn move_tree(
    sources: &[PathBuf],
    dest_dir: &Path,
    options: CopyOptions,
    cancel: CancelToken,
    tx: Sender<CopyEvent>,
) -> EngineResult<CopySummary> {
    let started = Instant::now();
    std::fs::create_dir_all(dest_dir).map_err(|e| io_err(dest_dir, e))?;

    let full_plan = plan_copy(sources, dest_dir)?;
    let total_files = full_plan.len();
    let total_bytes: u64 = full_plan.iter().map(|f| f.size).sum();
    let _ = tx.send(CopyEvent::Started {
        total_files,
        total_bytes,
    });

    let mut instant_files = 0usize;
    let mut instant_bytes = 0u64;
    let mut fallback_plan: Vec<PlannedFile> = Vec::new();
    let mut fallback_sources: Vec<PathBuf> = Vec::new();

    for src in sources {
        if cancel.is_cancelled() {
            let _ = tx.send(CopyEvent::Cancelled);
            return Err(EngineError::Cancelled);
        }
        let Some(name) = src.file_name() else { continue };
        let dst = dest_dir.join(name);

        let renamed = !dst.exists() && std::fs::rename(src, &dst).is_ok();
        if renamed {
            for f in full_plan.iter().filter(|f| f.src.starts_with(src)) {
                instant_files += 1;
                instant_bytes += f.size;
                let _ = tx.send(CopyEvent::FileDone { path: f.dst.clone() });
            }
        } else {
            fallback_sources.push(src.clone());
            fallback_plan.extend(full_plan.iter().filter(|f| f.src.starts_with(src)).cloned());
        }
    }

    let result = execute_plan(&fallback_plan, &options, &cancel, &tx);

    if result.files_failed == 0 && !result.cancelled {
        for src in &fallback_sources {
            let is_dir = std::fs::metadata(src).map(|m| m.is_dir()).unwrap_or(false);
            let removed = if is_dir {
                std::fs::remove_dir_all(src)
            } else {
                std::fs::remove_file(src)
            };
            if let Err(e) = removed {
                let _ = tx.send(CopyEvent::FileError {
                    path: src.clone(),
                    message: format!("copied but could not remove original: {e}"),
                });
            }
        }
    }

    let summary = CopySummary {
        files_copied: instant_files + result.files_copied,
        files_skipped: result.files_skipped,
        files_failed: result.files_failed,
        bytes_copied: instant_bytes + result.bytes_copied,
        elapsed_secs: started.elapsed().as_secs_f64(),
    };

    if result.cancelled {
        let _ = tx.send(CopyEvent::Cancelled);
    } else {
        let _ = tx.send(CopyEvent::Finished(summary.clone()));
    }

    Ok(summary)
}

struct PlanResult {
    files_copied: usize,
    files_skipped: usize,
    files_failed: usize,
    bytes_copied: u64,
    cancelled: bool,
}

impl PlanResult {
    fn into_summary(self, started: Instant) -> CopySummary {
        CopySummary {
            files_copied: self.files_copied,
            files_skipped: self.files_skipped,
            files_failed: self.files_failed,
            bytes_copied: self.bytes_copied,
            elapsed_secs: started.elapsed().as_secs_f64(),
        }
    }
}

/// Runs `plan` across a thread pool, sending per-file [`CopyEvent`]s as it
/// goes. Does not send `Started`/`Finished`/`Cancelled` — callers own that so
/// they can run this over a subset of a larger, already-announced job (as
/// [`move_tree`] does for its copy-then-delete fallback).
fn execute_plan(
    plan: &[PlannedFile],
    options: &CopyOptions,
    cancel: &CancelToken,
    tx: &Sender<CopyEvent>,
) -> PlanResult {
    let threads = if options.threads == 0 {
        std::thread::available_parallelism()
            .map(|n| n.get().clamp(2, 8))
            .unwrap_or(4)
    } else {
        options.threads
    };

    let files_copied = Arc::new(AtomicUsize::new(0));
    let files_skipped = Arc::new(AtomicUsize::new(0));
    let files_failed = Arc::new(AtomicUsize::new(0));
    let bytes_copied = Arc::new(AtomicU64::new(0));
    let cancelled = Arc::new(std::sync::atomic::AtomicBool::new(false));

    let pool = match rayon::ThreadPoolBuilder::new().num_threads(threads).build() {
        Ok(pool) => pool,
        Err(e) => {
            let _ = tx.send(CopyEvent::FileError {
                path: PathBuf::new(),
                message: e.to_string(),
            });
            return PlanResult {
                files_copied: 0,
                files_skipped: 0,
                files_failed: plan.len(),
                bytes_copied: 0,
                cancelled: false,
            };
        }
    };

    pool.install(|| {
        use rayon::prelude::*;
        plan.par_iter().for_each(|file| {
            if cancel.is_cancelled() {
                cancelled.store(true, Ordering::SeqCst);
                return;
            }
            match copy_one_file(file, options, cancel, tx) {
                Ok(CopyOutcome::Copied(bytes)) => {
                    files_copied.fetch_add(1, Ordering::Relaxed);
                    bytes_copied.fetch_add(bytes, Ordering::Relaxed);
                }
                Ok(CopyOutcome::Skipped) => {
                    files_skipped.fetch_add(1, Ordering::Relaxed);
                }
                Err(EngineError::Cancelled) => {
                    cancelled.store(true, Ordering::SeqCst);
                }
                Err(e) => {
                    files_failed.fetch_add(1, Ordering::Relaxed);
                    let _ = tx.send(CopyEvent::FileError {
                        path: file.src.clone(),
                        message: e.to_string(),
                    });
                }
            }
        });
    });

    PlanResult {
        files_copied: files_copied.load(Ordering::Relaxed),
        files_skipped: files_skipped.load(Ordering::Relaxed),
        files_failed: files_failed.load(Ordering::Relaxed),
        bytes_copied: bytes_copied.load(Ordering::Relaxed),
        cancelled: cancelled.load(Ordering::Relaxed),
    }
}

enum CopyOutcome {
    Copied(u64),
    Skipped,
}

fn copy_one_file(
    file: &PlannedFile,
    options: &CopyOptions,
    cancel: &CancelToken,
    tx: &Sender<CopyEvent>,
) -> EngineResult<CopyOutcome> {
    if let Some(parent) = file.dst.parent() {
        std::fs::create_dir_all(parent).map_err(|e| io_err(parent, e))?;
    }

    if file.dst.exists() {
        match options.overwrite {
            OverwritePolicy::Skip => {
                let _ = tx.send(CopyEvent::FileSkipped {
                    path: file.src.clone(),
                    reason: "destination exists".into(),
                });
                return Ok(CopyOutcome::Skipped);
            }
            OverwritePolicy::SkipIfSameSize => {
                if let Ok(meta) = std::fs::metadata(&file.dst) {
                    if meta.len() == file.size {
                        let _ = tx.send(CopyEvent::FileSkipped {
                            path: file.src.clone(),
                            reason: "already copied (same size)".into(),
                        });
                        return Ok(CopyOutcome::Skipped);
                    }
                }
            }
            OverwritePolicy::Always => {}
        }
        // Fast-path clone/CopyFileEx APIs require the destination to not
        // already exist, so clear the way for an overwrite.
        let _ = std::fs::remove_file(&file.dst);
    }

    let _ = tx.send(CopyEvent::FileStarted {
        path: file.src.clone(),
        size: file.size,
    });

    let bytes = copy_file(file, options, cancel, tx)?;

    if options.preserve_times {
        if let Ok(meta) = std::fs::metadata(&file.src) {
            let mtime = FileTime::from_last_modification_time(&meta);
            let _ = filetime::set_file_mtime(&file.dst, mtime);
        }
    }

    if options.verify {
        let src_hash = hash::hash_file_full(&file.src)?;
        let dst_hash = hash::hash_file_full(&file.dst)?;
        if src_hash != dst_hash {
            return Err(EngineError::Other(format!(
                "verification failed for {}",
                file.src.display()
            )));
        }
        let _ = tx.send(CopyEvent::FileVerified {
            path: file.src.clone(),
        });
    }

    let _ = tx.send(CopyEvent::FileDone {
        path: file.src.clone(),
    });

    Ok(CopyOutcome::Copied(bytes))
}

#[cfg(target_os = "macos")]
fn copy_file(
    file: &PlannedFile,
    options: &CopyOptions,
    cancel: &CancelToken,
    tx: &Sender<CopyEvent>,
) -> EngineResult<u64> {
    if options.use_fast_path {
        match crate::platform::try_clone_file(&file.src, &file.dst) {
            Ok(true) => {
                let _ = tx.send(CopyEvent::FileProgress {
                    path: file.src.clone(),
                    bytes_done: file.size,
                    size: file.size,
                });
                return Ok(file.size);
            }
            Ok(false) => { /* fall through to chunked copy */ }
            Err(e) => return Err(io_err(&file.src, e)),
        }
    }
    chunked_copy_reporting(file, cancel, tx)
}

#[cfg(windows)]
fn copy_file(
    file: &PlannedFile,
    options: &CopyOptions,
    cancel: &CancelToken,
    tx: &Sender<CopyEvent>,
) -> EngineResult<u64> {
    if options.use_fast_path && !options.verify {
        let src = file.src.clone();
        let size = file.size;
        let tx2 = tx.clone();
        let result = crate::platform::win_copy_file_with_progress(
            &file.src,
            &file.dst,
            move |done| {
                let _ = tx2.send(CopyEvent::FileProgress {
                    path: src.clone(),
                    bytes_done: done,
                    size,
                });
            },
            cancel,
        );
        return match result {
            Ok(bytes) => Ok(bytes),
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => Err(EngineError::Cancelled),
            Err(_) => chunked_copy_reporting(file, cancel, tx),
        };
    }
    chunked_copy_reporting(file, cancel, tx)
}

#[cfg(not(any(target_os = "macos", windows)))]
fn copy_file(
    file: &PlannedFile,
    _options: &CopyOptions,
    cancel: &CancelToken,
    tx: &Sender<CopyEvent>,
) -> EngineResult<u64> {
    chunked_copy_reporting(file, cancel, tx)
}

fn chunked_copy_reporting(
    file: &PlannedFile,
    cancel: &CancelToken,
    tx: &Sender<CopyEvent>,
) -> EngineResult<u64> {
    let src = file.src.clone();
    let size = file.size;
    let tx2 = tx.clone();
    chunked_copy(&file.src, &file.dst, file.size, None, cancel, move |done| {
        let _ = tx2.send(CopyEvent::FileProgress {
            path: src.clone(),
            bytes_done: done,
            size,
        });
    })
}

fn plan_copy(sources: &[PathBuf], dest_dir: &Path) -> EngineResult<Vec<PlannedFile>> {
    let mut plan = Vec::new();
    for src in sources {
        let meta = std::fs::metadata(src).map_err(|e| io_err(src, e))?;
        if meta.is_file() {
            let name = src
                .file_name()
                .ok_or_else(|| EngineError::Other(format!("bad path {}", src.display())))?;
            plan.push(PlannedFile {
                src: src.clone(),
                dst: dest_dir.join(name),
                size: meta.len(),
            });
        } else if meta.is_dir() {
            let base_name = src
                .file_name()
                .ok_or_else(|| EngineError::Other(format!("bad path {}", src.display())))?;
            let dst_root = dest_dir.join(base_name);
            for entry in WalkDir::new(src).into_iter().filter_map(|e| e.ok()) {
                if !entry.file_type().is_file() {
                    continue;
                }
                let rel = entry.path().strip_prefix(src).unwrap_or(entry.path());
                let dst = dst_root.join(rel);
                let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
                plan.push(PlannedFile {
                    src: entry.path().to_path_buf(),
                    dst,
                    size,
                });
            }
        }
    }
    // Copy the largest files first so multiple threads stay saturated for
    // longer instead of racing through many small files up front and then
    // serializing on one huge one at the end.
    plan.sort_by_key(|f| std::cmp::Reverse(f.size));
    Ok(plan)
}
