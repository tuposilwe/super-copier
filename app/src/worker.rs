//! Spawns engine operations on a background thread and hands back a
//! [`CancelToken`] plus a channel the UI can poll each frame with
//! `try_recv`/`try_iter` (never blocking the UI thread).

use std::path::PathBuf;

use crossbeam_channel::Receiver;
use engine::{copy, duplicates, fsops, large_files, sync, CancelToken};

pub struct Job<E> {
    pub cancel: CancelToken,
    pub rx: Receiver<E>,
}

pub fn spawn_copy(sources: Vec<PathBuf>, dest: PathBuf, options: copy::CopyOptions) -> Job<copy::CopyEvent> {
    let cancel = CancelToken::new();
    let (tx, rx) = crossbeam_channel::unbounded();
    let cancel2 = cancel.clone();
    std::thread::spawn(move || {
        let _ = copy::copy_tree(&sources, &dest, options, cancel2, tx);
    });
    Job { cancel, rx }
}

pub fn spawn_move(sources: Vec<PathBuf>, dest: PathBuf, options: copy::CopyOptions) -> Job<copy::CopyEvent> {
    let cancel = CancelToken::new();
    let (tx, rx) = crossbeam_channel::unbounded();
    let cancel2 = cancel.clone();
    std::thread::spawn(move || {
        let _ = copy::move_tree(&sources, &dest, options, cancel2, tx);
    });
    Job { cancel, rx }
}

pub fn spawn_duplicates(roots: Vec<PathBuf>, options: duplicates::DupOptions) -> Job<duplicates::DupEvent> {
    let cancel = CancelToken::new();
    let (tx, rx) = crossbeam_channel::unbounded();
    let cancel2 = cancel.clone();
    std::thread::spawn(move || {
        let _ = duplicates::find_duplicates(&roots, options, cancel2, tx);
    });
    Job { cancel, rx }
}

pub fn spawn_organize(
    dir: PathBuf,
    strategy: fsops::OrganizeStrategy,
) -> Job<fsops::OrganizeEvent> {
    let cancel = CancelToken::new();
    let (tx, rx) = crossbeam_channel::unbounded();
    let cancel2 = cancel.clone();
    std::thread::spawn(move || {
        let _ = fsops::organize_dir(&dir, strategy, cancel2, tx);
    });
    Job { cancel, rx }
}

pub fn spawn_large_files(
    roots: Vec<PathBuf>,
    options: large_files::LargeFilesOptions,
) -> Job<large_files::LargeFileEvent> {
    let cancel = CancelToken::new();
    let (tx, rx) = crossbeam_channel::unbounded();
    let cancel2 = cancel.clone();
    std::thread::spawn(move || {
        let _ = large_files::find_large_files(&roots, options, cancel2, tx);
    });
    Job { cancel, rx }
}

pub fn spawn_sync(src: PathBuf, dst: PathBuf, options: sync::SyncOptions) -> Job<sync::SyncEvent> {
    let cancel = CancelToken::new();
    let (tx, rx) = crossbeam_channel::unbounded();
    let cancel2 = cancel.clone();
    std::thread::spawn(move || {
        let _ = sync::sync_dirs(&src, &dst, options, cancel2, tx);
    });
    Job { cancel, rx }
}
