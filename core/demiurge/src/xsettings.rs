// XSETTINGS: The settings DEMIURGE serves to X11 GTK and Qt apps and the
// _XSETTINGS_SETTINGS property they are written into, shared by the X11
// stack's manager and demiurge-wl's on XWayland.
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

use crate::config::Config;

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
