// The title bar: A strip with macOS traffic lights at the left (close,
// minimize, zoom) and the title centered. DEMIURGE draws it on the frames of
// floating windows, and GNOSIS on itself when no window manager frames it.
// Plain Cairo, so every surface that carries it draws the same bar.

pub const HEIGHT: u32 = 30;

const LIGHT_R: f64 = 6.0;
const LIGHT_Y: f64 = HEIGHT as f64 / 2.0;
const LIGHT_X: [f64; 3] = [18.0, 38.0, 58.0];
const LIGHT_COLORS: [(f64, f64, f64); 3] = [(0.99, 0.32, 0.30), (0.99, 0.74, 0.18), (0.18, 0.78, 0.25)];
// An unfocused window's lights go grey, as on macOS.
const LIGHT_IDLE: (f64, f64, f64) = (0.36, 0.36, 0.36);
// The title stays clear of the lights on both sides, so it centers on the
// window.
const TITLE_INSET: f64 = 76.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Light {
    Close,
    Minimize,
    Zoom,
}

pub struct Colors {
    pub bg: (f64, f64, f64),
    pub fg: (f64, f64, f64),
}

// Draw the bar across the top `HEIGHT` pixels of a surface `w` wide.
pub fn draw(cr: &cairo::Context, layout: &pango::Layout, w: f64, title: &str, active: bool, colors: &Colors) {
    let lighten = |c: f64| (c + 0.06).min(1.0);
    let h = HEIGHT as f64;
    cr.set_source_rgb(lighten(colors.bg.0), lighten(colors.bg.1), lighten(colors.bg.2));
    cr.rectangle(0.0, 0.0, w, h);
    let _ = cr.fill();
    cr.set_source_rgba(0.0, 0.0, 0.0, 0.25);
    cr.rectangle(0.0, h - 1.0, w, 1.0);
    let _ = cr.fill();

    let tau = std::f64::consts::PI * 2.0;
    for (&cx, &color) in LIGHT_X.iter().zip(LIGHT_COLORS.iter()) {
        let (r, g, b) = if active { color } else { LIGHT_IDLE };
        cr.arc(cx, LIGHT_Y, LIGHT_R, 0.0, tau);
        cr.set_source_rgb(r, g, b);
        let _ = cr.fill();
        cr.arc(cx, LIGHT_Y, LIGHT_R, 0.0, tau);
        cr.set_source_rgba(0.0, 0.0, 0.0, 0.18);
        cr.set_line_width(1.0);
        let _ = cr.stroke();
    }

    let room = w - TITLE_INSET * 2.0;
    if room < 1.0 {
        return;
    }
    layout.set_width((room * pango::SCALE as f64) as i32);
    layout.set_ellipsize(pango::EllipsizeMode::End);
    layout.set_alignment(pango::Alignment::Center);
    layout.set_text(title);
    let (_, th) = layout.pixel_size();
    let dim = if active { 1.0 } else { 0.6 };
    cr.set_source_rgb(colors.fg.0 * dim, colors.fg.1 * dim, colors.fg.2 * dim);
    cr.move_to(TITLE_INSET, (h - th as f64) / 2.0);
    pangocairo::functions::show_layout(cr, layout);
}

// The light under a point in the bar's own coordinates, with a little slack
// around each circle.
pub fn light_at(x: f64, y: f64) -> Option<Light> {
    let reach = (LIGHT_R + 1.5) * (LIGHT_R + 1.5);
    let hit = LIGHT_X.iter().position(|&cx| (x - cx) * (x - cx) + (y - LIGHT_Y) * (y - LIGHT_Y) <= reach)?;
    Some([Light::Close, Light::Minimize, Light::Zoom][hit])
}
