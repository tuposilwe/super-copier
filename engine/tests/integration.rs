use std::fs;
use std::path::Path;

use engine::copy::{self, CopyOptions, OverwritePolicy};
use engine::duplicates::{self, DupOptions};
use engine::fsops::{self, OrganizeStrategy};
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
