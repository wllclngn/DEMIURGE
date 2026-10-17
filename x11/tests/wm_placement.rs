// Default placement: A floating window that asked for no position opens
// centered in the work area, below the bar; a user-specified position
// (USPosition in WM_NORMAL_HINTS) is kept.

use std::time::Duration;

use x11rb::connection::Connection;
use x11rb::properties::{WmSizeHints, WmSizeHintsSpecification};
use x11rb::protocol::xproto::*;

mod common;
use common::wm::{minimal_config, WmTest};

// Xephyr is 1280x720 and minimal_config's bar is 24 px tall.
const WORK_Y: i16 = 24;
const WORK_H: u16 = 720 - 24;

fn geometry(wm: &WmTest, win: Window) -> (i16, i16, u16, u16) {
    let g = wm.conn.get_geometry(win).unwrap().reply().unwrap();
    (g.x, g.y, g.width, g.height)
}

#[test]
fn floating_window_without_position_is_centered_below_the_bar() {
    let mut wm = WmTest::new();
    wm.start_demiurge(minimal_config());

    let win = wm.spawn_window(400, 300);
    let want = ((1280 - 400) / 2, WORK_Y + ((WORK_H - 300) / 2) as i16, 400, 300);
    assert!(
        wm.wait_for(|t| geometry(t, win) == want, Duration::from_secs(2)),
        "expected {:?}, got {:?}",
        want,
        geometry(&wm, win)
    );
}

#[test]
fn user_specified_position_is_kept() {
    let mut wm = WmTest::new();
    wm.start_demiurge(minimal_config());

    let win = wm.conn.generate_id().unwrap();
    wm.conn
        .create_window(0, win, wm.root, 50, 60, 320, 200, 0, WindowClass::INPUT_OUTPUT, 0, &CreateWindowAux::new())
        .unwrap();
    let mut hints = WmSizeHints::new();
    hints.position = Some((WmSizeHintsSpecification::UserSpecified, 50, 60));
    hints.set_normal_hints(&wm.conn, win).unwrap();
    wm.conn.map_window(win).unwrap();
    wm.conn.flush().unwrap();

    assert!(wm.wait_for(|t| t.client_list().contains(&win), Duration::from_secs(2)));
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(geometry(&wm, win), (50, 60, 320, 200));
}
