pub mod big_folders;
pub mod copy;
pub mod diskusage;
pub mod drives;
pub mod duplicates;
pub mod fsops;
pub mod hash;
pub mod large_files;
pub mod platform;
pub mod search;
pub mod share;
pub mod sync;

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    #[error("io error at {path:?}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("operation cancelled")]
    Cancelled,
    #[error("{0}")]
    Other(String),
}

pub type EngineResult<T> = Result<T, EngineError>;

pub(crate) fn io_err(path: &std::path::Path, source: std::io::Error) -> EngineError {
    EngineError::Io {
        path: path.to_path_buf(),
        source,
    }
}

/// Shared cancellation flag handed to background operations. The UI flips it
/// to request an early stop; operations check it between files and between
/// buffer chunks so cancellation lands within a fraction of a second.
#[derive(Clone, Default)]
pub struct CancelToken(Arc<AtomicBool>);

impl CancelToken {
    pub fn new() -> Self {
        Self(Arc::new(AtomicBool::new(false)))
    }

    pub fn cancel(&self) {
        self.0.store(true, Ordering::SeqCst);
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}
