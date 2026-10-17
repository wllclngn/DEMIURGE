// Output modes follow the panel: The tracker's decisions as pure cases, then
// the WM end to end in a resizeable Xephyr (nested in its own Xvfb), where a
// test client sets modes the way a game or `xrandr -s` does.

use std::process::Command;
use std::time::{Duration, Instant};

use demiurge_x11::modes::{ModeTracker, OutputMode, SetMode, GRACE};
use x11rb::connection::Connection;
use x11rb::protocol::xproto::*;
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as _;

mod common;
use common::wm::{minimal_config, WmTest};

const PBP: u32 = 0x1c1;
const SINGLE: u32 = 0x2a0;
const GAME: u32 = 0x1c5;

fn out(mode: u32, preferred: Option<u32>, modes: &[u32]) -> Vec<OutputMode> {
    vec![OutputMode { name: "HDMI-0".into(), mode, preferred, modes: modes.to_vec() }]
}

fn set(mode: u32) -> Vec<SetMode> {
    vec![SetMode { output: "HDMI-0".into(), mode }]
}

const NONE_PINNED: fn(&str) -> bool = |_| false;

#[test]
fn a_new_mode_list_takes_the_preferred_mode() {
    // The DualUp leaving PBP: The list changes, X keeps the old mode.
    let mut t = ModeTracker::new(&out(PBP, Some(PBP), &[PBP, GAME]));
    let now = Instant::now();
    let sets = t.on_outputs(&out(PBP, Some(SINGLE), &[SINGLE, PBP, GAME]), false, NONE_PINNED, now);
    assert_eq!(sets, set(SINGLE));
    // The mode set's own notify arrives: Nothing more to do.
    let sets = t.on_outputs(&out(SINGLE, Some(SINGLE), &[SINGLE, PBP, GAME]), false, NONE_PINNED, now);
    assert!(sets.is_empty());
}

#[test]
fn a_pinned_output_keeps_its_mode_through_a_list_change() {
    let mut t = ModeTracker::new(&out(PBP, Some(PBP), &[PBP]));
    let sets = t.on_outputs(&out(PBP, Some(SINGLE), &[SINGLE, PBP]), false, |n| n == "HDMI-0", Instant::now());
    assert!(sets.is_empty());
}

#[test]
fn a_mode_left_by_a_fullscreen_client_is_restored() {
    let modes = [PBP, GAME];
    let mut t = ModeTracker::new(&out(PBP, Some(PBP), &modes));
    let now = Instant::now();
    t.on_fullscreen_appeared(now);
    assert!(t.on_outputs(&out(GAME, Some(PBP), &modes), true, NONE_PINNED, now).is_empty());
    assert_eq!(t.on_fullscreen_gone(&out(GAME, Some(PBP), &modes), NONE_PINNED), set(PBP));
}

#[test]
fn a_mode_set_just_before_the_fullscreen_window_maps_is_claimed() {
    let modes = [PBP, GAME];
    let mut t = ModeTracker::new(&out(PBP, Some(PBP), &modes));
    let now = Instant::now();
    t.on_outputs(&out(GAME, Some(PBP), &modes), false, NONE_PINNED, now);
    t.on_fullscreen_appeared(now + GRACE / 2);
    assert_eq!(t.on_fullscreen_gone(&out(GAME, Some(PBP), &modes), NONE_PINNED), set(PBP));
}

#[test]
fn a_hand_set_mode_is_never_undone() {
    let modes = [PBP, GAME];
    let mut t = ModeTracker::new(&out(PBP, Some(PBP), &modes));
    let now = Instant::now();
    assert!(t.on_outputs(&out(GAME, Some(PBP), &modes), false, NONE_PINNED, now).is_empty());
    // A fullscreen client long after the change does not claim it.
    t.on_fullscreen_appeared(now + GRACE * 3);
    assert!(t.on_fullscreen_gone(&out(GAME, Some(PBP), &modes), NONE_PINNED).is_empty());
}

#[test]
fn a_client_that_restores_its_own_mode_needs_nothing() {
    let modes = [PBP, GAME];
    let mut t = ModeTracker::new(&out(PBP, Some(PBP), &modes));
    let now = Instant::now();
    t.on_fullscreen_appeared(now);
    t.on_outputs(&out(GAME, Some(PBP), &modes), true, NONE_PINNED, now);
    t.on_outputs(&out(PBP, Some(PBP), &modes), true, NONE_PINNED, now);
    assert!(t.on_fullscreen_gone(&out(PBP, Some(PBP), &modes), NONE_PINNED).is_empty());
}

// DOOM on archd: The game set 1920x1080 five seconds before its window went
// fullscreen (past GRACE, so it read as hand-set), then restored 2560x1440
// itself while fullscreen. Closing it must leave 2560x1440 alone; restoring
// to the "baseline" switched the panel back into the game's mode.
#[test]
fn a_game_that_restores_itself_late_is_left_alone() {
    let modes = [PBP, GAME];
    let mut t = ModeTracker::new(&out(PBP, Some(PBP), &modes));
    let start = Instant::now();
    assert!(t.on_outputs(&out(GAME, Some(PBP), &modes), false, NONE_PINNED, start).is_empty());
    t.on_fullscreen_appeared(start + GRACE * 3);
    assert!(t.on_outputs(&out(PBP, Some(PBP), &modes), true, NONE_PINNED, start + GRACE * 4).is_empty());
    assert!(t.on_fullscreen_gone(&out(PBP, Some(PBP), &modes), NONE_PINNED).is_empty());
}

#[test]
fn a_pinned_output_returns_to_its_pin_not_the_preferred_mode() {
    let modes = [PBP, SINGLE, GAME];
    let pinned = |n: &str| n == "HDMI-0";
    let mut t = ModeTracker::new(&out(PBP, Some(SINGLE), &modes));
    let now = Instant::now();
    t.on_fullscreen_appeared(now);
    t.on_outputs(&out(GAME, Some(SINGLE), &modes), true, pinned, now);
    assert_eq!(t.on_fullscreen_gone(&out(GAME, Some(SINGLE), &modes), pinned), set(PBP));
}

#[test]
fn a_new_output_is_recorded_not_changed() {
    let mut t = ModeTracker::new(&[]);
    assert!(t.on_outputs(&out(GAME, Some(PBP), &[PBP, GAME]), false, NONE_PINNED, Instant::now()).is_empty());
}

// End to end.

const WAIT: Duration = Duration::from_secs(3);

fn screen_size(wm: &WmTest) -> (u16, u16) {
    let g = wm.conn.get_geometry(wm.root).unwrap().reply().unwrap();
    (g.width, g.height)
}

fn xrandr_size(wm: &WmTest, size: &str) {
    let ok = Command::new("xrandr").args(["-display", &wm.display, "-s", size]).status().unwrap().success();
    assert!(ok, "xrandr -s {size}");
}

// A fullscreen window on its own connection, as a game holds one. Dropping
// the connection destroys it the way the server cleans up a killed client.
fn fullscreen_client(wm: &WmTest) -> RustConnection {
    let (conn, screen) = RustConnection::connect(Some(&wm.display)).unwrap();
    let root = conn.setup().roots[screen].root;
    let win = conn.generate_id().unwrap();
    conn.create_window(0, win, root, 0, 0, 800, 600, 0, WindowClass::INPUT_OUTPUT, 0, &CreateWindowAux::new())
        .unwrap();
    let state = conn.intern_atom(false, b"_NET_WM_STATE").unwrap().reply().unwrap().atom;
    let fs = conn.intern_atom(false, b"_NET_WM_STATE_FULLSCREEN").unwrap().reply().unwrap().atom;
    conn.change_property32(PropMode::REPLACE, win, state, AtomEnum::ATOM, &[fs]).unwrap();
    conn.map_window(win).unwrap();
    conn.flush().unwrap();
    assert!(wm.wait_for(|t| t.client_list().contains(&win), WAIT));
    conn
}

#[test]
fn a_killed_fullscreen_client_gets_its_mode_undone() {
    let mut wm = WmTest::new_resizeable();
    wm.start_demiurge(minimal_config());
    assert_eq!(screen_size(&wm), (1280, 720));

    let game = fullscreen_client(&wm);
    xrandr_size(&wm, "800x600");
    assert!(wm.wait_for(|t| screen_size(t) == (800, 600), WAIT));
    std::thread::sleep(Duration::from_millis(300));

    drop(game);
    assert!(
        wm.wait_for(|t| screen_size(t) == (1280, 720), WAIT),
        "mode left at {:?}",
        screen_size(&wm)
    );
}

#[test]
fn a_mode_set_before_the_window_maps_is_undone_too() {
    let mut wm = WmTest::new_resizeable();
    wm.start_demiurge(minimal_config());

    xrandr_size(&wm, "800x600");
    let game = fullscreen_client(&wm);
    assert!(wm.wait_for(|t| screen_size(t) == (800, 600), WAIT));
    std::thread::sleep(Duration::from_millis(300));

    drop(game);
    assert!(wm.wait_for(|t| screen_size(t) == (1280, 720), WAIT), "mode left at {:?}", screen_size(&wm));
}

#[test]
fn a_hand_set_mode_stays() {
    let mut wm = WmTest::new_resizeable();
    wm.start_demiurge(minimal_config());

    xrandr_size(&wm, "1024x768");
    assert!(wm.wait_for(|t| screen_size(t) == (1024, 768), WAIT));
    // GRACE passes, then a fullscreen client comes and goes.
    std::thread::sleep(GRACE + Duration::from_millis(500));
    drop(fullscreen_client(&wm));
    std::thread::sleep(Duration::from_millis(800));
    assert_eq!(screen_size(&wm), (1024, 768));
}

#[test]
fn tiles_follow_a_mode_change() {
    let mut wm = WmTest::new_resizeable();
    wm.start_demiurge(&minimal_config().replace("default_layout = \"floating\"", "default_layout = \"tile\""));
    let a = wm.spawn_window(300, 200);
    let b = wm.spawn_window(300, 200);
    assert!(wm.wait_for(|t| t.client_list().len() == 2, WAIT));

    xrandr_size(&wm, "1024x768");
    let inside = |t: &WmTest| {
        [a, b].iter().all(|&w| {
            let g = t.conn.get_geometry(w).unwrap().reply().unwrap();
            g.x as i32 + g.width as i32 <= 1024 && g.y as i32 + g.height as i32 <= 768
        })
    };
    let right_edge = |t: &WmTest| {
        [a, b]
            .iter()
            .map(|&w| {
                let g = t.conn.get_geometry(w).unwrap().reply().unwrap();
                g.x as i32 + g.width as i32
            })
            .max()
            .unwrap()
    };
    assert!(wm.wait_for(inside, WAIT), "tiles left outside 1024x768");
    assert!(right_edge(&wm) > 900, "tiles shrank but no longer fill the output");
}
