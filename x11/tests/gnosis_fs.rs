// GNOSIS file operations: Ordering, directory reads, URL encoding, copies
// and folder previews.

use std::path::{Path, PathBuf};

use demiurge_x11::gnosis::fs::*;

fn e(name: &str, is_dir: bool) -> Entry {
    Entry {
        name: name.into(),
        path: PathBuf::from(name),
        is_dir,
        is_symlink: false,
    }
}

#[test]
fn dirs_before_files_then_alpha() {
    let mut v = vec![
        e("zebra.txt", false),
        e("Apple", true),
        e("banana.txt", false),
        e("cherry", true),
    ];
    sort(&mut v);
    let order: Vec<&str> = v.iter().map(|x| x.name.as_str()).collect();
    assert_eq!(order, vec!["Apple", "cherry", "banana.txt", "zebra.txt"]);
}

#[test]
fn reads_a_real_dir() {
    // The root directory always holds usr or etc.
    let v = read_dir(Path::new("/"), false);
    assert!(v.iter().any(|e| e.name == "usr" || e.name == "etc"));
}

#[test]
fn url_encode_decode_roundtrip() {
    let p = "/home/mod/My Files/résumé (final).txt";
    let enc = url_encode(p);
    assert!(!enc.contains(' '), "spaces must be percent-encoded");
    assert!(enc.contains('/'), "slashes stay literal");
    assert_eq!(url_decode(&enc), p);
}

#[test]
fn copy_into_file_and_dir() {
    let tmp = tempfile::tempdir().unwrap();
    let src_dir = tmp.path().join("src");
    let dst_dir = tmp.path().join("dst");
    std::fs::create_dir_all(src_dir.join("inner")).unwrap();
    std::fs::create_dir_all(&dst_dir).unwrap();
    std::fs::write(src_dir.join("a.txt"), b"hi").unwrap();
    std::fs::write(src_dir.join("inner/b.txt"), b"yo").unwrap();

    copy_into(&src_dir, &dst_dir).unwrap();
    assert!(dst_dir.join("src/a.txt").exists());
    assert!(dst_dir.join("src/inner/b.txt").exists());
    assert_eq!(std::fs::read(dst_dir.join("src/a.txt")).unwrap(), b"hi");
}

#[test]
fn peek_samples_and_counts() {
    // /usr has subdirs (bin, lib, ...) -- peek should return cards
    // with at least one dir, and a positive total.
    let (cards, total) = peek(Path::new("/usr"), 4);
    assert!(total > 0);
    assert!(cards.len() <= 4);
    assert!(cards.iter().any(|&d| d), "expected a subdir in /usr preview");
}
