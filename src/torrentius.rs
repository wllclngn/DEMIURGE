// Torrentius: the rendering subsystem.
//
// Named for Johannes Torrentius, the 17th-century Dutch painter who reportedly
// used a camera obscura -- a dark chamber that projects the outside world as
// a visible image. This module is DEMIURGE's equivalent apparatus: light (X11
// pixel buffers) rendered into visible form (Cairo surfaces, bar glyphs,
// screenshots). Every drawing primitive in the WM lives here.

use std::path::Path;

use x11rb::protocol::xproto::{ConnectionExt as _, ImageFormat, Window};
use x11rb::rust_connection::RustConnection;

// Colors for a framed panel. RGB triples in [0.0, 1.0].
// Used by gordian_knot's X11 lock screen, not by demiurge's bar.
#[allow(dead_code)]
#[derive(Clone, Copy)]
pub struct PanelColors {
    pub bg: (f64, f64, f64),
    pub fg: (f64, f64, f64),
    pub accent: (f64, f64, f64),
    pub border: (f64, f64, f64),
}

// Two-column row. label on the left, value right-aligned.
#[allow(dead_code)]
pub struct PanelRow<'a> {
    pub label: &'a str,
    pub value: &'a str,
}

// Render a montauk-style framed panel: thin border, titled top-break label
// in the accent color, left-aligned labels + right-aligned values. Font is
// whatever the caller set on `layout` via set_font_description beforehand.
// (x, y) is the top-left of the panel; (w, h) its size in pixels.
#[allow(dead_code)]
pub fn draw_panel(
    cr: &cairo::Context,
    layout: &pango::Layout,
    x: f64,
    y: f64,
    w: f64,
    h: f64,
    label: &str,
    rows: &[PanelRow<'_>],
    colors: &PanelColors,
) {
    // Panel background (subtle if it differs from page bg; harmless if same).
    cr.set_source_rgb(colors.bg.0, colors.bg.1, colors.bg.2);
    cr.rectangle(x, y, w, h);
    let _ = cr.fill();

    // Border, 1 px, drawn inside the rect.
    cr.set_source_rgb(colors.border.0, colors.border.1, colors.border.2);
    cr.set_line_width(1.0);
    cr.rectangle(x + 0.5, y + 0.5, w - 1.0, h - 1.0);
    let _ = cr.stroke();

    // Label: "─── SYSTEM ───" floating over the top border, in accent color.
    // Rendered by punching a gap in the border with a bg-colored rect, then
    // drawing the label text on top of that gap.
    layout.set_text(&format!(" {} ", label));
    let (label_w, label_h) = layout.pixel_size();
    let label_x = x + 20.0;
    let label_y = y - (label_h as f64 / 2.0);
    cr.set_source_rgb(colors.bg.0, colors.bg.1, colors.bg.2);
    cr.rectangle(label_x, label_y, label_w as f64, label_h as f64);
    let _ = cr.fill();
    cr.set_source_rgb(colors.accent.0, colors.accent.1, colors.accent.2);
    cr.move_to(label_x, label_y);
    pangocairo::functions::show_layout(cr, layout);

    // Rows. Label left-aligned, value right-aligned, same row baseline.
    // Row pitch derived from layout's line height for the current font.
    layout.set_text("Ag");
    let (_, row_h) = layout.pixel_size();
    let row_pitch = (row_h as f64) * 1.25;
    let pad_x = 16.0;
    let pad_top = 12.0;

    cr.set_source_rgb(colors.fg.0, colors.fg.1, colors.fg.2);
    for (i, row) in rows.iter().enumerate() {
        let ry = y + pad_top + (i as f64) * row_pitch;

        layout.set_text(row.label);
        cr.move_to(x + pad_x, ry);
        pangocairo::functions::show_layout(cr, layout);

        layout.set_text(row.value);
        let (vw, _) = layout.pixel_size();
        cr.move_to(x + w - pad_x - (vw as f64), ry);
        pangocairo::functions::show_layout(cr, layout);
    }
}

// Compute the pixel height a panel needs to hold `n` rows with the given
// font (set on layout). Useful for stacking panels without hardcoding.
#[allow(dead_code)]
pub fn panel_height_for(layout: &pango::Layout, rows: usize) -> f64 {
    layout.set_text("Ag");
    let (_, row_h) = layout.pixel_size();
    let row_pitch = (row_h as f64) * 1.25;
    let pad_top = 12.0;
    let pad_bot = 12.0;
    pad_top + (rows as f64) * row_pitch + pad_bot
}

// Cairo font options configured for opaque-RGB24 subpixel anti-aliasing.
// Matches the subpixel_text.lua approach that previously lived in AwesomeWM.
pub fn make_font_options() -> Result<cairo::FontOptions, String> {
    let mut opts =
        cairo::FontOptions::new().map_err(|_| "cairo font options failed".to_string())?;
    opts.set_antialias(cairo::Antialias::Subpixel);
    opts.set_subpixel_order(cairo::SubpixelOrder::Rgb);
    opts.set_hint_style(cairo::HintStyle::Slight);
    opts.set_hint_metrics(cairo::HintMetrics::On);
    Ok(opts)
}

// Allocate a fresh opaque RGB24 surface + context sized to w x h pixels.
// Returns Err on allocation failure; callers typically `continue` on err.
pub fn new_surface(w: i32, h: i32) -> Result<(cairo::ImageSurface, cairo::Context), String> {
    let surface = cairo::ImageSurface::create(cairo::Format::Rgb24, w, h)
        .map_err(|e| format!("cairo surface {}x{}: {}", w, h, e))?;
    let cr = cairo::Context::new(&surface).map_err(|e| format!("cairo context: {}", e))?;
    Ok((surface, cr))
}

// Parse "#RRGGBB" (leading '#' optional) into normalized (r, g, b) floats
// suitable for cairo's set_source_rgb. Invalid input yields black.
pub fn parse_hex(color: &str) -> (f64, f64, f64) {
    let s = color.trim_start_matches('#');
    if s.len() < 6 {
        return (0.0, 0.0, 0.0);
    }
    let r = u8::from_str_radix(&s[0..2], 16).unwrap_or(0) as f64 / 255.0;
    let g = u8::from_str_radix(&s[2..4], 16).unwrap_or(0) as f64 / 255.0;
    let b = u8::from_str_radix(&s[4..6], 16).unwrap_or(0) as f64 / 255.0;
    (r, g, b)
}

// Parse "#RRGGBB" into the 0xRRGGBB pixel value X11 wants for background_pixel.
pub fn hex_to_pixel(color: &str) -> u32 {
    let s = color.trim_start_matches('#');
    u32::from_str_radix(s, 16).unwrap_or(0)
}

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
