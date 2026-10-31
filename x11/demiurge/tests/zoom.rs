// Zoom: Super+= fills the work area below the bar with the focused window,
// Super+- puts it back. A tiled window floats out and returns to its tile,
// an application's maximize request is the same zoom, a drag ends it, and a
// zoomed window follows a mode change.

use std::process::Command;
use std::time::Duration;

use x11rb::connection::Connection;
use x11rb::protocol::xproto::*;

mod common;
use common::wm::WmTest;

const WAIT: Duration = Duration::from_secs(2);
const WORK_AREA: (i16, i16, u16, u16) = (0, 24, 1280, 696);

fn config(layout: &str) -> String {
    format!(
        r#"
[general]
tags = ["A", "B", "C"]
default_layout = "{layout}"

[bar]
height = 24
font = "Noto Sans 9"

[startup]
commands = []

[[keybind]]
mods = ["Super"]
key = "equal"
action = "zoom_in"

[[keybind]]
mods = ["Super"]
key = "minus"
action = "zoom_out"
"#
    )
}

fn geometry(wm: &WmTest, win: Window) -> (i16, i16, u16, u16) {
    let g = wm.conn.get_geometry(win).unwrap().reply().unwrap();
    (g.x, g.y, g.width, g.height)
}

fn atom(wm: &WmTest, name: &[u8]) -> Atom {
    wm.conn.intern_atom(false, name).unwrap().reply().unwrap().atom
}

fn maximized(wm: &WmTest, win: Window) -> bool {
    let state = atom(wm, b"_NET_WM_STATE");
    let reply = wm.conn.get_property(false, win, state, AtomEnum::ATOM, 0, 32).unwrap().reply().unwrap();
    let atoms: Vec<Atom> = reply.value32().map(|v| v.collect()).unwrap_or_default();
    atoms.contains(&atom(wm, b"_NET_WM_STATE_MAXIMIZED_VERT")) && atoms.contains(&atom(wm, b"_NET_WM_STATE_MAXIMIZED_HORZ"))
}

fn super_key(wm: &WmTest, keysym: u32) {
    let (super_l, key) = (wm.keycode(0xffeb), wm.keycode(keysym));
    wm.fake(KEY_PRESS_EVENT, super_l, 0, 0);
    wm.fake(KEY_PRESS_EVENT, key, 0, 0);
    wm.fake(KEY_RELEASE_EVENT, key, 0, 0);
    wm.fake(KEY_RELEASE_EVENT, super_l, 0, 0);
}

const EQUAL: u32 = 0x003d;
const MINUS: u32 = 0x002d;

fn focused_window(wm: &WmTest, w: u16, h: u16) -> Window {
    let win = wm.spawn_window(w, h);
    assert!(wm.wait_for(|t| t.active_window() == Some(win), WAIT), "the new window never took focus");
    win
}

#[test]
fn super_equal_fills_the_work_area_and_super_minus_puts_it_back() {
    let mut wm = WmTest::new();
    wm.start_demiurge(&config("floating"));
    let win = focused_window(&wm, 300, 200);
    let before = geometry(&wm, win);

    super_key(&wm, EQUAL);
    assert!(wm.wait_for(|t| geometry(t, win) == WORK_AREA, WAIT), "zoomed to {:?}", geometry(&wm, win));
    assert!(maximized(&wm, win), "a zoomed window is not advertised as maximized");

    super_key(&wm, MINUS);
    assert!(wm.wait_for(|t| geometry(t, win) == before, WAIT), "restored to {:?}", geometry(&wm, win));
    assert!(!maximized(&wm, win));
}

#[test]
fn a_tiled_window_zooms_out_into_its_tile() {
    let mut wm = WmTest::new();
    wm.start_demiurge(&config("tile"));
    let first = focused_window(&wm, 300, 200);
    let second = focused_window(&wm, 300, 200);
    assert!(wm.wait_for(|t| geometry(t, first) != geometry(t, second), WAIT));
    let tiles = (geometry(&wm, first), geometry(&wm, second));

    super_key(&wm, EQUAL);
    assert!(wm.wait_for(|t| geometry(t, second) == WORK_AREA, WAIT), "zoomed to {:?}", geometry(&wm, second));

    super_key(&wm, MINUS);
    assert!(
        wm.wait_for(|t| (geometry(t, first), geometry(t, second)) == tiles, WAIT),
        "tiles {:?} became {:?}",
        tiles,
        (geometry(&wm, first), geometry(&wm, second))
    );
}

#[test]
fn an_applications_maximize_request_zooms() {
    let mut wm = WmTest::new();
    wm.start_demiurge(&config("floating"));
    let win = focused_window(&wm, 300, 200);
    let before = geometry(&wm, win);
    let (vert, horz) = (atom(&wm, b"_NET_WM_STATE_MAXIMIZED_VERT"), atom(&wm, b"_NET_WM_STATE_MAXIMIZED_HORZ"));
    let request = |action: u32| {
        let msg = ClientMessageEvent::new(32, win, atom(&wm, b"_NET_WM_STATE"), [action, vert, horz, 1, 0]);
        let mask = EventMask::SUBSTRUCTURE_REDIRECT | EventMask::SUBSTRUCTURE_NOTIFY;
        wm.conn.send_event(false, wm.root, mask, msg).unwrap();
        wm.conn.flush().unwrap();
    };

    request(1);
    assert!(wm.wait_for(|t| geometry(t, win) == WORK_AREA && maximized(t, win), WAIT));
    request(0);
    assert!(wm.wait_for(|t| geometry(t, win) == before && !maximized(t, win), WAIT));
}

#[test]
fn a_drag_ends_the_zoom() {
    let mut wm = WmTest::new();
    wm.start_demiurge(&config("floating"));
    let win = focused_window(&wm, 300, 200);
    super_key(&wm, EQUAL);
    assert!(wm.wait_for(|t| geometry(t, win) == WORK_AREA, WAIT));

    let super_l = wm.keycode(0xffeb);
    wm.fake(MOTION_NOTIFY_EVENT, 0, 640, 360);
    wm.fake(KEY_PRESS_EVENT, super_l, 0, 0);
    wm.fake(BUTTON_PRESS_EVENT, 1, 0, 0);
    std::thread::sleep(Duration::from_millis(100));
    wm.fake(MOTION_NOTIFY_EVENT, 0, 690, 400);
    assert!(wm.wait_for(|t| geometry(t, win) == (50, 64, 1280, 696), WAIT), "dragged to {:?}", geometry(&wm, win));
    wm.fake(BUTTON_RELEASE_EVENT, 1, 0, 0);
    wm.fake(KEY_RELEASE_EVENT, super_l, 0, 0);
    assert!(wm.wait_for(|t| !maximized(t, win), WAIT), "still advertised as maximized after the drag");

    super_key(&wm, MINUS);
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(geometry(&wm, win), (50, 64, 1280, 696), "Super+- after a drag moved the window");
}

#[test]
fn a_zoomed_window_follows_a_mode_change() {
    let mut wm = WmTest::new_resizeable();
    wm.start_demiurge(&config("floating"));
    let win = focused_window(&wm, 300, 200);
    super_key(&wm, EQUAL);
    assert!(wm.wait_for(|t| geometry(t, win) == WORK_AREA, WAIT));

    let ok = Command::new("xrandr").args(["-display", &wm.display, "-s", "1024x768"]).status().unwrap().success();
    assert!(ok, "xrandr -s 1024x768");
    assert!(
        wm.wait_for(|t| geometry(t, win) == (0, 24, 1024, 744), WAIT),
        "after the mode change {:?}",
        geometry(&wm, win)
    );
}
