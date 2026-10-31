use std::time::{Duration, Instant};

use demiurge_x11::bar::parse_hex;
use x11rb::protocol::xproto::{ConnectionExt as _, ImageFormat};

mod common;
use common::wm::WmTest;

const BAR_HEIGHT: u16 = 24;
const RED: (u8, u8, u8) = (255, 0, 0);

// A red bar whose empty tags are blue and occupied tags green, its active
// tag black on white so nothing else on it is green.
const BAR_CONFIG: &str = r##"
[general]
tags = ["A", "B", "C"]
default_layout = "floating"

[bar]
height = 24
font = "Noto Sans 9"
bg = "#ff0000"
clock_format = "%H:%M:%S"
tag_focused_bg = "#ffffff"
tag_focused_fg = "#000000"
tag_empty_fg = "#0000ff"
tag_occupied_fg = "#00ff00"

[startup]
commands = []
"##;

// The screen's pixels over `x..x + w` of the bar, row by row, as RGB.
fn bar_pixels(wm: &WmTest, x: i16, w: u16) -> Vec<(u8, u8, u8)> {
    let image = wm
        .conn
        .get_image(ImageFormat::Z_PIXMAP, wm.root, x, 0, w, BAR_HEIGHT, !0)
        .expect("get_image")
        .reply()
        .expect("get_image reply");
    image.data.chunks_exact(4).map(|p| (p[2], p[1], p[0])).collect()
}

// Xephyr's screen is 1280 wide; the clock keeps the bar's last 400 px.
const CLOCK_X: i16 = 1280 - 400;

#[test]
fn the_clock_is_put_every_second_and_nothing_else_is() {
    let mut wm = WmTest::new();
    wm.start_demiurge(BAR_CONFIG);
    std::thread::sleep(Duration::from_millis(500));
    let rest = bar_pixels(&wm, 0, CLOCK_X as u16);
    let mut clock = bar_pixels(&wm, CLOCK_X, 400);
    let (mut changes, mut last_change) = (0, Instant::now());
    let mut longest = Duration::ZERO;
    let end = Instant::now() + Duration::from_millis(4500);
    while Instant::now() < end {
        std::thread::sleep(Duration::from_millis(100));
        let now = bar_pixels(&wm, CLOCK_X, 400);
        // The text's rows only: The clock's region keeps the bar's red
        // above and below them.
        let edges = now.chunks_exact(400).enumerate().filter(|(y, _)| *y < 3 || *y >= BAR_HEIGHT as usize - 3);
        assert!(edges.flat_map(|(_, row)| row).all(|&p| p == RED), "the clock's region holds more than the clock");
        if now != clock {
            changes += 1;
            longest = longest.max(last_change.elapsed());
            last_change = Instant::now();
            clock = now;
        }
    }
    assert!(changes >= 4, "the clock changed {changes} times in 4.5 s");
    assert!(longest < Duration::from_millis(1500), "the clock went {longest:?} without a change");
    assert_eq!(bar_pixels(&wm, 0, CLOCK_X as u16), rest, "the bar changed outside its clock");
}

#[test]
fn a_window_moved_to_another_tag_marks_it_at_once() {
    let mut wm = WmTest::new();
    wm.start_demiurge(BAR_CONFIG);
    let window = wm.spawn_window(200, 200);
    assert!(wm.wait_for(|w| w.client_list().contains(&window), Duration::from_secs(2)));
    let occupied_drawn = |wm: &WmTest| bar_pixels(wm, 0, 400).iter().any(|&(r, g, b)| g > r && g > b);
    std::thread::sleep(Duration::from_millis(300));
    assert!(!occupied_drawn(&wm), "a tag was drawn occupied before any window left the active one");
    wm.request_move_to_tag(window, 1);
    assert!(wm.wait_for(occupied_drawn, Duration::from_secs(1)), "tag B was not drawn occupied once its window arrived");
}

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
