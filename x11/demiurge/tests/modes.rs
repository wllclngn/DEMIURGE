// Output modes follow the panel: The tracker's decisions as pure cases, then
// the WM end to end in a resizeable Xephyr (nested in its own Xvfb), where a
// test client sets modes the way a game or `xrandr -s` does.

use std::process::Command;
use std::time::Duration;

use demiurge_x11::modes::{ModeTracker, OutputMode, SetMode};
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
    vec![output("HDMI-0", mode, preferred, modes, true)]
}

fn output(name: &str, mode: u32, preferred: Option<u32>, modes: &[u32], connected: bool) -> OutputMode {
    OutputMode { name: name.into(), mode, preferred, modes: modes.to_vec(), connected }
}

fn set(mode: u32) -> Vec<SetMode> {
    vec![SetMode { output: "HDMI-0".into(), mode }]
}

const NONE_PINNED: fn(&str) -> bool = |_| false;

#[test]
fn a_new_mode_list_takes_the_preferred_mode() {
    // The DualUp leaving PBP: The list changes, X keeps the old mode.
    let mut t = ModeTracker::new(&out(PBP, Some(PBP), &[PBP, GAME]));
    let sets = t.on_outputs(&out(PBP, Some(SINGLE), &[SINGLE, PBP, GAME]), false, NONE_PINNED);
    assert_eq!(sets, set(SINGLE));
    // The mode set's own notify arrives: Nothing more to do.
    let sets = t.on_outputs(&out(SINGLE, Some(SINGLE), &[SINGLE, PBP, GAME]), false, NONE_PINNED);
    assert!(sets.is_empty());
}

#[test]
fn a_pinned_output_keeps_its_mode_through_a_list_change() {
    let mut t = ModeTracker::new(&out(PBP, Some(PBP), &[PBP]));
    let sets = t.on_outputs(&out(PBP, Some(SINGLE), &[SINGLE, PBP]), false, |n| n == "HDMI-0");
    assert!(sets.is_empty());
}

#[test]
fn a_mode_left_by_a_fullscreen_client_is_restored() {
    let modes = [PBP, GAME];
    let mut t = ModeTracker::new(&out(PBP, Some(PBP), &modes));
    t.on_fullscreen_appeared();
    assert!(t.on_outputs(&out(GAME, Some(PBP), &modes), true, NONE_PINNED).is_empty());
    assert_eq!(t.on_fullscreen_gone(&out(GAME, Some(PBP), &modes), NONE_PINNED), set(PBP));
}

// DOOM on archd: The game set 1920x1080 five seconds before its window went
// fullscreen, and was then killed. However long before, the change is the
// fullscreen window's while the output still sits on it.
#[test]
fn a_mode_set_before_the_fullscreen_window_maps_is_claimed() {
    let modes = [PBP, GAME];
    let mut t = ModeTracker::new(&out(PBP, Some(PBP), &modes));
    assert!(t.on_outputs(&out(GAME, Some(PBP), &modes), false, NONE_PINNED).is_empty());
    t.on_fullscreen_appeared();
    assert_eq!(t.on_fullscreen_gone(&out(GAME, Some(PBP), &modes), NONE_PINNED), set(PBP));
}

// With no fullscreen window around, a mode changed and changed back by hand
// leaves nothing for a later fullscreen window to claim, even on an output
// with no preferred mode to fall back on.
#[test]
fn a_mode_changed_back_by_hand_leaves_nothing_to_claim() {
    let modes = [PBP, GAME];
    let mut t = ModeTracker::new(&out(PBP, None, &modes));
    t.on_outputs(&out(GAME, None, &modes), false, NONE_PINNED);
    t.on_outputs(&out(PBP, None, &modes), false, NONE_PINNED);
    t.on_fullscreen_appeared();
    assert!(t.on_fullscreen_gone(&out(PBP, None, &modes), NONE_PINNED).is_empty());
}

#[test]
fn a_hand_set_mode_stays_while_no_fullscreen_window_comes() {
    let modes = [PBP, GAME];
    let mut t = ModeTracker::new(&out(PBP, Some(PBP), &modes));
    assert!(t.on_outputs(&out(GAME, Some(PBP), &modes), false, NONE_PINNED).is_empty());
    assert!(t.on_fullscreen_gone(&out(GAME, Some(PBP), &modes), NONE_PINNED).is_empty());
}

#[test]
fn a_client_that_restores_its_own_mode_needs_nothing() {
    let modes = [PBP, GAME];
    let mut t = ModeTracker::new(&out(PBP, Some(PBP), &modes));
    t.on_fullscreen_appeared();
    t.on_outputs(&out(GAME, Some(PBP), &modes), true, NONE_PINNED);
    t.on_outputs(&out(PBP, Some(PBP), &modes), true, NONE_PINNED);
    assert!(t.on_fullscreen_gone(&out(PBP, Some(PBP), &modes), NONE_PINNED).is_empty());
}

// DOOM, launched and quit normally: It set its mode before its window, then
// restored 2560x1440 itself while fullscreen. Closing it must leave
// 2560x1440 alone; restoring to an earlier "baseline" would switch the
// panel back into the game's mode.
#[test]
fn a_game_that_restores_itself_is_left_alone() {
    let modes = [PBP, GAME];
    let mut t = ModeTracker::new(&out(PBP, Some(PBP), &modes));
    assert!(t.on_outputs(&out(GAME, Some(PBP), &modes), false, NONE_PINNED).is_empty());
    t.on_fullscreen_appeared();
    assert!(t.on_outputs(&out(PBP, Some(PBP), &modes), true, NONE_PINNED).is_empty());
    assert!(t.on_fullscreen_gone(&out(PBP, Some(PBP), &modes), NONE_PINNED).is_empty());
}

#[test]
fn a_pinned_output_returns_to_its_pin_not_the_preferred_mode() {
    let modes = [PBP, SINGLE, GAME];
    let pinned = |n: &str| n == "HDMI-0";
    let mut t = ModeTracker::new(&out(PBP, Some(SINGLE), &modes));
    t.on_fullscreen_appeared();
    t.on_outputs(&out(GAME, Some(SINGLE), &modes), true, pinned);
    assert_eq!(t.on_fullscreen_gone(&out(GAME, Some(SINGLE), &modes), pinned), set(PBP));
}

#[test]
fn a_new_output_is_recorded_not_changed() {
    let mut t = ModeTracker::new(&[]);
    assert!(t.on_outputs(&out(GAME, Some(PBP), &[PBP, GAME]), false, NONE_PINNED).is_empty());
}

// archd's monitor swap: The 1440p panel on HDMI-0 pulled, an AW2521HF
// plugged into DP-4. X left DP-4 dark and HDMI-0 holding its CRTC.
const QHD: u32 = 0x1c1;
const FHD240: u32 = 0x1d0;

#[test]
fn a_monitor_swap_turns_the_pulled_one_off_and_lights_the_new_one() {
    let mut t = ModeTracker::new(&out(QHD, Some(QHD), &[QHD]));
    let swapped = [output("DP-4", 0, Some(FHD240), &[FHD240, PBP], true), output("HDMI-0", QHD, Some(QHD), &[QHD], false)];
    let sets = t.on_outputs(&swapped, false, NONE_PINNED);
    assert_eq!(
        sets,
        vec![SetMode { output: "HDMI-0".into(), mode: 0 }, SetMode { output: "DP-4".into(), mode: FHD240 }],
        "the pulled output goes first, then the new one at its desktop mode"
    );
    // The sets' own notifies: DP-4 lit, HDMI-0 gone.
    let after = [output("DP-4", FHD240, Some(FHD240), &[FHD240, PBP], true)];
    assert!(t.on_outputs(&after, false, NONE_PINNED).is_empty());
}

#[test]
fn an_output_turned_off_by_hand_stays_off() {
    let modes = [FHD240, PBP];
    let mut t = ModeTracker::new(&[output("DP-4", FHD240, Some(FHD240), &modes, true)]);
    assert!(t.on_outputs(&[output("DP-4", 0, Some(FHD240), &modes, true)], false, NONE_PINNED).is_empty());
    // Nor does a game's end bring it back.
    t.on_fullscreen_appeared();
    assert!(t.on_fullscreen_gone(&[output("DP-4", 0, Some(FHD240), &modes, true)], NONE_PINNED).is_empty());
}

#[test]
fn an_output_dark_when_demiurge_starts_stays_dark() {
    let dark = [output("DP-4", 0, Some(FHD240), &[FHD240], true)];
    let mut t = ModeTracker::new(&dark);
    assert!(t.on_outputs(&dark, false, NONE_PINNED).is_empty());
}

#[test]
fn a_monitor_unplugged_and_plugged_back_is_lit_again() {
    let modes = [FHD240, PBP];
    let mut t = ModeTracker::new(&[output("DP-4", FHD240, Some(FHD240), &modes, true)]);
    // Unplugged dark (X freed its CRTC): It drops out of the list.
    assert!(t.on_outputs(&[], false, NONE_PINNED).is_empty());
    let back = [output("DP-4", 0, Some(FHD240), &modes, true)];
    assert_eq!(t.on_outputs(&back, false, NONE_PINNED), vec![SetMode { output: "DP-4".into(), mode: FHD240 }]);
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

// A screen-sized window, as DOOM opens one: No fullscreen state, its size
// that of the output.
fn screen_sized_client(wm: &WmTest, (w, h): (u16, u16)) -> (RustConnection, Window) {
    let (conn, screen) = RustConnection::connect(Some(&wm.display)).unwrap();
    let root = conn.setup().roots[screen].root;
    let win = conn.generate_id().unwrap();
    conn.create_window(0, win, root, 0, 0, w, h, 0, WindowClass::INPUT_OUTPUT, 0, &CreateWindowAux::new()).unwrap();
    conn.map_window(win).unwrap();
    conn.flush().unwrap();
    assert!(wm.wait_for(|t| t.client_list().contains(&win), WAIT));
    (conn, win)
}

// DOOM's sequence end to end: The mode changes, the window comes seconds
// later at the new size without asking for fullscreen, and the game dies.
#[test]
fn a_game_window_mapped_long_after_its_mode_change_gets_the_mode_undone() {
    let mut wm = WmTest::new_resizeable();
    wm.start_demiurge(minimal_config());

    xrandr_size(&wm, "800x600");
    assert!(wm.wait_for(|t| screen_size(t) == (800, 600), WAIT));
    std::thread::sleep(Duration::from_millis(2500));
    let (game, win) = screen_sized_client(&wm, (800, 600));
    let state = wm.conn.intern_atom(false, b"_NET_WM_STATE").unwrap().reply().unwrap().atom;
    let fs = wm.conn.intern_atom(false, b"_NET_WM_STATE_FULLSCREEN").unwrap().reply().unwrap().atom;
    let fullscreen = |t: &WmTest| {
        t.conn
            .get_property(false, win, state, AtomEnum::ATOM, 0, 32)
            .ok()
            .and_then(|c| c.reply().ok())
            .is_some_and(|r| r.value32().is_some_and(|mut v| v.any(|a| a == fs)))
    };
    assert!(wm.wait_for(fullscreen, WAIT), "a screen-sized window was not made fullscreen");
    let g = wm.conn.get_geometry(win).unwrap().reply().unwrap();
    assert_eq!((g.x, g.y, g.width, g.height), (0, 0, 800, 600), "tiled under the bar instead");

    drop(game);
    assert!(wm.wait_for(|t| screen_size(t) == (1280, 720), WAIT), "mode left at {:?}", screen_size(&wm));
}

#[test]
fn a_hand_set_mode_stays() {
    let mut wm = WmTest::new_resizeable();
    wm.start_demiurge(minimal_config());

    xrandr_size(&wm, "1024x768");
    assert!(wm.wait_for(|t| screen_size(t) == (1024, 768), WAIT));
    // An ordinary window comes and goes.
    let win = wm.spawn_window(300, 200);
    assert!(wm.wait_for(|t| t.client_list().contains(&win), WAIT));
    wm.conn.destroy_window(win).unwrap();
    wm.conn.flush().unwrap();
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
