// Tests for torrentius rendering primitives. Surface creation and PNG capture
// need a Cairo allocator (fine) and an X server (not fine), respectively --
// screenshot paths are left to manual testing via demiurge's Screenshot
// action in a live session.

use demiurge_x11::torrentius::{hex_to_pixel, make_font_options, new_surface, parse_hex};

#[test]
fn parse_hex_black() {
    let (r, g, b) = parse_hex("#000000");
    assert_eq!(r, 0.0);
    assert_eq!(g, 0.0);
    assert_eq!(b, 0.0);
}

#[test]
fn parse_hex_white() {
    let (r, g, b) = parse_hex("#ffffff");
    assert!((r - 1.0).abs() < 1e-9);
    assert!((g - 1.0).abs() < 1e-9);
    assert!((b - 1.0).abs() < 1e-9);
}

#[test]
fn parse_hex_no_leading_hash() {
    let (r, _, _) = parse_hex("ff0000");
    assert!((r - 1.0).abs() < 1e-9);
}

#[test]
fn parse_hex_uppercase_accepted() {
    let (r, g, b) = parse_hex("#FF8800");
    assert!((r - 1.0).abs() < 1e-9);
    assert!((g - (0x88 as f64 / 255.0)).abs() < 1e-9);
    assert_eq!(b, 0.0);
}

#[test]
fn parse_hex_short_input_falls_back_to_black() {
    let (r, g, b) = parse_hex("#abc");
    assert_eq!(r, 0.0);
    assert_eq!(g, 0.0);
    assert_eq!(b, 0.0);
}

#[test]
fn hex_to_pixel_standard() {
    assert_eq!(hex_to_pixel("#121212"), 0x121212);
    assert_eq!(hex_to_pixel("#ffffff"), 0xffffff);
    assert_eq!(hex_to_pixel("000000"), 0x0);
}

#[test]
fn hex_to_pixel_invalid_returns_zero() {
    assert_eq!(hex_to_pixel("nope"), 0);
}

#[test]
fn font_options_have_subpixel_rgb_slight_on() {
    let opts = make_font_options().expect("font options should construct");
    assert_eq!(opts.antialias(), cairo::Antialias::Subpixel);
    assert_eq!(opts.subpixel_order(), cairo::SubpixelOrder::Rgb);
    assert_eq!(opts.hint_style(), cairo::HintStyle::Slight);
    assert_eq!(opts.hint_metrics(), cairo::HintMetrics::On);
}

#[test]
fn new_surface_yields_rgb24_surface_at_requested_size() {
    let (surface, _cr) = new_surface(400, 30).expect("surface should allocate");
    assert_eq!(surface.format(), cairo::Format::Rgb24);
    assert_eq!(surface.width(), 400);
    assert_eq!(surface.height(), 30);
}

