//! Looks at an ISO/IMG to decide how it can be turned into a bootable
//! drive, without reading more than its first few dozen kilobytes.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::{io_err, EngineResult};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ImageKind {
    /// Carries its own MBR (boot signature at bytes 510-511), like every
    /// "isohybrid" Linux ISO and raw disk image. A byte-for-byte write
    /// produces a bootable drive.
    Hybrid,
    /// A plain ISO 9660 image with no MBR — a Windows install ISO, or a
    /// CD-only Linux ISO. Writing it raw gives an unbootable drive; its
    /// files have to be copied onto a formatted drive instead.
    OpticalOnly,
    /// Neither an ISO nor anything with a boot signature.
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageInfo {
    pub size: u64,
    pub kind: ImageKind,
}

/// ISO 9660's primary volume descriptor lives at sector 16 and starts with
/// type 1 followed by "CD001".
const ISO_MAGIC_OFFSET: u64 = 16 * 2048;

pub fn analyze_image(path: &Path) -> EngineResult<ImageInfo> {
    let mut f = File::open(path).map_err(|e| io_err(path, e))?;
    let size = f.metadata().map_err(|e| io_err(path, e))?.len();

    let mut head = [0u8; 512];
    let has_head = f.read_exact(&mut head).is_ok();
    let boot_signature = has_head && head[510] == 0x55 && head[511] == 0xAA;

    let mut iso = [0u8; 6];
    let is_iso = f.seek(SeekFrom::Start(ISO_MAGIC_OFFSET)).is_ok()
        && f.read_exact(&mut iso).is_ok()
        && iso[0] == 1
        && &iso[1..6] == b"CD001";

    let kind = if boot_signature {
        ImageKind::Hybrid
    } else if is_iso {
        ImageKind::OpticalOnly
    } else {
        ImageKind::Unknown
    };
    Ok(ImageInfo { size, kind })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn image(boot_sig: bool, iso: bool, len: usize) -> tempfile::NamedTempFile {
        let mut data = vec![0u8; len];
        if boot_sig {
            data[510] = 0x55;
            data[511] = 0xAA;
        }
        if iso {
            let o = ISO_MAGIC_OFFSET as usize;
            data[o] = 1;
            data[o + 1..o + 6].copy_from_slice(b"CD001");
        }
        let mut f = tempfile::NamedTempFile::new().unwrap();
        f.write_all(&data).unwrap();
        f
    }

    #[test]
    fn classifies_hybrid_optical_and_unknown_images() {
        let hybrid = analyze_image(image(true, true, 40_000).path()).unwrap();
        assert_eq!(hybrid.kind, ImageKind::Hybrid, "an ISO with an MBR is raw-writable");
        assert_eq!(hybrid.size, 40_000);

        let windows_like = analyze_image(image(false, true, 40_000).path()).unwrap();
        assert_eq!(windows_like.kind, ImageKind::OpticalOnly);

        let junk = analyze_image(image(false, false, 40_000).path()).unwrap();
        assert_eq!(junk.kind, ImageKind::Unknown);

        let tiny = analyze_image(image(false, false, 100).path()).unwrap();
        assert_eq!(tiny.kind, ImageKind::Unknown, "files shorter than a sector must not panic");
    }
}
