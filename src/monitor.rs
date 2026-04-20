use x11rb::connection::Connection;
use x11rb::protocol::randr::ConnectionExt as RandrExt;
use x11rb::protocol::xproto::Window;
use x11rb::rust_connection::RustConnection;

#[derive(Debug, Clone)]
pub struct Monitor {
    pub name: String,
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

pub fn query(conn: &RustConnection, root: Window) -> Vec<Monitor> {
    match query_randr(conn, root) {
        Ok(monitors) if !monitors.is_empty() => monitors,
        _ => {
            let screen = &conn.setup().roots[0];
            vec![Monitor {
                name: "default".into(),
                x: 0,
                y: 0,
                width: screen.width_in_pixels as u32,
                height: screen.height_in_pixels as u32,
            }]
        }
    }
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
