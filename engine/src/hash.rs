//! Streaming BLAKE3 hashing used for duplicate detection and copy verification.

use std::fs::File;
use std::io::Read;
use std::path::Path;

use crate::{io_err, EngineResult};

const BUF_SIZE: usize = 1024 * 1024; // 1 MiB read buffer

/// Hashes the full contents of a file.
pub fn hash_file_full(path: &Path) -> EngineResult<blake3::Hash> {
    let mut file = File::open(path).map_err(|e| io_err(path, e))?;
    let mut hasher = blake3::Hasher::new();
    let mut buf = vec![0u8; BUF_SIZE];
    loop {
        let n = file.read(&mut buf).map_err(|e| io_err(path, e))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hasher.finalize())
}

/// Hashes only the first `bytes` of a file. Used as a cheap pre-filter before
/// committing to a full-file hash on large duplicate candidates.
pub fn hash_file_partial(path: &Path, bytes: usize) -> EngineResult<blake3::Hash> {
    let mut file = File::open(path).map_err(|e| io_err(path, e))?;
    let mut hasher = blake3::Hasher::new();
    let mut remaining = bytes;
    let mut buf = vec![0u8; BUF_SIZE.min(bytes.max(1))];
    while remaining > 0 {
        let want = remaining.min(buf.len());
        let n = file.read(&mut buf[..want]).map_err(|e| io_err(path, e))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        remaining -= n;
    }
    Ok(hasher.finalize())
}
