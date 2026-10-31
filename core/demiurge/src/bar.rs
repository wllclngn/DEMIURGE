// The bar's painter, the same on both stacks: Where a monitor's bar puts
// its tags, prompt, center text and clock, and their Cairo drawing into the
// bar's pixels. A region is drawn again only when what it shows changed, and
// the regions drawn are what each stack puts on its screen: X11 into its bar
// windows, demiurge-wl into its bar textures.

use crate::config::Config;
use crate::prompt::Prompt;
use crate::torrentius;

// The prompt's and the clock's room; the title takes what is left between
// them.
pub const PROMPT_MAX_W: i32 = 600;
pub const CLOCK_MAX_W: i32 = 400;

// The bar's edge to its first tag, and its last tag to the prompt.
pub const TAGS_EDGE_PAD: i32 = 20;

#[derive(Copy, Clone, Default, Debug, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Region {
    Tags,
    Prompt,
    Title,
    Clock,
}

const REGIONS: [Region; 4] = [Region::Tags, Region::Prompt, Region::Title, Region::Clock];

// The regions of a bar `width` by `height` whose tag strip is `tags_w` wide,
// left to right and side by side, so drawing one never touches another: On a
// bar too narrow for all of them the clock keeps its room first, then the
// tags, then the prompt.
fn regions(width: i32, height: i32, tags_w: i32) -> [Rect; 4] {
    let clock_x = (width - CLOCK_MAX_W).max(0);
    let tags_end = tags_w.min(clock_x);
    let prompt_end = (tags_end + PROMPT_MAX_W).min(clock_x);
    let span = |x: i32, end: i32| Rect { x, y: 0, w: end - x, h: height };
    [span(0, tags_end), span(tags_end, prompt_end), span(prompt_end, clock_x), span(clock_x, width)]
}

pub struct Style {
    pub font: pango::FontDescription,
    pub options: cairo::FontOptions,
    pub bg: (f64, f64, f64),
    pub fg: (f64, f64, f64),
    pub tag_focused_bg: (f64, f64, f64),
    pub tag_focused_fg: (f64, f64, f64),
    pub tag_occupied_fg: (f64, f64, f64),
    pub tag_empty_fg: (f64, f64, f64),
    pub notification_fg: (f64, f64, f64),
}

impl Style {
    pub fn from_config(config: &Config) -> Result<Self, String> {
        // An empty tag_focused_bg takes the desktop's [appearance] accent.
        let accent = if config.bar.tag_focused_bg.is_empty() {
            &config.appearance.accent
        } else {
            &config.bar.tag_focused_bg
        };
        Ok(Self {
            font: pango::FontDescription::from_string(&config.bar.font),
            options: torrentius::make_font_options()?,
            bg: torrentius::parse_hex(&config.bar.bg),
            fg: torrentius::parse_hex(&config.bar.fg),
            tag_focused_bg: torrentius::parse_hex(accent),
            tag_focused_fg: torrentius::parse_hex(&config.bar.tag_focused_fg),
            tag_occupied_fg: torrentius::parse_hex(&config.bar.tag_occupied_fg),
            tag_empty_fg: torrentius::parse_hex(&config.bar.tag_empty_fg),
            notification_fg: torrentius::parse_hex(&config.bar.notification_fg),
        })
    }

    pub fn layout(&self, cr: &cairo::Context) -> pango::Layout {
        cr.set_font_options(&self.options);
        let layout = pangocairo::functions::create_layout(cr);
        layout.set_font_description(Some(&self.font));
        pangocairo::functions::context_set_font_options(&layout.context(), Some(&self.options));
        layout
    }
}

/// What one monitor's bar shows besides the clock.
pub struct View<'a> {
    pub active_tag: usize,
    pub occupied: &'a [bool],
    pub urgent: &'a [bool],
    pub prompt: Option<&'a Prompt>,
    /// The focused window's title, or a notification (true) over it.
    pub center: Option<(&'a str, bool)>,
}

// What a panel's regions were last drawn showing.
#[derive(Default)]
struct Shown {
    active_tag: usize,
    occupied: Vec<bool>,
    urgent: Vec<bool>,
    // The prompt's input, cursor and completion.
    prompt: Option<(String, usize, Option<String>)>,
    center: Option<(String, bool)>,
    clock: String,
}

/// One monitor's bar: Its pixels, its regions, and what they show.
pub struct Panel {
    surface: cairo::ImageSurface,
    width: u32,
    rects: [Rect; 4],
    shown: Option<Shown>,
}

impl Panel {
    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn rect(&self, region: Region) -> Rect {
        self.rects[region as usize]
    }

    /// The bar's pixels, 32-bit xRGB (DRM's Xrgb8888, X's depth-24 ZPixmap
    /// on little-endian) in rows `stride` bytes apart, a pixel's 4 bytes
    /// times the width.
    pub fn with_pixels<R>(&self, f: impl FnOnce(&[u8], usize) -> R) -> Result<R, String> {
        let stride = self.surface.stride() as usize;
        let mut out = None;
        self.surface
            .with_data(|data| out = Some(f(data, stride)))
            .map_err(|e| format!("bar pixels: {}", e))?;
        out.ok_or_else(|| "bar pixels unread".to_string())
    }
}

/// The regions a paint drew: The whole bar, or each region whose view
/// changed.
#[derive(Copy, Clone, Default, Debug, PartialEq, Eq)]
pub struct Painted {
    whole: bool,
    regions: [bool; 4],
}

impl Painted {
    pub fn is_empty(&self) -> bool {
        !self.whole && !self.regions.contains(&true)
    }

    /// The rects to put on the screen: The whole bar as one, otherwise each
    /// region drawn.
    pub fn rects<'a>(&'a self, panel: &'a Panel) -> impl Iterator<Item = Rect> + 'a {
        let whole = self.whole.then_some(Rect { x: 0, y: 0, w: panel.width as i32, h: panel.rects[0].h });
        let regions = REGIONS
            .iter()
            .filter(move |&&r| !self.whole && self.regions[r as usize])
            .map(|&r| panel.rect(r));
        whole.into_iter().chain(regions).filter(|r| r.w > 0 && r.h > 0)
    }
}

pub struct Painter {
    pub style: Style,
    pub height: u32,
    tag_names: Vec<String>,
    // The tag strip's width, its edge pads with it.
    tags_w: i32,
    tag_extents: Vec<(i32, i32)>,
    clock_format: String,
    clock: String,
}

impl Painter {
    pub fn new(config: &Config) -> Result<Self, String> {
        let style = Style::from_config(config)?;
        let tag_names = config.general.tags.clone();
        let (probe, cr) = torrentius::new_surface(1, 1)?;
        let layout = style.layout(&cr);
        let mut x = TAGS_EDGE_PAD;
        let tag_extents = tag_names
            .iter()
            .map(|name| {
                layout.set_text(&format!("  {}  ", name));
                let start = x;
                x += layout.pixel_size().0;
                (start, x)
            })
            .collect();
        drop((layout, cr, probe));
        Ok(Self {
            style,
            height: config.bar.height,
            tag_names,
            tags_w: x + TAGS_EDGE_PAD,
            tag_extents,
            clock_format: config.bar.clock_format.clone(),
            clock: crate::clock::now(&config.bar.clock_format),
        })
    }

    /// Each tag's span on the bar, left and right edge, for clicks and for
    /// where the tag menu drops.
    pub fn tag_extents(&self) -> &[(i32, i32)] {
        &self.tag_extents
    }

    /// The clock read again; true when it shows something new.
    pub fn tick_clock(&mut self) -> bool {
        let now = crate::clock::now(&self.clock_format);
        if now == self.clock {
            return false;
        }
        self.clock = now;
        true
    }

    /// A bar `width` wide, drawn whole at its first paint.
    pub fn panel(&self, width: u32) -> Result<Panel, String> {
        let surface = cairo::ImageSurface::create(cairo::Format::Rgb24, width.max(1) as i32, self.height.max(1) as i32)
            .map_err(|e| format!("bar surface {}x{}: {}", width, self.height, e))?;
        Ok(Panel { surface, width, rects: regions(width as i32, self.height as i32, self.tags_w), shown: None })
    }

    /// Draw what changed on `panel` since it was last drawn: Every region the
    /// first time, then each region whose part of `view`, or the clock,
    /// differs from what it shows.
    pub fn paint(&self, panel: &mut Panel, view: &View) -> Painted {
        let whole = panel.shown.is_none();
        let shown = panel.shown.get_or_insert_with(Shown::default);
        let mut painted = Painted { whole, regions: [whole; 4] };
        if whole || shown.active_tag != view.active_tag || shown.occupied != view.occupied || shown.urgent != view.urgent {
            shown.active_tag = view.active_tag;
            shown.occupied.clear();
            shown.occupied.extend_from_slice(view.occupied);
            shown.urgent.clear();
            shown.urgent.extend_from_slice(view.urgent);
            painted.regions[Region::Tags as usize] = true;
        }
        let prompt_shown = match (view.prompt, &shown.prompt) {
            (None, None) => true,
            (Some(now), Some((input, cursor, completion))) => {
                now.input == *input && now.cursor == *cursor && now.completion == *completion
            }
            _ => false,
        };
        if whole || !prompt_shown {
            shown.prompt = view.prompt.map(|p| (p.input.clone(), p.cursor, p.completion.clone()));
            painted.regions[Region::Prompt as usize] = true;
        }
        if whole || shown.center.as_ref().map(|(t, n)| (t.as_str(), *n)) != view.center {
            shown.center = view.center.map(|(t, n)| (t.to_string(), n));
            painted.regions[Region::Title as usize] = true;
        }
        if whole || shown.clock != self.clock {
            shown.clock.clone_from(&self.clock);
            painted.regions[Region::Clock as usize] = true;
        }
        if painted.is_empty() {
            return painted;
        }
        let Ok(cr) = cairo::Context::new(&panel.surface) else {
            // Drawn whole once a context can be had.
            panel.shown = None;
            return Painted::default();
        };
        let layout = self.style.layout(&cr);
        for region in REGIONS.into_iter().filter(|&r| painted.regions[r as usize]) {
            let rect = panel.rects[region as usize];
            let _ = cr.save();
            cr.rectangle(rect.x as f64, rect.y as f64, rect.w as f64, rect.h as f64);
            cr.clip();
            cr.set_source_rgb(self.style.bg.0, self.style.bg.1, self.style.bg.2);
            let _ = cr.paint();
            match region {
                Region::Tags => self.draw_tags(&cr, &layout, rect, shown),
                Region::Prompt => self.draw_prompt(&cr, &layout, rect, shown),
                Region::Title => self.draw_title(&cr, &layout, rect, panel.width as i32, shown),
                Region::Clock => self.draw_clock(&cr, &layout, rect, panel.width as i32),
            }
            let _ = cr.restore();
        }
        drop((layout, cr));
        panel.surface.flush();
        painted
    }

    // The active tag on the accent, an urgent tag's label in it, then the
    // occupied and empty tags in their colors.
    fn draw_tags(&self, cr: &cairo::Context, layout: &pango::Layout, rect: Rect, shown: &Shown) {
        let s = &self.style;
        for (i, (name, &(x, end))) in self.tag_names.iter().zip(&self.tag_extents).enumerate() {
            layout.set_text(&format!("  {}  ", name));
            let (_, text_h) = layout.pixel_size();
            let ink = if i == shown.active_tag {
                cr.set_source_rgb(s.tag_focused_bg.0, s.tag_focused_bg.1, s.tag_focused_bg.2);
                cr.rectangle(x as f64, 0.0, (end - x) as f64, rect.h as f64);
                let _ = cr.fill();
                s.tag_focused_fg
            } else if shown.urgent.get(i).copied().unwrap_or(false) {
                s.tag_focused_bg
            } else if shown.occupied.get(i).copied().unwrap_or(false) {
                s.tag_occupied_fg
            } else {
                s.tag_empty_fg
            };
            cr.set_source_rgb(ink.0, ink.1, ink.2);
            cr.move_to(x as f64, ((rect.h - text_h) / 2) as f64);
            pangocairo::functions::show_layout(cr, layout);
        }
    }

    // What is typed after "> ", the rest of its completion dimmed, and a
    // cursor; the tag strip's trailing pad is the gap before it.
    fn draw_prompt(&self, cr: &cairo::Context, layout: &pango::Layout, rect: Rect, shown: &Shown) {
        let Some((input, cursor, completion)) = &shown.prompt else {
            return;
        };
        let prefix = "> ";
        let text = format!("{}{}", prefix, input);
        layout.set_text(&text);
        let (text_w, text_h) = layout.pixel_size();
        let y = ((rect.h - text_h) / 2) as f64;
        let (r, g, b) = self.style.fg;
        cr.set_source_rgb(r, g, b);
        cr.move_to(rect.x as f64, y);
        pangocairo::functions::show_layout(cr, layout);
        if let Some(completion) = completion
            && completion.len() > input.len()
        {
            layout.set_text(&completion[input.len()..]);
            cr.set_source_rgb(r * 0.4, g * 0.4, b * 0.4);
            cr.move_to((rect.x + text_w) as f64, y);
            pangocairo::functions::show_layout(cr, layout);
        }
        layout.set_text(&text[..prefix.len() + cursor]);
        let (cursor_x, _) = layout.pixel_size();
        cr.set_source_rgb(r, g, b);
        cr.rectangle((rect.x + cursor_x) as f64, 4.0, 1.0, (rect.h - 8) as f64);
        let _ = cr.fill();
    }

    // Centered on the bar, but kept 20 px inside its region and ellipsized
    // to fit it; a notification in its own color.
    fn draw_title(&self, cr: &cairo::Context, layout: &pango::Layout, rect: Rect, width: i32, shown: &Shown) {
        let Some((text, notification)) = &shown.center else {
            return;
        };
        let max_w = rect.w - 40;
        if text.is_empty() || max_w < 50 {
            return;
        }
        layout.set_text(text);
        layout.set_ellipsize(pango::EllipsizeMode::End);
        layout.set_width(max_w * pango::SCALE);
        let (text_w, text_h) = layout.pixel_size();
        let x = (width / 2 - text_w / 2).max(rect.x + 20).min(rect.x + rect.w - 20 - text_w);
        let (r, g, b) = if *notification { self.style.notification_fg } else { self.style.fg };
        cr.set_source_rgb(r, g, b);
        cr.move_to(x as f64, ((rect.h - text_h) / 2) as f64);
        pangocairo::functions::show_layout(cr, layout);
        layout.set_width(-1);
        layout.set_ellipsize(pango::EllipsizeMode::None);
    }

    // Against the bar's right edge, in tabular figures so the time keeps its
    // width as its digits change.
    fn draw_clock(&self, cr: &cairo::Context, layout: &pango::Layout, rect: Rect, width: i32) {
        layout.set_markup(&format!(
            "<span font_features=\"tnum\">{}</span>",
            pango::glib::markup_escape_text(&self.clock)
        ));
        let (clock_w, clock_h) = layout.pixel_size();
        let (r, g, b) = self.style.fg;
        cr.set_source_rgb(r, g, b);
        cr.move_to((width - clock_w - 10) as f64, ((rect.h - clock_h) / 2) as f64);
        pangocairo::functions::show_layout(cr, layout);
        layout.set_attributes(None);
    }
}
