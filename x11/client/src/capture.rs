// The screen read back through XGetImage: The window manager's Screenshot
// and MEDUSA's frozen backdrop.

use std::path::Path;

use x11rb::protocol::xproto::{ConnectionExt as _, ImageFormat, Window};
use x11rb::rust_connection::RustConnection;

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
    let surface = capture_region_to_surface(conn, drawable, x, y, w, h)?;
    demiurge_core::torrentius::surface_to_png(&surface, path)
}

// Grab a region as an in-memory Cairo Rgb24 surface. MEDUSA uses this
// to hold the frozen screenshot for dimming + cropping; the PNG
// variant above is the thin "capture then write" wrapper.
pub fn capture_region_to_surface(
    conn: &RustConnection,
    drawable: Window,
    x: i16,
    y: i16,
    w: u32,
    h: u32,
) -> Result<cairo::ImageSurface, String> {
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
    Ok(surface)
}

// Grab the whole root as an in-memory surface. The MEDUSA overlay's
// frozen backdrop.
pub fn capture_root_to_surface(
    conn: &RustConnection,
    root: Window,
) -> Result<cairo::ImageSurface, String> {
    let geom = conn
        .get_geometry(root)
        .map_err(|e| format!("get_geometry: {}", e))?
        .reply()
        .map_err(|e| format!("get_geometry reply: {}", e))?;
    capture_region_to_surface(conn, root, 0, 0, geom.width as u32, geom.height as u32)
}
