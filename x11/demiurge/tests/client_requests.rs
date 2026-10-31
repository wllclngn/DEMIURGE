// Requests other clients make of the WM: A ConfigureRequest that moves a
// floating window is remembered, so nothing later puts the window back, and
// _NET_ACTIVE_WINDOW for a window on a hidden tag follows its source.

use std::time::Duration;

use x11rb::connection::Connection;
use x11rb::properties::{WmSizeHints, WmSizeHintsSpecification};
use x11rb::protocol::xproto::*;

mod common;
use common::wm::{minimal_config, WmTest};

const WAIT: Duration = Duration::from_secs(2);

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
            &CreateWindowAux::new().background_pixel(wm.conn.setup().roots[0].white_pixel),
        )
        .unwrap();
    let mut hints = WmSizeHints::new();
    hints.position = Some((WmSizeHintsSpecification::UserSpecified, x as i32, y as i32));
    hints.set_normal_hints(&wm.conn, win).unwrap();
    wm.conn.map_window(win).unwrap();
    wm.conn.flush().unwrap();
    assert!(wm.wait_for(|t| t.client_list().contains(&win), WAIT));
    win
}

fn geometry(wm: &WmTest, win: Window) -> (i16, i16, u16, u16) {
    let g = wm.conn.get_geometry(win).unwrap().reply().unwrap();
    (g.x, g.y, g.width, g.height)
}

fn atom(wm: &WmTest, name: &[u8]) -> Atom {
    wm.conn.intern_atom(false, name).unwrap().reply().unwrap().atom
}

fn client_message(wm: &WmTest, win: Window, kind: &[u8], data: [u32; 5]) {
    let msg = ClientMessageEvent::new(32, win, atom(wm, kind), data);
    wm.conn
        .send_event(
            false,
            wm.root,
            EventMask::SUBSTRUCTURE_REDIRECT | EventMask::SUBSTRUCTURE_NOTIFY,
            msg,
        )
        .unwrap();
    wm.conn.flush().unwrap();
}

// The window moves itself, the way cherrypie or an application does.
fn move_by_request(wm: &WmTest, win: Window, x: i32, y: i32) {
    wm.conn.configure_window(win, &ConfigureWindowAux::new().x(x).y(y)).unwrap();
    wm.conn.flush().unwrap();
    assert!(wm.wait_for(|t| (geometry(t, win).0, geometry(t, win).1) == (x as i16, y as i16), WAIT));
}

#[test]
fn leaving_fullscreen_returns_to_where_the_window_moved() {
    let mut wm = WmTest::new();
    wm.start_demiurge(minimal_config());
    let win = placed_window(&wm, 100, 100, 300, 200);
    move_by_request(&wm, win, 400, 300);

    let fullscreen = atom(&wm, b"_NET_WM_STATE_FULLSCREEN");
    client_message(&wm, win, b"_NET_WM_STATE", [1, fullscreen, 0, 1, 0]);
    assert!(wm.wait_for(|t| geometry(t, win) == (0, 0, 1280, 720), WAIT));
    client_message(&wm, win, b"_NET_WM_STATE", [0, fullscreen, 0, 1, 0]);
    assert!(
        wm.wait_for(|t| geometry(t, win) == (400, 300, 300, 200), WAIT),
        "restored to {:?}",
        geometry(&wm, win)
    );
}

#[test]
fn super_drag_starts_from_where_the_window_moved() {
    let mut wm = WmTest::new();
    wm.start_demiurge(minimal_config());
    let win = placed_window(&wm, 100, 100, 300, 200);
    move_by_request(&wm, win, 400, 300);

    let super_l = wm.keycode(0xffeb);
    wm.fake(MOTION_NOTIFY_EVENT, 0, 550, 400);
    wm.fake(KEY_PRESS_EVENT, super_l, 0, 0);
    wm.fake(BUTTON_PRESS_EVENT, 1, 0, 0);
    std::thread::sleep(Duration::from_millis(100));
    wm.fake(MOTION_NOTIFY_EVENT, 0, 600, 400);
    let moved = wm.wait_for(|t| geometry(t, win) == (450, 300, 300, 200), WAIT);
    wm.fake(BUTTON_RELEASE_EVENT, 1, 0, 0);
    wm.fake(KEY_RELEASE_EVENT, super_l, 0, 0);
    assert!(moved, "the drag started from {:?}", geometry(&wm, win));
}

fn demands_attention(wm: &WmTest, win: Window) -> bool {
    let (state, attention) = (atom(wm, b"_NET_WM_STATE"), atom(wm, b"_NET_WM_STATE_DEMANDS_ATTENTION"));
    wm.conn
        .get_property(false, win, state, AtomEnum::ATOM, 0, 64)
        .unwrap()
        .reply()
        .unwrap()
        .value32()
        .is_some_and(|mut v| v.any(|a| a == attention))
}

// Window `a` visible on tag 0, window `b` moved to the hidden tag 1.
fn hidden_pair(wm: &mut WmTest) -> (Window, Window) {
    wm.start_demiurge(minimal_config());
    let b = placed_window(wm, 600, 100, 300, 200);
    wm.request_move_to_tag(b, 1);
    assert!(wm.wait_for(|t| t.wm_desktop(b) == Some(1), WAIT));
    let a = placed_window(wm, 100, 100, 300, 200);
    assert!(wm.wait_for(|t| t.active_window() == Some(a), WAIT));
    (a, b)
}

#[test]
fn an_application_activating_a_hidden_window_marks_it_urgent() {
    let mut wm = WmTest::new();
    let (a, b) = hidden_pair(&mut wm);

    client_message(&wm, b, b"_NET_ACTIVE_WINDOW", [1, 0, 0, 0, 0]);
    assert!(wm.wait_for(|t| demands_attention(t, b), WAIT));
    assert_eq!(wm.active_window(), Some(a));
    assert_eq!(wm.current_desktop(), Some(0));
}

#[test]
fn a_pager_activating_a_hidden_window_shows_its_tag() {
    let mut wm = WmTest::new();
    let (_a, b) = hidden_pair(&mut wm);

    client_message(&wm, b, b"_NET_ACTIVE_WINDOW", [2, 0, 0, 0, 0]);
    assert!(wm.wait_for(|t| t.current_desktop() == Some(1) && t.active_window() == Some(b), WAIT));
    assert!(!demands_attention(&wm, b));
}
