// A client that withdraws its own window (unmaps it, as Steam does going to
// the tray) is unmanaged, even after DEMIURGE has hidden and shown it on the
// way: Every unmap DEMIURGE counts as its own must be one the server echoes.
// And a window moved from a hidden tag onto the one in view shows.

use std::process::Command;
use std::time::Duration;

use x11rb::connection::Connection;
use x11rb::protocol::xproto::*;

mod common;
use common::wm::{minimal_config, WmTest};

const WAIT: Duration = Duration::from_secs(2);

fn hidden_on(wm: &WmTest, win: Window, tag: u32) {
    wm.request_move_to_tag(win, tag);
    assert!(wm.wait_for(|t| t.wm_desktop(win) == Some(tag) && t.map_state(win) != Some(MapState::VIEWABLE), WAIT));
}

fn shown_then_withdrawn(wm: &WmTest, win: Window, tag: u32) {
    wm.request_view_tag(tag);
    assert!(wm.wait_for(|t| t.map_state(win) == Some(MapState::VIEWABLE), WAIT), "the tag did not show the window");
    wm.conn.unmap_window(win).unwrap();
    wm.conn.flush().unwrap();
    assert!(
        wm.wait_for(|t| !t.client_list().contains(&win), WAIT),
        "the withdrawn window is still managed"
    );
}

#[test]
fn a_mode_change_leaves_hidden_windows_withdrawable() {
    let mut wm = WmTest::new_resizeable();
    wm.start_demiurge(minimal_config());
    let win = wm.spawn_window(300, 200);
    assert!(wm.wait_for(|t| t.client_list().contains(&win), WAIT));
    hidden_on(&wm, win, 1);

    let ok = Command::new("xrandr").args(["-display", &wm.display, "-s", "1024x768"]).status().unwrap().success();
    assert!(ok, "xrandr -s 1024x768");
    assert!(wm.wait_for(|t| t.desktop_geometry() == vec![1024, 768], WAIT), "DEMIURGE never saw the mode change");

    shown_then_withdrawn(&wm, win, 1);
}

#[test]
fn a_move_between_hidden_tags_leaves_the_window_withdrawable() {
    let mut wm = WmTest::new();
    wm.start_demiurge(minimal_config());
    let win = wm.spawn_window(300, 200);
    assert!(wm.wait_for(|t| t.client_list().contains(&win), WAIT));
    hidden_on(&wm, win, 1);
    hidden_on(&wm, win, 2);

    shown_then_withdrawn(&wm, win, 2);
}

#[test]
fn a_window_moved_onto_the_tag_in_view_shows() {
    let mut wm = WmTest::new();
    wm.start_demiurge(minimal_config());
    let win = wm.spawn_window(300, 200);
    assert!(wm.wait_for(|t| t.client_list().contains(&win), WAIT));
    hidden_on(&wm, win, 1);

    wm.request_move_to_tag(win, 0);
    assert!(wm.wait_for(|t| t.map_state(win) == Some(MapState::VIEWABLE), WAIT), "moved onto tag A but still hidden");
    wm.conn.unmap_window(win).unwrap();
    wm.conn.flush().unwrap();
    assert!(wm.wait_for(|t| !t.client_list().contains(&win), WAIT), "the withdrawn window is still managed");
}
