// The XSETTINGS blob: Header framing, setting count and theme names.

use demiurge_x11::config::Config;
use demiurge_x11::xsettings::serialize;

// Decode just enough of the blob header to assert the framing is
// well-formed: byte order, serial, count.
#[test]
fn blob_header_is_well_formed() {
    let cfg = Config::default();
    let blob = serialize(&cfg, 7);
    assert_eq!(blob[0], 1, "LSBFirst byte order");
    assert_eq!(&blob[1..4], &[0, 0, 0], "header pad");
    let serial = u32::from_le_bytes([blob[4], blob[5], blob[6], blob[7]]);
    assert_eq!(serial, 7);
    let n = u32::from_le_bytes([blob[8], blob[9], blob[10], blob[11]]);
    // Defaults: font + cursor theme + cursor size + accent color = 4
    // (no gtk_theme / icon_theme by default).
    assert_eq!(n, 4);
    // Whole blob stays 4-byte aligned.
    assert_eq!(blob.len() % 4, 0);
}

#[test]
fn theme_names_appear_when_set() {
    let mut cfg = Config::default();
    cfg.appearance.gtk_theme = "Adwaita-dark".into();
    cfg.appearance.icon_theme = "Papirus-Dark".into();
    let blob = serialize(&cfg, 0);
    let n = u32::from_le_bytes([blob[8], blob[9], blob[10], blob[11]]);
    assert_eq!(n, 6, "font + cursor name + cursor size + accent + 2 themes");
    // The theme name string is present somewhere in the blob.
    let haystack = String::from_utf8_lossy(&blob);
    assert!(haystack.contains("Net/ThemeName"));
    assert!(haystack.contains("Adwaita-dark"));
}
