// Bar: in-process compositor element. Each render frame, the
// compositor calls Bar::texture() to obtain a GlesTexture sized to
// the monitor's width × bar height, with the current tag strip and
// clock rendered into it via Cairo+Pango (the same `torrentius`
// primitives the X11 bar uses). The render loop then composites the
// texture as a TextureRenderElement at the top of the screen, above
// the tiled clients.
//
// Why in-process: X11's bar is its own override-redirect window. The
// Wayland equivalent would be a layer-shell client, but that doubles
// the surface count and forces a separate render pipeline. Doing it
// inside the compositor reuses the GlesRenderer that already handles
// client surfaces and lets the bar share state with the Wm directly
// (no IPC for active_tag changes, no protocol round-trips for redraw
// triggers).
//
// Scope for v0.8.0: bg, tag strip with active tag highlighted, clock
// on the right. The X11 bar's prompt + title + completion regions
// land alongside keybind dispatch in a follow-on slice -- those
// require interactive input that the dev winit backend can route but
// the X11 path's keybind dispatch hasn't been ported yet.

use std::time::Instant;

use smithay::backend::allocator::Fourcc;
use smithay::backend::renderer::ImportMem;
use smithay::backend::renderer::gles::{GlesRenderer, GlesTexture};
use smithay::utils::{Buffer, Size};

use demiurge_core::config::Config;
use demiurge_core::torrentius;

pub struct Bar {
    pub height: u32,
    bg: (f64, f64, f64),
    fg: (f64, f64, f64),
    tag_focused_bg: (f64, f64, f64),
    tag_focused_fg: (f64, f64, f64),
    tag_occupied_fg: (f64, f64, f64),
    tag_empty_fg: (f64, f64, f64),
    font_desc: pango::FontDescription,
    font_options: cairo::FontOptions,
    tag_names: Vec<String>,
    clock_format: String,

    // Cached upload state. The texture is recreated when width
    // changes; otherwise we update its pixels in place via
    // ImportMem::update_memory. last_redraw_at gates redraws to once
    // per second so the clock advances without redrawing every frame.
    texture: Option<GlesTexture>,
    last_width: u32,
    last_redraw_at: Option<Instant>,
    last_active_tag: Option<usize>,
}

impl Bar {
    pub fn new(cfg: &Config) -> Result<Self, String> {
        let font_desc = pango::FontDescription::from_string(&cfg.bar.font);
        let font_options = torrentius::make_font_options()?;
        Ok(Self {
            height: cfg.bar.height,
            bg: torrentius::parse_hex(&cfg.bar.bg),
            fg: torrentius::parse_hex(&cfg.bar.fg),
            // Empty tag_focused_bg inherits the shared [appearance].accent,
            // matching the X11 bar's accent-fallback behaviour.
            tag_focused_bg: torrentius::parse_hex(if cfg.bar.tag_focused_bg.is_empty() {
                &cfg.appearance.accent
            } else {
                &cfg.bar.tag_focused_bg
            }),
            tag_focused_fg: torrentius::parse_hex(&cfg.bar.tag_focused_fg),
            tag_occupied_fg: torrentius::parse_hex(&cfg.bar.tag_occupied_fg),
            tag_empty_fg: torrentius::parse_hex(&cfg.bar.tag_empty_fg),
            font_desc,
            font_options,
            tag_names: cfg.general.tags.clone(),
            clock_format: cfg.bar.clock_format.clone(),
            texture: None,
            last_width: 0,
            last_redraw_at: None,
            last_active_tag: None,
        })
    }

    /// Render (or re-render) the bar into Cairo and (re-)upload as a
    /// GlesTexture. Returns the texture for compositing into the
    /// frame. Skips redraw when nothing relevant changed.
    pub fn texture(
        &mut self,
        renderer: &mut GlesRenderer,
        width: u32,
        active_tag: usize,
        occupied_tags: &[bool],
    ) -> Result<&GlesTexture, String> {
        let need_resize = self.last_width != width || self.texture.is_none();
        let clock_tick = self
            .last_redraw_at
            .map(|t| t.elapsed().as_secs() >= 1)
            .unwrap_or(true);
        let tag_changed = self.last_active_tag != Some(active_tag);
        let redraw = need_resize || clock_tick || tag_changed;

        if redraw {
            let pixels = self.draw(width, active_tag, occupied_tags)?;
            let size: Size<i32, Buffer> =
                Size::from((width as i32, self.height as i32));

            if need_resize {
                // Recreate the texture at the new size.
                let tex = renderer
                    .import_memory(&pixels, Fourcc::Argb8888, size, false)
                    .map_err(|e| format!("import_memory: {}", e))?;
                self.texture = Some(tex);
            } else {
                // Same dimensions: update in place to avoid the
                // allocation churn of recreating the texture every
                // second.
                if let Some(tex) = self.texture.as_mut() {
                    let rect = smithay::utils::Rectangle::from_size(size);
                    renderer
                        .update_memory(tex, &pixels, rect)
                        .map_err(|e| format!("update_memory: {}", e))?;
                }
            }

            self.last_width = width;
            self.last_redraw_at = Some(Instant::now());
            self.last_active_tag = Some(active_tag);
        }

        self.texture
            .as_ref()
            .ok_or_else(|| "bar texture missing after upload".to_string())
    }

    // Render bg + tag strip + clock into a fresh ARgb32 surface, then
    // pull the byte buffer out. Cairo's ARgb32 byte layout in memory
    // is BGRA on little-endian, which matches DRM Fourcc Argb8888 --
    // no per-pixel byte swap needed before upload.
    fn draw(
        &self,
        width: u32,
        active_tag: usize,
        occupied_tags: &[bool],
    ) -> Result<Vec<u8>, String> {
        let surface = cairo::ImageSurface::create(
            cairo::Format::ARgb32,
            width as i32,
            self.height as i32,
        )
        .map_err(|e| format!("cairo bar surface: {}", e))?;
        let cr =
            cairo::Context::new(&surface).map_err(|e| format!("cairo bar context: {}", e))?;
        cr.set_font_options(&self.font_options);

        // Background.
        cr.set_source_rgb(self.bg.0, self.bg.1, self.bg.2);
        cr.rectangle(0.0, 0.0, width as f64, self.height as f64);
        let _ = cr.fill();

        let layout = pangocairo::functions::create_layout(&cr);
        layout.set_font_description(Some(&self.font_desc));
        let pango_ctx = layout.context();
        pangocairo::functions::context_set_font_options(&pango_ctx, Some(&self.font_options));

        // Tag strip on the left.
        let edge_pad = 20.0;
        let mut x = edge_pad;
        for (i, name) in self.tag_names.iter().enumerate() {
            let padded = format!("  {}  ", name);
            layout.set_text(&padded);
            let (tw, th) = layout.pixel_size();
            let cell_w = tw as f64;
            let cell_h = self.height as f64;
            let text_y = (cell_h - th as f64) / 2.0;

            if i == active_tag {
                cr.set_source_rgb(
                    self.tag_focused_bg.0,
                    self.tag_focused_bg.1,
                    self.tag_focused_bg.2,
                );
                cr.rectangle(x, 0.0, cell_w, cell_h);
                let _ = cr.fill();
                cr.set_source_rgb(
                    self.tag_focused_fg.0,
                    self.tag_focused_fg.1,
                    self.tag_focused_fg.2,
                );
            } else if occupied_tags.get(i).copied().unwrap_or(false) {
                cr.set_source_rgb(
                    self.tag_occupied_fg.0,
                    self.tag_occupied_fg.1,
                    self.tag_occupied_fg.2,
                );
            } else {
                cr.set_source_rgb(
                    self.tag_empty_fg.0,
                    self.tag_empty_fg.1,
                    self.tag_empty_fg.2,
                );
            }
            cr.move_to(x, text_y);
            pangocairo::functions::show_layout(&cr, &layout);
            x += cell_w;
        }

        // Clock on the right. Format string from config; chrono drives
        // the formatting since it's already in the workspace via
        // gordian_knot/sysinfo (dev-time confirmed).
        let now_str = format_clock(&self.clock_format);
        layout.set_text(&now_str);
        let (cw, ch) = layout.pixel_size();
        let cy = (self.height as f64 - ch as f64) / 2.0;
        cr.set_source_rgb(self.fg.0, self.fg.1, self.fg.2);
        cr.move_to(width as f64 - cw as f64 - edge_pad, cy);
        pangocairo::functions::show_layout(&cr, &layout);

        // Snapshot the byte buffer. Cairo ARgb32 is 32-bit per pixel;
        // stride may be > width*4 due to alignment, so copy row-by-row
        // into a tightly-packed Vec.
        drop(cr);
        let mut surface = surface;
        let stride = surface.stride() as usize;
        let row_bytes = (width as usize) * 4;
        let data =
            surface.data().map_err(|e| format!("cairo surface data: {}", e))?;
        let mut packed = Vec::with_capacity((self.height as usize) * row_bytes);
        for row in 0..self.height as usize {
            let off = row * stride;
            packed.extend_from_slice(&data[off..off + row_bytes]);
        }
        Ok(packed)
    }
}

// Format the current local time. Uses libc::strftime to match the
// existing pattern in x11/src/wm.rs / gordian_knot/sysinfo -- avoids
// a new chrono dependency for one strftime call.
fn format_clock(format: &str) -> String {
    unsafe {
        let mut buf = [0i8; 64];
        let mut tm: libc::tm = std::mem::zeroed();
        let now = libc::time(std::ptr::null_mut());
        libc::localtime_r(&now, &mut tm);
        let cfmt = match std::ffi::CString::new(format) {
            Ok(s) => s,
            Err(_) => return String::new(),
        };
        let len = libc::strftime(
            buf.as_mut_ptr(),
            buf.len(),
            cfmt.as_ptr(),
            &tm,
        );
        if len == 0 {
            return String::new();
        }
        std::ffi::CStr::from_ptr(buf.as_ptr())
            .to_string_lossy()
            .into_owned()
    }
}
