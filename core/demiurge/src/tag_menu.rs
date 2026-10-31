// The tag menu both stacks show: Clicking the tag already in view on the
// bar, or pressing its Super+N again, drops a list of the tag's windows
// under the tag, minimized ones marked. Up and Down move, Enter or a click
// picks, Escape or a click anywhere else closes. Here are its entries, its
// size and place, its selection and its pixels; each stack puts them on the
// screen its own way.

use crate::config::Config;

const PAD_X: i32 = 14;
const MIN_W: i32 = 220;
const MAX_W: i32 = 560;
// macOS marks a minimized window in its Window menu the same way.
const MINIMIZED_MARK: &str = "\u{25C6}";

pub struct Entry {
    pub id: u64,
    pub title: String,
    pub minimized: bool,
    pub icon: Option<Icon>,
}

// A window's icon: Cairo ARGB32 pixels, premultiplied, width * 4 a row.
pub struct Icon {
    pub width: i32,
    pub height: i32,
    pub data: Vec<u8>,
}

pub struct Style {
    font: pango::FontDescription,
    options: cairo::FontOptions,
    bg: (f64, f64, f64),
    fg: (f64, f64, f64),
    dim: (f64, f64, f64),
    accent: (f64, f64, f64),
    accent_fg: (f64, f64, f64),
}

impl Style {
    // The bar's font and colors, so the menu reads as part of the bar.
    pub fn from_config(config: &Config) -> Result<Self, String> {
        let accent = if config.bar.tag_focused_bg.is_empty() {
            &config.appearance.accent
        } else {
            &config.bar.tag_focused_bg
        };
        Ok(Self {
            font: pango::FontDescription::from_string(&config.bar.font),
            options: crate::torrentius::make_font_options()?,
            bg: crate::torrentius::parse_hex(&config.bar.bg),
            fg: crate::torrentius::parse_hex(&config.bar.fg),
            dim: crate::torrentius::parse_hex(&config.bar.tag_empty_fg),
            accent: crate::torrentius::parse_hex(accent),
            accent_fg: crate::torrentius::parse_hex(&config.bar.tag_focused_fg),
        })
    }

    fn layout(&self, cr: &cairo::Context) -> pango::Layout {
        cr.set_font_options(&self.options);
        let layout = pangocairo::functions::create_layout(cr);
        layout.set_font_description(Some(&self.font));
        pangocairo::functions::context_set_font_options(&layout.context(), Some(&self.options));
        layout
    }
}

pub struct Menu {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    row_h: i32,
    gutter: i32,
    // Where an entry's icon goes and its side, when any entry has one.
    icon_x: i32,
    icon_side: i32,
    pub entries: Vec<Entry>,
    pub selected: Option<usize>,
}

impl Menu {
    // The menu with its top-left corner at `at`, kept inside the monitor
    // spanning `bounds` (x, width), sized to its widest entry.
    pub fn new(
        style: &Style,
        at: (i32, i32),
        bounds: (i32, u32),
        entries: Vec<Entry>,
        selected: Option<usize>,
    ) -> Result<Self, String> {
        let (probe, cr) = crate::torrentius::new_surface(1, 1)?;
        let layout = style.layout(&cr);
        layout.set_text(MINIMIZED_MARK);
        let (mark_w, _) = layout.pixel_size();
        let gutter = mark_w + PAD_X / 2;
        layout.set_text("Ag");
        let (_, text_h) = layout.pixel_size();
        let widest = entries
            .iter()
            .map(|e| {
                layout.set_text(label(e));
                layout.pixel_size().0
            })
            .max()
            .unwrap_or(0);
        drop((layout, cr, probe));

        // An icon as tall as the text, after the minimized mark.
        let icon_x = PAD_X + gutter;
        let icon_side = if entries.iter().any(|e| e.icon.is_some()) { text_h } else { 0 };
        let gutter = if icon_side > 0 { gutter + icon_side + PAD_X / 2 } else { gutter };
        let w = (widest + gutter + PAD_X * 2).clamp(MIN_W, MAX_W);
        let x = at.0.min(bounds.0 + bounds.1 as i32 - w).max(bounds.0);
        Ok(Self { x, y: at.1, w, row_h: text_h + 10, gutter, icon_x, icon_side, entries, selected })
    }

    pub fn height(&self) -> i32 {
        self.row_h * self.entries.len().max(1) as i32
    }

    // The menu's pixels, w by height, Rgb24.
    pub fn draw(&self, style: &Style) -> Result<cairo::ImageSurface, String> {
        let h = self.height();
        let (surface, cr) = crate::torrentius::new_surface(self.w, h)?;
        let layout = style.layout(&cr);
        layout.set_width((self.w - self.gutter - PAD_X * 2) * pango::SCALE);
        layout.set_ellipsize(pango::EllipsizeMode::End);

        cr.set_source_rgb(style.bg.0, style.bg.1, style.bg.2);
        let _ = cr.paint();
        for (i, entry) in self.entries.iter().enumerate() {
            let top = (i as i32 * self.row_h) as f64;
            let chosen = self.selected == Some(i);
            if chosen {
                cr.set_source_rgb(style.accent.0, style.accent.1, style.accent.2);
                cr.rectangle(0.0, top, self.w as f64, self.row_h as f64);
                let _ = cr.fill();
            }
            let ink = match (chosen, entry.minimized) {
                (true, _) => style.accent_fg,
                (false, true) => style.dim,
                (false, false) => style.fg,
            };
            cr.set_source_rgb(ink.0, ink.1, ink.2);
            layout.set_text(label(entry));
            let (_, text_h) = layout.pixel_size();
            let text_y = top + ((self.row_h - text_h) / 2) as f64;
            cr.move_to((PAD_X + self.gutter) as f64, text_y);
            pangocairo::functions::show_layout(&cr, &layout);
            if entry.minimized {
                let mark = style.layout(&cr);
                mark.set_text(MINIMIZED_MARK);
                cr.move_to(PAD_X as f64, text_y);
                pangocairo::functions::show_layout(&cr, &mark);
            }
            if let Some(icon) = &entry.icon {
                let top = top + ((self.row_h - self.icon_side) / 2) as f64;
                draw_icon(&cr, icon, self.icon_x as f64, top, self.icon_side as f64);
            }
        }
        cr.set_source_rgb(style.dim.0, style.dim.1, style.dim.2);
        cr.set_line_width(1.0);
        cr.rectangle(0.5, 0.5, self.w as f64 - 1.0, h as f64 - 1.0);
        let _ = cr.stroke();
        drop(layout);
        drop(cr);
        surface.flush();
        Ok(surface)
    }

    // The entry under a point in the screen's coordinates.
    pub fn entry_at(&self, x: i32, y: i32) -> Option<usize> {
        let h = self.row_h * self.entries.len() as i32;
        if x < self.x || x >= self.x + self.w || y < self.y || y >= self.y + h {
            return None;
        }
        Some(((y - self.y) / self.row_h) as usize)
    }

    // Move the selection one entry, wrapping; the first step from no
    // selection lands on the first or last entry.
    pub fn step(&mut self, forward: bool) {
        let n = self.entries.len();
        if n == 0 {
            return;
        }
        self.selected = Some(match (self.selected, forward) {
            (None, true) => 0,
            (None, false) => n - 1,
            (Some(i), true) => (i + 1) % n,
            (Some(i), false) => (i + n - 1) % n,
        });
    }
}

// `icon` scaled into the square of side `side` at (x, y).
fn draw_icon(cr: &cairo::Context, icon: &Icon, x: f64, y: f64, side: f64) {
    let Ok(pixels) =
        cairo::ImageSurface::create_for_data(icon.data.clone(), cairo::Format::ARgb32, icon.width, icon.height, icon.width * 4)
    else {
        return;
    };
    cr.save().ok();
    cr.translate(x, y);
    cr.scale(side / icon.width as f64, side / icon.height as f64);
    let _ = cr.set_source_surface(&pixels, 0.0, 0.0);
    let _ = cr.paint();
    cr.restore().ok();
}

fn label(entry: &Entry) -> &str {
    if entry.title.is_empty() { "(untitled)" } else { &entry.title }
}
