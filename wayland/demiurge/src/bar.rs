// Bar: in-process compositor element. Each render frame, the
// compositor calls Bar::element() to obtain a TextureRenderElement sized
// to the monitor's width × bar height, drawn at the top of the screen above
// every window. core's painter draws it, as it does the X11 bar, and says
// which regions it drew; those alone are uploaded.
//
// Why in-process: X11's bar is its own override-redirect window. The
// Wayland equivalent would be a layer-shell client, but that doubles
// the surface count and forces a separate render pipeline. Doing it
// inside the compositor reuses the GlesRenderer that already handles
// client surfaces and lets the bar share state with the Wm directly
// (no IPC for active_tag changes, no protocol round-trips for redraw
// triggers).

use smithay::backend::allocator::Fourcc;
use smithay::backend::renderer::ImportMem;
use smithay::backend::renderer::element::Kind;
use smithay::backend::renderer::element::texture::{TextureBuffer, TextureRenderBuffer, TextureRenderElement};
use smithay::backend::renderer::gles::{GlesRenderer, GlesTexture};
use smithay::utils::{Buffer, Point, Rectangle, Size, Transform};

use demiurge_core::bar::{Painter, Panel, View};
use demiurge_core::config::Config;
use demiurge_core::torrentius;

pub struct Bar {
    painter: Painter,
    // Each monitor's bar and its texture, by monitor index.
    panels: Vec<Option<(Panel, TextureRenderBuffer<GlesTexture>)>>,
    // The tag menu's pixels, and the version of the menu they show.
    menu: Option<(u64, TextureBuffer<GlesTexture>)>,
    // Each titled window's bar: Its title, focus and width as drawn, and
    // its pixels.
    titles: std::collections::HashMap<u64, (String, bool, u32, TextureBuffer<GlesTexture>)>,
    // The Alt+Tab overlay's panel: Its size, columns, titles and choice as
    // drawn, and its pixels.
    cycle_panel: Option<((i32, i32), i32, Vec<String>, usize, TextureBuffer<GlesTexture>)>,
}

impl Bar {
    pub fn new(cfg: &Config) -> Result<Self, String> {
        Ok(Self {
            painter: Painter::new(cfg)?,
            panels: Vec::new(),
            menu: None,
            titles: std::collections::HashMap::new(),
            cycle_panel: None,
        })
    }

    pub fn height(&self) -> u32 {
        self.painter.height
    }

    /// Each tag's span on the bar, left and right edge.
    pub fn tag_extents(&self) -> &[(i32, i32)] {
        self.painter.tag_extents()
    }

    /// The clock read again; true when it shows something new.
    pub fn tick_clock(&mut self) -> bool {
        self.painter.tick_clock()
    }

    /// Monitor `monitor`'s bar as a render element at `location`: Uploaded
    /// whole when it is new or its width changed, otherwise only in the
    /// regions the painter drew again.
    pub fn element(
        &mut self,
        renderer: &mut GlesRenderer,
        monitor: usize,
        location: (i32, i32),
        width: u32,
        view: &View,
    ) -> Result<TextureRenderElement<GlesTexture>, String> {
        let size = Size::<i32, Buffer>::from((width.max(1) as i32, self.painter.height.max(1) as i32));
        let opaque = Some(vec![Rectangle::<i32, Buffer>::from_size(size)]);
        if self.panels.len() <= monitor {
            self.panels.resize_with(monitor + 1, || None);
        }
        let painter = &self.painter;
        let shown = &mut self.panels[monitor];
        match shown {
            Some((panel, buffer)) if panel.width() == width => {
                let painted = painter.paint(panel, view);
                if !painted.is_empty() {
                    panel
                        .with_pixels(|pixels, _| {
                            painted.rects(panel).try_for_each(|r| {
                                let region = Rectangle::new((r.x, r.y).into(), (r.w, r.h).into());
                                buffer.update_from_memory(renderer, pixels, region, opaque.clone())
                            })
                        })?
                        .map_err(|e| format!("update_memory: {}", e))?;
                }
            }
            _ => {
                let mut panel = painter.panel(width)?;
                painter.paint(&mut panel, view);
                let buffer = panel
                    .with_pixels(|pixels, _| {
                        TextureRenderBuffer::from_memory(
                            renderer,
                            pixels,
                            Fourcc::Xrgb8888,
                            size,
                            false,
                            1,
                            Transform::Normal,
                            opaque,
                        )
                    })?
                    .map_err(|e| format!("import_memory: {}", e))?;
                *shown = Some((panel, buffer));
            }
        }
        let (_, buffer) = shown.as_ref().ok_or("bar panel missing")?;
        Ok(TextureRenderElement::from_texture_render_buffer(
            Point::from((location.0 as f64, location.1 as f64)),
            buffer,
            None,
            None,
            None,
            Kind::Unspecified,
        ))
    }

    /// Window `id`'s title bar at `location`, `w` wide at `scale`, in the
    /// bar's font and colors; drawn again only when its title, focus or
    /// width changed.
    #[allow(clippy::too_many_arguments)]
    pub fn title_element(
        &mut self,
        renderer: &mut GlesRenderer,
        id: u64,
        location: (i32, i32),
        w: u32,
        title: &str,
        active: bool,
        scale: f64,
    ) -> Result<TextureRenderElement<GlesTexture>, String> {
        let drawn = self.titles.get(&id).is_some_and(|(t, a, tw, _)| t == title && *a == active && *tw == w);
        if !drawn {
            let h = demiurge_core::titlebar::HEIGHT as i32;
            let (mut surface, cr) = torrentius::new_surface(w.max(1) as i32, h)?;
            let style = &self.painter.style;
            let layout = style.layout(&cr);
            let colors = demiurge_core::titlebar::Colors { bg: style.bg, fg: style.fg };
            demiurge_core::titlebar::draw(&cr, &layout, w as f64, title, active, &colors);
            drop((layout, cr));
            surface.flush();
            let stride = surface.stride() as usize;
            let row_bytes = w.max(1) as usize * 4;
            let data = surface.data().map_err(|e| format!("title bar surface: {}", e))?;
            let mut pixels = Vec::with_capacity(row_bytes * h as usize);
            for row in 0..h as usize {
                pixels.extend_from_slice(&data[row * stride..row * stride + row_bytes]);
            }
            drop(data);
            let size = Size::<i32, Buffer>::from((w.max(1) as i32, h));
            let texture = renderer
                .import_memory(&pixels, Fourcc::Xrgb8888, size, false)
                .map_err(|e| format!("import_memory: {}", e))?;
            let opaque = Rectangle::<i32, Buffer>::from_size(size);
            let buffer = TextureBuffer::from_texture(renderer, texture, 1, Transform::Normal, Some(vec![opaque]));
            self.titles.insert(id, (title.to_string(), active, w, buffer));
        }
        let buffer = &self.titles.get(&id).ok_or("title bar missing")?.3;
        let size = ((w as f64 * scale).round() as i32, (demiurge_core::titlebar::HEIGHT as f64 * scale).round() as i32);
        Ok(TextureRenderElement::from_texture_buffer(
            Point::from((location.0 as f64, location.1 as f64)),
            buffer,
            None,
            None,
            Some(size.into()),
            Kind::Unspecified,
        ))
    }

    /// The Alt+Tab overlay's panel at `location`, `size` big, its tiles in
    /// `cols` columns: Each tile framed, the chosen one in the accent, its
    /// title under it.
    pub fn cycle_element(
        &mut self,
        renderer: &mut GlesRenderer,
        location: (i32, i32),
        size: (i32, i32),
        cols: i32,
        titles: &[String],
        chosen: usize,
    ) -> Result<TextureRenderElement<GlesTexture>, String> {
        use crate::cycle::{LABEL_H, PAD, TILE_H, TILE_W};
        let drawn = self
            .cycle_panel
            .as_ref()
            .is_some_and(|(s, c, t, ch, _)| *s == size && *c == cols && t.as_slice() == titles && *ch == chosen);
        if !drawn {
            let (w, h) = size;
            let (mut surface, cr) = torrentius::new_surface(w, h)?;
            let style = &self.painter.style;
            let layout = style.layout(&cr);
            layout.set_width(TILE_W * pango::SCALE);
            layout.set_ellipsize(pango::EllipsizeMode::End);
            layout.set_alignment(pango::Alignment::Center);
            cr.set_source_rgb(style.bg.0, style.bg.1, style.bg.2);
            let _ = cr.paint();
            cr.set_source_rgb(style.tag_empty_fg.0, style.tag_empty_fg.1, style.tag_empty_fg.2);
            cr.set_line_width(1.0);
            cr.rectangle(0.5, 0.5, w as f64 - 1.0, h as f64 - 1.0);
            let _ = cr.stroke();
            for (i, title) in titles.iter().enumerate() {
                let i = i as i32;
                let (tx, ty) = (PAD + (i % cols) * (TILE_W + PAD), PAD + (i / cols) * (TILE_H + LABEL_H + PAD));
                let (ink, line) = if i as usize == chosen { (style.tag_focused_bg, 3.0) } else { (style.tag_empty_fg, 1.0) };
                cr.set_source_rgb(ink.0, ink.1, ink.2);
                cr.set_line_width(line);
                cr.rectangle(tx as f64 - 3.0, ty as f64 - 3.0, TILE_W as f64 + 6.0, TILE_H as f64 + 6.0);
                let _ = cr.stroke();
                layout.set_text(if title.is_empty() { "(untitled)" } else { title });
                cr.set_source_rgb(style.fg.0, style.fg.1, style.fg.2);
                cr.move_to(tx as f64, (ty + TILE_H + 6) as f64);
                pangocairo::functions::show_layout(&cr, &layout);
            }
            drop((layout, cr));
            surface.flush();
            let stride = surface.stride() as usize;
            let row_bytes = w as usize * 4;
            let data = surface.data().map_err(|e| format!("cycle panel surface: {}", e))?;
            let mut pixels = Vec::with_capacity(row_bytes * h as usize);
            for row in 0..h as usize {
                pixels.extend_from_slice(&data[row * stride..row * stride + row_bytes]);
            }
            drop(data);
            let buffer_size = Size::<i32, Buffer>::from((w, h));
            let texture = renderer
                .import_memory(&pixels, Fourcc::Xrgb8888, buffer_size, false)
                .map_err(|e| format!("import_memory: {}", e))?;
            let opaque = Rectangle::<i32, Buffer>::from_size(buffer_size);
            let buffer = TextureBuffer::from_texture(renderer, texture, 1, Transform::Normal, Some(vec![opaque]));
            self.cycle_panel = Some((size, cols, titles.to_vec(), chosen, buffer));
        }
        let buffer = &self.cycle_panel.as_ref().ok_or("cycle panel missing")?.4;
        Ok(TextureRenderElement::from_texture_buffer(
            Point::from((location.0 as f64, location.1 as f64)),
            buffer,
            None,
            None,
            None,
            Kind::Unspecified,
        ))
    }

    /// Forget the bars of windows no longer titled.
    pub fn keep_titles(&mut self, titled: impl Fn(u64) -> bool) {
        self.titles.retain(|&id, _| titled(id));
    }

    /// The tag menu as a render element at `location`, its pixels uploaded
    /// again only when it changed.
    pub fn menu_element(
        &mut self,
        renderer: &mut GlesRenderer,
        location: (i32, i32),
        open: &crate::tag_menu::TagMenu,
        style: &demiurge_core::tag_menu::Style,
    ) -> Result<TextureRenderElement<GlesTexture>, String> {
        if self.menu.as_ref().map(|(v, _)| *v) != Some(open.version) {
            let mut surface = open.menu.draw(style)?;
            let (w, h) = (open.menu.w, open.menu.height());
            let stride = surface.stride() as usize;
            let row_bytes = w as usize * 4;
            let data = surface.data().map_err(|e| format!("tag menu surface: {}", e))?;
            let mut pixels = Vec::with_capacity(row_bytes * h as usize);
            for row in 0..h as usize {
                pixels.extend_from_slice(&data[row * stride..row * stride + row_bytes]);
            }
            drop(data);
            let size = Size::<i32, Buffer>::from((w, h));
            let texture = renderer
                .import_memory(&pixels, Fourcc::Xrgb8888, size, false)
                .map_err(|e| format!("import_memory: {}", e))?;
            let opaque = Rectangle::<i32, Buffer>::from_size(size);
            let buffer = TextureBuffer::from_texture(renderer, texture, 1, Transform::Normal, Some(vec![opaque]));
            self.menu = Some((open.version, buffer));
        }
        let buffer = &self.menu.as_ref().ok_or("tag menu buffer missing")?.1;
        Ok(TextureRenderElement::from_texture_buffer(
            Point::from((location.0 as f64, location.1 as f64)),
            buffer,
            None,
            None,
            None,
            Kind::Unspecified,
        ))
    }
}
