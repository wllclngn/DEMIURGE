// The tag menu's X11 window: core's menu (demiurge_core::tag_menu) in an
// override-redirect window above everything. While it is open DEMIURGE
// holds the keyboard and the pointer, the way the run prompt holds the
// keyboard.

use std::ops::{Deref, DerefMut};

use x11rb::connection::Connection;
use x11rb::protocol::xproto::*;
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as _;

pub use demiurge_core::tag_menu::{Entry, Menu, Style};

// The name the menu window carries, so it can be found from outside.
pub const WINDOW_NAME: &str = "demiurge-tag-menu";

pub struct TagMenu {
    pub window: Window,
    gc: Gcontext,
    depth: u8,
    menu: Menu,
}

impl Deref for TagMenu {
    type Target = Menu;

    fn deref(&self) -> &Menu {
        &self.menu
    }
}

impl DerefMut for TagMenu {
    fn deref_mut(&mut self) -> &mut Menu {
        &mut self.menu
    }
}

impl TagMenu {
    // Open the menu with its top-left corner at `at`, kept inside the
    // monitor spanning `bounds` (x, width).
    pub fn open(
        conn: &RustConnection,
        root: Window,
        style: &Style,
        at: (i32, i32),
        bounds: (i32, u32),
        entries: Vec<Entry>,
        selected: Option<usize>,
    ) -> Result<Self, String> {
        let menu = Menu::new(style, at, bounds, entries, selected)?;
        let screen = &conn.setup().roots[0];
        let depth = screen.root_depth;
        let window = conn.generate_id().map_err(|e| format!("tag menu id: {e}"))?;
        let gc = conn.generate_id().map_err(|e| format!("tag menu gc id: {e}"))?;
        conn.create_window(
            depth,
            window,
            root,
            menu.x as i16,
            menu.y as i16,
            menu.w as u16,
            menu.height() as u16,
            0,
            WindowClass::INPUT_OUTPUT,
            0,
            &CreateWindowAux::new()
                .override_redirect(1)
                .background_pixel(screen.black_pixel)
                .event_mask(EventMask::EXPOSURE),
        )
        .map_err(|e| format!("create tag menu: {e}"))?;
        let _ = conn.change_property8(PropMode::REPLACE, window, AtomEnum::WM_NAME, AtomEnum::STRING, WINDOW_NAME.as_bytes());
        conn.create_gc(gc, window, &CreateGCAux::new()).map_err(|e| format!("tag menu gc: {e}"))?;
        let _ = conn.map_window(window);
        let _ = conn.configure_window(window, &ConfigureWindowAux::new().stack_mode(StackMode::ABOVE));

        let menu = Self { window, gc, depth, menu };
        menu.draw(conn, style);
        Ok(menu)
    }

    pub fn draw(&self, conn: &RustConnection, style: &Style) {
        let Ok(surface) = self.menu.draw(style) else {
            return;
        };
        let (w, h) = (self.menu.w, self.menu.height());
        let stride = surface.stride() as usize;
        let row_bytes = w as usize * 4;
        let _ = surface.with_data(|data| {
            let mut buf = Vec::with_capacity(row_bytes * h as usize);
            for row in 0..h as usize {
                buf.extend_from_slice(&data[row * stride..row * stride + row_bytes]);
            }
            let _ = conn.put_image(ImageFormat::Z_PIXMAP, self.window, self.gc, w as u16, h as u16, 0, 0, 0, self.depth, &buf);
        });
        let _ = conn.flush();
    }

    pub fn close(&self, conn: &RustConnection) {
        let _ = conn.free_gc(self.gc);
        let _ = conn.destroy_window(self.window);
    }
}
