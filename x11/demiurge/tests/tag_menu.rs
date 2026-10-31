// Minimize and the tag menu: A minimized window hides on its tag and stays
// hidden through tag switches; the tag in view, pressed again or clicked on
// the bar, drops a menu of its windows that the keyboard and the mouse drive,
// and picking a minimized entry restores it.

use std::time::Duration;

use x11rb::connection::Connection;
use x11rb::protocol::xproto::*;

mod common;
use common::wm::WmTest;

const WAIT: Duration = Duration::from_secs(2);

const CONFIG: &str = r#"
[general]
tags = ["A", "B", "C"]
default_layout = "floating"

[bar]
height = 24
font = "Noto Sans 9"

[startup]
commands = []

[[keybind]]
mods = ["Super"]
key = "1"
action = "view_tag"
args = "1"

[[keybind]]
mods = ["Super"]
key = "2"
action = "view_tag"
args = "2"

[[keybind]]
mods = ["Super"]
key = "n"
action = "minimize"
"#;

const SUPER_L: u32 = 0xffeb;
const KEY_1: u32 = 0x31;
const KEY_2: u32 = 0x32;
const KEY_N: u32 = 0x6e;
const DOWN: u32 = 0xff54;
const RETURN: u32 = 0xff0d;
const ESCAPE: u32 = 0xff1b;

fn press(wm: &WmTest, keysym: u32) {
    let key = wm.keycode(keysym);
    wm.fake(KEY_PRESS_EVENT, key, 0, 0);
    wm.fake(KEY_RELEASE_EVENT, key, 0, 0);
}

fn super_key(wm: &WmTest, keysym: u32) {
    let super_l = wm.keycode(SUPER_L);
    wm.fake(KEY_PRESS_EVENT, super_l, 0, 0);
    press(wm, keysym);
    wm.fake(KEY_RELEASE_EVENT, super_l, 0, 0);
}

fn click(wm: &WmTest, x: i16, y: i16) {
    wm.fake(MOTION_NOTIFY_EVENT, 0, x, y);
    wm.fake(BUTTON_PRESS_EVENT, 1, 0, 0);
    wm.fake(BUTTON_RELEASE_EVENT, 1, 0, 0);
}

fn atom(wm: &WmTest, name: &[u8]) -> Atom {
    wm.conn.intern_atom(false, name).unwrap().reply().unwrap().atom
}

fn viewable(wm: &WmTest, win: Window) -> bool {
    wm.map_state(win) == Some(MapState::VIEWABLE)
}

fn hidden_state(wm: &WmTest, win: Window) -> bool {
    let state = atom(wm, b"_NET_WM_STATE");
    let reply = wm.conn.get_property(false, win, state, AtomEnum::ATOM, 0, 32).unwrap().reply().unwrap();
    reply.value32().is_some_and(|mut v| v.any(|a| a == atom(wm, b"_NET_WM_STATE_HIDDEN")))
}

fn wm_state(wm: &WmTest, win: Window) -> Option<u32> {
    let a = atom(wm, b"WM_STATE");
    let reply = wm.conn.get_property(false, win, a, a, 0, 2).unwrap().reply().unwrap();
    reply.value32().and_then(|mut v| v.next())
}

// The open menu's window and geometry, if one is on screen.
fn menu(wm: &WmTest) -> Option<(Window, (i16, i16, u16, u16))> {
    let tree = wm.conn.query_tree(wm.root).unwrap().reply().unwrap();
    tree.children.into_iter().find_map(|w| {
        let name = wm.conn.get_property(false, w, AtomEnum::WM_NAME, AtomEnum::STRING, 0, 64).ok()?.reply().ok()?;
        if name.value != b"demiurge-tag-menu" || !viewable(wm, w) {
            return None;
        }
        let g = wm.conn.get_geometry(w).ok()?.reply().ok()?;
        Some((w, (g.x, g.y, g.width, g.height)))
    })
}

fn focused_window(wm: &WmTest) -> Window {
    let win = wm.spawn_window(300, 200);
    assert!(wm.wait_for(|t| t.active_window() == Some(win), WAIT), "the new window never took focus");
    win
}

fn iconify(wm: &WmTest, win: Window) {
    let msg = ClientMessageEvent::new(32, win, atom(wm, b"WM_CHANGE_STATE"), [3u32, 0, 0, 0, 0]);
    let mask = EventMask::SUBSTRUCTURE_REDIRECT | EventMask::SUBSTRUCTURE_NOTIFY;
    wm.conn.send_event(false, wm.root, mask, msg).unwrap();
    wm.conn.flush().unwrap();
}

#[test]
fn a_minimized_window_hides_on_its_tag() {
    let mut wm = WmTest::new();
    wm.start_demiurge(CONFIG);
    let first = focused_window(&wm);
    let second = focused_window(&wm);

    iconify(&wm, second);
    assert!(wm.wait_for(|t| !viewable(t, second), WAIT), "the minimized window is still on screen");
    assert!(wm.client_list().contains(&second), "minimize unmanaged the window");
    assert_eq!(wm.wm_desktop(second), Some(0));
    assert!(hidden_state(&wm, second) && wm_state(&wm, second) == Some(3));
    assert!(wm.wait_for(|t| t.active_window() == Some(first), WAIT), "focus stayed on the minimized window");

    super_key(&wm, KEY_2);
    assert!(wm.wait_for(|t| t.current_desktop() == Some(1), WAIT));
    super_key(&wm, KEY_1);
    assert!(wm.wait_for(|t| t.current_desktop() == Some(0) && viewable(t, first), WAIT));
    std::thread::sleep(Duration::from_millis(200));
    assert!(!viewable(&wm, second), "a tag switch brought the minimized window back");
}

#[test]
fn its_key_again_opens_the_menu_and_enter_restores() {
    let mut wm = WmTest::new();
    wm.start_demiurge(CONFIG);
    let first = focused_window(&wm);
    let second = focused_window(&wm);
    super_key(&wm, KEY_N);
    assert!(wm.wait_for(|t| !viewable(t, second) && t.active_window() == Some(first), WAIT));

    super_key(&wm, KEY_1);
    assert!(wm.wait_for(|t| menu(t).is_some(), WAIT), "Super+1 on tag A opened no menu");
    let (_, (_, y, _, h)) = menu(&wm).unwrap();
    assert_eq!(y, 24, "the menu does not hang from the bar");
    assert_eq!(h % 2, 0, "two equal rows expected, got height {h}");

    press(&wm, DOWN);
    press(&wm, RETURN);
    assert!(wm.wait_for(|t| menu(t).is_none(), WAIT), "Enter left the menu open");
    assert!(wm.wait_for(|t| viewable(t, second) && t.active_window() == Some(second), WAIT), "the pick did not restore");
    assert!(!hidden_state(&wm, second) && wm_state(&wm, second) == Some(1));
}

#[test]
fn escape_or_a_click_elsewhere_closes_the_menu() {
    let mut wm = WmTest::new();
    wm.start_demiurge(CONFIG);
    let win = focused_window(&wm);

    super_key(&wm, KEY_1);
    assert!(wm.wait_for(|t| menu(t).is_some(), WAIT));
    press(&wm, ESCAPE);
    assert!(wm.wait_for(|t| menu(t).is_none(), WAIT), "Escape left the menu open");

    // Tag A on the bar starts 20 px in.
    click(&wm, 25, 12);
    assert!(wm.wait_for(|t| menu(t).is_some(), WAIT), "clicking the tag in view opened no menu");
    click(&wm, 900, 600);
    assert!(wm.wait_for(|t| menu(t).is_none(), WAIT), "a click elsewhere left the menu open");
    assert_eq!(wm.active_window(), Some(win));

    // The keyboard and pointer are free again: Super+2 switches tags.
    super_key(&wm, KEY_2);
    assert!(wm.wait_for(|t| t.current_desktop() == Some(1), WAIT), "the menu kept its grabs");
}

#[test]
fn clicking_an_entry_picks_it() {
    let mut wm = WmTest::new();
    wm.start_demiurge(CONFIG);
    let first = focused_window(&wm);
    let _second = focused_window(&wm);

    click(&wm, 25, 12);
    assert!(wm.wait_for(|t| menu(t).is_some(), WAIT));
    let (_, (x, y, w, h)) = menu(&wm).unwrap();
    click(&wm, x + w as i16 / 2, y + h as i16 / 4);
    assert!(wm.wait_for(|t| menu(t).is_none() && t.active_window() == Some(first), WAIT), "the click did not pick the first entry");
}

#[test]
fn an_empty_tag_opens_no_menu() {
    let mut wm = WmTest::new();
    wm.start_demiurge(CONFIG);
    super_key(&wm, KEY_1);
    std::thread::sleep(Duration::from_millis(300));
    assert!(menu(&wm).is_none());
}
