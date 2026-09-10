//! Platform-specific fast paths for whole-file copies. Every platform also
//! has the portable [`generic::chunked_copy`] available, which is used
//! whenever resume or streaming verification is needed, or as a fallback
//! when the fast path can't be used (e.g. copying across volumes).

pub mod generic;

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
pub use macos::try_clone_file;

#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use windows::copy_file_with_progress as win_copy_file_with_progress;
