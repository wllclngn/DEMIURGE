// The X11 lock's UI child: Draws the SYSTEM and USER panels into the
// holder's lock window, and nothing else. It runs as the user with
// NO_NEW_PRIVS, re-exec'd from the holder (`gordian_knot --ui FD WINDOW`),
// and never sees the password: The holder sends it only the number of
// characters typed, a busy flag and a status line.
//
// Each panel is its own persistent surface, put to the window as one
// checked put_image: A few hundred KB, far under the server's request
// limit at any screen size, and only the panel whose content changed is
// sent (the 1 Hz tick touches SYSTEM only). Once the first frame is
// accepted the child reports "painted" (the holder grabs input only after
// that) and sandboxes itself with landlock + seccomp.
//
// Holder -> child: 'S' u16-LE dots, u8 busy, UTF-8 status (one datagram).
// Child -> holder: 'P' once the first frame is accepted.

use std::os::fd::RawFd;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use x11rb::connection::Connection;
use x11rb::protocol::randr::ConnectionExt as _;
use x11rb::protocol::xproto::*;
use x11rb::protocol::Event;
use x11rb::rust_connection::RustConnection;

use crate::config;
use crate::gordian_knot::{landlock, seccomp, sysinfo};
use crate::torrentius::{self, PanelColors, PanelRow};

const PANEL_W: i32 = 520;
const GAP: i32 = 24;
const STATUS_MAX_CHARS: usize = 56;

struct Region {
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    surface: cairo::ImageSurface,
}

struct Panels {
    conn: RustConnection,
    win: Window,
    gc: Gcontext,
    depth: u8,
    font: pango::FontDescription,
    opts: cairo::FontOptions,
    colors: PanelColors,
    bg: (f64, f64, f64),
    prompt: String,
    user: String,
    sys: Region,
    usr: Region,
    panel_x: f64,
    sys_y: f64,
    sys_h: f64,
    usr_y: f64,
    usr_h: f64,
    dots: usize,
    busy: bool,
    status: String,
    last_clock: String,
}

pub fn run(sock: RawFd, win: Window, sandbox: bool) -> i32 {
    if unsafe { libc::geteuid() } == 0 {
        eprintln!("[gordian_knot ui] refusing to render as root");
        return 1;
    }
    let mut p = match Panels::new(win) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("[gordian_knot ui] {}", e);
            return 1;
        }
    };
    if let Err(e) = p.draw_sys().and_then(|_| p.draw_usr()) {
        eprintln!("[gordian_knot ui] first frame: {}", e);
        return 1;
    }
    send_byte(sock, b'P');

    if sandbox {
        let home = std::env::var("HOME").unwrap_or_default();
        if !landlock::install_sandbox(&home) {
            eprintln!("[gordian_knot ui] landlock unavailable; continuing");
        }
        if !seccomp::install_filter() {
            eprintln!("[gordian_knot ui] seccomp install failed; continuing");
        }
    }

    let xfd = {
        use std::os::fd::AsRawFd;
        p.conn.stream().as_raw_fd()
    };
    let mut buf = [0u8; 512];
    loop {
        let mut fds = [
            libc::pollfd { fd: sock, events: libc::POLLIN, revents: 0 },
            libc::pollfd { fd: xfd, events: libc::POLLIN, revents: 0 },
        ];
        unsafe { libc::poll(fds.as_mut_ptr(), 2, ms_to_next_second()) };

        if fds[0].revents & (libc::POLLIN | libc::POLLHUP) != 0 {
            let n = unsafe { libc::recv(sock, buf.as_mut_ptr() as *mut libc::c_void, buf.len(), 0) };
            if n <= 0 {
                return 0;
            }
            if let Some((dots, busy, status)) = parse_state(&buf[..n as usize]) {
                p.dots = dots;
                p.busy = busy;
                p.status = status;
                if let Err(e) = p.draw_usr() {
                    eprintln!("[gordian_knot ui] {}", e);
                    return 1;
                }
            }
        }

        loop {
            match p.conn.poll_for_event() {
                Ok(Some(Event::Expose(ex))) if ex.count == 0 => {
                    if let Err(e) = p.draw_sys().and_then(|_| p.draw_usr()) {
                        eprintln!("[gordian_knot ui] {}", e);
                        return 1;
                    }
                }
                Ok(Some(_)) => {}
                Ok(None) => break,
                Err(e) => {
                    eprintln!("[gordian_knot ui] X connection: {}", e);
                    return 1;
                }
            }
        }

        if sysinfo::current_time() != p.last_clock {
            if let Err(e) = p.draw_sys() {
                eprintln!("[gordian_knot ui] {}", e);
                return 1;
            }
        }
    }
}

pub fn parse_state(b: &[u8]) -> Option<(usize, bool, String)> {
    if b.len() < 4 || b[0] != b'S' {
        return None;
    }
    let dots = u16::from_le_bytes([b[1], b[2]]) as usize;
    Some((dots, b[3] != 0, String::from_utf8_lossy(&b[4..]).into_owned()))
}

pub fn encode_state(dots: usize, busy: bool, status: &str) -> Vec<u8> {
    let mut v = Vec::with_capacity(4 + status.len());
    v.push(b'S');
    v.extend_from_slice(&(dots.min(u16::MAX as usize) as u16).to_le_bytes());
    v.push(busy as u8);
    v.extend_from_slice(&status.as_bytes()[..status.len().min(500)]);
    v
}

fn send_byte(sock: RawFd, b: u8) {
    unsafe { libc::send(sock, &b as *const u8 as *const libc::c_void, 1, libc::MSG_NOSIGNAL) };
}

fn ms_to_next_second() -> i32 {
    let ms = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or(Duration::ZERO).subsec_millis();
    (1000 - ms as i32).max(10)
}

impl Panels {
    fn new(win: Window) -> Result<Panels, String> {
        let paths = config::Paths::init()?;
        let mut cfg = config::load(&paths)?;
        if cfg.gordian_knot.accent.is_empty() {
            cfg.gordian_knot.accent = cfg.appearance.accent.clone();
        }
        let gk = &cfg.gordian_knot;

        let (conn, screen_num) = RustConnection::connect(None).map_err(|e| format!("X connect: {}", e))?;
        let screen = &conn.setup().roots[screen_num];
        let (root, depth) = (screen.root, screen.root_depth);
        let geo = conn
            .get_geometry(win)
            .map_err(|e| e.to_string())?
            .reply()
            .map_err(|e| format!("lock window: {}", e))?;
        let (mx, my, mw, mh) = primary_monitor(&conn, root)
            .unwrap_or((0, 0, geo.width as i32, geo.height as i32));

        let gc = conn.generate_id().map_err(|e| e.to_string())?;
        conn.create_gc(gc, win, &CreateGCAux::new()).map_err(|e| e.to_string())?;
        conn.change_window_attributes(win, &ChangeWindowAttributesAux::new().event_mask(EventMask::EXPOSURE))
            .map_err(|e| e.to_string())?;

        let font = pango::FontDescription::from_string(&gk.font);
        let opts = torrentius::make_font_options()?;
        let (row_h, sys_h, usr_h) = {
            let (_s, cr) = torrentius::new_surface(1, 1)?;
            let layout = pangocairo::functions::create_layout(&cr);
            layout.set_font_description(Some(&font));
            layout.set_text("Ag");
            let (_, rh) = layout.pixel_size();
            (rh, torrentius::panel_height_for(&layout, 5), torrentius::panel_height_for(&layout, 3))
        };
        let stack_h = sys_h as i32 + GAP + usr_h as i32;
        let panel_x = mx + (mw - PANEL_W) / 2;
        let sys_y = my + (mh - stack_h) / 2;
        let usr_y = sys_y + sys_h as i32 + GAP;
        // The title label straddles the panel's top border, so each region
        // reaches a row above it.
        let lift = row_h + 2;
        let region = |y: i32, h: f64| -> Result<Region, String> {
            let (x, w, ry, rh) = (panel_x - 2, PANEL_W + 4, y - lift, h as i32 + lift + 2);
            let surface = cairo::ImageSurface::create(cairo::Format::Rgb24, w, rh)
                .map_err(|e| format!("cairo surface {}x{}: {}", w, rh, e))?;
            Ok(Region { x, y: ry, w, h: rh, surface })
        };

        let user = {
            let pw = unsafe { libc::getpwuid(libc::getuid()) };
            if pw.is_null() {
                String::from("user")
            } else {
                unsafe { std::ffi::CStr::from_ptr((*pw).pw_name) }.to_string_lossy().into_owned()
            }
        };

        Ok(Panels {
            sys: region(sys_y, sys_h)?,
            usr: region(usr_y, usr_h)?,
            conn,
            win,
            gc,
            depth,
            font,
            opts,
            colors: PanelColors {
                bg: torrentius::parse_hex(&gk.bg),
                fg: torrentius::parse_hex(&gk.fg),
                accent: torrentius::parse_hex(&gk.accent),
                border: torrentius::parse_hex(&gk.fg),
            },
            bg: torrentius::parse_hex(&gk.bg),
            prompt: gk.prompt.clone(),
            user,
            panel_x: panel_x as f64,
            sys_y: sys_y as f64,
            sys_h,
            usr_y: usr_y as f64,
            usr_h,
            dots: 0,
            busy: false,
            status: String::new(),
            last_clock: String::new(),
        })
    }

    fn draw_sys(&mut self) -> Result<(), String> {
        let time = sysinfo::current_time();
        let (host, kernel, date, uptime) = (
            sysinfo::hostname(),
            sysinfo::kernel_release(),
            sysinfo::current_date(),
            sysinfo::uptime_formatted(),
        );
        let rows = [
            PanelRow { label: "HOSTNAME", value: &host },
            PanelRow { label: "KERNEL", value: &kernel },
            PanelRow { label: "DATE", value: &date },
            PanelRow { label: "TIME", value: &time },
            PanelRow { label: "UPTIME", value: &uptime },
        ];
        let (x, y, h) = (self.panel_x, self.sys_y, self.sys_h);
        self.paint(false, x, y, h, "SYSTEM", &rows)?;
        self.last_clock = time;
        Ok(())
    }

    fn draw_usr(&mut self) -> Result<(), String> {
        let dots: String = std::iter::repeat_n('\u{25cf}', self.dots).collect();
        let status = if self.busy { "Verifying...".to_string() } else { self.status.clone() };
        let status: String = status.chars().take(STATUS_MAX_CHARS).collect();
        let user = self.user.clone();
        let prompt = self.prompt.clone();
        let rows = [
            PanelRow { label: "USER", value: &user },
            PanelRow { label: &prompt, value: &dots },
            PanelRow { label: "STATUS", value: &status },
        ];
        let (x, y, h) = (self.panel_x, self.usr_y, self.usr_h);
        self.paint(true, x, y, h, "USER", &rows)
    }

    fn paint(&mut self, usr: bool, x: f64, y: f64, h: f64, label: &str, rows: &[PanelRow<'_>]) -> Result<(), String> {
        let region = if usr { &mut self.usr } else { &mut self.sys };
        {
            let cr = cairo::Context::new(&region.surface).map_err(|e| format!("cairo context: {}", e))?;
            cr.set_font_options(&self.opts);
            cr.set_source_rgb(self.bg.0, self.bg.1, self.bg.2);
            let _ = cr.paint();
            cr.translate(-(region.x as f64), -(region.y as f64));
            let layout = pangocairo::functions::create_layout(&cr);
            layout.set_font_description(Some(&self.font));
            pangocairo::functions::context_set_font_options(&layout.context(), Some(&self.opts));
            torrentius::draw_panel(&cr, &layout, x, y, PANEL_W as f64, h, label, rows, &self.colors);
        }
        region.surface.flush();
        let (rx, ry, rw, rh) = (region.x, region.y, region.w, region.h);
        let (conn, win, gc, depth) = (&self.conn, self.win, self.gc, self.depth);
        let mut put = Ok(());
        let _ = region.surface.with_data(|data| {
            put = conn
                .put_image(ImageFormat::Z_PIXMAP, win, gc, rw as u16, rh as u16, rx as i16, ry as i16, 0, depth, data)
                .map_err(|e| format!("put_image: {}", e))
                .and_then(|c| c.check().map_err(|e| format!("put_image refused: {}", e)));
        });
        put
    }
}

// The primary RandR output's rectangle, so the panels center on one
// monitor instead of between two.
fn primary_monitor(conn: &RustConnection, root: Window) -> Option<(i32, i32, i32, i32)> {
    let out = conn.randr_get_output_primary(root).ok()?.reply().ok()?.output;
    if out == 0 {
        return None;
    }
    let info = conn.randr_get_output_info(out, 0).ok()?.reply().ok()?;
    if info.crtc == 0 {
        return None;
    }
    let c = conn.randr_get_crtc_info(info.crtc, 0).ok()?.reply().ok()?;
    (c.width > 0 && c.height > 0).then_some((c.x as i32, c.y as i32, c.width as i32, c.height as i32))
}
