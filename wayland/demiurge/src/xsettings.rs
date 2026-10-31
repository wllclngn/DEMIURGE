// The settings GTK and Qt apps read, from DEMIURGE's [font], [cursor] and
// [appearance], as the X11 stack serves them: On XWayland the same
// XSETTINGS manager, owning _XSETTINGS_S0 for the X11 apps
// (demiurge_core::xsettings holds the settings and their wire format);
// for Wayland GTK apps, which read GSettings rather than XSETTINGS,
// org.gnome.desktop.interface's keys, written as the real screen's
// session starts.

use smithay::reexports::x11rb;
use x11rb::connection::Connection;
use x11rb::protocol::xproto::*;
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as _;

use demiurge_core::config::Config;

pub struct XSettings {
    // A connection of its own, whose window holds the selection for as long
    // as the compositor runs.
    _conn: RustConnection,
}

impl XSettings {
    /// Own _XSETTINGS_S0 on XWayland's `display` and serve `blob`, the
    /// _XSETTINGS_SETTINGS property.
    pub fn serve(display: u32, blob: &[u8]) -> Result<Self, String> {
        let (conn, screen_num) = RustConnection::connect(Some(&format!(":{display}"))).map_err(|e| e.to_string())?;
        let screen = &conn.setup().roots[screen_num];
        let (root, depth, visual) = (screen.root, screen.root_depth, screen.root_visual);
        let atom = |name: &str| -> Result<Atom, String> {
            let cookie = conn.intern_atom(false, name.as_bytes()).map_err(|e| e.to_string())?;
            Ok(cookie.reply().map_err(|e| e.to_string())?.atom)
        };
        let selection = atom(&format!("_XSETTINGS_S{screen_num}"))?;
        let settings = atom("_XSETTINGS_SETTINGS")?;
        let manager = atom("MANAGER")?;

        let win = conn.generate_id().map_err(|e| e.to_string())?;
        conn.create_window(depth, win, root, 0, 0, 1, 1, 0, WindowClass::INPUT_OUTPUT, visual, &CreateWindowAux::new())
            .map_err(|e| format!("create_window: {e}"))?;
        conn.change_property8(PropMode::REPLACE, win, settings, settings, blob)
            .map_err(|e| format!("change_property: {e}"))?;
        conn.set_selection_owner(win, selection, x11rb::CURRENT_TIME)
            .map_err(|e| format!("set_selection_owner: {e}"))?;
        let owner = conn
            .get_selection_owner(selection)
            .map_err(|e| e.to_string())?
            .reply()
            .map_err(|e| e.to_string())?
            .owner;
        if owner != win {
            return Err("another XSETTINGS manager owns the selection".into());
        }
        // ICCCM's manager announcement: MANAGER { time, selection, owner }.
        let data = ClientMessageData::from([x11rb::CURRENT_TIME, selection, win, 0, 0]);
        let _ = conn.send_event(false, root, EventMask::STRUCTURE_NOTIFY, ClientMessageEvent::new(32, root, manager, data));
        conn.flush().map_err(|e| e.to_string())?;
        Ok(Self { _conn: conn })
    }
}

/// The `gsettings set` commands carrying `cfg` to Wayland GTK apps, in
/// org.gnome.desktop.interface; a theme only where [appearance] names one.
pub fn gsettings_commands(cfg: &Config) -> Vec<String> {
    let mut keys = vec![
        ("font-name", cfg.font.default.clone()),
        ("cursor-theme", cfg.cursor.theme.clone()),
        ("cursor-size", cfg.cursor.size.to_string()),
    ];
    if !cfg.appearance.gtk_theme.is_empty() {
        keys.push(("gtk-theme", cfg.appearance.gtk_theme.clone()));
    }
    if !cfg.appearance.icon_theme.is_empty() {
        keys.push(("icon-theme", cfg.appearance.icon_theme.clone()));
    }
    keys.into_iter()
        .map(|(key, value)| format!("gsettings set org.gnome.desktop.interface {key} {}", quoted(&value)))
        .collect()
}

// `value` as one word to sh.
fn quoted(value: &str) -> String {
    format!("'{}'", value.replace('\'', r"'\''"))
}
