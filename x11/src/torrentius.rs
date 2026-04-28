// Torrentius (X11): the X11-specific rendering bits, layered on top
// of the server-agnostic primitives in demiurge_core::torrentius.
//
// Server-agnostic primitives (PanelColors, PanelRow, draw_panel,
// panel_height_for, make_font_options, new_surface, parse_hex,
// hex_to_pixel) are re-exported here so callers using
// crate::torrentius::* paths keep compiling untouched. The
// X11-specific functions (capture_root_to_png, capture_region_to_png,
// which use XGetImage / ZPixmap) live here directly.

use std::path::Path;

use x11rb::protocol::xproto::{ConnectionExt as _, ImageFormat, Window};
use x11rb::rust_connection::RustConnection;

// Re-exports: keep crate::torrentius::* compatibility for the x11
// implementation's existing call sites. Some symbols
// (PanelColors/PanelRow/draw_panel/panel_height_for/new_surface) are
// only used by the gordian_knot binary; #[allow(unused_imports)]
// suppresses dead-code lints when the demiurge binary is built alone.
#[allow(unused_imports)]
pub use demiurge_core::torrentius::{
    PanelColors, PanelRow, draw_panel, hex_to_pixel, make_font_options, new_surface,
    panel_height_for, parse_hex,
};

// Grab the full root window as a PNG. Uses X11 ZPixmap get_image, converts
// to cairo's ARGB32 layout (X returns BGRA on little-endian truecolor), and
// writes via cairo's PNG encoder.
pub fn capture_root_to_png(
    conn: &RustConnection,
    root: Window,
    path: &Path,
) -> Result<(), String> {
    let geom = conn
        .get_geometry(root)
        .map_err(|e| format!("get_geometry: {}", e))?
        .reply()
        .map_err(|e| format!("get_geometry reply: {}", e))?;
    capture_region_to_png(conn, root, 0, 0, geom.width as u32, geom.height as u32, path)
}

pub fn capture_region_to_png(
    conn: &RustConnection,
    drawable: Window,
    x: i16,
    y: i16,
    w: u32,
    h: u32,
    path: &Path,
) -> Result<(), String> {
    let reply = conn
        .get_image(ImageFormat::Z_PIXMAP, drawable, x, y, w as u16, h as u16, !0u32)
        .map_err(|e| format!("get_image: {}", e))?
        .reply()
        .map_err(|e| format!("get_image reply: {}", e))?;

    // X11 ZPixmap on modern truecolor visuals is 32-bit little-endian BGRA
    // (or BGRX if depth=24). Cairo's ARGB32 on little-endian is also BGRA
    // in memory. So the pixel layout passes through with an alpha fill.
    let stride =
        cairo::Format::Rgb24.stride_for_width(w).map_err(|e| format!("stride: {}", e))?;
    let mut surface =
        cairo::ImageSurface::create(cairo::Format::Rgb24, w as i32, h as i32)
            .map_err(|e| format!("surface: {}", e))?;

    {
        let mut data = surface.data().map_err(|e| format!("surface data: {}", e))?;
        let src = &reply.data;
        let row_bytes = (w * 4) as usize;
        let stride = stride as usize;
        let h_usize = h as usize;
        if src.len() < row_bytes * h_usize {
            return Err(format!(
                "get_image returned {} bytes, expected {}",
                src.len(),
                row_bytes * h_usize
            ));
        }
        for row in 0..h_usize {
            let src_off = row * row_bytes;
            let dst_off = row * stride;
            data[dst_off..dst_off + row_bytes]
                .copy_from_slice(&src[src_off..src_off + row_bytes]);
        }
    }

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("mkdir {:?}: {}", parent, e))?;
    }
    let mut file = std::fs::File::create(path).map_err(|e| format!("create {:?}: {}", path, e))?;
    surface
        .write_to_png(&mut file)
        .map_err(|e| format!("write_to_png: {}", e))?;
    Ok(())
}
