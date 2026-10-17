// XSETTINGS manager.
//
// DEMIURGE owns the _XSETTINGS_S<screen> selection and serves the
// XSETTINGS property blob, so GTK / Qt apps pick up the theme, icon
// theme, font, and cursor live -- the job xsettingsd does in the
// awesome-era stack. With this in place, xsettingsd is unnecessary:
// [appearance] / [cursor] / [font] are the single source of truth and
// changes re-publish on `reload_config`, re-theming running apps
// without a restart.
//
// Protocol: https://specifications.freedesktop.org/xsettings-spec/0.5/
//
// Wire format of the _XSETTINGS_SETTINGS property:
//   CARD8   byte-order (0 = MSB, 1 = LSB)
//   CARD8   pad[3]
//   CARD32  serial
//   CARD32  n_settings
//   SETTING settings[n_settings]
// where each SETTING is:
//   CARD8   type (0 = integer, 1 = string, 2 = color)
//   CARD8   pad
//   CARD16  name-len
//   STRING8 name             (name-len bytes, then pad to 4)
//   CARD32  last-change-serial
//   VALUE   value            (int: INT32; string: CARD32 len + bytes
//                             + pad to 4; color: 4x CARD16)

use x11rb::connection::Connection;
use x11rb::protocol::xproto::*;
use x11rb::rust_connection::RustConnection;

use crate::config::Config;

pub struct XSettings {
    // Own connection, independent of the WM's primary one. The
    // selection-owner window lives as long as this connection (and the
    // process), which is exactly the manager's required lifetime.
    conn: RustConnection,
    win: Window,
    settings_atom: Atom,
    serial: u32,
}

impl XSettings {
    // Acquire the _XSETTINGS_S<screen> selection on a fresh connection.
    // Returns Err if another XSETTINGS manager (e.g. a stray
    // xsettingsd) already owns it -- the caller logs and continues
    // without live theming.
    pub fn acquire() -> Result<Self, String> {
        let (conn, screen_num) =
            RustConnection::connect(None).map_err(|e| format!("x11 connect: {}", e))?;
        let screen = &conn.setup().roots[screen_num];
        let root = screen.root;

        let sel_atom = intern(&conn, &format!("_XSETTINGS_S{}", screen_num))?;
        let settings_atom = intern(&conn, "_XSETTINGS_SETTINGS")?;
        let manager_atom = intern(&conn, "MANAGER")?;

        // A 1x1 unmapped window holds the selection + the settings
        // property. PROPERTY_CHANGE so we'd notice if asked, though we
        // only ever write our own property.
        let win = conn
            .generate_id()
            .map_err(|e| format!("generate_id: {}", e))?;
        conn.create_window(
            screen.root_depth,
            win,
            root,
            0,
            0,
            1,
            1,
            0,
            WindowClass::INPUT_OUTPUT,
            screen.root_visual,
            &CreateWindowAux::new().event_mask(EventMask::PROPERTY_CHANGE),
        )
        .map_err(|e| format!("create_window: {}", e))?;

        conn.set_selection_owner(win, sel_atom, x11rb::CURRENT_TIME)
            .map_err(|e| format!("set_selection_owner: {}", e))?;

        let owner = conn
            .get_selection_owner(sel_atom)
            .map_err(|e| format!("get_selection_owner: {}", e))?
            .reply()
            .map_err(|e| format!("get_selection_owner reply: {}", e))?
            .owner;
        if owner != win {
            let _ = conn.destroy_window(win);
            let _ = conn.flush();
            return Err("another XSETTINGS manager already owns the selection".into());
        }

        // Announce ownership to the root window per ICCCM manager
        // selection convention: ClientMessage MANAGER { time, selection,
        // owner-window }.
        let data = ClientMessageData::from([x11rb::CURRENT_TIME, sel_atom, win, 0, 0]);
        let ev = ClientMessageEvent::new(32, root, manager_atom, data);
        let _ = conn.send_event(false, root, EventMask::STRUCTURE_NOTIFY, ev);
        let _ = conn.flush();

        Ok(Self {
            conn,
            win,
            settings_atom,
            serial: 0,
        })
    }

    // Serialize the current [appearance] / [cursor] / [font] into the
    // XSETTINGS blob and replace the property. Bumping the serial is
    // what tells clients to re-read. Call once at startup and again on
    // every reload_config.
    pub fn publish(&mut self, cfg: &Config) -> Result<(), String> {
        let blob = serialize(cfg, self.serial);
        self.conn
            .change_property(
                PropMode::REPLACE,
                self.win,
                self.settings_atom,
                self.settings_atom,
                8,
                blob.len() as u32,
                &blob,
            )
            .map_err(|e| format!("change_property: {}", e))?;
        self.conn.flush().map_err(|e| format!("flush: {}", e))?;
        self.serial = self.serial.wrapping_add(1);
        Ok(())
    }
}

// One setting value. Color is parsed from a "#RRGGBB" hex.
enum Value {
    Int(i32),
    Str(String),
    Color(u16, u16, u16, u16),
}

// Build the list of settings DEMIURGE serves. Font + cursor always
// have values (config defaults); theme / icon names only when set in
// [appearance] (empty = leave the GTK default in place).
fn settings(cfg: &Config) -> Vec<(&'static str, Value)> {
    let mut s: Vec<(&'static str, Value)> = Vec::new();
    s.push(("Gtk/FontName", Value::Str(cfg.font.default.clone())));
    s.push(("Gtk/CursorThemeName", Value::Str(cfg.cursor.theme.clone())));
    s.push(("Gtk/CursorThemeSize", Value::Int(cfg.cursor.size as i32)));
    if !cfg.appearance.gtk_theme.is_empty() {
        s.push(("Net/ThemeName", Value::Str(cfg.appearance.gtk_theme.clone())));
    }
    if !cfg.appearance.icon_theme.is_empty() {
        s.push((
            "Net/IconThemeName",
            Value::Str(cfg.appearance.icon_theme.clone()),
        ));
    }
    // The shared accent, exposed as a color in case a theme or app
    // wants to read it. Harmless if ignored.
    let (r, g, b) = crate::torrentius::parse_hex(&cfg.appearance.accent);
    s.push((
        "DEMIURGE/Accent",
        Value::Color(
            (r * 65535.0) as u16,
            (g * 65535.0) as u16,
            (b * 65535.0) as u16,
            65535,
        ),
    ));
    s
}

pub fn serialize(cfg: &Config, serial: u32) -> Vec<u8> {
    let items = settings(cfg);
    let mut buf: Vec<u8> = Vec::with_capacity(64 + items.len() * 32);

    buf.push(1); // LSBFirst -- DEMIURGE only targets little-endian
    buf.extend_from_slice(&[0, 0, 0]); // pad
    buf.extend_from_slice(&serial.to_le_bytes());
    buf.extend_from_slice(&(items.len() as u32).to_le_bytes());

    for (name, value) in &items {
        let ty: u8 = match value {
            Value::Int(_) => 0,
            Value::Str(_) => 1,
            Value::Color(..) => 2,
        };
        buf.push(ty);
        buf.push(0); // pad
        let name_bytes = name.as_bytes();
        buf.extend_from_slice(&(name_bytes.len() as u16).to_le_bytes());
        buf.extend_from_slice(name_bytes);
        pad_to_4(&mut buf);
        // last-change-serial: the serial this setting last changed at.
        // We treat the whole blob as changing together, so the current
        // serial is correct for every entry.
        buf.extend_from_slice(&serial.to_le_bytes());
        match value {
            Value::Int(i) => buf.extend_from_slice(&i.to_le_bytes()),
            Value::Str(st) => {
                let sb = st.as_bytes();
                buf.extend_from_slice(&(sb.len() as u32).to_le_bytes());
                buf.extend_from_slice(sb);
                pad_to_4(&mut buf);
            }
            Value::Color(r, g, b, a) => {
                buf.extend_from_slice(&r.to_le_bytes());
                buf.extend_from_slice(&g.to_le_bytes());
                buf.extend_from_slice(&b.to_le_bytes());
                buf.extend_from_slice(&a.to_le_bytes());
            }
        }
    }
    buf
}

fn pad_to_4(buf: &mut Vec<u8>) {
    while buf.len() % 4 != 0 {
        buf.push(0);
    }
}

fn intern(conn: &RustConnection, name: &str) -> Result<Atom, String> {
    conn.intern_atom(false, name.as_bytes())
        .map_err(|e| format!("intern_atom {}: {}", name, e))?
        .reply()
        .map_err(|e| format!("intern_atom {} reply: {}", name, e))
        .map(|r| r.atom)
}
