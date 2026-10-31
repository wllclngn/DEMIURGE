// What a key does to the password field, for every lock: Each decodes its
// keys through xkbcommon (the X11 lock with the server's own layout, the
// Wayland lock with the compositor's keymap) and maps the keysym here.

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
