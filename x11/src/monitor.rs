// X11 monitor query: RandR-based output enumeration. The Monitor +
// Monitors data types live in demiurge_core::monitor and are
// re-exported here so call sites that use crate::monitor::Monitor
// keep compiling. This module only owns the X11-specific
// subscription (select_input via NotifyMask) and the RandR
// enumeration; the wayland implementation provides equivalents
// against wl_output.

use x11rb::connection::Connection;
use x11rb::protocol::randr::{ConnectionExt as RandrExt, NotifyMask};
use x11rb::protocol::xproto::Window;
use x11rb::rust_connection::RustConnection;

pub use demiurge_core::monitor::{Monitor, Monitors};

// Subscribe to RandR notifications on the root. SCREEN_CHANGE fires on
// `xrandr -s` resolution changes; CRTC/OUTPUT_CHANGE catch hot-plug and
// per-output mode swaps. Server-side cost is negligible -- one bit set
// per client.
pub fn select_input(conn: &RustConnection, root: Window) -> Result<(), String> {
    let mask = NotifyMask::SCREEN_CHANGE | NotifyMask::CRTC_CHANGE | NotifyMask::OUTPUT_CHANGE;
    conn.randr_select_input(root, mask)
        .map_err(|e| format!("randr_select_input: {}", e))?
        .check()
        .map_err(|e| format!("randr_select_input check: {}", e))?;
    Ok(())
}

pub fn query(conn: &RustConnection, root: Window) -> Monitors {
    if let Ok(monitors) = query_randr(conn, root) {
        if let Some(m) = Monitors::try_new(monitors) {
            return m;
        }
    }
    let screen = &conn.setup().roots[0];
    Monitors::single(Monitor {
        name: "default".into(),
        x: 0,
        y: 0,
        width: screen.width_in_pixels as u32,
        height: screen.height_in_pixels as u32,
    })
}

fn query_randr(conn: &RustConnection, root: Window) -> Result<Vec<Monitor>, String> {
    let resources = conn
        .randr_get_screen_resources_current(root)
        .map_err(|e| format!("randr: {}", e))?
        .reply()
        .map_err(|e| format!("randr reply: {}", e))?;

    let mut monitors = Vec::new();

    for &output_id in &resources.outputs {
        let output_info = match conn.randr_get_output_info(output_id, 0) {
            Ok(cookie) => match cookie.reply() {
                Ok(info) => info,
                Err(_) => continue,
            },
            Err(_) => continue,
        };

        if output_info.crtc == 0
            || output_info.connection != x11rb::protocol::randr::Connection::CONNECTED
        {
            continue;
        }

        let crtc_info = match conn.randr_get_crtc_info(output_info.crtc, 0) {
            Ok(cookie) => match cookie.reply() {
                Ok(info) => info,
                Err(_) => continue,
            },
            Err(_) => continue,
        };

        monitors.push(Monitor {
            name: String::from_utf8_lossy(&output_info.name).to_string(),
            x: crtc_info.x as i32,
            y: crtc_info.y as i32,
            width: crtc_info.width as u32,
            height: crtc_info.height as u32,
        });
    }

    Ok(monitors)
}
