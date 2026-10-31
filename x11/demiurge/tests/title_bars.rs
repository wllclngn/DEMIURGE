// Title bars: With [general] title_bars on, a floating window sits in a frame
// below a 30 px title bar whose lights close, minimize and zoom it and whose
// bar drags it. Tiling the tag takes the frame away, a window that asks for
// no decorations never gets one, fullscreen hides the bar, a withdrawn
// window is handed back to the root, and so is every window if DEMIURGE dies.

use std::time::Duration;

use x11rb::connection::Connection;
use x11rb::protocol::xproto::*;
use x11rb::protocol::Event;
use x11rb::wrapper::ConnectionExt as _;

mod common;
use common::wm::WmTest;

const WAIT: Duration = Duration::from_secs(2);
const BAR: i16 = 30;

const CONFIG: &str = r#"
[general]
tags = ["A", "B", "C"]
default_layout = "floating"
title_bars = true

[bar]
height = 24
font = "Noto Sans 9"

[startup]
commands = []

[[keybind]]
mods = ["Super"]
key = "space"
action = "toggle_layout"
"#;

fn atom(wm: &WmTest, name: &[u8]) -> Atom {
    wm.conn.intern_atom(false, name).unwrap().reply().unwrap().atom
}

fn parent(wm: &WmTest, win: Window) -> Window {
    wm.conn.query_tree(win).unwrap().reply().unwrap().parent
}

fn geometry(wm: &WmTest, win: Window) -> (i16, i16, u16, u16) {
    let g = wm.conn.get_geometry(win).unwrap().reply().unwrap();
    (g.x, g.y, g.width, g.height)
}

fn viewable(wm: &WmTest, win: Window) -> bool {
    wm.map_state(win) == Some(MapState::VIEWABLE)
}

fn frame_extents(wm: &WmTest, win: Window) -> Vec<u32> {
    let a = atom(wm, b"_NET_FRAME_EXTENTS");
    let reply = wm.conn.get_property(false, win, a, AtomEnum::CARDINAL, 0, 4).unwrap().reply().unwrap();
    reply.value32().map(|v| v.collect()).unwrap_or_default()
}

// A 300x200 window, mapped and managed; `prepare` sets its properties first.
fn window_with(wm: &WmTest, prepare: impl Fn(&WmTest, Window)) -> Window {
    let win = wm.conn.generate_id().unwrap();
    let screen = &wm.conn.setup().roots[0];
    wm.conn
        .create_window(
            screen.root_depth,
            win,
            wm.root,
            0,
            0,
            300,
            200,
            0,
            WindowClass::INPUT_OUTPUT,
            x11rb::COPY_FROM_PARENT,
            &CreateWindowAux::new().background_pixel(screen.white_pixel).event_mask(EventMask::STRUCTURE_NOTIFY),
        )
        .unwrap();
    wm.conn.change_property8(PropMode::REPLACE, win, AtomEnum::WM_NAME, AtomEnum::STRING, b"framed").unwrap();
    prepare(wm, win);
    wm.conn.map_window(win).unwrap();
    wm.conn.flush().unwrap();
    assert!(wm.wait_for(|t| t.client_list().contains(&win) && t.active_window() == Some(win), WAIT));
    win
}

fn framed_window(wm: &WmTest) -> (Window, Window) {
    let win = window_with(wm, |_, _| {});
    assert!(wm.wait_for(|t| parent(t, win) != t.root, WAIT), "the window was never framed");
    (win, parent(wm, win))
}

fn click(wm: &WmTest, x: i16, y: i16) {
    wm.fake(MOTION_NOTIFY_EVENT, 0, x, y);
    wm.fake(BUTTON_PRESS_EVENT, 1, 0, 0);
    wm.fake(BUTTON_RELEASE_EVENT, 1, 0, 0);
}

fn start(config: &str) -> WmTest {
    let mut wm = WmTest::new();
    wm.start_demiurge(config);
    wm
}

#[test]
fn a_floating_window_sits_in_a_frame_below_its_title_bar() {
    let wm = start(CONFIG);
    let (win, frame) = framed_window(&wm);

    // 300x(200 + 30) centered in the 1280x696 work area below the bar.
    assert_eq!(geometry(&wm, frame), (490, 24 + 233, 300, 230));
    assert_eq!(geometry(&wm, win), (0, BAR, 300, 200));
    assert_eq!(frame_extents(&wm, win), vec![0, 0, 30, 0]);
    assert!(viewable(&wm, win));
    assert_eq!(wm.client_list(), vec![win], "the frame, not the window, was listed");
}

#[test]
fn the_lights_close_minimize_and_zoom() {
    let wm = start(CONFIG);
    let protocols = atom(&wm, b"WM_PROTOCOLS");
    let delete = atom(&wm, b"WM_DELETE_WINDOW");
    let win = window_with(&wm, |wm, win| {
        wm.conn.change_property32(PropMode::REPLACE, win, protocols, AtomEnum::ATOM, &[delete]).unwrap();
    });
    assert!(wm.wait_for(|t| parent(t, win) != t.root, WAIT));
    let frame = parent(&wm, win);
    let (fx, fy, _, _) = geometry(&wm, frame);

    // Zoom fills the work area, frame and all; zoom again puts it back.
    click(&wm, fx + 58, fy + 15);
    assert!(wm.wait_for(|t| geometry(t, frame) == (0, 24, 1280, 696), WAIT), "zoomed to {:?}", geometry(&wm, frame));
    assert_eq!(geometry(&wm, win), (0, BAR, 1280, 666));
    click(&wm, 58, 24 + 15);
    assert!(wm.wait_for(|t| geometry(t, frame) == (fx, fy, 300, 230), WAIT), "back at {:?}", geometry(&wm, frame));

    // Close asks the window to close.
    while wm.conn.poll_for_event().unwrap().is_some() {}
    click(&wm, fx + 18, fy + 15);
    let asked = wm.wait_for(
        |t| {
            std::iter::from_fn(|| t.conn.poll_for_event().ok().flatten()).any(|ev| {
                matches!(ev, Event::ClientMessage(m) if m.window == win && m.data.as_data32()[0] == delete)
            })
        },
        WAIT,
    );
    assert!(asked, "the close light sent no WM_DELETE_WINDOW");

    // Minimize hides it on its tag.
    click(&wm, fx + 38, fy + 15);
    assert!(wm.wait_for(|t| !viewable(t, win) && t.client_list().contains(&win), WAIT), "the minimize light left it up");
}

#[test]
fn the_title_bar_and_super_drag_move_the_frame() {
    let wm = start(CONFIG);
    let (win, frame) = framed_window(&wm);
    let (fx, fy, _, _) = geometry(&wm, frame);

    wm.fake(MOTION_NOTIFY_EVENT, 0, fx + 150, fy + 15);
    wm.fake(BUTTON_PRESS_EVENT, 1, 0, 0);
    std::thread::sleep(Duration::from_millis(100));
    wm.fake(MOTION_NOTIFY_EVENT, 0, fx + 250, fy + 65);
    let moved = wm.wait_for(|t| geometry(t, frame) == (fx + 100, fy + 50, 300, 230), WAIT);
    wm.fake(BUTTON_RELEASE_EVENT, 1, 0, 0);
    assert!(moved, "the title bar drag left the frame at {:?}", geometry(&wm, frame));
    let on_root = wm.conn.translate_coordinates(win, wm.root, 0, 0).unwrap().reply().unwrap();
    assert_eq!((on_root.dst_x, on_root.dst_y), (fx + 100, fy + 50 + BAR));

    // Super+drag from inside the window: The root grab finds the frame.
    let super_l = wm.keycode(0xffeb);
    wm.fake(MOTION_NOTIFY_EVENT, 0, fx + 250, fy + 150);
    wm.fake(KEY_PRESS_EVENT, super_l, 0, 0);
    wm.fake(BUTTON_PRESS_EVENT, 1, 0, 0);
    std::thread::sleep(Duration::from_millis(100));
    wm.fake(MOTION_NOTIFY_EVENT, 0, fx + 200, fy + 150);
    let moved = wm.wait_for(|t| geometry(t, frame) == (fx + 50, fy + 50, 300, 230), WAIT);
    wm.fake(BUTTON_RELEASE_EVENT, 1, 0, 0);
    wm.fake(KEY_RELEASE_EVENT, super_l, 0, 0);
    assert!(moved, "Super+drag left the frame at {:?}", geometry(&wm, frame));
}

#[test]
fn tiling_the_tag_takes_the_frame_away_and_floating_brings_it_back() {
    let wm = start(CONFIG);
    let (win, _) = framed_window(&wm);
    let super_l = wm.keycode(0xffeb);
    let space = wm.keycode(0x20);
    let toggle = || {
        wm.fake(KEY_PRESS_EVENT, super_l, 0, 0);
        wm.fake(KEY_PRESS_EVENT, space, 0, 0);
        wm.fake(KEY_RELEASE_EVENT, space, 0, 0);
        wm.fake(KEY_RELEASE_EVENT, super_l, 0, 0);
    };

    toggle(); // tile
    assert!(wm.wait_for(|t| parent(t, win) == t.root && viewable(t, win), WAIT), "tiled but still framed");
    assert!(wm.wait_for(|t| geometry(t, win) == (0, 24, 1280, 696), WAIT), "tiled to {:?}", geometry(&wm, win));
    assert_eq!(frame_extents(&wm, win), vec![0, 0, 0, 0]);
    assert!(wm.client_list().contains(&win), "unframing unmanaged the window");

    toggle(); // monocle
    toggle(); // floating
    assert!(wm.wait_for(|t| parent(t, win) != t.root && viewable(t, win), WAIT), "floating again but unframed");
}

#[test]
fn a_window_that_asks_for_no_decorations_gets_no_frame() {
    let wm = start(CONFIG);
    let motif = atom(&wm, b"_MOTIF_WM_HINTS");
    let win = window_with(&wm, |wm, win| {
        wm.conn.change_property32(PropMode::REPLACE, win, motif, motif, &[2, 0, 0, 0, 0]).unwrap();
    });
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(parent(&wm, win), wm.root);
    assert_eq!(frame_extents(&wm, win), vec![0, 0, 0, 0]);
}

#[test]
fn fullscreen_hides_the_title_bar_and_leaving_restores_it() {
    let wm = start(CONFIG);
    let (win, frame) = framed_window(&wm);
    let before = geometry(&wm, frame);
    let state = atom(&wm, b"_NET_WM_STATE");
    let fullscreen = atom(&wm, b"_NET_WM_STATE_FULLSCREEN");
    let request = |action: u32| {
        let msg = ClientMessageEvent::new(32, win, state, [action, fullscreen, 0, 1, 0]);
        let mask = EventMask::SUBSTRUCTURE_REDIRECT | EventMask::SUBSTRUCTURE_NOTIFY;
        wm.conn.send_event(false, wm.root, mask, msg).unwrap();
        wm.conn.flush().unwrap();
    };

    request(1);
    assert!(wm.wait_for(|t| geometry(t, frame) == (0, 0, 1280, 720) && geometry(t, win) == (0, 0, 1280, 720), WAIT));
    request(0);
    assert!(wm.wait_for(|t| geometry(t, frame) == before && geometry(t, win) == (0, BAR, 300, 200), WAIT));
}

#[test]
fn a_withdrawn_window_is_unmanaged_and_handed_back_to_the_root() {
    let wm = start(CONFIG);
    let (win, frame) = framed_window(&wm);
    wm.conn.unmap_window(win).unwrap();
    wm.conn.flush().unwrap();
    assert!(wm.wait_for(|t| !t.client_list().contains(&win), WAIT), "the withdrawn window is still managed");
    assert!(wm.wait_for(|t| parent(t, win) == t.root, WAIT), "the window went down with its frame");
    assert!(wm.conn.get_geometry(frame).unwrap().reply().is_err(), "the frame outlived its window");
}

#[test]
fn windows_outlive_demiurge_through_the_save_set() {
    let wm = start(CONFIG);
    let (win, _) = framed_window(&wm);
    unsafe { libc::kill(wm.demiurge_pid() as i32, libc::SIGKILL) };
    assert!(wm.wait_for(|t| t.conn.query_tree(win).ok().and_then(|c| c.reply().ok()).is_some_and(|r| r.parent == t.root), WAIT), "the window died with DEMIURGE's frame");
}
