//! Spawns engine operations on a background thread and hands back a
//! [`CancelToken`] plus a channel the UI can poll each frame with
//! `try_recv`/`try_iter` (never blocking the UI thread).

use std::path::PathBuf;

use crossbeam_channel::Receiver;
use engine::{copy, duplicates, fsops, large_files, search, sync, CancelToken};

pub struct Job<E> {
    pub cancel: CancelToken,
    pub rx: Receiver<E>,
}

/// A trash-delete in flight: the paths it was asked to delete (so the
/// caller knows what to remove from its own state once it hears back) and
/// the channel reporting whether it worked.
pub type DeleteJob = (Vec<PathBuf>, Receiver<Result<usize, String>>);

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

pub fn spawn_search(roots: Vec<PathBuf>, options: search::SearchOptions) -> Job<search::SearchEvent> {
    let cancel = CancelToken::new();
    let (tx, rx) = crossbeam_channel::unbounded();
    let cancel2 = cancel.clone();
    std::thread::spawn(move || {
        let _ = search::search_files(&roots, options, cancel2, tx);
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

/// Moves `paths` to the OS trash/Recycle Bin on a fresh background thread.
///
/// This *must not* run on the UI thread on Windows: the `trash` crate
/// initializes COM (`CoInitializeEx`) the first time it's used on a given
/// thread, and **panics** if that thread already has COM initialized in a
/// different apartment-threading mode — which the UI thread often does by
/// the time the user clicks delete (native file dialogs and other Windows
/// GUI machinery can initialize COM on it first). A brand new thread has no
/// prior COM state, so this sidesteps the conflict entirely rather than
/// needing to reason about what else touched COM first.
pub fn spawn_delete_to_trash(paths: Vec<PathBuf>) -> Receiver<Result<usize, String>> {
    let (tx, rx) = crossbeam_channel::bounded(1);
    let count = paths.len();
    std::thread::spawn(move || {
        let result = fsops::delete_to_trash(&paths).map(|()| count).map_err(|e| e.to_string());
        let _ = tx.send(result);
    });
    rx
}
