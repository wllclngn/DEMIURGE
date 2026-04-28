// Torrentius (core): the rendering subsystem's server-agnostic parts.
//
// Named for Johannes Torrentius, the 17th-century Dutch painter who
// reportedly used a camera obscura -- a dark chamber that projects
// the outside world as a visible image. This module is DEMIURGE's
// equivalent apparatus: light rendered into visible form (Cairo
// surfaces, bar glyphs).
//
// What's here: pure Cairo + Pango primitives that are blit-target-
// agnostic. Both the X11 implementation (XPutImage from a Cairo
// surface into an XImage) and the Wayland implementation (Cairo
// surface uploaded to a wl_shm buffer or a GL texture) consume them
// unchanged.
//
// What's *not* here: protocol-specific I/O. X11's get_image-based
// capture lives in `demiurge-x11`'s torrentius module; the Wayland
// equivalent (zwlr_screencopy or session-lock screenshot mechanisms)
// lands in `demiurge-wl` when those features are added.

// Colors for a framed panel. RGB triples in [0.0, 1.0].
// Used by GORDIAN KNOT's lock screen, not by the bar.
#[derive(Clone, Copy)]
pub struct PanelColors {
    pub bg: (f64, f64, f64),
    pub fg: (f64, f64, f64),
    pub accent: (f64, f64, f64),
    pub border: (f64, f64, f64),
}

// Two-column row. label on the left, value right-aligned.
pub struct PanelRow<'a> {
    pub label: &'a str,
    pub value: &'a str,
}

// Render a montauk-style framed panel: thin border, titled top-break label
// in the accent color, left-aligned labels + right-aligned values. Font is
// whatever the caller set on `layout` via set_font_description beforehand.
// (x, y) is the top-left of the panel; (w, h) its size in pixels.
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
    cr.set_source_rgb(colors.bg.0, colors.bg.1, colors.bg.2);
    cr.rectangle(x, y, w, h);
    let _ = cr.fill();

    cr.set_source_rgb(colors.border.0, colors.border.1, colors.border.2);
    cr.set_line_width(1.0);
    cr.rectangle(x + 0.5, y + 0.5, w - 1.0, h - 1.0);
    let _ = cr.stroke();

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

// Parse "#RRGGBB" into the 0xRRGGBB packed value. X11 takes this as
// background_pixel; Wayland takes it (with 0xFF000000 OR'd in for
// alpha) as the RGB channels of an ARGB32 wl_shm buffer. Same bit
// layout in both cases.
pub fn hex_to_pixel(color: &str) -> u32 {
    let s = color.trim_start_matches('#');
    u32::from_str_radix(s, 16).unwrap_or(0)
}
