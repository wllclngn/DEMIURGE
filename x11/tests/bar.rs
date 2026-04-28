use demiurge_x11::bar::{find_completion, parse_hex, scan_path};

// parse_hex tests

#[test]
fn parse_hex_black() {
    let (r, g, b) = parse_hex("#000000");
    assert!((r - 0.0).abs() < f64::EPSILON);
    assert!((g - 0.0).abs() < f64::EPSILON);
    assert!((b - 0.0).abs() < f64::EPSILON);
}

#[test]
fn parse_hex_white() {
    let (r, g, b) = parse_hex("#ffffff");
    assert!((r - 1.0).abs() < f64::EPSILON);
    assert!((g - 1.0).abs() < f64::EPSILON);
    assert!((b - 1.0).abs() < f64::EPSILON);
}

#[test]
fn parse_hex_color() {
    let (r, g, b) = parse_hex("#4A4881");
    assert!((r - 0x4A as f64 / 255.0).abs() < f64::EPSILON);
    assert!((g - 0x48 as f64 / 255.0).abs() < f64::EPSILON);
    assert!((b - 0x81 as f64 / 255.0).abs() < f64::EPSILON);
}

#[test]
fn parse_hex_uppercase() {
    let (r, g, b) = parse_hex("#FF8800");
    assert!((r - 1.0).abs() < f64::EPSILON);
    assert!((g - 0x88 as f64 / 255.0).abs() < f64::EPSILON);
    assert!((b - 0.0).abs() < f64::EPSILON);
}

#[test]
fn parse_hex_no_hash() {
    let (r, _, _) = parse_hex("FF0000");
    assert!((r - 1.0).abs() < f64::EPSILON);
}

#[test]
fn parse_hex_short_returns_black() {
    let (r, g, b) = parse_hex("#FFF");
    assert!((r - 0.0).abs() < f64::EPSILON);
    assert!((g - 0.0).abs() < f64::EPSILON);
    assert!((b - 0.0).abs() < f64::EPSILON);
}

// scan_path tests

#[test]
fn scan_path_sorted() {
    let exes = scan_path();
    for pair in exes.windows(2) {
        assert!(pair[0] <= pair[1], "scan_path not sorted: '{}' > '{}'", pair[0], pair[1]);
    }
}

#[test]
fn scan_path_no_duplicates() {
    let exes = scan_path();
    for pair in exes.windows(2) {
        assert_ne!(pair[0], pair[1], "scan_path has duplicate: '{}'", pair[0]);
    }
}

// find_completion tests

#[test]
fn find_completion_match() {
    let exes = vec!["cat".into(), "chmod".into(), "chown".into(), "cp".into()];
    assert_eq!(find_completion(&exes, "ch"), Some("chmod".into()));
}

#[test]
fn find_completion_exact() {
    let exes = vec!["cat".into(), "chmod".into(), "cp".into()];
    assert_eq!(find_completion(&exes, "cat"), Some("cat".into()));
}

#[test]
fn find_completion_no_match() {
    let exes = vec!["cat".into(), "cp".into()];
    assert_eq!(find_completion(&exes, "zz"), None);
}

#[test]
fn find_completion_empty_input() {
    let exes = vec!["cat".into()];
    assert_eq!(find_completion(&exes, ""), None);
}

#[test]
fn find_completion_empty_list() {
    let exes: Vec<String> = vec![];
    assert_eq!(find_completion(&exes, "cat"), None);
}
