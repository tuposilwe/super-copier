//! Creating bootable USB drives from ISO/IMG files.
//!
//! Two ways of doing it, chosen by what the image is:
//! - **Raw**: write the image byte-for-byte to the device, then read it
//!   back and compare. Works for "hybrid" images (most Linux distros,
//!   rescue disks), which carry their own partition table and boot code.
//! - **Windows files**: partition + format the device as FAT32 and copy the
//!   ISO's contents onto it. Windows install ISOs aren't hybrid, so a raw
//!   write produces a drive that won't boot.
//!
//! Writing to a raw device is destructive and needs administrator rights,
//! so the actual work runs in a separate elevated helper process (see
//! [`helper`]) that re-checks the target is a safe device before touching
//! it — the GUI's choice is never trusted on its own.

pub mod devices;
pub mod helper;
pub mod image;
pub mod winmedia;
pub mod write;

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// A physical drive that could be turned into a bootable stick.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Device {
    /// Short stable identifier shown to (and typed by) the user, e.g.
    /// `disk4`, `sdb`, `Disk 2`.
    pub id: String,
    /// Path opened for raw writes, e.g. `/dev/rdisk4`, `/dev/sdb`,
    /// `\\.\PhysicalDrive2`.
    pub node: String,
    pub name: String,
    pub size: u64,
    pub bus: String,
    /// Mount points of volumes on this device that must be unmounted first.
    pub mounted: Vec<String>,
    /// True for the disk the running system booted from (or that holds
    /// critical mounts). Such devices are never offered as targets.
    pub is_system: bool,
    /// True for a disk image mounted as a virtual disk. Only offered when
    /// `SUPER_COPIER_ALLOW_VIRTUAL_DISKS` is set (used for testing without a
    /// real USB stick).
    pub is_virtual: bool,
}

impl Device {
    /// Whether this may be offered as (and accepted as) a write target.
    pub fn is_safe_target(&self) -> bool {
        if self.is_system || self.size == 0 {
            return false;
        }
        if self.is_virtual && !allow_virtual_disks() {
            return false;
        }
        true
    }
}

pub(crate) fn allow_virtual_disks() -> bool {
    std::env::var_os("SUPER_COPIER_ALLOW_VIRTUAL_DISKS").is_some()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Mode {
    Raw,
    WindowsFiles,
}

/// Everything the elevated helper needs, handed over as a JSON file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FlashJob {
    pub image: PathBuf,
    pub device_id: String,
    pub mode: Mode,
    pub verify: bool,
    /// The helper appends one JSON [`Event`] per line here; the GUI tails it.
    pub progress_file: PathBuf,
    /// The helper stops as soon as this file exists.
    pub cancel_file: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum Event {
    Phase { name: String },
    Progress { done: u64, total: u64 },
    Log { line: String },
    Finished,
    Failed { message: String },
    Cancelled,
}

impl Event {
    pub fn is_terminal(&self) -> bool {
        matches!(self, Event::Finished | Event::Failed { .. } | Event::Cancelled)
    }
}
