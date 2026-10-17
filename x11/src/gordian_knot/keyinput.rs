// Key decoding for the X11 lock, through xkbcommon with the server's own
// layout (the _XKB_RULES_NAMES root property). The hand-rolled core-map
// lookup this replaces ignored Caps Lock, AltGr and NumLock, dropped
// keypad digits and non-ASCII, and typed `u` for Ctrl+U.

use x11rb::connection::Connection;
use x11rb::protocol::xproto::{AtomEnum, ConnectionExt as _, KeyPressEvent, Window};
use xkbcommon::xkb;

#[derive(Debug, PartialEq, Eq)]
pub enum KeyAction {
    Submit,
    Backspace,
    Clear,
    Append(String),
    Ignore,
}

// What a decoded key does to the password field. Pure, so the mapping is
// testable without a server.
pub fn action(keysym: u32, utf8: &str, ctrl: bool) -> KeyAction {
    use xkb::keysyms as ks;
    match keysym {
        ks::KEY_Return | ks::KEY_KP_Enter => KeyAction::Submit,
        ks::KEY_BackSpace => KeyAction::Backspace,
        ks::KEY_Escape => KeyAction::Clear,
        ks::KEY_u | ks::KEY_U if ctrl => KeyAction::Clear,
        _ if ctrl => KeyAction::Ignore,
        _ if !utf8.is_empty() && !utf8.chars().any(char::is_control) => {
            KeyAction::Append(utf8.to_string())
        }
        _ => KeyAction::Ignore,
    }
}

pub struct Decoder {
    state: xkb::State,
}

impl Decoder {
    pub fn from_server<C: Connection>(conn: &C, root: Window) -> Option<Decoder> {
        let [rules, model, layout, variant, options] = rules_names(conn, root);
        let ctx = xkb::Context::new(xkb::CONTEXT_NO_FLAGS);
        let options = (!options.is_empty()).then_some(options);
        let keymap = xkb::Keymap::new_from_names(
            &ctx,
            &rules,
            &model,
            &layout,
            &variant,
            options,
            xkb::KEYMAP_COMPILE_NO_FLAGS,
        )?;
        Some(Decoder { state: xkb::State::new(&keymap) })
    }

    // The core event state carries the eight real modifiers in xkbcommon's
    // index order (Shift, Lock, Control, Mod1..Mod5) and the group in bits
    // 13-14, so the event alone sets the level: Caps Lock and NumLock
    // included.
    pub fn decode(&mut self, ev: &KeyPressEvent) -> KeyAction {
        let state = u16::from(ev.state) as u32;
        self.state.update_mask(state & 0xff, 0, 0, 0, 0, (state >> 13) & 0x3);
        let code: xkb::Keycode = (ev.detail as u32).into();
        let keysym: u32 = self.state.key_get_one_sym(code).into();
        let ctrl = self.state.mod_name_is_active(xkb::MOD_NAME_CTRL, xkb::STATE_MODS_EFFECTIVE);
        action(keysym, &self.state.key_get_utf8(code), ctrl)
    }
}

// rules, model, layout, variant, options; evdev/pc105/us when the server
// publishes none.
fn rules_names<C: Connection>(conn: &C, root: Window) -> [String; 5] {
    let mut out = ["evdev", "pc105", "us", "", ""].map(String::from);
    let Ok(atom) = conn
        .intern_atom(true, b"_XKB_RULES_NAMES")
        .map_err(|_| ())
        .and_then(|c| c.reply().map_err(|_| ()))
    else {
        return out;
    };
    let Some(reply) = conn
        .get_property(false, root, atom.atom, AtomEnum::STRING, 0, 1024)
        .ok()
        .and_then(|c| c.reply().ok())
    else {
        return out;
    };
    for (slot, field) in out.iter_mut().zip(reply.value.split(|&b| b == 0)) {
        if !field.is_empty() {
            *slot = String::from_utf8_lossy(field).into_owned();
        }
    }
    out
}
