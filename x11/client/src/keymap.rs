// The X server's keyboard mapping, as the window manager and the X11
// components read keys from it.

use x11rb::connection::Connection;
use x11rb::protocol::xproto::*;
use x11rb::rust_connection::RustConnection;

// Cached keyboard mapping. Built once at startup and rebuilt on MappingNotify.
// Replaces per-call get_keyboard_mapping roundtrips, which previously fired
// once per keystroke in the run prompt and once per init for find_alt_keycodes.
pub struct KeyMap {
    pub min_keycode: u8,
    pub syms_per_code: usize,
    pub keysyms: Vec<u32>,
}

pub fn load_keymap(conn: &RustConnection) -> KeyMap {
    let setup = conn.setup();
    let min_keycode = setup.min_keycode;
    let max_keycode = setup.max_keycode;
    let count = (max_keycode - min_keycode) as u32 + 1;

    let (syms_per_code, keysyms) = conn
        .get_keyboard_mapping(min_keycode, count as u8)
        .ok()
        .and_then(|c| c.reply().ok())
        .map(|m| (m.keysyms_per_keycode as usize, m.keysyms))
        .unwrap_or((0, Vec::new()));

    KeyMap {
        min_keycode,
        syms_per_code,
        keysyms,
    }
}

pub fn keymap_keysym(map: &KeyMap, keycode: Keycode, state: u16) -> u32 {
    if map.syms_per_code == 0 {
        return 0;
    }
    let idx = (keycode - map.min_keycode) as usize;
    let base = idx * map.syms_per_code;
    if base >= map.keysyms.len() {
        return 0;
    }
    let shifted = state & u16::from(ModMask::SHIFT) != 0;
    let col = if shifted && map.syms_per_code > 1 { 1 } else { 0 };
    map.keysyms.get(base + col).copied().unwrap_or(0)
}
