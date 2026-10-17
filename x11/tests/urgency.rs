// Urgency: The WM_HINTS urgency bit and _NET_WM_STATE_DEMANDS_ATTENTION mark
// a client urgent, the bar shows its tag in the accent, and focus clears it.

use std::time::Duration;

use x11rb::connection::Connection;
use x11rb::properties::{WmHints, WmSizeHints, WmSizeHintsSpecification};
use x11rb::protocol::xproto::*;
use x11rb::protocol::xtest::ConnectionExt as _;

mod common;
use common::wm::{minimal_config, WmTest};

const WAIT: Duration = Duration::from_secs(2);

fn placed_window(wm: &WmTest, x: i16, y: i16) -> Window {
    let win = wm.conn.generate_id().unwrap();
    let depth = wm.conn.setup().roots[0].root_depth;
    wm.conn
        .create_window(
            depth,
            win,
            wm.root,
            x,
            y,
            300,
            200,
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

fn atom(wm: &WmTest, name: &[u8]) -> Atom {
    wm.conn.intern_atom(false, name).unwrap().reply().unwrap().atom
}

fn set_hints_urgent(wm: &WmTest, win: Window, urgent: bool) {
    let mut hints = WmHints::new();
    hints.urgent = urgent;
    hints.set(&wm.conn, win).unwrap();
    wm.conn.flush().unwrap();
}

fn hints_urgent(wm: &WmTest, win: Window) -> bool {
    WmHints::get(&wm.conn, win).unwrap().reply().unwrap().is_some_and(|h| h.urgent)
}

fn demands_attention(wm: &WmTest, win: Window) -> bool {
    let state = atom(wm, b"_NET_WM_STATE");
    let attention = atom(wm, b"_NET_WM_STATE_DEMANDS_ATTENTION");
    wm.conn
        .get_property(false, win, state, AtomEnum::ATOM, 0, 64)
        .unwrap()
        .reply()
        .unwrap()
        .value32()
        .is_some_and(|mut v| v.any(|a| a == attention))
}

fn click(wm: &WmTest, x: i16, y: i16) {
    for (kind, detail) in [(MOTION_NOTIFY_EVENT, 0), (BUTTON_PRESS_EVENT, 1), (BUTTON_RELEASE_EVENT, 1)] {
        wm.conn.xtest_fake_input(kind, detail, x11rb::CURRENT_TIME, wm.root, x, y, 0).unwrap();
    }
    wm.conn.flush().unwrap();
}

#[test]
fn hints_urgency_marks_an_unfocused_client_and_focus_clears_it() {
    let mut wm = WmTest::new();
    wm.start_demiurge(minimal_config());
    let a = placed_window(&wm, 40, 80);
    let b = placed_window(&wm, 600, 80);
    assert!(wm.wait_for(|t| t.active_window() == Some(b), WAIT));

    set_hints_urgent(&wm, a, true);
    assert!(wm.wait_for(|t| demands_attention(t, a), WAIT));

    click(&wm, 190, 180);
    assert!(wm.wait_for(|t| t.active_window() == Some(a), WAIT));
    assert!(wm.wait_for(|t| !demands_attention(t, a), WAIT));
    assert!(!hints_urgent(&wm, a), "focus leaves the WM_HINTS urgency bit set");
}

#[test]
fn focused_client_is_never_urgent() {
    let mut wm = WmTest::new();
    wm.start_demiurge(minimal_config());
    let a = placed_window(&wm, 40, 80);
    assert!(wm.wait_for(|t| t.active_window() == Some(a), WAIT));

    set_hints_urgent(&wm, a, true);
    std::thread::sleep(Duration::from_millis(300));
    assert!(!demands_attention(&wm, a));
}

#[test]
fn demands_attention_request_marks_the_client() {
    let mut wm = WmTest::new();
    wm.start_demiurge(minimal_config());
    let a = placed_window(&wm, 40, 80);
    let b = placed_window(&wm, 600, 80);
    assert!(wm.wait_for(|t| t.active_window() == Some(b), WAIT));

    let msg = ClientMessageEvent::new(
        32,
        a,
        atom(&wm, b"_NET_WM_STATE"),
        [1, atom(&wm, b"_NET_WM_STATE_DEMANDS_ATTENTION"), 0, 1, 0],
    );
    wm.conn
        .send_event(
            false,
            wm.root,
            EventMask::SUBSTRUCTURE_REDIRECT | EventMask::SUBSTRUCTURE_NOTIFY,
            msg,
        )
        .unwrap();
    wm.conn.flush().unwrap();
    assert!(wm.wait_for(|t| demands_attention(t, a), WAIT));
}

// Red-dominant pixels across the bar strip of the root image. Antialiased
// glyphs rarely reach the exact accent; nothing else on this bar is red.
fn accent_pixels(wm: &WmTest) -> usize {
    let image = wm
        .conn
        .get_image(ImageFormat::Z_PIXMAP, wm.root, 0, 0, 400, 24, !0)
        .unwrap()
        .reply()
        .unwrap();
    image.data.chunks_exact(4).filter(|p| p[2] > 0x80 && p[0] < 0x50 && p[1] < 0x50).count()
}

#[test]
fn bar_shows_an_urgent_hidden_tag_in_the_accent() {
    let mut wm = WmTest::new();
    let config = format!("{}\n[appearance]\naccent = \"#ff0000\"\n", minimal_config());
    wm.start_demiurge(&config);
    let a = placed_window(&wm, 40, 80);
    let b = placed_window(&wm, 600, 80);
    assert!(wm.wait_for(|t| t.active_window() == Some(b), WAIT));
    wm.request_move_to_tag(a, 1);
    assert!(wm.wait_for(|t| t.wm_desktop(a) == Some(1), WAIT));
    std::thread::sleep(Duration::from_millis(300));
    let before = accent_pixels(&wm);

    set_hints_urgent(&wm, a, true);
    assert!(wm.wait_for(|t| demands_attention(t, a), WAIT));
    assert!(
        wm.wait_for(|t| accent_pixels(t) > before, WAIT),
        "no accent text appeared on the bar (before {before}, after {})",
        accent_pixels(&wm)
    );
}
