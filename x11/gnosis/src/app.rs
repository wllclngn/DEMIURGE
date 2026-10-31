// GNOSIS app: core's browser in an X window.
//
// A normal top-level window (the WM manages / tiles it -- not
// override-redirect like the lock or MEDUSA). The browser draws and
// answers keys and clicks; this file gives it the window, its size, focus
// and frame, rounds its corners while it draws its own title bar, and does
// what its lights ask of the window manager.

use std::os::fd::AsRawFd;
use std::path::PathBuf;

use x11rb::connection::Connection;
use x11rb::protocol::Event;
use x11rb::protocol::shape::{self, ConnectionExt as _};
use x11rb::protocol::xproto::*;
use x11rb::rust_connection::RustConnection;

use demiurge_core::config::Config;
use demiurge_core::torrentius;
use gnosis::browser::{self, Browser, Outcome};
use x11_client::keymap::{self as keys, KeyMap};

pub struct App {
    conn: RustConnection,
    win: Window,
    root: Window,
    gc: Gcontext,
    depth: u8,
    keymap: KeyMap,
    browser: Browser,
}

impl App {
    pub fn run(config: &Config, start: PathBuf) -> Result<(), String> {
        let (conn, screen_num) =
            RustConnection::connect(None).map_err(|e| format!("x11 connect: {}", e))?;
        let screen = &conn.setup().roots[screen_num];
        let root = screen.root;
        let depth = screen.root_depth;
        let (width, height) = (browser::WIDTH, browser::HEIGHT);

        let win = conn.generate_id().map_err(|e| format!("gen id: {}", e))?;
        let gc = conn.generate_id().map_err(|e| format!("gen gc: {}", e))?;
        conn.create_window(
            depth,
            win,
            root,
            0,
            0,
            width,
            height,
            0,
            WindowClass::INPUT_OUTPUT,
            0,
            &CreateWindowAux::new().background_pixel(screen.black_pixel).event_mask(
                EventMask::KEY_PRESS
                    | EventMask::BUTTON_PRESS
                    | EventMask::EXPOSURE
                    | EventMask::STRUCTURE_NOTIFY
                    | EventMask::PROPERTY_CHANGE
                    | EventMask::FOCUS_CHANGE,
            ),
        )
        .map_err(|e| format!("create_window: {}", e))?;
        conn.create_gc(gc, win, &CreateGCAux::new())
            .map_err(|e| format!("create_gc: {}", e))?;

        // Window manager hints: title + class, so DEMIURGE (and
        // cherrypie matching) can target it.
        set_wm_name(&conn, win, browser::TITLE);
        set_wm_class(&conn, win, browser::CLASS);

        let _ = conn.map_window(win);
        let _ = conn.flush();

        let keymap = keys::load_keymap(&conn);

        let mut app = App { conn, win, root, gc, depth, keymap, browser: Browser::new(config, start) };
        app.apply_shape();
        app.event_loop()
    }

    // Round the window's outer corners via the SHAPE extension, for the
    // macOS look. Best-effort: a no-op if SHAPE is unavailable. Builds a
    // 1-bit mask (body rectangles + quarter-circle corners) and sets it
    // as the window's bounding shape. Recomputed on resize.
    // Inside a window manager's frame the corners stay square.
    fn apply_shape(&self) {
        let (w, h) = self.browser.size();
        let r = browser::CORNER_R as u16;
        if w < r * 2 || h < r * 2 {
            return;
        }
        if self.conn.shape_query_version().ok().and_then(|c| c.reply().ok()).is_none() {
            return;
        }
        if self.browser.framed() {
            let _ = self.conn.shape_mask(shape::SO::SET, shape::SK::BOUNDING, self.win, 0, 0, x11rb::NONE);
            let _ = self.conn.flush();
            return;
        }
        let mask = match self.conn.generate_id() {
            Ok(id) => id,
            Err(_) => return,
        };
        let gc1 = match self.conn.generate_id() {
            Ok(id) => id,
            Err(_) => return,
        };
        let gc0 = match self.conn.generate_id() {
            Ok(id) => id,
            Err(_) => return,
        };
        // Depth-1 pixmap mask.
        if self.conn.create_pixmap(1, mask, self.win, w, h).is_err() {
            return;
        }
        let _ = self.conn.create_gc(gc1, mask, &CreateGCAux::new().foreground(1));
        let _ = self.conn.create_gc(gc0, mask, &CreateGCAux::new().foreground(0));
        // Clear to 0 (outside the shape).
        let _ = self.conn.poly_fill_rectangle(
            mask,
            gc0,
            &[Rectangle { x: 0, y: 0, width: w, height: h }],
        );
        // Body: a plus made of two rectangles, leaving the four corners.
        let _ = self.conn.poly_fill_rectangle(
            mask,
            gc1,
            &[
                Rectangle { x: r as i16, y: 0, width: w - 2 * r, height: h },
                Rectangle { x: 0, y: r as i16, width: w, height: h - 2 * r },
            ],
        );
        // Four corner quarter-circles (angles in 1/64 degree).
        let d = 2 * r;
        let _ = self.conn.poly_fill_arc(
            mask,
            gc1,
            &[
                Arc { x: 0, y: 0, width: d, height: d, angle1: 90 * 64, angle2: 90 * 64 },
                Arc { x: (w - d) as i16, y: 0, width: d, height: d, angle1: 0, angle2: 90 * 64 },
                Arc { x: 0, y: (h - d) as i16, width: d, height: d, angle1: 180 * 64, angle2: 90 * 64 },
                Arc {
                    x: (w - d) as i16,
                    y: (h - d) as i16,
                    width: d,
                    height: d,
                    angle1: 270 * 64,
                    angle2: 90 * 64,
                },
            ],
        );
        let _ = self.conn.shape_mask(
            shape::SO::SET,
            shape::SK::BOUNDING,
            self.win,
            0,
            0,
            mask,
        );
        let _ = self.conn.free_gc(gc1);
        let _ = self.conn.free_gc(gc0);
        let _ = self.conn.free_pixmap(mask);
        let _ = self.conn.flush();
    }

    // Whether the window manager frames GNOSIS, read from _NET_FRAME_EXTENTS.
    fn frame_extent_top(&self) -> bool {
        let extents = self.atom("_NET_FRAME_EXTENTS");
        self.conn
            .get_property(false, self.win, extents, AtomEnum::CARDINAL, 0, 4)
            .ok()
            .and_then(|c| c.reply().ok())
            .and_then(|r| r.value32().and_then(|mut v| v.nth(2)))
            .is_some_and(|top| top > 0)
    }

    fn event_loop(&mut self) -> Result<(), String> {
        self.render()?;
        let fd = self.conn.stream().as_raw_fd();
        let [thumbs_fd, drives_fd] = self.browser.wake_fds();
        let mut pfds = [
            libc::pollfd { fd, events: libc::POLLIN, revents: 0 },
            libc::pollfd { fd: thumbs_fd, events: libc::POLLIN, revents: 0 },
            libc::pollfd { fd: drives_fd, events: libc::POLLIN, revents: 0 },
        ];
        loop {
            // Drain events; only render when something changed.
            let mut dirty = self.browser.woke(pfds[1].revents & libc::POLLIN != 0, pfds[2].revents & libc::POLLIN != 0);
            loop {
                match self.conn.poll_for_event() {
                    Ok(Some(ev)) => match self.handle(ev) {
                        Outcome::Quit => return Ok(()),
                        Outcome::Dirty => dirty = true,
                        Outcome::Minimize => self.iconify(),
                        Outcome::Zoom => self.toggle_zoom(),
                        Outcome::Idle => {}
                    },
                    Ok(None) => break,
                    Err(e) => return Err(format!("poll_for_event: {}", e)),
                }
            }
            if dirty {
                self.render()?;
            }
            let _ = self.conn.flush();
            for p in &mut pfds {
                p.revents = 0;
            }
            // Block until the next X event or thumbnail; idle = zero work.
            let _ = unsafe { libc::poll(pfds.as_mut_ptr(), pfds.len() as libc::nfds_t, -1) };
        }
    }

    fn handle(&mut self, ev: Event) -> Outcome {
        match ev {
            Event::Expose(e) if e.count == 0 => Outcome::Dirty,
            Event::ConfigureNotify(e) => {
                if self.browser.resize(e.width, e.height) {
                    self.apply_shape();
                    Outcome::Dirty
                } else {
                    Outcome::Idle
                }
            }
            Event::KeyPress(kp) => {
                let shift = u16::from(kp.state) & u16::from(ModMask::SHIFT) != 0;
                self.browser.key(keycode_to_keysym(kp.detail, shift, &self.keymap))
            }
            Event::ButtonPress(bp) => self.browser.click(bp.event_x as f64, bp.event_y as f64),
            Event::PropertyNotify(p) if p.atom == self.atom("_NET_FRAME_EXTENTS") => {
                if !self.browser.set_framed(self.frame_extent_top()) {
                    return Outcome::Idle;
                }
                self.apply_shape();
                Outcome::Dirty
            }
            Event::FocusIn(_) | Event::FocusOut(_) => {
                self.browser.set_focused(matches!(ev, Event::FocusIn(_)));
                Outcome::Dirty
            }
            _ => Outcome::Idle,
        }
    }

    fn atom(&self, name: &str) -> Atom {
        self.conn
            .intern_atom(false, name.as_bytes())
            .ok()
            .and_then(|c| c.reply().ok())
            .map(|r| r.atom)
            .unwrap_or(0)
    }

    // ICCCM WM_CHANGE_STATE -> IconicState (3): ask the WM to minimize.
    fn iconify(&self) {
        let wm_change_state = self.atom("WM_CHANGE_STATE");
        if wm_change_state == 0 {
            return;
        }
        let ev = ClientMessageEvent::new(32, self.win, wm_change_state, [3, 0, 0, 0, 0]);
        let _ = self.conn.send_event(
            false,
            self.root,
            EventMask::SUBSTRUCTURE_NOTIFY | EventMask::SUBSTRUCTURE_REDIRECT,
            ev,
        );
        let _ = self.conn.flush();
    }

    // Zoom is EWMH maximize: A _NET_WM_STATE toggle of both maximized
    // states, which DEMIURGE answers with its Super+= zoom.
    fn toggle_zoom(&self) {
        let net_wm_state = self.atom("_NET_WM_STATE");
        let vert = self.atom("_NET_WM_STATE_MAXIMIZED_VERT");
        let horz = self.atom("_NET_WM_STATE_MAXIMIZED_HORZ");
        if net_wm_state == 0 || vert == 0 || horz == 0 {
            return;
        }
        // data: [action(2=toggle), prop1, prop2, source(1=app), 0]
        let ev = ClientMessageEvent::new(32, self.win, net_wm_state, [2, vert, horz, 1, 0]);
        let _ = self.conn.send_event(
            false,
            self.root,
            EventMask::SUBSTRUCTURE_NOTIFY | EventMask::SUBSTRUCTURE_REDIRECT,
            ev,
        );
        let _ = self.conn.flush();
    }

    fn render(&mut self) -> Result<(), String> {
        let (w, h) = self.browser.size();
        let (surface, cr) = torrentius::new_surface(w as i32, h as i32)?;
        self.browser.draw(&cr)?;
        drop(cr);
        surface.flush();
        let _ = surface.with_data(|data| {
            let _ = self.conn.put_image(
                ImageFormat::Z_PIXMAP,
                self.win,
                self.gc,
                w,
                h,
                0,
                0,
                0,
                self.depth,
                data,
            );
        });
        let _ = self.conn.flush();
        Ok(())
    }
}

fn set_wm_name(conn: &RustConnection, win: Window, name: &str) {
    let _ = conn.change_property(
        PropMode::REPLACE,
        win,
        AtomEnum::WM_NAME,
        AtomEnum::STRING,
        8,
        name.len() as u32,
        name.as_bytes(),
    );
}

fn set_wm_class(conn: &RustConnection, win: Window, class: &str) {
    // WM_CLASS is "instance\0class\0".
    let mut bytes = Vec::new();
    bytes.extend_from_slice(class.as_bytes());
    bytes.push(0);
    bytes.extend_from_slice(class.as_bytes());
    bytes.push(0);
    let _ = conn.change_property(
        PropMode::REPLACE,
        win,
        AtomEnum::WM_CLASS,
        AtomEnum::STRING,
        8,
        bytes.len() as u32,
        &bytes,
    );
}

fn keycode_to_keysym(keycode: u8, shift: bool, keymap: &KeyMap) -> u32 {
    if keycode < keymap.min_keycode {
        return 0;
    }
    let base = (keycode - keymap.min_keycode) as usize * keymap.syms_per_code;
    let idx = base + if shift && keymap.syms_per_code > 1 { 1 } else { 0 };
    keymap.keysyms.get(idx).copied().unwrap_or(0)
}
