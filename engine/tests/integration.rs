use std::fs;
use std::path::Path;

use engine::copy::{self, CopyOptions, OverwritePolicy};
use engine::duplicates::{self, DupOptions};
use engine::fsops::{self, OrganizeStrategy};
use engine::large_files::{self, LargeFilesOptions};
use engine::search::{self, SearchOptions};
use engine::sync::{self, SyncOptions};
use engine::CancelToken;

fn write_file(path: &Path, contents: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, contents).unwrap();
}

#[test]
fn copy_tree_copies_files_and_nested_dirs() {
    let src_dir = tempfile::tempdir().unwrap();
    let dst_dir = tempfile::tempdir().unwrap();

    write_file(&src_dir.path().join("a.txt"), "hello");
    write_file(&src_dir.path().join("nested/b.txt"), "world");

    let sources = vec![src_dir.path().join("a.txt"), src_dir.path().join("nested")];
    let (tx, rx) = crossbeam_channel::unbounded();
    let summary = copy::copy_tree(
        &sources,
        dst_dir.path(),
        CopyOptions::default(),
        CancelToken::new(),
        tx,
    )
    .unwrap();
    let _ = rx.try_iter().count();

    assert_eq!(summary.files_copied, 2);
    assert_eq!(summary.files_failed, 0);
    assert_eq!(fs::read_to_string(dst_dir.path().join("a.txt")).unwrap(), "hello");
    assert_eq!(
        fs::read_to_string(dst_dir.path().join("nested/b.txt")).unwrap(),
        "world"
    );
}

#[test]
fn copy_tree_skips_already_copied_files_on_rerun() {
    let src_dir = tempfile::tempdir().unwrap();
    let dst_dir = tempfile::tempdir().unwrap();
    write_file(&src_dir.path().join("a.txt"), "hello");

    let sources = vec![src_dir.path().join("a.txt")];
    let options = CopyOptions {
        overwrite: OverwritePolicy::SkipIfSameSize,
        ..CopyOptions::default()
    };

    let (tx1, _rx1) = crossbeam_channel::unbounded();
    let first = copy::copy_tree(&sources, dst_dir.path(), options.clone(), CancelToken::new(), tx1).unwrap();
    assert_eq!(first.files_copied, 1);

    let (tx2, _rx2) = crossbeam_channel::unbounded();
    let second = copy::copy_tree(&sources, dst_dir.path(), options, CancelToken::new(), tx2).unwrap();
    assert_eq!(second.files_copied, 0);
    assert_eq!(second.files_skipped, 1);
}

#[test]
fn copy_tree_with_verify_produces_byte_identical_output() {
    let src_dir = tempfile::tempdir().unwrap();
    let dst_dir = tempfile::tempdir().unwrap();
    write_file(&src_dir.path().join("a.bin"), &"x".repeat(200_000));

    let sources = vec![src_dir.path().join("a.bin")];
    let options = CopyOptions {
        verify: true,
        ..CopyOptions::default()
    };
    let (tx, _rx) = crossbeam_channel::unbounded();
    let summary = copy::copy_tree(&sources, dst_dir.path(), options, CancelToken::new(), tx).unwrap();
    assert_eq!(summary.files_copied, 1);
    assert_eq!(summary.files_failed, 0);
    assert_eq!(
        fs::metadata(dst_dir.path().join("a.bin")).unwrap().len(),
        200_000
    );
}

#[test]
fn find_duplicates_groups_identical_files_only() {
    let dir = tempfile::tempdir().unwrap();
    write_file(&dir.path().join("a.txt"), "same content here");
    write_file(&dir.path().join("b.txt"), "same content here");
    write_file(&dir.path().join("c.txt"), "different content");

    let (tx, _rx) = crossbeam_channel::unbounded();
    let groups = duplicates::find_duplicates(
        &[dir.path().to_path_buf()],
        DupOptions { min_size: 0 },
        CancelToken::new(),
        tx,
    )
    .unwrap();

    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0].paths.len(), 2);
}

#[test]
fn organize_dir_sorts_files_by_extension_category() {
    let dir = tempfile::tempdir().unwrap();
    write_file(&dir.path().join("photo.jpg"), "img");
    write_file(&dir.path().join("report.pdf"), "doc");
    write_file(&dir.path().join("mystery.xyz"), "???");

    let (tx, _rx) = crossbeam_channel::unbounded();
    let summary = fsops::organize_dir(
        dir.path(),
        OrganizeStrategy::ByExtension,
        CancelToken::new(),
        tx,
    )
    .unwrap();

    assert_eq!(summary.moved, 3);
    assert!(dir.path().join("Images/photo.jpg").exists());
    assert!(dir.path().join("Documents/report.pdf").exists());
    assert!(dir.path().join("Other/mystery.xyz").exists());
}

#[test]
fn sync_dirs_copies_then_mirrors_deletions() {
    let src_dir = tempfile::tempdir().unwrap();
    let dst_dir = tempfile::tempdir().unwrap();
    write_file(&src_dir.path().join("keep.txt"), "keep");
    write_file(&src_dir.path().join("remove_me.txt"), "temp");

    let (tx, _rx) = crossbeam_channel::unbounded();
    let summary = sync::sync_dirs(
        src_dir.path(),
        dst_dir.path(),
        SyncOptions::default(),
        CancelToken::new(),
        tx,
    )
    .unwrap();
    assert_eq!(summary.copied, 2);
    assert!(dst_dir.path().join("keep.txt").exists());
    assert!(dst_dir.path().join("remove_me.txt").exists());

    fs::remove_file(src_dir.path().join("remove_me.txt")).unwrap();

    let (tx2, _rx2) = crossbeam_channel::unbounded();
    let summary2 = sync::sync_dirs(
        src_dir.path(),
        dst_dir.path(),
        SyncOptions {
            mirror: true,
            verify: false,
        },
        CancelToken::new(),
        tx2,
    )
    .unwrap();
    assert_eq!(summary2.deleted, 1);
    assert!(dst_dir.path().join("keep.txt").exists());
    assert!(!dst_dir.path().join("remove_me.txt").exists());
}

#[test]
fn move_tree_moves_files_and_removes_originals() {
    let src_dir = tempfile::tempdir().unwrap();
    let dst_dir = tempfile::tempdir().unwrap();
    write_file(&src_dir.path().join("a.txt"), "hello");
    write_file(&src_dir.path().join("nested/b.txt"), "world");

    let sources = vec![src_dir.path().join("a.txt"), src_dir.path().join("nested")];
    let (tx, _rx) = crossbeam_channel::unbounded();
    let summary = copy::move_tree(
        &sources,
        dst_dir.path(),
        CopyOptions::default(),
        CancelToken::new(),
        tx,
    )
    .unwrap();

    assert_eq!(summary.files_copied, 2);
    assert_eq!(summary.files_failed, 0);
    assert_eq!(fs::read_to_string(dst_dir.path().join("a.txt")).unwrap(), "hello");
    assert_eq!(
        fs::read_to_string(dst_dir.path().join("nested/b.txt")).unwrap(),
        "world"
    );
    // Originals must be gone.
    assert!(!src_dir.path().join("a.txt").exists());
    assert!(!src_dir.path().join("nested").exists());
}

#[test]
fn move_tree_falls_back_and_still_removes_original_when_dest_name_taken() {
    let src_dir = tempfile::tempdir().unwrap();
    let dst_dir = tempfile::tempdir().unwrap();
    write_file(&src_dir.path().join("a.txt"), "new content");
    // A same-named destination already exists, so the plain-rename fast
    // path can't apply and move_tree must fall back to copy-then-delete.
    write_file(&dst_dir.path().join("a.txt"), "old content");

    let sources = vec![src_dir.path().join("a.txt")];
    let options = CopyOptions {
        overwrite: OverwritePolicy::Always,
        ..CopyOptions::default()
    };
    let (tx, _rx) = crossbeam_channel::unbounded();
    let summary = copy::move_tree(&sources, dst_dir.path(), options, CancelToken::new(), tx).unwrap();

    assert_eq!(summary.files_copied, 1);
    assert_eq!(summary.files_failed, 0);
    assert_eq!(fs::read_to_string(dst_dir.path().join("a.txt")).unwrap(), "new content");
    assert!(!src_dir.path().join("a.txt").exists());
}

#[test]
fn apply_rename_pattern_expands_all_tokens() {
    assert_eq!(fsops::apply_rename_pattern("{name}.{ext}", "photo", "jpg", 1), "photo.jpg");
    assert_eq!(
        fsops::apply_rename_pattern("vacation_{nnn}.{ext}", "img1234", "png", 7),
        "vacation_007.png"
    );
    assert_eq!(fsops::apply_rename_pattern("{n}_{name}", "a", "", 12), "12_a");
}

#[test]
fn rename_batch_renames_in_order() {
    let dir = tempfile::tempdir().unwrap();
    write_file(&dir.path().join("one.txt"), "1");
    write_file(&dir.path().join("two.txt"), "2");

    let paths = vec![dir.path().join("one.txt"), dir.path().join("two.txt")];
    let renamed = fsops::rename_batch(&paths, "item_{nn}.{ext}").unwrap();

    assert_eq!(renamed[0].file_name().unwrap(), "item_01.txt");
    assert_eq!(renamed[1].file_name().unwrap(), "item_02.txt");
    assert!(renamed[0].exists());
    assert!(renamed[1].exists());
    assert!(!dir.path().join("one.txt").exists());
}

#[test]
fn find_large_files_returns_only_files_over_the_threshold_sorted_desc() {
    let dir = tempfile::tempdir().unwrap();
    write_file(&dir.path().join("small.txt"), &"x".repeat(10));
    write_file(&dir.path().join("medium.bin"), &"x".repeat(5_000));
    write_file(&dir.path().join("large.bin"), &"x".repeat(20_000));

    let (tx, _rx) = crossbeam_channel::unbounded();
    let results = large_files::find_large_files(
        &[dir.path().to_path_buf()],
        LargeFilesOptions { min_size: 1_000 },
        CancelToken::new(),
        tx,
    )
    .unwrap();

    assert_eq!(results.len(), 2);
    assert_eq!(results[0].path.file_name().unwrap(), "large.bin");
    assert_eq!(results[0].size, 20_000);
    assert_eq!(results[1].path.file_name().unwrap(), "medium.bin");
    assert_eq!(results[1].size, 5_000);
}

#[test]
fn search_files_matches_by_name_case_insensitively_and_skips_dirs_by_default() {
    let dir = tempfile::tempdir().unwrap();
    write_file(&dir.path().join("Vacation_Photo.jpg"), "x");
    write_file(&dir.path().join("other.txt"), "x");
    write_file(&dir.path().join("subdir/vacation_notes.txt"), "x");
    std::fs::create_dir_all(dir.path().join("vacation_folder")).unwrap();

    let (tx, _rx) = crossbeam_channel::unbounded();
    let results = search::search_files(
        &[dir.path().to_path_buf()],
        SearchOptions {
            query: "vacation".to_string(),
            case_sensitive: false,
            include_dirs: false,
        },
        CancelToken::new(),
        tx,
    )
    .unwrap();

    let names: Vec<String> = results
        .iter()
        .map(|m| m.path.file_name().unwrap().to_string_lossy().to_string())
        .collect();
    assert_eq!(results.len(), 2);
    assert!(names.contains(&"Vacation_Photo.jpg".to_string()));
    assert!(names.contains(&"vacation_notes.txt".to_string()));
    assert!(results.iter().all(|m| !m.is_dir));
}

#[test]
fn concurrent_copy_batches_to_the_same_destination_dont_interfere() {
    // This is the exact scenario the "add files mid-copy" UI feature
    // creates: two independent copy_tree calls, running at the same time,
    // targeting the same destination folder (including a shared nested
    // subdirectory neither call created first).
    let src_dir = tempfile::tempdir().unwrap();
    let dst_dir = tempfile::tempdir().unwrap();

    for i in 0..20 {
        write_file(&src_dir.path().join(format!("batch_a/{i}.txt")), &format!("a{i}"));
        write_file(&src_dir.path().join(format!("batch_b/{i}.txt")), &format!("b{i}"));
    }

    let (tx_a, rx_a) = crossbeam_channel::unbounded();
    let (tx_b, rx_b) = crossbeam_channel::unbounded();

    let sources_a = vec![src_dir.path().join("batch_a")];
    let sources_b = vec![src_dir.path().join("batch_b")];
    let dst_a = dst_dir.path().to_path_buf();
    let dst_b = dst_dir.path().to_path_buf();

    let handle_a = std::thread::spawn(move || {
        copy::copy_tree(&sources_a, &dst_a, CopyOptions::default(), CancelToken::new(), tx_a)
    });
    let handle_b = std::thread::spawn(move || {
        copy::copy_tree(&sources_b, &dst_b, CopyOptions::default(), CancelToken::new(), tx_b)
    });

    let summary_a = handle_a.join().unwrap().unwrap();
    let summary_b = handle_b.join().unwrap().unwrap();
    let _ = rx_a.try_iter().count();
    let _ = rx_b.try_iter().count();

    assert_eq!(summary_a.files_copied, 20);
    assert_eq!(summary_b.files_copied, 20);
    assert_eq!(summary_a.files_failed, 0);
    assert_eq!(summary_b.files_failed, 0);

    for i in 0..20 {
        assert_eq!(
            fs::read_to_string(dst_dir.path().join(format!("batch_a/{i}.txt"))).unwrap(),
            format!("a{i}")
        );
        assert_eq!(
            fs::read_to_string(dst_dir.path().join(format!("batch_b/{i}.txt"))).unwrap(),
            format!("b{i}")
        );
    }
}

// Both sub-tests below bind engine::share's fixed ports (DISCOVERY_PORT /
// TRANSFER_PORT), so they're combined into one #[test] run sequentially
// rather than two separate ones that could race for the same port under
// cargo test's default parallelism.
#[test]
fn share_transfer_and_discovery() {
    use engine::share;
    use std::net::SocketAddr;

    // --- transfer: send accepts, files land correctly, and a
    // path-traversal attempt in the manifest is neutralized rather than
    // escaping the destination folder ---
    let src_dir = tempfile::tempdir().unwrap();
    let dst_dir = tempfile::tempdir().unwrap();
    write_file(&src_dir.path().join("a.txt"), "hello");
    write_file(&src_dir.path().join("sub/b.txt"), "world");

    let recv_cancel = CancelToken::new();
    let (recv_tx, recv_rx) = crossbeam_channel::unbounded();
    let recv_cancel2 = recv_cancel.clone();
    let recv_dest = dst_dir.path().to_path_buf();
    let recv_handle = std::thread::spawn(move || {
        share::run_receiver(recv_dest, recv_cancel2, recv_tx);
    });

    // Give the listener a moment to bind before we connect.
    std::thread::sleep(std::time::Duration::from_millis(200));

    let files = vec![
        share::FileToSend {
            abs_path: src_dir.path().join("a.txt"),
            rel_path: "a.txt".to_string(),
            size: 5,
        },
        share::FileToSend {
            abs_path: src_dir.path().join("sub/b.txt"),
            rel_path: "../../etc/evil.txt".to_string(), // traversal attempt
            size: 5,
        },
    ];

    let peer_addr: SocketAddr = format!("127.0.0.1:{}", share::TRANSFER_PORT).parse().unwrap();
    let (send_tx, send_rx) = crossbeam_channel::unbounded();
    let send_handle = std::thread::spawn(move || {
        share::send_files(peer_addr, "test-sender", files, CancelToken::new(), send_tx)
    });

    // Auto-accept the incoming request from the receiver side.
    let mut finished = None;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while finished.is_none() && std::time::Instant::now() < deadline {
        if let Ok(event) = recv_rx.recv_timeout(std::time::Duration::from_millis(200)) {
            match event {
                share::ReceiveEvent::IncomingRequest { manifest, respond } => {
                    assert_eq!(manifest.sender_name, "test-sender");
                    assert_eq!(manifest.files.len(), 2);
                    let _ = respond.send(true);
                }
                share::ReceiveEvent::Finished { files_received, .. } => {
                    finished = Some(files_received);
                }
                share::ReceiveEvent::Failed(msg) => panic!("receive failed: {msg}"),
                share::ReceiveEvent::Progress { .. } => {}
            }
        }
    }
    assert_eq!(finished, Some(2), "receiver never reported completion");

    send_handle.join().unwrap().unwrap();
    let _ = send_rx.try_iter().count();

    assert_eq!(fs::read_to_string(dst_dir.path().join("a.txt")).unwrap(), "hello");
    // The traversal attempt ("../../etc/evil.txt") has its ".." components
    // stripped, landing at dst_dir/etc/evil.txt — structurally impossible
    // to land anywhere outside dst_dir, since the sanitized path never
    // contains ".." for `dst_dir.join(..)` to walk back up through.
    assert_eq!(fs::read_to_string(dst_dir.path().join("etc/evil.txt")).unwrap(), "world");

    recv_cancel.cancel();
    // Nudge the listener out of accept() so the thread actually exits and
    // releases TRANSFER_PORT before the discovery sub-test binds it too.
    let _ = std::net::TcpStream::connect(peer_addr);
    recv_handle.join().unwrap();

    // --- discovery: two peers with different session IDs find each other
    // and don't discover themselves ---
    let disc_a_cancel = CancelToken::new();
    let disc_b_cancel = CancelToken::new();
    let (a_tx, a_rx) = crossbeam_channel::unbounded();
    let (b_tx, b_rx) = crossbeam_channel::unbounded();
    let a_cancel2 = disc_a_cancel.clone();
    let a_handle = std::thread::spawn(move || {
        share::run_discovery("Peer A".to_string(), [1u8; 16], a_cancel2, a_tx);
    });
    let b_cancel2 = disc_b_cancel.clone();
    let b_handle = std::thread::spawn(move || {
        share::run_discovery("Peer B".to_string(), [2u8; 16], b_cancel2, b_tx);
    });

    let mut a_found_b = false;
    let mut b_found_a = false;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while (!a_found_b || !b_found_a) && std::time::Instant::now() < deadline {
        if let Ok(share::DiscoveryEvent::PeerFound(p)) = a_rx.recv_timeout(std::time::Duration::from_millis(200)) {
            if p.name == "Peer B" {
                a_found_b = true;
            }
        }
        if let Ok(share::DiscoveryEvent::PeerFound(p)) = b_rx.recv_timeout(std::time::Duration::from_millis(200)) {
            if p.name == "Peer A" {
                b_found_a = true;
            }
        }
    }
    disc_a_cancel.cancel();
    disc_b_cancel.cancel();
    a_handle.join().unwrap();
    b_handle.join().unwrap();

    assert!(a_found_b, "Peer A never discovered Peer B");
    assert!(b_found_a, "Peer B never discovered Peer A");
}

#[test]
fn list_drives_returns_at_least_one_existing_path() {
    let drives = engine::drives::list_drives();
    assert!(!drives.is_empty());
    for d in &drives {
        assert!(d.exists(), "listed drive {} does not exist", d.display());
    }
}

#[test]
fn disk_space_reports_sane_totals_for_every_drive() {
    let drives = engine::diskusage::drive_usage();
    assert!(!drives.is_empty());
    for drive in &drives {
        let space = drive
            .space
            .unwrap_or_else(|| panic!("no usage reported for {}", drive.path.display()));
        assert!(space.total > 0, "{} reported zero total space", drive.path.display());
        assert!(
            space.free <= space.total,
            "{} reported free ({}) > total ({})",
            drive.path.display(),
            space.free,
            space.total
        );
        assert!(space.used_fraction() >= 0.0 && space.used_fraction() <= 1.0);
    }
}
