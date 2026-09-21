//! Checks our archives against the system's own `zip`/`unzip`, which is what
//! users will actually open them with. Skipped where those tools aren't
//! installed.
#![cfg(unix)]

use std::fs;
use std::path::Path;
use std::process::Command;

use engine::archive::{create_zip, extract_zip, ZipOptions};
use engine::CancelToken;

fn have(tool: &str) -> bool {
    Command::new("which").arg(tool).output().map(|o| o.status.success()).unwrap_or(false)
}

fn write(path: &Path, bytes: &[u8]) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, bytes).unwrap();
}

#[test]
fn system_unzip_accepts_our_archives() {
    if !have("unzip") {
        eprintln!("skipping: unzip not installed");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("proj");
    write(&src.join("readme.md"), b"# hi\n");
    write(&src.join("src/main.rs"), &vec![b'x'; 200_000]);
    write(&src.join("naïve/文件.txt"), "unicode".as_bytes());
    fs::create_dir_all(src.join("empty")).unwrap();

    let zip = dir.path().join("proj.zip");
    let (tx, _rx) = crossbeam_channel::unbounded();
    create_zip(std::slice::from_ref(&src), &zip, ZipOptions::default(), CancelToken::new(), tx).unwrap();

    let test = Command::new("unzip").arg("-t").arg(&zip).output().unwrap();
    assert!(test.status.success(), "unzip -t rejected our archive: {}", String::from_utf8_lossy(&test.stdout));

    let out = dir.path().join("unzipped");
    let x = Command::new("unzip").arg("-q").arg(&zip).arg("-d").arg(&out).output().unwrap();
    assert!(x.status.success(), "{}", String::from_utf8_lossy(&x.stderr));
    assert_eq!(fs::read(out.join("proj/src/main.rs")).unwrap(), vec![b'x'; 200_000]);
    assert!(out.join("proj/empty").is_dir());
    assert_eq!(fs::read_to_string(out.join("proj/naïve/文件.txt")).unwrap(), "unicode");
}

#[test]
fn we_extract_archives_made_by_the_system_zip() {
    if !have("zip") {
        eprintln!("skipping: zip not installed");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("made-by-zip");
    write(&src.join("a.txt"), b"alpha");
    write(&src.join("nested/b.bin"), &(0..100_000u32).map(|i| (i % 253) as u8).collect::<Vec<_>>());
    write(&src.join("naïve/文件.txt"), "unicode".as_bytes());

    let zip = dir.path().join("theirs.zip");
    let s = Command::new("zip")
        .current_dir(dir.path())
        // Without a UTF-8 locale Info-ZIP replaces non-ASCII bytes with '?'.
        .env("LC_ALL", "en_US.UTF-8")
        .args(["-r", "-q"]).arg(&zip).arg("made-by-zip").output().unwrap();
    assert!(s.status.success(), "{}", String::from_utf8_lossy(&s.stderr));

    let out = dir.path().join("ours");
    let (tx, _rx) = crossbeam_channel::unbounded();
    let summary = extract_zip(&zip, &out, false, CancelToken::new(), tx).unwrap();
    assert_eq!(summary.failed, 0);

    let diff = Command::new("diff").arg("-r").arg(&src).arg(out.join("made-by-zip")).output().unwrap();
    assert!(diff.status.success(), "trees differ: {}", String::from_utf8_lossy(&diff.stdout));
}

/// Entries over 4 GiB need the zip64 extensions. Slow (streams ~4.5 GB
/// through the compressor), so it's opt-in:
/// `cargo test -p engine --test zip_interop -- --ignored`
#[test]
#[ignore]
fn a_file_over_4_gib_round_trips_through_zip64() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("huge");
    fs::create_dir_all(&src).unwrap();
    let size: u64 = 4_500_000_000;
    let f = fs::File::create(src.join("big.bin")).unwrap();
    f.set_len(size).unwrap(); // sparse: no real disk use

    let zip = dir.path().join("huge.zip");
    let (tx, _rx) = crossbeam_channel::unbounded();
    let s = create_zip(&[src], &zip, ZipOptions { level: 1 }, CancelToken::new(), tx).unwrap();
    assert_eq!(s.bytes, size);

    if have("unzip") {
        let t = Command::new("unzip").arg("-t").arg(&zip).output().unwrap();
        assert!(t.status.success(), "system unzip rejected the zip64 archive: {}", String::from_utf8_lossy(&t.stdout));
    }
    let out = dir.path().join("out");
    let (tx, _rx) = crossbeam_channel::unbounded();
    extract_zip(&zip, &out, false, CancelToken::new(), tx).unwrap();
    assert_eq!(fs::metadata(out.join("huge/big.bin")).unwrap().len(), size);
}
