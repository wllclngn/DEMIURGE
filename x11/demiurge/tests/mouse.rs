// Pointer handling: Click-to-focus grabs only unfocused clients, and
// _NET_WM_MOVERESIZE drives edge and corner resizes as well as moves.

use std::time::{Duration, Instant};

use demiurge_x11::mouse::{resize_geometry, Edges};
use x11rb::connection::Connection;
use x11rb::properties::{WmSizeHints, WmSizeHintsSpecification};
use x11rb::protocol::xproto::*;
use x11rb::protocol::xtest::ConnectionExt as _;
use x11rb::protocol::Event;

mod common;
use common::wm::{minimal_config, WmTest};

const BAR: i32 = 24;

#[test]
fn moveresize_directions_map_clockwise_from_top_left() {
    let e = |d| Edges::from_moveresize(d).unwrap();
    assert_eq!(e(0), Edges { left: true, top: true, right: false, bottom: false });
    assert_eq!(e(1), Edges { left: false, top: true, right: false, bottom: false });
    assert_eq!(e(2), Edges { left: false, top: true, right: true, bottom: false });
    assert_eq!(e(3), Edges { left: false, top: false, right: true, bottom: false });
    assert_eq!(e(4), Edges::BOTTOM_RIGHT);
    assert_eq!(e(5), Edges { left: false, top: false, right: false, bottom: true });
    assert_eq!(e(6), Edges { left: true, top: false, right: false, bottom: true });
    assert_eq!(e(7), Edges { left: true, top: false, right: false, bottom: false });
    assert_eq!(Edges::from_moveresize(8), None);
}

#[test]
fn bottom_right_resize_grows_from_the_fixed_origin() {
    let g = resize_geometry(Edges::BOTTOM_RIGHT, (100, 100, 400, 300), 50, -20, BAR);
    assert_eq!(g, (100, 100, 450, 280));
}

#[test]
fn left_edge_moves_while_right_edge_holds() {
    let left = Edges::from_moveresize(7).unwrap();
    let (x, _, w, _) = resize_geometry(left, (100, 100, 400, 300), -60, 0, BAR);
    assert_eq!((x, w), (40, 460));
    assert_eq!(x + w as i32, 500);
}

#[test]
fn top_edge_stops_at_the_bar() {
    let top = Edges::from_moveresize(1).unwrap();
    let (_, y, _, h) = resize_geometry(top, (100, 100, 400, 300), 0, -500, BAR);
    assert_eq!((y, h), (BAR, 400 - BAR as u32));
}

#[test]
fn resize_never_shrinks_below_the_minimum() {
    let corner = Edges::from_moveresize(0).unwrap();
    let (x, y, w, h) = resize_geometry(corner, (100, 100, 400, 300), 1000, 1000, BAR);
    assert_eq!((w, h), (50, 50));
    assert_eq!((x + w as i32, y + h as i32), (500, 400));
}

// A floating window at a user-specified position that reports its own
// button presses to the test connection.
fn placed_window(wm: &WmTest, x: i16, y: i16, w: u16, h: u16) -> Window {
    let win = wm.conn.generate_id().unwrap();
    let depth = wm.conn.setup().roots[0].root_depth;
    wm.conn
        .create_window(
            depth,
            win,
            wm.root,
            x,
            y,
            w,
            h,
            0,
            WindowClass::INPUT_OUTPUT,
            x11rb::COPY_FROM_PARENT,
            &CreateWindowAux::new()
                .background_pixel(wm.conn.setup().roots[0].white_pixel)
                .event_mask(EventMask::BUTTON_PRESS),
        )
        .unwrap();
    let mut hints = WmSizeHints::new();
    hints.position = Some((WmSizeHintsSpecification::UserSpecified, x as i32, y as i32));
    hints.set_normal_hints(&wm.conn, win).unwrap();
    wm.conn.map_window(win).unwrap();
    wm.conn.flush().unwrap();
    assert!(wm.wait_for(|t| t.client_list().contains(&win), Duration::from_secs(2)));
    win
}

fn fake(wm: &WmTest, kind: u8, detail: u8, x: i16, y: i16) {
    wm.conn.xtest_fake_input(kind, detail, x11rb::CURRENT_TIME, wm.root, x, y, 0).unwrap();
    wm.conn.flush().unwrap();
}

fn click(wm: &WmTest, x: i16, y: i16) {
    fake(wm, MOTION_NOTIFY_EVENT, 0, x, y);
    fake(wm, BUTTON_PRESS_EVENT, 1, 0, 0);
    fake(wm, BUTTON_RELEASE_EVENT, 1, 0, 0);
}

// True once `win` receives a ButtonPress on the test connection.
fn press_reaches(wm: &WmTest, win: Window, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        while let Some(ev) = wm.conn.poll_for_event().unwrap() {
            if let Event::ButtonPress(p) = ev
                && p.event == win
            {
                return true;
            }
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    false
}

fn drain(wm: &WmTest) {
    while wm.conn.poll_for_event().unwrap().is_some() {}
}

#[test]
fn click_on_unfocused_window_focuses_it_and_reaches_the_app() {
    let mut wm = WmTest::new();
    wm.start_demiurge(minimal_config());
    let a = placed_window(&wm, 40, 80, 300, 200);
    let b = placed_window(&wm, 600, 80, 300, 200);
    assert!(wm.wait_for(|t| t.active_window() == Some(b), Duration::from_secs(2)));
    drain(&wm);

    click(&wm, 190, 180);
    assert!(wm.wait_for(|t| t.active_window() == Some(a), Duration::from_secs(2)));
    assert!(press_reaches(&wm, a, Duration::from_secs(2)), "the focusing click was swallowed");

    // Focus moves back to b, so a gets its grab back and a click on it
    // focuses it again.
    click(&wm, 750, 180);
    assert!(wm.wait_for(|t| t.active_window() == Some(b), Duration::from_secs(2)));
    click(&wm, 190, 180);
    assert!(wm.wait_for(|t| t.active_window() == Some(a), Duration::from_secs(2)));
}

// The focused window holds no WM grab: With demiurge stopped, a click on it
// still reaches the app instead of freezing the pointer for the WM.
#[test]
fn click_on_focused_window_needs_no_wm_round_trip() {
    let mut wm = WmTest::new();
    wm.start_demiurge(minimal_config());
    let a = placed_window(&wm, 40, 80, 300, 200);
    assert!(wm.wait_for(|t| t.active_window() == Some(a), Duration::from_secs(2)));
    drain(&wm);

    let pid = wm.demiurge_pid() as libc::pid_t;
    unsafe { libc::kill(pid, libc::SIGSTOP) };
    click(&wm, 190, 180);
    let reached = press_reaches(&wm, a, Duration::from_secs(1));
    unsafe { libc::kill(pid, libc::SIGCONT) };
    assert!(reached, "the click waited on the stopped WM");
}

#[test]
fn csd_top_left_resize_keeps_the_bottom_right_corner() {
    let mut wm = WmTest::new();
    wm.start_demiurge(minimal_config());
    let a = placed_window(&wm, 300, 200, 400, 300);
    assert!(wm.wait_for(|t| t.active_window() == Some(a), Duration::from_secs(2)));

    let moveresize = wm
        .conn
        .intern_atom(false, b"_NET_WM_MOVERESIZE")
        .unwrap()
        .reply()
        .unwrap()
        .atom;
    let send = |direction: u32| {
        let msg = ClientMessageEvent::new(32, a, moveresize, [300, 200, direction, 1, 1]);
        wm.conn
            .send_event(
                false,
                wm.root,
                EventMask::SUBSTRUCTURE_REDIRECT | EventMask::SUBSTRUCTURE_NOTIFY,
                msg,
            )
            .unwrap();
        wm.conn.flush().unwrap();
    };
    fake(&wm, MOTION_NOTIFY_EVENT, 0, 300, 200);
    send(0);
    std::thread::sleep(Duration::from_millis(200));
    fake(&wm, MOTION_NOTIFY_EVENT, 0, 250, 150);
    let geometry = |t: &WmTest| {
        let g = t.conn.get_geometry(a).unwrap().reply().unwrap();
        (g.x, g.y, g.width, g.height)
    };
    let ok = wm.wait_for(|t| geometry(t) == (250, 150, 450, 350), Duration::from_secs(2));
    send(11);
    assert!(ok, "got {:?}", geometry(&wm));
}
