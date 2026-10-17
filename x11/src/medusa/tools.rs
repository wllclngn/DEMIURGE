// MEDUSA annotation tools.
//
// Phase 2 shipped the two-point shapes (arrow / rectangle / ellipse /
// line). Phase 3 widens the model into an enum so it can also carry
// freehand paths (marker / pencil), text, and blur regions. Every
// variant is keyboard-placeable for the mouseless path and
// mouse-drawable when a pointer is present.
//
// Coordinates are screen space; the overlay translates into the
// cropped region on export.

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ToolKind {
    Arrow,
    Rectangle,
    Ellipse,
    Line,
    Marker,
    Pencil,
    Text,
    Blur,
    // Auto-incrementing numbered step markers (flameshot's
    // circlecount). Single-point placement.
    Counter,
}

impl ToolKind {
    pub fn label(self) -> &'static str {
        match self {
            ToolKind::Arrow => "arrow",
            ToolKind::Rectangle => "rect",
            ToolKind::Ellipse => "ellipse",
            ToolKind::Line => "line",
            ToolKind::Marker => "marker",
            ToolKind::Pencil => "pencil",
            ToolKind::Text => "text",
            ToolKind::Blur => "blur",
            ToolKind::Counter => "counter",
        }
    }

    // Single-point tools place at one location (counter, text).
    pub fn is_single_point(self) -> bool {
        matches!(self, ToolKind::Counter)
    }

    // Two-point tools place an anchor + a second point. Path / text
    // tools place differently (accumulated points / a typed string).
    pub fn is_two_point(self) -> bool {
        matches!(
            self,
            ToolKind::Arrow
                | ToolKind::Rectangle
                | ToolKind::Ellipse
                | ToolKind::Line
                | ToolKind::Blur
        )
    }

    pub fn is_path(self) -> bool {
        matches!(self, ToolKind::Marker | ToolKind::Pencil)
    }
}

#[derive(Clone, Debug)]
pub enum Annotation {
    // Arrow / Rectangle / Ellipse / Line.
    Shape {
        kind: ToolKind,
        x1: i32,
        y1: i32,
        x2: i32,
        y2: i32,
        color: (f64, f64, f64),
        size: f64,
    },
    // Marker (translucent highlighter) or pencil (opaque freehand).
    Path {
        marker: bool,
        points: Vec<(i32, i32)>,
        color: (f64, f64, f64),
        size: f64,
    },
    // A typed label. `size` is the font pixel size.
    Text {
        x: i32,
        y: i32,
        text: String,
        color: (f64, f64, f64),
        size: f64,
    },
    // Pixelate the underlying screenshot in this rectangle. Applied at
    // export against the real pixels; drawn as a frosted box in the
    // interactive preview.
    Blur {
        x1: i32,
        y1: i32,
        x2: i32,
        y2: i32,
    },
    // A numbered step marker -- a filled disc with `n` centered.
    Counter {
        x: i32,
        y: i32,
        n: u32,
        color: (f64, f64, f64),
        size: f64,
    },
}

pub const PALETTE_LEN: usize = 7;

pub fn palette(accent: (f64, f64, f64)) -> [(f64, f64, f64); PALETTE_LEN] {
    [
        accent,
        (0.90, 0.20, 0.20),
        (0.20, 0.75, 0.30),
        (0.25, 0.55, 0.95),
        (0.95, 0.80, 0.20),
        (0.95, 0.95, 0.95),
        (0.05, 0.05, 0.05),
    ]
}

// Render one annotation in screen space. Blur draws a frosted preview
// box here; the real pixelation happens in apply_blur at export.
pub fn draw(cr: &cairo::Context, a: &Annotation, font_opts: &cairo::FontOptions) {
    match a {
        Annotation::Shape {
            kind,
            x1,
            y1,
            x2,
            y2,
            color,
            size,
        } => draw_shape(cr, *kind, *x1, *y1, *x2, *y2, *color, *size),
        Annotation::Path {
            marker,
            points,
            color,
            size,
        } => draw_path(cr, *marker, points, *color, *size),
        Annotation::Text {
            x,
            y,
            text,
            color,
            size,
        } => draw_text(cr, *x, *y, text, *color, *size, font_opts),
        Annotation::Blur { x1, y1, x2, y2 } => {
            let x = (*x1).min(*x2) as f64;
            let y = (*y1).min(*y2) as f64;
            let w = (x2 - x1).abs() as f64;
            let h = (y2 - y1).abs() as f64;
            cr.set_source_rgba(0.5, 0.5, 0.5, 0.55);
            cr.rectangle(x, y, w, h);
            let _ = cr.fill();
        }
        Annotation::Counter {
            x,
            y,
            n,
            color,
            size,
        } => draw_counter(cr, *x, *y, *n, *color, *size, font_opts),
    }
}

fn draw_counter(
    cr: &cairo::Context,
    x: i32,
    y: i32,
    n: u32,
    color: (f64, f64, f64),
    size: f64,
    font_opts: &cairo::FontOptions,
) {
    let radius = (size * 3.0).max(11.0);
    let (cx, cy) = (x as f64, y as f64);
    cr.set_source_rgb(color.0, color.1, color.2);
    cr.arc(cx, cy, radius, 0.0, std::f64::consts::TAU);
    let _ = cr.fill();
    // Number, centered, in a contrasting colour (white on dark, dark
    // on light) chosen by the accent's luminance.
    let lum = 0.299 * color.0 + 0.587 * color.1 + 0.114 * color.2;
    let fg = if lum > 0.6 { 0.05 } else { 0.97 };
    let layout = pangocairo::functions::create_layout(cr);
    let font_desc = pango::FontDescription::from_string(&format!("Sans Bold {}", radius as i32));
    layout.set_font_description(Some(&font_desc));
    pangocairo::functions::context_set_font_options(&layout.context(), Some(font_opts));
    layout.set_text(&n.to_string());
    let (tw, th) = layout.pixel_size();
    cr.set_source_rgb(fg, fg, fg);
    cr.move_to(cx - tw as f64 / 2.0, cy - th as f64 / 2.0);
    pangocairo::functions::show_layout(cr, &layout);
}

fn draw_shape(
    cr: &cairo::Context,
    kind: ToolKind,
    x1: i32,
    y1: i32,
    x2: i32,
    y2: i32,
    color: (f64, f64, f64),
    size: f64,
) {
    cr.set_source_rgb(color.0, color.1, color.2);
    cr.set_line_width(size);
    cr.set_line_cap(cairo::LineCap::Round);
    cr.set_line_join(cairo::LineJoin::Round);
    let (x1, y1, x2, y2) = (x1 as f64, y1 as f64, x2 as f64, y2 as f64);
    match kind {
        ToolKind::Line => {
            cr.move_to(x1, y1);
            cr.line_to(x2, y2);
            let _ = cr.stroke();
        }
        ToolKind::Arrow => {
            cr.move_to(x1, y1);
            cr.line_to(x2, y2);
            let _ = cr.stroke();
            draw_arrowhead(cr, x1, y1, x2, y2, size);
        }
        ToolKind::Rectangle => {
            cr.rectangle(x1.min(x2), y1.min(y2), (x2 - x1).abs(), (y2 - y1).abs());
            let _ = cr.stroke();
        }
        ToolKind::Ellipse => {
            let cx = (x1 + x2) / 2.0;
            let cy = (y1 + y2) / 2.0;
            let rx = ((x2 - x1).abs() / 2.0).max(0.5);
            let ry = ((y2 - y1).abs() / 2.0).max(0.5);
            cr.save().ok();
            cr.translate(cx, cy);
            cr.scale(rx, ry);
            cr.arc(0.0, 0.0, 1.0, 0.0, std::f64::consts::TAU);
            cr.restore().ok();
            let _ = cr.stroke();
        }
        _ => {}
    }
}

fn draw_path(cr: &cairo::Context, marker: bool, points: &[(i32, i32)], color: (f64, f64, f64), size: f64) {
    if points.is_empty() {
        return;
    }
    // Marker is a translucent highlighter; pencil is opaque.
    let alpha = if marker { 0.4 } else { 1.0 };
    let width = if marker { size * 3.0 } else { size };
    cr.set_source_rgba(color.0, color.1, color.2, alpha);
    cr.set_line_width(width);
    cr.set_line_cap(cairo::LineCap::Round);
    cr.set_line_join(cairo::LineJoin::Round);
    let (sx, sy) = points[0];
    cr.move_to(sx as f64, sy as f64);
    for &(px, py) in &points[1..] {
        cr.line_to(px as f64, py as f64);
    }
    if points.len() == 1 {
        // A single point: draw a dot.
        cr.line_to(sx as f64 + 0.1, sy as f64);
    }
    let _ = cr.stroke();
}

fn draw_text(
    cr: &cairo::Context,
    x: i32,
    y: i32,
    text: &str,
    color: (f64, f64, f64),
    size: f64,
    font_opts: &cairo::FontOptions,
) {
    if text.is_empty() {
        return;
    }
    let layout = pangocairo::functions::create_layout(cr);
    let font_desc = pango::FontDescription::from_string(&format!("Sans {}", size as i32));
    layout.set_font_description(Some(&font_desc));
    pangocairo::functions::context_set_font_options(&layout.context(), Some(font_opts));
    layout.set_text(text);
    cr.set_source_rgb(color.0, color.1, color.2);
    cr.move_to(x as f64, y as f64);
    pangocairo::functions::show_layout(cr, &layout);
}

fn draw_arrowhead(cr: &cairo::Context, x1: f64, y1: f64, x2: f64, y2: f64, size: f64) {
    let dx = x2 - x1;
    let dy = y2 - y1;
    let len = (dx * dx + dy * dy).sqrt();
    if len < 1.0 {
        return;
    }
    let (ux, uy) = (dx / len, dy / len);
    let barb = (size * 4.0).max(10.0);
    let angle = 0.5_f64;
    let (ca, sa) = (angle.cos(), angle.sin());
    let (rx1, ry1) = (-ux * ca + uy * sa, -ux * sa - uy * ca);
    let (rx2, ry2) = (-ux * ca - uy * sa, ux * sa - uy * ca);
    cr.move_to(x2, y2);
    cr.line_to(x2 + rx1 * barb, y2 + ry1 * barb);
    cr.move_to(x2, y2);
    cr.line_to(x2 + rx2 * barb, y2 + ry2 * barb);
    let _ = cr.stroke();
}

// Pixelate a rectangular region of an Rgb24 surface in place: average
// each BLOCK x BLOCK cell and fill it flat. Used at export for every
// Blur annotation, applied to the cropped image.
pub fn apply_blur(surface: &mut cairo::ImageSurface, x: i32, y: i32, w: i32, h: i32) {
    const BLOCK: i32 = 12;
    let sw = surface.width();
    let sh = surface.height();
    let stride = surface.stride();
    let x0 = x.max(0);
    let y0 = y.max(0);
    let x1 = (x + w).min(sw);
    let y1 = (y + h).min(sh);
    if x1 <= x0 || y1 <= y0 {
        return;
    }
    let mut data = match surface.data() {
        Ok(d) => d,
        Err(_) => return,
    };
    let stride = stride as usize;
    let mut by = y0;
    while by < y1 {
        let mut bx = x0;
        while bx < x1 {
            let cell_x1 = (bx + BLOCK).min(x1);
            let cell_y1 = (by + BLOCK).min(y1);
            // Average the cell.
            let (mut rs, mut gs, mut bs, mut n) = (0u64, 0u64, 0u64, 0u64);
            for py in by..cell_y1 {
                let row = py as usize * stride;
                for px in bx..cell_x1 {
                    let o = row + px as usize * 4;
                    bs += data[o] as u64;
                    gs += data[o + 1] as u64;
                    rs += data[o + 2] as u64;
                    n += 1;
                }
            }
            if n > 0 {
                let (r, g, b) = ((rs / n) as u8, (gs / n) as u8, (bs / n) as u8);
                for py in by..cell_y1 {
                    let row = py as usize * stride;
                    for px in bx..cell_x1 {
                        let o = row + px as usize * 4;
                        data[o] = b;
                        data[o + 1] = g;
                        data[o + 2] = r;
                    }
                }
            }
            bx += BLOCK;
        }
        by += BLOCK;
    }
}
