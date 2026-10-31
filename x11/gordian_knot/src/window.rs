// The renderer's X11 output: The lock window the holder made, drawn a panel
// at a time with one checked put_image, the panels centered on the primary
// monitor.

use std::os::fd::{AsRawFd, RawFd};

use gordian_knot_core::ui::Output;
use x11rb::connection::Connection;
use x11rb::protocol::Event;
use x11rb::protocol::randr::ConnectionExt as _;
use x11rb::protocol::xproto::*;
use x11rb::rust_connection::RustConnection;

pub struct LockWindow {
    conn: RustConnection,
    win: Window,
    gc: Gcontext,
    depth: u8,
    rect: (i32, i32, i32, i32),
}

impl LockWindow {
    pub fn open(win: Window) -> Result<Self, String> {
        let (conn, screen_num) = RustConnection::connect(None).map_err(|e| format!("X connect: {}", e))?;
        let screen = &conn.setup().roots[screen_num];
        let (root, depth) = (screen.root, screen.root_depth);
        let geo = conn
            .get_geometry(win)
            .map_err(|e| e.to_string())?
            .reply()
            .map_err(|e| format!("lock window: {}", e))?;
        let rect = primary_monitor(&conn, root).unwrap_or((0, 0, geo.width as i32, geo.height as i32));
        let gc = conn.generate_id().map_err(|e| e.to_string())?;
        conn.create_gc(gc, win, &CreateGCAux::new()).map_err(|e| e.to_string())?;
        conn.change_window_attributes(win, &ChangeWindowAttributesAux::new().event_mask(EventMask::EXPOSURE))
            .map_err(|e| e.to_string())?;
        Ok(LockWindow { conn, win, gc, depth, rect })
    }
}

impl Output for LockWindow {
    fn rect(&self) -> (i32, i32, i32, i32) {
        self.rect
    }

    fn put(&mut self, data: &[u8], _stride: usize, (x, y, w, h): (i32, i32, i32, i32)) -> Result<(), String> {
        self.conn
            .put_image(ImageFormat::Z_PIXMAP, self.win, self.gc, w as u16, h as u16, x as i16, y as i16, 0, self.depth, data)
            .map_err(|e| format!("put_image: {}", e))
            .and_then(|c| c.check().map_err(|e| format!("put_image refused: {}", e)))
    }

    fn fd(&self) -> RawFd {
        self.conn.stream().as_raw_fd()
    }

    fn exposed(&mut self) -> Result<bool, String> {
        let mut exposed = false;
        loop {
            match self.conn.poll_for_event() {
                Ok(Some(Event::Expose(ex))) if ex.count == 0 => exposed = true,
                Ok(Some(_)) => {}
                Ok(None) => return Ok(exposed),
                Err(e) => return Err(format!("X connection: {}", e)),
            }
        }
    }
}

// The primary RandR output's rectangle, so the panels center on one
// monitor instead of between two.
fn primary_monitor(conn: &RustConnection, root: Window) -> Option<(i32, i32, i32, i32)> {
    let out = conn.randr_get_output_primary(root).ok()?.reply().ok()?.output;
    if out == 0 {
        return None;
    }
    let info = conn.randr_get_output_info(out, 0).ok()?.reply().ok()?;
    if info.crtc == 0 {
        return None;
    }
    let c = conn.randr_get_crtc_info(info.crtc, 0).ok()?.reply().ok()?;
    (c.width > 0 && c.height > 0).then_some((c.x as i32, c.y as i32, c.width as i32, c.height as i32))
}
