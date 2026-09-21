//! Creating and extracting `.zip` archives, with progress and cancellation
//! like the copy engine.
//!
//! Extraction treats every archive as hostile: entry names are checked so a
//! crafted `../../evil` can't escape the destination folder ("zip slip"),
//! symlink entries are written out as plain files rather than followed, and
//! stored permissions are masked so an archive can't create setuid files.

use std::fs::File;
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Instant, SystemTime};

use chrono::{Datelike, Local, NaiveDate, TimeZone, Timelike};
use crossbeam_channel::Sender;
use walkdir::WalkDir;
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

use crate::{io_err, CancelToken, EngineError, EngineResult};

const CHUNK: usize = 1024 * 1024;

#[derive(Debug, Clone)]
pub enum ArchiveEvent {
    Started { files: usize, bytes: u64 },
    Progress { done: u64, total: u64 },
    FileSkipped { path: String, reason: String },
    FileError { path: String, message: String },
    Finished(ArchiveSummary),
    Cancelled,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ArchiveSummary {
    pub files: usize,
    pub skipped: usize,
    pub failed: usize,
    /// Uncompressed bytes processed.
    pub bytes: u64,
    /// Size of the resulting `.zip` (0 when extracting).
    pub archive_bytes: u64,
    pub elapsed_secs: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ZipOptions {
    /// 0 stores files uncompressed; 1 (fastest) to 9 (smallest) deflates.
    pub level: u8,
}

impl Default for ZipOptions {
    fn default() -> Self {
        Self { level: 6 }
    }
}

fn zip_err(e: zip::result::ZipError) -> EngineError {
    EngineError::Other(e.to_string())
}

/// Throttles progress events so a fast run doesn't flood the channel.
struct Throttle(Instant);

impl Throttle {
    fn new() -> Self {
        Self(Instant::now() - std::time::Duration::from_secs(1))
    }
    fn ready(&mut self) -> bool {
        if self.0.elapsed() >= std::time::Duration::from_millis(80) {
            self.0 = Instant::now();
            true
        } else {
            false
        }
    }
}

// ------------------------------------------------------------- timestamps

fn to_zip_time(t: SystemTime) -> Option<zip::DateTime> {
    let local: chrono::DateTime<Local> = t.into();
    let year = u16::try_from(local.year()).ok()?;
    zip::DateTime::from_date_and_time(
        year,
        local.month() as u8,
        local.day() as u8,
        local.hour() as u8,
        local.minute() as u8,
        local.second() as u8,
    )
    .ok()
}

fn from_zip_time(t: zip::DateTime) -> Option<SystemTime> {
    let naive = NaiveDate::from_ymd_opt(t.year().into(), t.month().into(), t.day().into())?
        .and_hms_opt(t.hour().into(), t.minute().into(), t.second().into())?;
    Some(Local.from_local_datetime(&naive).earliest()?.into())
}

// --------------------------------------------------------------- creating

struct PlannedEntry {
    path: PathBuf,
    /// Name inside the archive, always `/`-separated.
    name: String,
    size: u64,
    is_dir: bool,
}

fn archive_name(rel: &Path) -> String {
    rel.components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/")
}

fn plan_zip(sources: &[PathBuf], dest: &Path) -> EngineResult<Vec<PlannedEntry>> {
    let dest_name = dest.file_name().map(|n| n.to_owned());
    let dest_abs = dest
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."))
        .canonicalize()
        .ok()
        .zip(dest_name.clone())
        .map(|(p, n)| p.join(n));
    // Don't pack the archive we're about to (over)write into itself.
    let is_the_archive = |p: &Path| {
        p.file_name() == dest_name.as_deref() && p.canonicalize().ok().as_deref() == dest_abs.as_deref()
    };

    let mut plan: Vec<PlannedEntry> = Vec::new();
    for src in sources {
        let meta = std::fs::metadata(src).map_err(|e| io_err(src, e))?;
        let base = src.parent().unwrap_or(Path::new(""));
        if meta.is_file() {
            if is_the_archive(src) {
                continue;
            }
            let name = src
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .ok_or_else(|| EngineError::Other(format!("bad path {}", src.display())))?;
            plan.push(PlannedEntry { path: src.clone(), name, size: meta.len(), is_dir: false });
        } else if meta.is_dir() {
            for entry in WalkDir::new(src).into_iter().filter_map(|e| e.ok()) {
                let ft = entry.file_type();
                if !(ft.is_file() || ft.is_dir()) {
                    continue; // symlinks and other special files are skipped
                }
                if ft.is_file() && is_the_archive(entry.path()) {
                    continue;
                }
                let rel = entry.path().strip_prefix(base).unwrap_or(entry.path());
                let name = archive_name(rel);
                if name.is_empty() {
                    continue;
                }
                let size = if ft.is_file() { entry.metadata().map(|m| m.len()).unwrap_or(0) } else { 0 };
                plan.push(PlannedEntry { path: entry.path().to_path_buf(), name, size, is_dir: ft.is_dir() });
            }
        }
    }

    let mut seen = std::collections::HashSet::new();
    for e in &plan {
        if !seen.insert(e.name.clone()) {
            return Err(EngineError::Other(format!(
                "two items would both be stored as \"{}\" — rename one and try again",
                e.name
            )));
        }
    }
    Ok(plan)
}

/// Zips `sources` (files and/or folders) into `dest`. The archive is built
/// under a temporary name and only moved into place when complete, so a
/// cancelled or failed run never leaves a truncated `.zip` behind (or
/// clobbers an existing one).
pub fn create_zip(
    sources: &[PathBuf],
    dest: &Path,
    options: ZipOptions,
    cancel: CancelToken,
    tx: Sender<ArchiveEvent>,
) -> EngineResult<ArchiveSummary> {
    let started = Instant::now();
    let plan = plan_zip(sources, dest)?;
    let files = plan.iter().filter(|e| !e.is_dir).count();
    let total: u64 = plan.iter().map(|e| e.size).sum();
    let _ = tx.send(ArchiveEvent::Started { files, bytes: total });

    let mut partial_name = dest.file_name().map(|n| n.to_owned()).unwrap_or_default();
    partial_name.push(".partial");
    let partial = dest.with_file_name(partial_name);

    let result = write_zip(&plan, &partial, options, total, &cancel, &tx);
    match result {
        Ok(mut summary) => {
            std::fs::rename(&partial, dest).map_err(|e| {
                let _ = std::fs::remove_file(&partial);
                io_err(dest, e)
            })?;
            summary.archive_bytes = std::fs::metadata(dest).map(|m| m.len()).unwrap_or(0);
            summary.elapsed_secs = started.elapsed().as_secs_f64();
            let _ = tx.send(ArchiveEvent::Finished(summary.clone()));
            Ok(summary)
        }
        Err(e) => {
            let _ = std::fs::remove_file(&partial);
            if matches!(e, EngineError::Cancelled) {
                let _ = tx.send(ArchiveEvent::Cancelled);
            }
            Err(e)
        }
    }
}

fn write_zip(
    plan: &[PlannedEntry],
    partial: &Path,
    options: ZipOptions,
    total: u64,
    cancel: &CancelToken,
    tx: &Sender<ArchiveEvent>,
) -> EngineResult<ArchiveSummary> {
    let out = File::create(partial).map_err(|e| io_err(partial, e))?;
    let mut zip = ZipWriter::new(BufWriter::new(out));
    let method = if options.level == 0 { CompressionMethod::Stored } else { CompressionMethod::Deflated };
    let level = (options.level > 0).then_some(i64::from(options.level.min(9)));

    let mut summary = ArchiveSummary::default();
    let mut done = 0u64;
    let mut throttle = Throttle::new();
    let mut buf = vec![0u8; CHUNK];

    for entry in plan {
        if cancel.is_cancelled() {
            return Err(EngineError::Cancelled);
        }
        let meta = match std::fs::metadata(&entry.path) {
            Ok(m) => m,
            Err(e) => {
                summary.failed += 1;
                let _ = tx.send(ArchiveEvent::FileError { path: entry.name.clone(), message: e.to_string() });
                continue;
            }
        };
        let mut opts = SimpleFileOptions::default().compression_method(method).compression_level(level);
        if let Some(t) = meta.modified().ok().and_then(to_zip_time) {
            opts = opts.last_modified_time(t);
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            opts = opts.unix_permissions(meta.permissions().mode() & 0o777);
        }

        if entry.is_dir {
            zip.add_directory(&entry.name, opts).map_err(zip_err)?;
            continue;
        }

        // Open before starting the entry, so an unreadable file is skipped
        // cleanly instead of leaving a half-written entry in the archive.
        let file = match File::open(&entry.path) {
            Ok(f) => f,
            Err(e) => {
                summary.failed += 1;
                let _ = tx.send(ArchiveEvent::FileError { path: entry.name.clone(), message: e.to_string() });
                done += entry.size;
                continue;
            }
        };
        zip.start_file(&entry.name, opts.large_file(entry.size >= u64::from(u32::MAX))).map_err(zip_err)?;
        let mut reader = BufReader::new(file);
        loop {
            if cancel.is_cancelled() {
                return Err(EngineError::Cancelled);
            }
            let n = reader.read(&mut buf).map_err(|e| io_err(&entry.path, e))?;
            if n == 0 {
                break;
            }
            zip.write_all(&buf[..n]).map_err(|e| io_err(partial, e))?;
            done += n as u64;
            summary.bytes += n as u64;
            if throttle.ready() {
                let _ = tx.send(ArchiveEvent::Progress { done, total });
            }
        }
        summary.files += 1;
    }
    let _ = tx.send(ArchiveEvent::Progress { done: total, total });
    zip.finish().map_err(zip_err)?.flush().map_err(|e| io_err(partial, e))?;
    Ok(summary)
}

// ------------------------------------------------------------- extracting

/// The entry's name. The zip format says names without its UTF-8 flag are
/// CP437, but in practice Info-ZIP and most Unix tools write UTF-8 without
/// setting the flag — decoding those as CP437 turns "café" into "cafÃ©". So
/// valid UTF-8 wins, and CP437 is only the fallback for bytes that aren't.
fn entry_name(entry: &zip::read::ZipFile<'_, impl Read>) -> String {
    String::from_utf8(entry.name_raw().to_vec()).unwrap_or_else(|_| entry.name().to_owned())
}

/// Turns an archive entry name into a path that is guaranteed to stay inside
/// whatever folder it's joined onto, or `None` if it can't be made safe.
/// Both `/` and `\` separate components (old Windows tools write `\`);
/// empty and `.` components (which includes a leading `/`) are dropped, so an
/// absolute path is re-rooted inside the destination; any `..` is refused.
pub fn safe_relative_path(name: &str) -> Option<PathBuf> {
    if name.contains('\0') {
        return None;
    }
    let mut out = PathBuf::new();
    for part in name.replace('\\', "/").split('/') {
        match part {
            "" | "." => {}
            ".." => return None,
            p if cfg!(windows) && p.contains(':') => return None, // drive letters, NTFS streams
            p => out.push(p),
        }
    }
    (!out.as_os_str().is_empty()).then_some(out)
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ZipInfo {
    pub entries: usize,
    pub files: usize,
    pub uncompressed: u64,
    pub compressed: u64,
}

/// Reads just the archive's table of contents — cheap even for huge zips.
pub fn inspect_zip(path: &Path) -> EngineResult<ZipInfo> {
    let file = File::open(path).map_err(|e| io_err(path, e))?;
    let mut archive = ZipArchive::new(BufReader::new(file)).map_err(zip_err)?;
    let mut info = ZipInfo { entries: archive.len(), ..ZipInfo::default() };
    for i in 0..archive.len() {
        let f = archive.by_index_raw(i).map_err(zip_err)?;
        if !f.is_dir() {
            info.files += 1;
        }
        info.uncompressed += f.size();
        info.compressed += f.compressed_size();
    }
    Ok(info)
}

/// Extracts `archive` into `dest_dir` (created if needed). Files already
/// there are skipped unless `overwrite` is set. On cancel, files extracted
/// so far are left in place.
pub fn extract_zip(
    archive: &Path,
    dest_dir: &Path,
    overwrite: bool,
    cancel: CancelToken,
    tx: Sender<ArchiveEvent>,
) -> EngineResult<ArchiveSummary> {
    let started = Instant::now();
    let file = File::open(archive).map_err(|e| io_err(archive, e))?;
    let mut zip = ZipArchive::new(BufReader::new(file)).map_err(zip_err)?;
    std::fs::create_dir_all(dest_dir).map_err(|e| io_err(dest_dir, e))?;

    let mut total = 0u64;
    let mut files = 0usize;
    for i in 0..zip.len() {
        let f = zip.by_index_raw(i).map_err(zip_err)?;
        if !f.is_dir() {
            total += f.size();
            files += 1;
        }
    }
    let _ = tx.send(ArchiveEvent::Started { files, bytes: total });

    let mut summary = ArchiveSummary::default();
    let mut done = 0u64;
    let mut throttle = Throttle::new();
    let mut buf = vec![0u8; CHUNK];

    for i in 0..zip.len() {
        if cancel.is_cancelled() {
            let _ = tx.send(ArchiveEvent::Cancelled);
            return Err(EngineError::Cancelled);
        }
        let raw_name = entry_name(&zip.by_index_raw(i).map_err(zip_err)?);
        let mut entry = match zip.by_index(i) {
            Ok(e) => e,
            Err(e) => {
                summary.failed += 1;
                let _ = tx.send(ArchiveEvent::FileError { path: raw_name, message: e.to_string() });
                continue;
            }
        };
        let Some(rel) = safe_relative_path(&raw_name) else {
            summary.failed += 1;
            let _ = tx.send(ArchiveEvent::FileError {
                path: raw_name,
                message: "unsafe path (would land outside the destination) — skipped".into(),
            });
            continue;
        };
        let out_path = dest_dir.join(&rel);

        if entry.is_dir() {
            if let Err(e) = std::fs::create_dir_all(&out_path) {
                summary.failed += 1;
                let _ = tx.send(ArchiveEvent::FileError { path: raw_name, message: e.to_string() });
            }
            continue;
        }
        if !overwrite && out_path.exists() {
            summary.skipped += 1;
            done += entry.size();
            let _ = tx.send(ArchiveEvent::FileSkipped { path: raw_name, reason: "already exists".into() });
            continue;
        }

        let mtime = entry.last_modified().and_then(from_zip_time);
        let mode = entry.unix_mode();
        let size = entry.size();
        let done_before = done;
        let written = (|| -> EngineResult<()> {
            if let Some(parent) = out_path.parent() {
                std::fs::create_dir_all(parent).map_err(|e| io_err(parent, e))?;
            }
            // A symlink entry's "content" is its target path; writing that as
            // a plain file is safe, whereas creating the link could redirect
            // later entries outside the destination.
            let mut out = BufWriter::new(File::create(&out_path).map_err(|e| io_err(&out_path, e))?);
            loop {
                if cancel.is_cancelled() {
                    return Err(EngineError::Cancelled);
                }
                let n = entry.read(&mut buf).map_err(|e| EngineError::Other(format!("reading {raw_name}: {e}")))?;
                if n == 0 {
                    break;
                }
                out.write_all(&buf[..n]).map_err(|e| io_err(&out_path, e))?;
                done += n as u64;
                summary.bytes += n as u64;
                if throttle.ready() {
                    let _ = tx.send(ArchiveEvent::Progress { done, total });
                }
            }
            out.flush().map_err(|e| io_err(&out_path, e))?;
            Ok(())
        })();

        match written {
            Ok(()) => {
                summary.files += 1;
                if let Some(t) = mtime {
                    let _ = filetime::set_file_mtime(&out_path, filetime::FileTime::from_system_time(t));
                }
                #[cfg(unix)]
                if let Some(m) = mode {
                    use std::os::unix::fs::PermissionsExt;
                    // Owner keeps read/write; setuid/setgid/sticky are dropped.
                    let _ = std::fs::set_permissions(&out_path, std::fs::Permissions::from_mode((m & 0o777) | 0o600));
                }
                #[cfg(not(unix))]
                let _ = mode;
            }
            Err(EngineError::Cancelled) => {
                let _ = std::fs::remove_file(&out_path);
                let _ = tx.send(ArchiveEvent::Cancelled);
                return Err(EngineError::Cancelled);
            }
            Err(e) => {
                let _ = std::fs::remove_file(&out_path);
                summary.failed += 1;
                done = done_before + size; // keep the progress bar moving past it
                let _ = tx.send(ArchiveEvent::FileError { path: raw_name, message: e.to_string() });
            }
        }
    }

    let _ = tx.send(ArchiveEvent::Progress { done: total, total });
    summary.elapsed_secs = started.elapsed().as_secs_f64();
    let _ = tx.send(ArchiveEvent::Finished(summary.clone()));
    Ok(summary)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn write(path: &Path, bytes: &[u8]) {
        if let Some(p) = path.parent() {
            fs::create_dir_all(p).unwrap();
        }
        fs::write(path, bytes).unwrap();
    }

    fn zip_it(sources: &[PathBuf], dest: &Path) -> ArchiveSummary {
        let (tx, _rx) = crossbeam_channel::unbounded();
        create_zip(sources, dest, ZipOptions::default(), CancelToken::new(), tx).unwrap()
    }

    fn unzip_it(archive: &Path, dest: &Path, overwrite: bool) -> (ArchiveSummary, Vec<ArchiveEvent>) {
        let (tx, rx) = crossbeam_channel::unbounded();
        let s = extract_zip(archive, dest, overwrite, CancelToken::new(), tx).unwrap();
        (s, rx.try_iter().collect())
    }

    /// Builds a zip by hand, so tests can include entries `create_zip` would
    /// never produce (hostile names, symlinks, setuid bits).
    fn hostile_zip(path: &Path, build: impl FnOnce(&mut ZipWriter<File>)) {
        let mut w = ZipWriter::new(File::create(path).unwrap());
        build(&mut w);
        w.finish().unwrap();
    }

    #[test]
    fn round_trip_preserves_tree_contents_names_and_permissions() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("photos");
        let big: Vec<u8> = (0..3_000_000u32).map(|i| (i % 251) as u8).collect();
        write(&src.join("a.txt"), b"hello");
        write(&src.join("sub/deep/café ☕.txt"), "unicode ok".as_bytes());
        write(&src.join("sub/big.bin"), &big);
        fs::create_dir_all(src.join("empty_dir")).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            write(&src.join("run.sh"), b"#!/bin/sh\n");
            fs::set_permissions(src.join("run.sh"), fs::Permissions::from_mode(0o755)).unwrap();
        }
        let old = filetime::FileTime::from_unix_time(1_600_000_000, 0);
        filetime::set_file_mtime(src.join("a.txt"), old).unwrap();

        let zip = dir.path().join("out.zip");
        let summary = zip_it(std::slice::from_ref(&src), &zip);
        assert_eq!(summary.failed, 0);
        assert_eq!(summary.files, 3 + cfg!(unix) as usize, "a.txt, café, big.bin (+ run.sh on unix)");
        assert!(summary.archive_bytes > 0 && summary.archive_bytes < big.len() as u64, "should actually compress");

        let out = dir.path().join("extracted");
        unzip_it(&zip, &out, false);
        assert_eq!(fs::read(out.join("photos/a.txt")).unwrap(), b"hello");
        assert_eq!(fs::read(out.join("photos/sub/deep/café ☕.txt")).unwrap(), "unicode ok".as_bytes());
        assert_eq!(fs::read(out.join("photos/sub/big.bin")).unwrap(), big);
        assert!(out.join("photos/empty_dir").is_dir(), "empty folders are kept");

        let got = filetime::FileTime::from_last_modification_time(&fs::metadata(out.join("photos/a.txt")).unwrap());
        assert!((got.unix_seconds() - 1_600_000_000).abs() <= 2, "mtime survives (zip has 2s resolution): {got:?}");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(fs::metadata(out.join("photos/run.sh")).unwrap().permissions().mode() & 0o777, 0o755);
        }
    }

    #[test]
    fn single_files_and_folders_can_be_mixed_and_the_archive_never_contains_itself() {
        let dir = tempfile::tempdir().unwrap();
        let folder = dir.path().join("docs");
        write(&folder.join("one.txt"), b"1");
        write(&dir.path().join("loose.txt"), b"2");
        // Output goes *inside* the folder being zipped.
        let zip = folder.join("docs.zip");
        zip_it(&[folder.clone(), dir.path().join("loose.txt")], &zip);
        zip_it(&[folder.clone(), dir.path().join("loose.txt")], &zip); // and again over the old one

        let info = inspect_zip(&zip).unwrap();
        assert_eq!(info.files, 2, "only one.txt and loose.txt — not docs.zip itself");
        let names: Vec<String> = {
            let mut a = ZipArchive::new(File::open(&zip).unwrap()).unwrap();
            (0..a.len()).map(|i| a.by_index(i).unwrap().name().to_owned()).collect()
        };
        assert!(names.contains(&"docs/one.txt".to_owned()) && names.contains(&"loose.txt".to_owned()), "{names:?}");
    }

    #[test]
    fn colliding_names_are_rejected_before_anything_is_written() {
        let dir = tempfile::tempdir().unwrap();
        write(&dir.path().join("x/report.txt"), b"a");
        write(&dir.path().join("y/report.txt"), b"b");
        let (tx, _rx) = crossbeam_channel::unbounded();
        let zip = dir.path().join("o.zip");
        let r = create_zip(
            &[dir.path().join("x/report.txt"), dir.path().join("y/report.txt")],
            &zip,
            ZipOptions::default(),
            CancelToken::new(),
            tx,
        );
        assert!(r.unwrap_err().to_string().contains("report.txt"));
        assert!(!zip.exists());
    }

    #[test]
    fn a_cancelled_create_leaves_no_archive_or_partial_file_behind() {
        let dir = tempfile::tempdir().unwrap();
        write(&dir.path().join("src/big.bin"), &vec![7u8; 5_000_000]);
        let zip = dir.path().join("o.zip");
        let cancel = CancelToken::new();
        cancel.cancel();
        let (tx, rx) = crossbeam_channel::unbounded();
        let r = create_zip(&[dir.path().join("src")], &zip, ZipOptions::default(), cancel, tx);
        assert!(matches!(r, Err(EngineError::Cancelled)));
        assert!(rx.try_iter().any(|e| matches!(e, ArchiveEvent::Cancelled)));
        assert!(!zip.exists());
        assert!(!dir.path().join("o.zip.partial").exists());
    }

    #[test]
    fn cancelling_never_clobbers_an_existing_archive() {
        let dir = tempfile::tempdir().unwrap();
        write(&dir.path().join("src/f.txt"), b"new");
        let zip = dir.path().join("o.zip");
        fs::write(&zip, b"PRECIOUS OLD ARCHIVE").unwrap();
        let cancel = CancelToken::new();
        cancel.cancel();
        let (tx, _rx) = crossbeam_channel::unbounded();
        let _ = create_zip(&[dir.path().join("src")], &zip, ZipOptions::default(), cancel, tx);
        assert_eq!(fs::read(&zip).unwrap(), b"PRECIOUS OLD ARCHIVE");
    }

    #[test]
    fn zip_slip_entries_cannot_write_outside_the_destination() {
        let dir = tempfile::tempdir().unwrap();
        let zip = dir.path().join("evil.zip");
        hostile_zip(&zip, |w| {
            let o = SimpleFileOptions::default();
            for name in ["../escaped.txt", "sub/../../escaped2.txt", "/tmp/absolute-escape.txt", "good.txt"] {
                w.start_file(name, o).unwrap();
                w.write_all(b"payload").unwrap();
            }
        });
        let dest = dir.path().join("safe/inside");
        let (summary, events) = unzip_it(&zip, &dest, false);

        assert!(dest.join("good.txt").exists(), "the harmless entry still extracts");
        assert!(!dir.path().join("safe/escaped.txt").exists(), "../ escaped the destination");
        assert!(!dir.path().join("escaped2.txt").exists() && !dir.path().join("safe/escaped2.txt").exists());
        assert!(!Path::new("/tmp/absolute-escape.txt").exists(), "absolute path was honoured");
        // Entries that climb out with `..` are rejected and reported. A leading
        // `/` is instead stripped, so the entry lands harmlessly *inside* the
        // destination rather than at the absolute location it asked for.
        assert!(dest.join("tmp/absolute-escape.txt").exists(), "absolute path should be re-rooted inside dest");
        assert_eq!(summary.failed, 2);
        assert_eq!(summary.files, 2);
        let unsafe_reports = events
            .iter()
            .filter(|e| matches!(e, ArchiveEvent::FileError { message, .. } if message.contains("unsafe path")))
            .count();
        assert_eq!(unsafe_reports, 2, "each escaping entry is reported, not silently dropped");
    }

    #[cfg(unix)]
    #[test]
    fn symlink_entries_become_plain_files_and_setuid_bits_are_dropped() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let zip = dir.path().join("tricky.zip");
        hostile_zip(&zip, |w| {
            let o = SimpleFileOptions::default();
            w.add_symlink("link", "/etc/passwd", o).unwrap();
            w.start_file("suid", o.unix_permissions(0o4755)).unwrap();
            w.write_all(b"x").unwrap();
        });
        let dest = dir.path().join("out");
        unzip_it(&zip, &dest, false);

        let link_meta = fs::symlink_metadata(dest.join("link")).unwrap();
        assert!(link_meta.file_type().is_file(), "must not create a real symlink");
        assert_eq!(fs::read_to_string(dest.join("link")).unwrap(), "/etc/passwd");
        let mode = fs::metadata(dest.join("suid")).unwrap().permissions().mode();
        assert_eq!(mode & 0o7000, 0, "setuid/setgid/sticky must be stripped, got {mode:o}");
        assert_eq!(mode & 0o777, 0o755);
    }

    #[test]
    fn existing_files_are_skipped_unless_overwrite_is_on() {
        let dir = tempfile::tempdir().unwrap();
        write(&dir.path().join("src/f.txt"), b"from zip");
        let zip = dir.path().join("o.zip");
        zip_it(&[dir.path().join("src")], &zip);
        let dest = dir.path().join("dest");
        write(&dest.join("src/f.txt"), b"mine");

        let (s, events) = unzip_it(&zip, &dest, false);
        assert_eq!((s.skipped, s.files), (1, 0));
        assert!(events.iter().any(|e| matches!(e, ArchiveEvent::FileSkipped { .. })));
        assert_eq!(fs::read(dest.join("src/f.txt")).unwrap(), b"mine");

        let (s, _) = unzip_it(&zip, &dest, true);
        assert_eq!((s.skipped, s.files), (0, 1));
        assert_eq!(fs::read(dest.join("src/f.txt")).unwrap(), b"from zip");
    }

    #[test]
    fn extract_reports_full_progress_and_can_be_cancelled() {
        let dir = tempfile::tempdir().unwrap();
        write(&dir.path().join("src/a.bin"), &vec![1u8; 2_000_000]);
        let zip = dir.path().join("o.zip");
        zip_it(&[dir.path().join("src")], &zip);

        let (_, events) = unzip_it(&zip, &dir.path().join("ok"), false);
        assert!(events.iter().any(|e| matches!(e, ArchiveEvent::Progress { done, total } if done == total && *total == 2_000_000)));

        let cancel = CancelToken::new();
        cancel.cancel();
        let (tx, _rx) = crossbeam_channel::unbounded();
        let r = extract_zip(&zip, &dir.path().join("nope"), false, cancel, tx);
        assert!(matches!(r, Err(EngineError::Cancelled)));
    }

    #[test]
    fn safe_relative_path_keeps_good_names_and_refuses_escapes() {
        let ok = |n: &str| safe_relative_path(n).map(|p| p.to_string_lossy().replace('\\', "/"));
        assert_eq!(ok("a/b.txt").as_deref(), Some("a/b.txt"));
        assert_eq!(ok("dir/").as_deref(), Some("dir"));
        assert_eq!(ok("./a//b").as_deref(), Some("a/b"), "empty and . components are dropped");
        assert_eq!(ok("/etc/passwd").as_deref(), Some("etc/passwd"), "absolute paths are re-rooted");
        assert_eq!(ok("win\\style\\file.txt").as_deref(), Some("win/style/file.txt"));
        assert_eq!(ok("café ☕/文件.txt").as_deref(), Some("café ☕/文件.txt"));
        for bad in ["../x", "a/../../x", "a/..", "..\\x", "a\\..\\..\\x", "", "/", "./", "a\0b"] {
            assert_eq!(ok(bad), None, "{bad:?} must be refused");
        }
    }

    #[test]
    fn a_non_zip_file_is_a_clean_error_not_a_panic() {
        let dir = tempfile::tempdir().unwrap();
        let bogus = dir.path().join("x.zip");
        fs::write(&bogus, b"this is not a zip file at all").unwrap();
        assert!(inspect_zip(&bogus).is_err());
        let (tx, _rx) = crossbeam_channel::unbounded();
        assert!(extract_zip(&bogus, &dir.path().join("o"), false, CancelToken::new(), tx).is_err());
    }

    #[test]
    fn level_zero_stores_without_compressing() {
        let dir = tempfile::tempdir().unwrap();
        write(&dir.path().join("src/z.bin"), &vec![0u8; 1_000_000]);
        let (tx, _rx) = crossbeam_channel::unbounded();
        let stored = dir.path().join("stored.zip");
        create_zip(&[dir.path().join("src")], &stored, ZipOptions { level: 0 }, CancelToken::new(), tx).unwrap();
        let deflated = dir.path().join("deflated.zip");
        zip_it(&[dir.path().join("src")], &deflated);
        assert!(fs::metadata(&stored).unwrap().len() > 1_000_000);
        assert!(fs::metadata(&deflated).unwrap().len() < 10_000);
    }
}
