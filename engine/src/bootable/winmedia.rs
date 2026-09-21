//! "Windows files" mode: Windows install ISOs aren't hybrid images, so
//! instead of a raw write the drive is partitioned and formatted as FAT32
//! (which both UEFI and legacy BIOS can boot from) and the ISO's contents
//! are copied onto it.
//!
//! The one snag is FAT32's 4 GiB file limit: newer `install.wim` files are
//! bigger, so they're split into `.swm` pieces, which Windows Setup reads
//! natively. Splitting needs `dism` on Windows and `wimlib-imagex` elsewhere.

use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

use walkdir::WalkDir;

use super::write::{finish_device, Throttle};
use super::{Device, Event};
use crate::{io_err, CancelToken, EngineError, EngineResult};

/// Largest file FAT32 can hold: 4 GiB minus one byte.
pub const FAT32_MAX_FILE: u64 = 4 * 1024 * 1024 * 1024 - 1;
/// Piece size handed to the splitter, in MiB — comfortably under the limit.
const SPLIT_MB: &str = "3800";
const VOLUME_LABEL: &str = "WINSETUP";

/// What to do with each file found in the ISO.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CopyAction {
    Copy,
    /// A `.wim` too large for FAT32 — split instead of copied.
    SplitWim,
}

/// Decides how each file is handled, or errors on a file that can't fit on
/// FAT32 and isn't a WIM we know how to split.
pub fn plan_file(rel: &Path, size: u64) -> EngineResult<CopyAction> {
    if size <= FAT32_MAX_FILE {
        return Ok(CopyAction::Copy);
    }
    let is_wim = rel.extension().is_some_and(|e| e.eq_ignore_ascii_case("wim"));
    if is_wim {
        Ok(CopyAction::SplitWim)
    } else {
        Err(EngineError::Other(format!(
            "{} is {} bytes, over FAT32's 4 GiB file limit, and can't be split automatically",
            rel.display(),
            size
        )))
    }
}

fn run(cmd: &mut Command, what: &str) -> EngineResult<String> {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    let out = cmd.output().map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            EngineError::Other(format!("{what}: required tool {:?} isn't installed", cmd.get_program()))
        } else {
            EngineError::Other(format!("{what}: {e}"))
        }
    })?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        Err(EngineError::Other(format!(
            "{what} failed: {} {}",
            String::from_utf8_lossy(&out.stdout).trim(),
            String::from_utf8_lossy(&out.stderr).trim()
        )))
    }
}

// ------------------------------------------------------------ host ops

/// A freshly formatted, mounted FAT32 volume on the target drive.
struct Target {
    root: PathBuf,
    #[cfg(windows)]
    letter: char,
}

/// A mounted (read-only) view of the ISO's files. Detaches on drop.
struct MountedIso {
    root: PathBuf,
    detach: Box<dyn Fn()>,
}

impl Drop for MountedIso {
    fn drop(&mut self) {
        (self.detach)();
    }
}

#[cfg(target_os = "macos")]
fn format_target(device: &Device, emit: &mut dyn FnMut(Event)) -> EngineResult<Target> {
    use super::devices::macos::diskutil_json;
    let _ = emit;
    run(Command::new("diskutil").args(["unmountDisk", "force", &device.id]), "unmounting the drive")?;
    run(
        Command::new("diskutil").args(["eraseDisk", "MS-DOS", VOLUME_LABEL, "MBR", &device.id]),
        "formatting the drive",
    )?;
    let info = diskutil_json(&["info", "-plist", &format!("{}s1", device.id)])?;
    let mount = info["MountPoint"]
        .as_str()
        .ok_or_else(|| EngineError::Other("the new volume didn't mount".into()))?;
    Ok(Target { root: PathBuf::from(mount) })
}

#[cfg(target_os = "macos")]
fn mount_iso(iso: &Path) -> EngineResult<MountedIso> {
    use super::devices::macos::plist_to_json;
    let text = run(
        Command::new("hdiutil").args(["attach", "-readonly", "-nobrowse", "-plist"]).arg(iso),
        "opening the ISO",
    )?;
    let json = plist_to_json(&text)?;
    let entities = json["system-entities"].as_array().cloned().unwrap_or_default();
    let mount = entities
        .iter()
        .find_map(|e| e["mount-point"].as_str())
        .ok_or_else(|| EngineError::Other("the ISO didn't mount a volume".into()))?
        .to_owned();
    let dev = entities
        .iter()
        .find_map(|e| e["dev-entry"].as_str())
        .ok_or_else(|| EngineError::Other("the ISO reported no device".into()))?
        .to_owned();
    Ok(MountedIso {
        root: PathBuf::from(mount),
        detach: Box::new(move || {
            let _ = Command::new("hdiutil").args(["detach", "-force", &dev]).output();
        }),
    })
}

#[cfg(target_os = "macos")]
fn finish_target(_target: &Target, device: &Device, emit: &mut dyn FnMut(Event)) {
    let _ = Command::new("sync").output();
    finish_device(device, emit);
}

#[cfg(target_os = "linux")]
fn scratch_dir(tag: &str) -> EngineResult<PathBuf> {
    let p = std::env::temp_dir().join(format!("supercopier-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&p).map_err(|e| io_err(&p, e))?;
    Ok(p)
}

#[cfg(target_os = "linux")]
fn partition_node(node: &str) -> String {
    if node.chars().last().is_some_and(|c| c.is_ascii_digit()) {
        format!("{node}p1")
    } else {
        format!("{node}1")
    }
}

#[cfg(target_os = "linux")]
fn format_target(device: &Device, emit: &mut dyn FnMut(Event)) -> EngineResult<Target> {
    let _ = emit;
    for m in &device.mounted {
        run(Command::new("umount").arg(m), &format!("unmounting {m}"))?;
    }
    run(Command::new("parted").args(["-s", &device.node, "mklabel", "msdos"]), "creating the partition table")?;
    run(
        Command::new("parted").args(["-s", &device.node, "mkpart", "primary", "fat32", "1MiB", "100%"]),
        "creating the partition",
    )?;
    run(Command::new("parted").args(["-s", &device.node, "set", "1", "boot", "on"]), "marking the partition bootable")?;
    let _ = Command::new("udevadm").arg("settle").output();
    let part = partition_node(&device.node);
    run(Command::new("mkfs.vfat").args(["-F", "32", "-n", VOLUME_LABEL, &part]), "formatting the drive")?;
    let root = scratch_dir("usb")?;
    run(Command::new("mount").arg(&part).arg(&root), "mounting the new volume")?;
    Ok(Target { root })
}

#[cfg(target_os = "linux")]
fn mount_iso(iso: &Path) -> EngineResult<MountedIso> {
    let root = scratch_dir("iso")?;
    run(Command::new("mount").args(["-o", "loop,ro"]).arg(iso).arg(&root), "opening the ISO")?;
    let r2 = root.clone();
    Ok(MountedIso {
        root,
        detach: Box::new(move || {
            let _ = Command::new("umount").arg(&r2).output();
            let _ = std::fs::remove_dir(&r2);
        }),
    })
}

#[cfg(target_os = "linux")]
fn finish_target(target: &Target, device: &Device, emit: &mut dyn FnMut(Event)) {
    let _ = Command::new("sync").output();
    let _ = Command::new("umount").arg(&target.root).output();
    let _ = std::fs::remove_dir(&target.root);
    finish_device(device, emit);
}

#[cfg(windows)]
fn free_drive_letter() -> EngineResult<char> {
    ('D'..='Z')
        .rev()
        .find(|c| !Path::new(&format!("{c}:\\")).exists())
        .ok_or_else(|| EngineError::Other("no free drive letter available".into()))
}

#[cfg(windows)]
fn format_target(device: &Device, emit: &mut dyn FnMut(Event)) -> EngineResult<Target> {
    let _ = emit;
    let letter = free_drive_letter()?;
    let n = super::write::windows_disk_number(device)?;
    super::write::diskpart(&format!(
        "select disk {n}\nclean\ncreate partition primary\nselect partition 1\nactive\nformat fs=fat32 quick label={VOLUME_LABEL}\nassign letter={letter}\n"
    ))?;
    let root = PathBuf::from(format!("{letter}:\\"));
    for _ in 0..20 {
        if root.exists() {
            return Ok(Target { root, letter });
        }
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
    Err(EngineError::Other("the new volume didn't appear".into()))
}

#[cfg(windows)]
fn mount_iso(iso: &Path) -> EngineResult<MountedIso> {
    let quoted = iso.display().to_string().replace('\'', "''");
    let out = run(
        Command::new("powershell").args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            &format!("(Mount-DiskImage -ImagePath '{quoted}' -PassThru | Get-Volume).DriveLetter"),
        ]),
        "opening the ISO",
    )?;
    let letter = out.trim().chars().next().ok_or_else(|| EngineError::Other("the ISO didn't get a drive letter".into()))?;
    Ok(MountedIso {
        root: PathBuf::from(format!("{letter}:\\")),
        detach: Box::new(move || {
            let mut c = Command::new("powershell");
            c.args(["-NoProfile", "-NonInteractive", "-Command", &format!("Dismount-DiskImage -ImagePath '{quoted}'")]);
            {
                use std::os::windows::process::CommandExt;
                c.creation_flags(0x0800_0000);
            }
            let _ = c.output();
        }),
    })
}

#[cfg(windows)]
fn finish_target(_target: &Target, device: &Device, emit: &mut dyn FnMut(Event)) {
    finish_device(device, emit);
}

#[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
fn format_target(_: &Device, _: &mut dyn FnMut(Event)) -> EngineResult<Target> {
    Err(EngineError::Other("unsupported platform".into()))
}
#[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
fn mount_iso(_: &Path) -> EngineResult<MountedIso> {
    Err(EngineError::Other("unsupported platform".into()))
}
#[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
fn finish_target(_: &Target, _: &Device, _: &mut dyn FnMut(Event)) {}

fn split_wim(wim: &Path, dest_dir: &Path) -> EngineResult<()> {
    std::fs::create_dir_all(dest_dir).map_err(|e| io_err(dest_dir, e))?;
    let swm = dest_dir.join("install.swm");
    #[cfg(windows)]
    {
        run(
            Command::new("dism")
                .arg("/Split-Image")
                .arg(format!("/ImageFile:{}", wim.display()))
                .arg(format!("/SWMFile:{}", swm.display()))
                .arg(format!("/FileSize:{SPLIT_MB}")),
            "splitting install.wim",
        )?;
    }
    #[cfg(not(windows))]
    {
        run(
            Command::new("wimlib-imagex").arg("split").arg(wim).arg(&swm).arg(SPLIT_MB),
            "splitting install.wim (install wimlib: `brew install wimlib` or your package manager's wimtools)",
        )?;
    }
    Ok(())
}

// ---------------------------------------------------------------- main

/// Copies `src_root` onto `dst_root`, reporting byte progress and splitting
/// any FAT32-oversized WIM. Split out from the host plumbing so it can be
/// tested against plain directories.
pub fn copy_iso_contents(
    src_root: &Path,
    dst_root: &Path,
    cancel: &CancelToken,
    emit: &mut dyn FnMut(Event),
) -> EngineResult<()> {
    let mut plan: Vec<(PathBuf, u64, CopyAction)> = Vec::new();
    let mut total = 0u64;
    for entry in WalkDir::new(src_root).into_iter().filter_map(|e| e.ok()) {
        if !entry.file_type().is_file() {
            continue;
        }
        let rel = entry.path().strip_prefix(src_root).unwrap_or(entry.path()).to_path_buf();
        let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
        let action = plan_file(&rel, size)?;
        if action == CopyAction::Copy {
            total += size;
        }
        plan.push((rel, size, action));
    }

    emit(Event::Phase { name: "Copying files".into() });
    let mut done = 0u64;
    let mut throttle = Throttle::new();
    let mut buf = vec![0u8; 1024 * 1024];
    let mut to_split = Vec::new();
    for (rel, _size, action) in &plan {
        if cancel.is_cancelled() {
            return Err(EngineError::Cancelled);
        }
        let src = src_root.join(rel);
        if *action == CopyAction::SplitWim {
            to_split.push(rel.clone());
            continue;
        }
        let dst = dst_root.join(rel);
        if let Some(parent) = dst.parent() {
            std::fs::create_dir_all(parent).map_err(|e| io_err(parent, e))?;
        }
        let mut input = File::open(&src).map_err(|e| io_err(&src, e))?;
        let mut output = File::create(&dst).map_err(|e| io_err(&dst, e))?;
        loop {
            if cancel.is_cancelled() {
                return Err(EngineError::Cancelled);
            }
            let n = input.read(&mut buf).map_err(|e| io_err(&src, e))?;
            if n == 0 {
                break;
            }
            output.write_all(&buf[..n]).map_err(|e| io_err(&dst, e))?;
            done += n as u64;
            if throttle.ready() {
                emit(Event::Progress { done, total });
            }
        }
    }
    emit(Event::Progress { done: total, total });

    for rel in to_split {
        emit(Event::Phase { name: "Splitting install.wim for FAT32 (this can take several minutes)".into() });
        let dest_dir = dst_root.join(rel.parent().unwrap_or(Path::new("")));
        split_wim(&src_root.join(&rel), &dest_dir)?;
    }
    Ok(())
}

pub fn flash_windows_files(
    image: &Path,
    device: &Device,
    cancel: &CancelToken,
    emit: &mut dyn FnMut(Event),
) -> EngineResult<()> {
    emit(Event::Phase { name: "Formatting the drive as FAT32".into() });
    let target = format_target(device, emit)?;

    emit(Event::Phase { name: "Opening the ISO".into() });
    let iso = mount_iso(image)?;

    copy_iso_contents(&iso.root, &target.root, cancel, emit)?;

    #[cfg(windows)]
    {
        // Legacy-BIOS boot needs a boot sector; UEFI boots from the files.
        let bootsect = iso.root.join("boot").join("bootsect.exe");
        if bootsect.exists() {
            emit(Event::Phase { name: "Writing the boot sector".into() });
            let drive = format!("{}:", target.letter);
            if let Err(e) = run(
                Command::new(&bootsect).args(["/nt60", &drive, "/force", "/mbr"]),
                "writing the boot sector",
            ) {
                emit(Event::Log { line: format!("Boot sector step failed (UEFI boot still works): {e}") });
            }
        }
    }

    emit(Event::Phase { name: "Finishing up".into() });
    drop(iso);
    finish_target(&target, device, emit);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn oversized_wims_are_split_but_other_oversized_files_are_refused() {
        assert_eq!(plan_file(Path::new("sources/boot.wim"), 500_000_000).unwrap(), CopyAction::Copy);
        assert_eq!(plan_file(Path::new("sources/install.wim"), FAT32_MAX_FILE).unwrap(), CopyAction::Copy);
        assert_eq!(
            plan_file(Path::new("sources/install.wim"), FAT32_MAX_FILE + 1).unwrap(),
            CopyAction::SplitWim
        );
        assert_eq!(
            plan_file(Path::new("sources/INSTALL.WIM"), 6_000_000_000).unwrap(),
            CopyAction::SplitWim,
            "extension match is case-insensitive"
        );
        assert!(plan_file(Path::new("extras/big.iso"), 6_000_000_000).is_err());
    }

    #[test]
    fn copies_the_whole_tree_and_reports_full_progress() {
        let src = tempfile::tempdir().unwrap();
        let dst = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(src.path().join("sources")).unwrap();
        std::fs::create_dir_all(src.path().join("efi/boot")).unwrap();
        std::fs::write(src.path().join("bootmgr"), vec![1u8; 3000]).unwrap();
        std::fs::write(src.path().join("sources/install.wim"), vec![2u8; 2_500_000]).unwrap();
        std::fs::write(src.path().join("efi/boot/bootx64.efi"), b"efi").unwrap();

        let mut events = Vec::new();
        copy_iso_contents(src.path(), dst.path(), &CancelToken::new(), &mut |e| events.push(e)).unwrap();

        assert_eq!(std::fs::read(dst.path().join("bootmgr")).unwrap(), vec![1u8; 3000]);
        assert_eq!(std::fs::read(dst.path().join("sources/install.wim")).unwrap().len(), 2_500_000);
        assert_eq!(std::fs::read(dst.path().join("efi/boot/bootx64.efi")).unwrap(), b"efi");
        let total = 3000 + 2_500_000 + 3;
        assert!(events.contains(&Event::Progress { done: total, total }));
    }

    #[test]
    fn cancelling_stops_before_copying_everything() {
        let src = tempfile::tempdir().unwrap();
        let dst = tempfile::tempdir().unwrap();
        for i in 0..5 {
            std::fs::write(src.path().join(format!("f{i}")), vec![0u8; 4_000_000]).unwrap();
        }
        let cancel = CancelToken::new();
        let c2 = cancel.clone();
        let r = copy_iso_contents(src.path(), dst.path(), &cancel, &mut |e| {
            if matches!(e, Event::Progress { .. }) {
                c2.cancel();
            }
        });
        assert!(matches!(r, Err(EngineError::Cancelled)));
        let copied = std::fs::read_dir(dst.path()).unwrap().count();
        assert!(copied < 5, "cancel should stop the copy early, but {copied} of 5 files were copied");
    }

    #[test]
    fn an_oversized_wim_is_never_copied_whole_onto_fat32() {
        let src = tempfile::tempdir().unwrap();
        let dst = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(src.path().join("sources")).unwrap();
        // Sparse file: reports 4.5 GB but takes no disk space.
        let wim = File::create(src.path().join("sources/install.wim")).unwrap();
        wim.set_len(4_500_000_000).unwrap();
        std::fs::write(src.path().join("bootmgr"), b"x").unwrap();

        // Splitting needs wimlib/dism, which a test machine may not have, and
        // a zero-filled file isn't a valid WIM anyway — so the split step is
        // expected to fail. What must hold either way: no full-size copy of
        // the WIM ever lands on the FAT32 volume.
        let r = copy_iso_contents(src.path(), dst.path(), &CancelToken::new(), &mut |_| {});
        assert!(r.is_err());
        assert!(r.unwrap_err().to_string().contains("splitting install.wim"));
        let copied = dst.path().join("sources/install.wim");
        assert!(!copied.exists(), "the oversized WIM must not be copied as-is");
    }
}
