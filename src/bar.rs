use std::ffi::CString;
use std::os::unix::fs::PermissionsExt;

use x11rb::connection::Connection;
use x11rb::protocol::xproto::*;
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as _;

use crate::atoms::Atoms;
use crate::config::Config;
use crate::monitor::Monitor;

// Fixed per-region horizontal extents. Each region owns a non-overlapping
// slice of the panel width. When a region is dirty, render into its slice
// and put_image only that rect. Tag/prompt/clock widths are bounded by
// these constants; anything wider gets ellipsized or clipped.
const PROMPT_MAX_W: i32 = 600;
const CLOCK_MAX_W: i32 = 400;

// Horizontal padding between the bar's left edge and the first tag, and
// mirrored on the right side of the last tag before the prompt begins.
const TAGS_EDGE_PAD: i32 = 20;

#[derive(Copy, Clone, Default, Debug)]
struct Rect {
    x: i32,
    y: i32,
    w: i32,
    h: i32,
}

// Non-overlapping region rects for a panel of the given width. `tags_w`
// is the measured width of the tag strip (edge pad + sum of padded tag
// text widths + edge pad); the prompt abuts it so the "> " prefix lands
// TAGS_EDGE_PAD pixels past the last tag's right edge -- mirroring the
// edge pad used for the first tag.
fn region_rects(panel_w: i32, panel_h: i32, tags_w: i32) -> RegionRects {
    let tags = Rect {
        x: 0,
        y: 0,
        w: tags_w,
        h: panel_h,
    };
    let prompt = Rect {
        x: tags_w,
        y: 0,
        w: PROMPT_MAX_W,
        h: panel_h,
    };
    let clock = Rect {
        x: panel_w - CLOCK_MAX_W,
        y: 0,
        w: CLOCK_MAX_W,
        h: panel_h,
    };
    let title_x = tags_w + PROMPT_MAX_W;
    let title_w = (panel_w - CLOCK_MAX_W) - title_x;
    let title = Rect {
        x: title_x,
        y: 0,
        w: title_w.max(0),
        h: panel_h,
    };
    RegionRects {
        tags,
        prompt,
        title,
        clock,
    }
}

#[derive(Copy, Clone, Debug)]
struct RegionRects {
    tags: Rect,
    prompt: Rect,
    title: Rect,
    clock: Rect,
}

// Measure the total pixel width the tag strip will occupy once rendered:
// edge pad + sum of padded tag text widths + edge pad. Mirrors the layout
// used in render_tags so that the prompt rect can abut the strip tightly
// instead of sitting inside a fixed gap.
fn measure_tags_total_w(
    font_desc: &pango::FontDescription,
    font_options: &cairo::FontOptions,
    tag_names: &[String],
) -> i32 {
    let fallback = TAGS_EDGE_PAD * 2 + (tag_names.len() as i32) * 32;
    let surface = match cairo::ImageSurface::create(cairo::Format::Rgb24, 1, 1) {
        Ok(s) => s,
        Err(_) => return fallback,
    };
    let cr = match cairo::Context::new(&surface) {
        Ok(c) => c,
        Err(_) => return fallback,
    };
    cr.set_font_options(font_options);
    let layout = pangocairo::functions::create_layout(&cr);
    layout.set_font_description(Some(font_desc));
    let pango_ctx = layout.context();
    pangocairo::functions::context_set_font_options(&pango_ctx, Some(font_options));
    let mut total = 0;
    for name in tag_names {
        let padded = format!("  {}  ", name);
        layout.set_text(&padded);
        let (w, _) = layout.pixel_size();
        total += w;
    }
    TAGS_EDGE_PAD * 2 + total
}

struct BarPanel {
    window: Window,
    gc: Gcontext,
    width: u32,
    // Pinned Cairo ImageSurface. Allocated once at Bar::create time,
    // reused for every partial redraw. Recreated only on bar height
    // change (triggered by update_appearance).
    surface: cairo::ImageSurface,
    rects: RegionRects,
}

// Per-region dirty flags. State mutations mark their region dirty; the
// event-loop commit pass consumes them, renders, and put_images only the
// dirty rects. `all` is the escape hatch for full redraw (reload, Expose,
// bar height change).
#[derive(Default, Copy, Clone, Debug)]
struct BarDirty {
    tags: bool,
    prompt: bool,
    title: bool,
    clock: bool,
    all: bool,
}

impl BarDirty {
    fn any(&self) -> bool {
        self.tags || self.prompt || self.title || self.clock || self.all
    }
}

pub struct Bar {
    panels: Vec<BarPanel>,
    pub height: u32,
    depth: u8,
    font_desc: pango::FontDescription,
    font_options: cairo::FontOptions,
    colors: BarColors,
    clock_format: String,
    tag_names: Vec<String>,
    pub path_executables: Vec<String>,
    pub prompt: Option<PromptState>,
    pub tag_extents: Vec<(i32, i32)>,
    dirty: BarDirty,
    last_clock_text: String,
}

struct BarColors {
    bg: (f64, f64, f64),
    fg: (f64, f64, f64),
    tag_focused_bg: (f64, f64, f64),
    tag_focused_fg: (f64, f64, f64),
    tag_occupied_fg: (f64, f64, f64),
    tag_empty_fg: (f64, f64, f64),
    notification_fg: (f64, f64, f64),
}

pub struct PromptState {
    pub input: String,
    pub cursor: usize,
    pub completion: Option<String>,
}

impl Bar {
    pub fn create(
        conn: &RustConnection,
        root: Window,
        atoms: &Atoms,
        config: &Config,
        monitors: &[Monitor],
        tag_names: &[String],
    ) -> Result<Self, String> {
        let screen = &conn.setup().roots[0];
        let depth = screen.root_depth;
        let height = config.bar.height;
        let bg_pixel = crate::torrentius::hex_to_pixel(&config.bar.bg);

        // Font setup hoisted above the panel loop so region_rects can
        // size the tag strip against the actual rendered width.
        let font_desc = pango::FontDescription::from_string(&config.bar.font);
        let font_options = crate::torrentius::make_font_options()?;
        let tags_w = measure_tags_total_w(&font_desc, &font_options, tag_names);

        let mut panels = Vec::new();

        for (i, mon) in monitors.iter().enumerate() {
            let window = conn.generate_id().map_err(|e| format!("bar window id: {}", e))?;
            let gc = conn.generate_id().map_err(|e| format!("bar gc id: {}", e))?;

            conn.create_window(
                depth,
                window,
                root,
                mon.x as i16,
                mon.y as i16,
                mon.width as u16,
                height as u16,
                0,
                WindowClass::INPUT_OUTPUT,
                0,
                &CreateWindowAux::new()
                    .override_redirect(1)
                    .background_pixel(bg_pixel)
                    .event_mask(EventMask::EXPOSURE | EventMask::BUTTON_PRESS),
            )
            .map_err(|e| format!("create bar window: {}", e))?;

            let _ = conn.change_property32(
                PropMode::REPLACE,
                window,
                atoms._NET_WM_WINDOW_TYPE,
                AtomEnum::ATOM,
                &[atoms._NET_WM_WINDOW_TYPE_DOCK],
            );

            let strut: [u32; 12] = [
                0,
                0,
                height,
                0,
                0,
                0,
                0,
                0,
                mon.x as u32,
                mon.x as u32 + mon.width - 1,
                0,
                0,
            ];
            let _ = conn.change_property32(
                PropMode::REPLACE,
                window,
                atoms._NET_WM_STRUT_PARTIAL,
                AtomEnum::CARDINAL,
                &strut,
            );

            conn.create_gc(gc, window, &CreateGCAux::new())
                .map_err(|e| format!("create bar gc: {}", e))?;

            let _ = conn.map_window(window);
            let _ = conn.configure_window(
                window,
                &ConfigureWindowAux::new().stack_mode(StackMode::ABOVE),
            );

            let surface = cairo::ImageSurface::create(
                cairo::Format::Rgb24,
                mon.width as i32,
                height as i32,
            )
            .map_err(|e| format!("bar surface {}x{}: {}", mon.width, height, e))?;

            let rects = region_rects(mon.width as i32, height as i32, tags_w);

            panels.push(BarPanel {
                window,
                gc,
                width: mon.width,
                surface,
                rects,
            });

            eprintln!(
                "[demiurge] bar {}: {}x{} on '{}'",
                i, mon.width, height, mon.name,
            );
        }

        let _ = conn.flush();

        let colors = BarColors {
            bg: parse_hex_local(&config.bar.bg),
            fg: parse_hex_local(&config.bar.fg),
            tag_focused_bg: parse_hex_local(&config.bar.tag_focused_bg),
            tag_focused_fg: parse_hex_local(&config.bar.tag_focused_fg),
            tag_occupied_fg: parse_hex_local(&config.bar.tag_occupied_fg),
            tag_empty_fg: parse_hex_local(&config.bar.tag_empty_fg),
            notification_fg: parse_hex_local(&config.bar.notification_fg),
        };

        let path_executables = scan_path();

        eprintln!(
            "[demiurge] bar: {} PATH executables",
            path_executables.len(),
        );

        let mut bar = Self {
            panels,
            height,
            depth,
            font_desc,
            font_options,
            colors,
            clock_format: config.bar.clock_format.clone(),
            tag_names: tag_names.to_vec(),
            path_executables,
            prompt: None,
            tag_extents: Vec::new(),
            dirty: BarDirty {
                all: true,
                ..Default::default()
            },
            last_clock_text: String::new(),
        };
        bar.last_clock_text = bar.format_clock();
        Ok(bar)
    }

    pub fn contains_window(&self, window: Window) -> bool {
        self.panels.iter().any(|p| p.window == window)
    }

    pub fn raise_all(&self, conn: &RustConnection) {
        for panel in &self.panels {
            let _ = conn.configure_window(
                panel.window,
                &ConfigureWindowAux::new().stack_mode(StackMode::ABOVE),
            );
        }
    }

    pub fn destroy_all(&self, conn: &RustConnection) {
        for panel in &self.panels {
            let _ = conn.destroy_window(panel.window);
        }
    }

    // Mark helpers: state mutations call these; nothing renders yet.
    pub fn mark_tags_dirty(&mut self) {
        self.dirty.tags = true;
    }
    pub fn mark_prompt_dirty(&mut self) {
        self.dirty.prompt = true;
    }
    pub fn mark_title_dirty(&mut self) {
        self.dirty.title = true;
    }

    // Clock: gate on actual text change. A 1 Hz timer tick that lands
    // inside the same wall-clock second produces no dirty work, and --
    // more importantly -- identical formatted text would otherwise produce
    // an identical put_image that still generates a damage event for the
    // compositor. Skipping at the source saves that work too.
    pub fn mark_clock_dirty(&mut self) {
        let now = self.format_clock();
        if now == self.last_clock_text {
            return;
        }
        self.last_clock_text = now;
        self.dirty.clock = true;
    }

    pub fn mark_all_dirty(&mut self) {
        self.dirty.all = true;
    }

    // Called once per event-loop iteration. Fast path: nothing dirty ->
    // return immediately. Otherwise render each dirty region into its
    // panel's persistent surface and put_image just that rect.
    pub fn commit(
        &mut self,
        conn: &RustConnection,
        active_tag: usize,
        occupied: &[bool],
        center_text: &str,
        is_notification: bool,
    ) {
        if !self.dirty.any() {
            return;
        }
        let dirty = std::mem::take(&mut self.dirty);

        // We iterate panels here; each panel owns its surface + rects.
        // The render_* helpers take a panel reference so they can draw
        // into the right surface, and commit then pushes the dirty rect(s)
        // via put_region.
        //
        // pi == 0 is the "primary" panel where tags are click-registered
        // and the prompt + title/notification + clock live. Secondary
        // panels currently only render the tags/clock (matching prior
        // behavior of the monolithic draw function where prompt and
        // title rendering were gated on pi == 0).
        for pi in 0..self.panels.len() {
            let panel_w = self.panels[pi].width as i32;
            let panel_h = self.height as i32;

            // On the all path, re-render every region. Otherwise each
            // dirty flag triggers only its own region. Non-overlapping
            // rects mean later region writes can't clobber earlier ones.
            let want_tags = dirty.all || dirty.tags;
            let want_prompt = dirty.all || dirty.prompt;
            let want_title = dirty.all || dirty.title;
            let want_clock = dirty.all || dirty.clock;

            if want_tags {
                self.render_tags(pi, active_tag, occupied);
            }
            if want_prompt {
                self.render_prompt(pi);
            }
            if want_title {
                self.render_title(pi, center_text, is_notification);
            }
            if want_clock {
                self.render_clock(pi);
            }

            // Push dirty rect(s) to X. Full-panel on `all`; otherwise
            // one put_image per dirty region.
            if dirty.all {
                let rect = Rect {
                    x: 0,
                    y: 0,
                    w: panel_w,
                    h: panel_h,
                };
                put_region(&self.panels[pi], conn, rect, self.depth);
            } else {
                if dirty.tags {
                    put_region(&self.panels[pi], conn, self.panels[pi].rects.tags, self.depth);
                }
                if dirty.prompt {
                    put_region(&self.panels[pi], conn, self.panels[pi].rects.prompt, self.depth);
                }
                if dirty.title {
                    put_region(&self.panels[pi], conn, self.panels[pi].rects.title, self.depth);
                }
                if dirty.clock {
                    put_region(&self.panels[pi], conn, self.panels[pi].rects.clock, self.depth);
                }
            }
        }

        let _ = conn.flush();
    }

    // Render tags region onto the persistent surface. Paints the
    // region's background first to clear stale pixels, then the tags.
    // Also updates self.tag_extents (button-hit map) when rendering the
    // primary panel.
    fn render_tags(&mut self, pi: usize, active_tag: usize, occupied: &[bool]) {
        let rect = self.panels[pi].rects.tags;
        let cr = match cairo::Context::new(&self.panels[pi].surface) {
            Ok(c) => c,
            Err(_) => return,
        };
        cr.set_font_options(&self.font_options);

        clear_rect(&cr, rect, self.colors.bg);

        let layout = pangocairo::functions::create_layout(&cr);
        layout.set_font_description(Some(&self.font_desc));
        let pango_ctx = layout.context();
        pangocairo::functions::context_set_font_options(&pango_ctx, Some(&self.font_options));

        let h = rect.h;
        let mut x = rect.x + TAGS_EDGE_PAD;
        let mut new_extents: Vec<(i32, i32)> = Vec::new();
        let tag_count = self.tag_names.len().min(occupied.len());

        for i in 0..tag_count {
            let padded = format!("  {}  ", self.tag_names[i]);
            layout.set_text(&padded);
            let (text_w, text_h) = layout.pixel_size();
            let y = (h - text_h) / 2;

            if i == active_tag {
                let (fbr, fbg, fbb) = self.colors.tag_focused_bg;
                cr.set_source_rgb(fbr, fbg, fbb);
                cr.rectangle(x as f64, 0.0, text_w as f64, h as f64);
                let _ = cr.fill();
                let (fr, fg, fb) = self.colors.tag_focused_fg;
                cr.set_source_rgb(fr, fg, fb);
            } else if occupied[i] {
                let (or, og, ob) = self.colors.tag_occupied_fg;
                cr.set_source_rgb(or, og, ob);
            } else {
                let (er, eg, eb) = self.colors.tag_empty_fg;
                cr.set_source_rgb(er, eg, eb);
            }

            cr.move_to(x as f64, y as f64);
            pangocairo::functions::show_layout(&cr, &layout);
            let start = x;
            x += text_w;
            if pi == 0 {
                new_extents.push((start, x));
            }
        }

        if pi == 0 {
            self.tag_extents = new_extents;
        }

        drop(cr);
        self.panels[pi].surface.flush();
    }

    fn render_prompt(&mut self, pi: usize) {
        let rect = self.panels[pi].rects.prompt;
        let cr = match cairo::Context::new(&self.panels[pi].surface) {
            Ok(c) => c,
            Err(_) => return,
        };
        cr.set_font_options(&self.font_options);

        clear_rect(&cr, rect, self.colors.bg);

        // Prompt only rendered on the primary panel.
        if pi != 0 {
            drop(cr);
            self.panels[pi].surface.flush();
            return;
        }
        let prompt = match self.prompt.as_ref() {
            Some(p) => p,
            None => {
                drop(cr);
                self.panels[pi].surface.flush();
                return;
            }
        };

        let layout = pangocairo::functions::create_layout(&cr);
        layout.set_font_description(Some(&self.font_desc));
        let pango_ctx = layout.context();
        pangocairo::functions::context_set_font_options(&pango_ctx, Some(&self.font_options));

        let h = rect.h;
        // Prompt text starts flush with rect.x: the tag rect's trailing
        // TAGS_EDGE_PAD already provides the visual gap between the last
        // tag's right edge and the "> " prefix.
        let mut x = rect.x;

        let prefix = "> ";
        let prompt_text = format!("{}{}", prefix, prompt.input);
        layout.set_text(&prompt_text);
        let (text_w, text_h) = layout.pixel_size();
        let y = (h - text_h) / 2;

        let (fr, fg, fb) = self.colors.fg;
        cr.set_source_rgb(fr, fg, fb);
        cr.move_to(x as f64, y as f64);
        pangocairo::functions::show_layout(&cr, &layout);
        let text_start_x = x;
        x += text_w;

        if let Some(ref comp) = prompt.completion
            && comp.len() > prompt.input.len()
        {
            let ghost = &comp[prompt.input.len()..];
            layout.set_text(ghost);
            let (_ghost_w, _) = layout.pixel_size();
            cr.set_source_rgb(fr * 0.4, fg * 0.4, fb * 0.4);
            cr.move_to(x as f64, y as f64);
            pangocairo::functions::show_layout(&cr, &layout);
        }

        layout.set_text(&prompt_text[..prefix.len() + prompt.cursor]);
        let (cursor_x, _) = layout.pixel_size();
        cr.set_source_rgb(fr, fg, fb);
        cr.rectangle(
            (text_start_x + cursor_x) as f64,
            4.0,
            1.0,
            (h - 8) as f64,
        );
        let _ = cr.fill();

        drop(cr);
        self.panels[pi].surface.flush();
    }

    fn render_title(&mut self, pi: usize, center_text: &str, is_notification: bool) {
        let rect = self.panels[pi].rects.title;
        let cr = match cairo::Context::new(&self.panels[pi].surface) {
            Ok(c) => c,
            Err(_) => return,
        };
        cr.set_font_options(&self.font_options);

        clear_rect(&cr, rect, self.colors.bg);

        if pi != 0 {
            drop(cr);
            self.panels[pi].surface.flush();
            return;
        }
        if center_text.is_empty() {
            drop(cr);
            self.panels[pi].surface.flush();
            return;
        }

        let layout = pangocairo::functions::create_layout(&cr);
        layout.set_font_description(Some(&self.font_desc));
        let pango_ctx = layout.context();
        pangocairo::functions::context_set_font_options(&pango_ctx, Some(&self.font_options));

        // Title text is centered on the PANEL midpoint (not the title
        // rect's midpoint), but clipped to the title rect so it can't
        // overrun into adjacent regions.
        let panel_w = self.panels[pi].width as i32;
        let panel_mid = panel_w / 2;
        let max_w = rect.w - 40; // leave a little padding
        if max_w < 50 {
            drop(cr);
            self.panels[pi].surface.flush();
            return;
        }

        layout.set_text(center_text);
        layout.set_ellipsize(pango::EllipsizeMode::End);
        layout.set_width(max_w * pango::SCALE);
        let (text_w, text_h) = layout.pixel_size();
        let mut title_x = panel_mid - text_w / 2;
        // Clamp within the title rect.
        if title_x < rect.x + 20 {
            title_x = rect.x + 20;
        }
        if title_x + text_w > rect.x + rect.w - 20 {
            title_x = rect.x + rect.w - 20 - text_w;
        }
        let title_y = (rect.h - text_h) / 2;

        let (cr_r, cr_g, cr_b) = if is_notification {
            self.colors.notification_fg
        } else {
            self.colors.fg
        };
        cr.set_source_rgb(cr_r, cr_g, cr_b);
        cr.move_to(title_x as f64, title_y as f64);
        pangocairo::functions::show_layout(&cr, &layout);

        layout.set_width(-1);
        layout.set_ellipsize(pango::EllipsizeMode::None);

        drop(cr);
        self.panels[pi].surface.flush();
    }

    fn render_clock(&mut self, pi: usize) {
        let rect = self.panels[pi].rects.clock;
        let cr = match cairo::Context::new(&self.panels[pi].surface) {
            Ok(c) => c,
            Err(_) => return,
        };
        cr.set_font_options(&self.font_options);

        clear_rect(&cr, rect, self.colors.bg);

        let layout = pangocairo::functions::create_layout(&cr);
        layout.set_font_description(Some(&self.font_desc));
        let pango_ctx = layout.context();
        pangocairo::functions::context_set_font_options(&pango_ctx, Some(&self.font_options));

        let clock_text = &self.last_clock_text;
        let clock_markup = format!(
            "<span font_features=\"tnum\">{}</span>",
            escape_markup(clock_text),
        );
        layout.set_markup(&clock_markup);
        let (clock_w, clock_h) = layout.pixel_size();

        // Render flush to the right edge of the panel, within clock rect.
        let panel_w = self.panels[pi].width as i32;
        let clock_x = panel_w - clock_w - 10;
        let clock_y = (rect.h - clock_h) / 2;

        let (fr, fg, fb) = self.colors.fg;
        cr.set_source_rgb(fr, fg, fb);
        cr.move_to(clock_x as f64, clock_y as f64);
        pangocairo::functions::show_layout(&cr, &layout);

        layout.set_attributes(None);

        drop(cr);
        self.panels[pi].surface.flush();
    }

    fn format_clock(&self) -> String {
        unsafe {
            let mut t: libc::time_t = 0;
            libc::time(&mut t);
            let mut tm: libc::tm = std::mem::zeroed();
            libc::localtime_r(&t, &mut tm);
            let mut buf = [0u8; 256];
            let fmt = match CString::new(self.clock_format.as_bytes()) {
                Ok(f) => f,
                Err(_) => return String::new(),
            };
            let len = libc::strftime(
                buf.as_mut_ptr() as *mut libc::c_char,
                buf.len(),
                fmt.as_ptr(),
                &tm,
            );
            String::from_utf8_lossy(&buf[..len]).to_string()
        }
    }

    pub fn start_prompt(&mut self) {
        self.prompt = Some(PromptState {
            input: String::new(),
            cursor: 0,
            completion: None,
        });
    }

    pub fn update_appearance(
        &mut self,
        config: &Config,
        conn: &RustConnection,
        atoms: &Atoms,
        monitors: &[Monitor],
    ) {
        self.colors = BarColors {
            bg: parse_hex_local(&config.bar.bg),
            fg: parse_hex_local(&config.bar.fg),
            tag_focused_bg: parse_hex_local(&config.bar.tag_focused_bg),
            tag_focused_fg: parse_hex_local(&config.bar.tag_focused_fg),
            tag_occupied_fg: parse_hex_local(&config.bar.tag_occupied_fg),
            tag_empty_fg: parse_hex_local(&config.bar.tag_empty_fg),
            notification_fg: parse_hex_local(&config.bar.notification_fg),
        };
        self.font_desc = pango::FontDescription::from_string(&config.bar.font);
        self.clock_format = config.bar.clock_format.clone();

        // Font may have changed, which shifts the rendered tag strip
        // width; recompute before slicing rects so the prompt still abuts
        // the last tag.
        let tags_w = measure_tags_total_w(&self.font_desc, &self.font_options, &self.tag_names);

        // Height change: reconfigure each panel window, refresh strut,
        // reallocate the panel's surface (new height = different buffer
        // size).
        if config.bar.height != self.height {
            let new_height = config.bar.height;
            for (panel, mon) in self.panels.iter_mut().zip(monitors.iter()) {
                let _ = conn.configure_window(
                    panel.window,
                    &ConfigureWindowAux::new().height(new_height),
                );
                let strut: [u32; 12] = [
                    0,
                    0,
                    new_height,
                    0,
                    0,
                    0,
                    0,
                    0,
                    mon.x as u32,
                    mon.x as u32 + mon.width - 1,
                    0,
                    0,
                ];
                let _ = conn.change_property32(
                    PropMode::REPLACE,
                    panel.window,
                    atoms._NET_WM_STRUT_PARTIAL,
                    AtomEnum::CARDINAL,
                    &strut,
                );

                if let Ok(new_surface) = cairo::ImageSurface::create(
                    cairo::Format::Rgb24,
                    panel.width as i32,
                    new_height as i32,
                ) {
                    panel.surface = new_surface;
                }
            }
            self.height = new_height;
            let _ = conn.flush();
        }

        // Always refresh rects: font changes alone alter tags_w even when
        // height is stable.
        for panel in self.panels.iter_mut() {
            panel.rects = region_rects(panel.width as i32, self.height as i32, tags_w);
        }

        // Any appearance change forces a full redraw.
        self.dirty.all = true;
    }

    pub fn handle_prompt_key(&mut self, keysym: u32) -> Option<String> {
        let prompt = self.prompt.as_mut()?;

        match keysym {
            0xff0d => {
                // Return/Enter
                if prompt.input.is_empty() {
                    self.prompt = None;
                    return None;
                }
                let cmd = prompt.input.clone();
                self.prompt = None;
                return Some(cmd);
            }
            0xff1b => {
                // Escape
                self.prompt = None;
                return None;
            }
            0xff09 => {
                // Tab: accept completion
                if let Some(ref comp) = prompt.completion {
                    prompt.input = comp.clone();
                    prompt.cursor = prompt.input.len();
                    prompt.completion = find_completion(&self.path_executables, &prompt.input);
                }
            }
            0xff08 => {
                // BackSpace
                if prompt.cursor > 0 {
                    prompt.input.remove(prompt.cursor - 1);
                    prompt.cursor -= 1;
                    prompt.completion = find_completion(&self.path_executables, &prompt.input);
                }
                if prompt.input.is_empty() {
                    prompt.completion = None;
                }
            }
            sym if (0x0020..=0x007e).contains(&sym) => {
                // Printable ASCII
                let ch = sym as u8 as char;
                prompt.input.insert(prompt.cursor, ch);
                prompt.cursor += 1;
                prompt.completion = find_completion(&self.path_executables, &prompt.input);
            }
            _ => {}
        }
        None
    }
}

// Paint `rect` with `(r, g, b)` on the current Cairo context. Used at the
// start of each region render to clear the old pixels.
fn clear_rect(cr: &cairo::Context, rect: Rect, (r, g, b): (f64, f64, f64)) {
    cr.save().ok();
    cr.rectangle(rect.x as f64, rect.y as f64, rect.w as f64, rect.h as f64);
    cr.clip();
    cr.set_source_rgb(r, g, b);
    let _ = cr.paint();
    cr.restore().ok();
}

// Extract `rect` bytes from the panel's persistent surface and put_image
// them to the panel window at (rect.x, rect.y). Row-by-row memcpy out of
// the stride-aligned surface buffer into a contiguous rect-sized buffer.
fn put_region(panel: &BarPanel, conn: &RustConnection, rect: Rect, depth: u8) {
    if rect.w <= 0 || rect.h <= 0 {
        return;
    }
    let stride = panel.surface.stride() as usize;
    let row_bytes = (rect.w as usize) * 4;
    let _ = panel.surface.with_data(|full| {
        let mut buf: Vec<u8> = Vec::with_capacity(row_bytes * rect.h as usize);
        for row in 0..rect.h as usize {
            let src_row = rect.y as usize + row;
            let start = src_row * stride + (rect.x as usize) * 4;
            buf.extend_from_slice(&full[start..start + row_bytes]);
        }
        let _ = conn.put_image(
            ImageFormat::Z_PIXMAP,
            panel.window,
            panel.gc,
            rect.w as u16,
            rect.h as u16,
            rect.x as i16,
            rect.y as i16,
            0,
            depth,
            &buf,
        );
    });
}

// Kept public because tests/bar.rs references bar::parse_hex directly.
// Delegates to torrentius where the actual implementation lives.
#[allow(dead_code)]
pub fn parse_hex(color: &str) -> (f64, f64, f64) {
    crate::torrentius::parse_hex(color)
}

fn parse_hex_local(color: &str) -> (f64, f64, f64) {
    crate::torrentius::parse_hex(color)
}

// Escape text for Pango markup so a user-chosen clock_format containing
// &, <, or > doesn't corrupt the surrounding <span font_features="tnum">.
fn escape_markup(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            _ => out.push(c),
        }
    }
    out
}

pub fn scan_path() -> Vec<String> {
    let path = match std::env::var("PATH") {
        Ok(p) => p,
        Err(_) => return Vec::new(),
    };

    let mut names: Vec<String> = Vec::new();

    for dir in path.split(':') {
        let entries = match std::fs::read_dir(dir) {
            Ok(e) => e,
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            let meta = match entry.metadata() {
                Ok(m) => m,
                Err(_) => continue,
            };
            if !meta.is_file() {
                continue;
            }
            if meta.permissions().mode() & 0o111 == 0 {
                continue;
            }
            if let Some(name) = entry.file_name().to_str() {
                names.push(name.to_string());
            }
        }
    }

    names.sort_unstable();
    names.dedup();
    names
}

pub fn find_completion(executables: &[String], input: &str) -> Option<String> {
    if input.is_empty() {
        return None;
    }
    executables
        .iter()
        .find(|name| name.starts_with(input))
        .cloned()
}
