// Title-bar frames. With [general] title_bars on, a floating normal or
// dialog window is reparented into a frame DEMIURGE draws the title bar on
// (demiurge_core::titlebar), the client below it. The frame takes the
// client's place on screen: The client's x, y, w and h are the frame's rect,
// title bar included, and the frame is what maps, unmaps and stacks while
// the client stays mapped inside it. Every framed client is in the save-set,
// so the server hands it back to the root if DEMIURGE goes away.

use x11rb::connection::Connection;
use x11rb::protocol::xproto::*;
use x11rb::rust_connection::RustConnection;

use demiurge_core::titlebar::{self, Light};

use crate::config::Config;
use crate::ewmh;
use crate::layout::Layout;
use crate::wm::Wm;

#[derive(Debug, Clone)]
pub struct Frame {
    pub window: Window,
    gc: Gcontext,
    // The width the bar was last drawn at; a move alone does not redraw.
    drawn_w: u32,
}

pub struct Style {
    font: pango::FontDescription,
    options: cairo::FontOptions,
    colors: titlebar::Colors,
    bg_pixel: u32,
}

impl Style {
    // The bar's font and colors.
    pub fn from_config(config: &Config) -> Result<Self, String> {
        Ok(Self {
            font: pango::FontDescription::from_string(&config.bar.font),
            options: crate::torrentius::make_font_options()?,
            colors: titlebar::Colors {
                bg: crate::torrentius::parse_hex(&config.bar.bg),
                fg: crate::torrentius::parse_hex(&config.bar.fg),
            },
            bg_pixel: crate::torrentius::hex_to_pixel(&config.bar.bg),
        })
    }
}

impl Frame {
    fn create(conn: &RustConnection, root: Window, (x, y, w, h): (i32, i32, u32, u32), bg_pixel: u32) -> Result<Self, String> {
        let window = conn.generate_id().map_err(|e| format!("frame id: {e}"))?;
        let gc = conn.generate_id().map_err(|e| format!("frame gc id: {e}"))?;
        conn.create_window(
            x11rb::COPY_DEPTH_FROM_PARENT,
            window,
            root,
            x as i16,
            y as i16,
            w.max(1) as u16,
            h.max(1) as u16,
            0,
            WindowClass::INPUT_OUTPUT,
            x11rb::COPY_FROM_PARENT,
            &CreateWindowAux::new().background_pixel(bg_pixel).event_mask(
                EventMask::SUBSTRUCTURE_REDIRECT
                    | EventMask::SUBSTRUCTURE_NOTIFY
                    | EventMask::EXPOSURE
                    | EventMask::BUTTON_PRESS,
            ),
        )
        .map_err(|e| format!("create frame: {e}"))?;
        conn.create_gc(gc, window, &CreateGCAux::new()).map_err(|e| format!("frame gc: {e}"))?;
        Ok(Self { window, gc, drawn_w: 0 })
    }

    fn draw(&mut self, conn: &RustConnection, w: u32, title: &str, active: bool, style: &Style) {
        let h = titlebar::HEIGHT as i32;
        let Ok((surface, cr)) = crate::torrentius::new_surface(w.max(1) as i32, h) else {
            return;
        };
        cr.set_font_options(&style.options);
        let layout = pangocairo::functions::create_layout(&cr);
        layout.set_font_description(Some(&style.font));
        pangocairo::functions::context_set_font_options(&layout.context(), Some(&style.options));
        titlebar::draw(&cr, &layout, w as f64, title, active, &style.colors);
        drop(layout);
        drop(cr);
        surface.flush();

        let depth = conn.setup().roots[0].root_depth;
        let stride = surface.stride() as usize;
        let row_bytes = w.max(1) as usize * 4;
        let _ = surface.with_data(|data| {
            let mut buf = Vec::with_capacity(row_bytes * h as usize);
            for row in 0..h as usize {
                buf.extend_from_slice(&data[row * stride..row * stride + row_bytes]);
            }
            let _ = conn.put_image(ImageFormat::Z_PIXMAP, self.window, self.gc, w.max(1) as u16, h as u16, 0, 0, 0, depth, &buf);
        });
        self.drawn_w = w;
    }

    fn destroy(&self, conn: &RustConnection) {
        let _ = conn.free_gc(self.gc);
        let _ = conn.destroy_window(self.window);
    }
}

impl Wm {
    // The title bar a client shows now: Its frame's, unless fullscreen.
    pub fn decor(&self, pos: usize) -> u32 {
        let c = &self.clients[pos];
        if c.frame.is_some() && !c.fullscreen { titlebar::HEIGHT } else { 0 }
    }

    // The window that stands for a client on screen: Its frame, or itself.
    pub fn top_window(&self, pos: usize) -> Window {
        let c = &self.clients[pos];
        c.frame.as_ref().map_or(c.window, |f| f.window)
    }

    // The client whose window or frame `window` is.
    pub fn client_of(&self, window: Window) -> Option<usize> {
        self.clients
            .iter()
            .position(|c| c.window == window || c.frame.as_ref().is_some_and(|f| f.window == window))
    }

    // The client whose frame `window` is.
    pub fn frame_owner(&self, window: Window) -> Option<usize> {
        self.clients.iter().position(|c| c.frame.as_ref().is_some_and(|f| f.window == window))
    }

    // Whether a window takes a title bar: Normal windows and dialogs, unless
    // _MOTIF_WM_HINTS asks for no decorations (applications that draw their
    // own, like Chromium's tab strip and GTK header bars).
    pub fn wants_decoration(&self, window: Window) -> bool {
        let kind = self.window_type_name(window);
        if kind != "normal" && kind != "dialog" {
            return false;
        }
        let hints: Vec<u32> = self
            .conn
            .get_property(false, window, self.atoms._MOTIF_WM_HINTS, AtomEnum::ANY, 0, 5)
            .ok()
            .and_then(|c| c.reply().ok())
            .and_then(|r| r.value32().map(|v| v.collect()))
            .unwrap_or_default();
        // flags bit 1 (MWM_HINTS_DECORATIONS) makes the third field count.
        !matches!(hints.as_slice(), [flags, _, decorations, ..] if flags & 2 != 0 && *decorations == 0)
    }

    fn wants_frame(&self, pos: usize) -> bool {
        let c = &self.clients[pos];
        self.title_bars && self.frame_style.is_some() && c.decorated && (c.floating || self.layouts[c.tag] == Layout::Floating)
    }

    // Frame every client that should have one and unframe the rest: Run
    // before each arrange, so a layout change or a window floating out
    // brings its title bar with it.
    pub fn sync_frames(&mut self) {
        for pos in 0..self.clients.len() {
            // A fullscreen window keeps what it has until it comes back.
            if self.clients[pos].fullscreen {
                continue;
            }
            match (self.wants_frame(pos), self.clients[pos].frame.is_some()) {
                (true, false) => self.frame_client(pos),
                (false, true) => self.unframe_client(pos),
                _ => {}
            }
        }
    }

    fn frame_client(&mut self, pos: usize) {
        let Some(style) = self.frame_style.as_ref() else {
            return;
        };
        let c = &self.clients[pos];
        let window = c.window;
        let frame = match Frame::create(&self.conn, self.root, (c.x, c.y, c.w, c.h), style.bg_pixel) {
            Ok(f) => f,
            Err(e) => {
                eprintln!("[{}] [WARN]   {}", crate::wm::local_time(), e);
                return;
            }
        };
        let mapped = self
            .conn
            .get_window_attributes(window)
            .ok()
            .and_then(|c| c.reply().ok())
            .is_some_and(|a| a.map_state != MapState::UNMAPPED);
        // Reparenting a mapped window unmaps it first; that echo is ours.
        if mapped {
            let c = &mut self.clients[pos];
            c.unmap_ignore = c.unmap_ignore.saturating_add(1);
        }
        let _ = self.conn.change_save_set(SetMode::INSERT, window);
        let _ = self.conn.reparent_window(window, frame.window, 0, titlebar::HEIGHT as i16);
        let _ = self.conn.map_window(window);
        if mapped {
            let _ = self.conn.map_window(frame.window);
        }
        self.clients[pos].frame = Some(frame);
        ewmh::set_frame_extents(&self.conn, window, &self.atoms, titlebar::HEIGHT);
        self.apply_geometry(pos);
        self.draw_frame(pos);
    }

    fn unframe_client(&mut self, pos: usize) {
        let Some(frame) = self.clients[pos].frame.take() else {
            return;
        };
        let c = &self.clients[pos];
        let (window, x, y) = (c.window, c.x, c.y);
        let showing = self.tag_visible(c.tag) && !c.minimized;
        // The reparent's unmap is reported to the frame, which is gone by
        // the time DEMIURGE reads it; nothing to count.
        let _ = self.conn.reparent_window(window, self.root, x as i16, y as i16);
        let _ = self.conn.change_save_set(SetMode::DELETE, window);
        frame.destroy(&self.conn);
        ewmh::set_frame_extents(&self.conn, window, &self.atoms, 0);
        // The reparent mapped it on the root; right only if it was showing.
        if !showing {
            self.hide(pos);
        }
        self.apply_geometry(pos);
    }

    // A client leaving management: Out of its frame and back on the root
    // (a withdrawn window must outlive its frame), then the frame goes. For
    // a destroyed client the reparent fails harmlessly.
    pub fn drop_frame(&mut self, pos: usize) {
        let Some(frame) = self.clients[pos].frame.take() else {
            return;
        };
        let c = &self.clients[pos];
        let _ = self.conn.reparent_window(c.window, self.root, c.x as i16, (c.y + titlebar::HEIGHT as i32) as i16);
        let _ = self.conn.change_save_set(SetMode::DELETE, c.window);
        frame.destroy(&self.conn);
    }

    // Put a client at its recorded rect: The frame there with the client
    // below the bar, or the client itself. A framed client is told where it
    // landed on the root (ICCCM 4.1.5), since moving the frame does not.
    pub fn apply_geometry(&mut self, pos: usize) {
        let d = self.decor(pos);
        let c = &self.clients[pos];
        let (x, y, w, h) = (c.x, c.y, c.w.max(1), c.h.max(1));
        let Some(frame) = c.frame.as_ref() else {
            let _ = self.conn.configure_window(c.window, &ConfigureWindowAux::new().x(x).y(y).width(w).height(h));
            return;
        };
        let inner = h.saturating_sub(d).max(1);
        let _ = self.conn.configure_window(frame.window, &ConfigureWindowAux::new().x(x).y(y).width(w).height(h));
        let _ = self.conn.configure_window(c.window, &ConfigureWindowAux::new().x(0).y(d as i32).width(w).height(inner));
        let notify = ConfigureNotifyEvent {
            response_type: CONFIGURE_NOTIFY_EVENT,
            sequence: 0,
            event: c.window,
            window: c.window,
            above_sibling: x11rb::NONE,
            x: x as i16,
            y: (y + d as i32) as i16,
            width: w as u16,
            height: inner as u16,
            border_width: 0,
            override_redirect: false,
        };
        let _ = self.conn.send_event(false, c.window, EventMask::STRUCTURE_NOTIFY, notify);
        if frame.drawn_w != w {
            self.draw_frame(pos);
        }
    }

    // Record a client's rect and put it there.
    pub fn set_geometry(&mut self, pos: usize, x: i32, y: i32, w: u32, h: u32) {
        let c = &mut self.clients[pos];
        (c.x, c.y, c.w, c.h) = (x, y, w, h);
        self.apply_geometry(pos);
    }

    // Cover `rect` with the client, title bar hidden, and raise it: The
    // fullscreen geometry, which leaves the recorded rect alone for the way
    // back.
    pub fn place_fullscreen(&mut self, pos: usize, (x, y, w, h): (i32, i32, u32, u32)) {
        let c = &self.clients[pos];
        let top = c.frame.as_ref().map_or(c.window, |f| f.window);
        let _ = self.conn.configure_window(
            top,
            &ConfigureWindowAux::new().x(x).y(y).width(w).height(h).stack_mode(StackMode::ABOVE),
        );
        if c.frame.is_some() {
            let _ = self.conn.configure_window(c.window, &ConfigureWindowAux::new().x(0).y(0).width(w).height(h));
        }
    }

    pub fn show(&mut self, pos: usize) {
        let _ = self.conn.map_window(self.top_window(pos));
    }

    // Unmap a client's frame, or the client itself, counting the echo of an
    // unframed client's unmap as DEMIURGE's own.
    pub fn hide(&mut self, pos: usize) {
        let c = &mut self.clients[pos];
        match c.frame.as_ref() {
            Some(frame) => {
                let _ = self.conn.unmap_window(frame.window);
            }
            None => {
                c.unmap_ignore = c.unmap_ignore.saturating_add(1);
                let _ = self.conn.unmap_window(c.window);
            }
        }
    }

    pub fn raise(&mut self, pos: usize) {
        let _ = self.conn.configure_window(self.top_window(pos), &ConfigureWindowAux::new().stack_mode(StackMode::ABOVE));
    }

    pub fn draw_frame(&mut self, pos: usize) {
        let active = self.focus == Some(self.clients[pos].window);
        let Some(style) = self.frame_style.as_ref() else {
            return;
        };
        let c = &mut self.clients[pos];
        let w = c.w;
        if let Some(frame) = c.frame.as_mut() {
            frame.draw(&self.conn, w, &c.title, active, style);
        }
    }

    // A press on a frame's title bar: Focus the window, then a light acts or
    // the bar starts a move.
    pub fn on_frame_press(&mut self, pos: usize, ev: &ButtonPressEvent) {
        let window = self.clients[pos].window;
        self.focus_window(Some(window));
        if ev.detail != 1 || ev.event_y as u32 >= titlebar::HEIGHT {
            return;
        }
        match titlebar::light_at(ev.event_x as f64, ev.event_y as f64) {
            Some(Light::Close) => {
                ewmh::close_window(&self.conn, window, &self.atoms);
            }
            Some(Light::Minimize) => self.minimize(window),
            Some(Light::Zoom) if self.clients[pos].saved_zoom.is_some() => self.zoom_out(window),
            Some(Light::Zoom) => self.zoom_in(window),
            None => crate::mouse::start_csd_drag(self, window, ev.root_x, ev.root_y, crate::mouse::DragKind::Move),
        }
    }
}
