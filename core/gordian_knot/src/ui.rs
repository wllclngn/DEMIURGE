// The lock's UI child: Draws the SYSTEM and USER panels, and nothing else,
// into the X11 lock window (the X11 binary's `--ui FD WINDOW`) or into the
// framebuffer of the VT lock (`--ui-fb FD FBFD`, either binary). It runs as
// the user with NO_NEW_PRIVS, re-exec'd from the holder, and never sees the
// password: The holder sends it only the number of characters typed, a busy
// flag and a status line.
//
// Each panel is its own persistent surface, put to the window as one
// checked put_image (a few hundred KB, far under the server's request limit
// at any screen size) or copied into the mapped framebuffer, and only the
// panel whose content changed is sent (the 1 Hz tick touches SYSTEM only).
// Once the first frame is out the child reports "painted" (the holder takes
// input only after that) and sandboxes itself with landlock + seccomp; the
// framebuffer is mapped before, so drawing into it needs no syscall.
//
// Holder -> child: 'S' u16-LE dots, u8 busy, UTF-8 status (one datagram).
// Child -> holder: 'P' once the first frame is out.

use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use demiurge_core::config;
use crate::privsep::Ident;
use crate::{fb, landlock, seccomp, sysinfo};
use demiurge_core::torrentius::{self, PanelColors, PanelRow};

const RESPAWN_INTERVAL: Duration = Duration::from_secs(1);

// Where the holder's renderer draws.
#[derive(Clone, Copy)]
pub enum Target {
    // The X11 lock window, drawn by the X11 binary's renderer.
    Window(u32),
    // An fbdev fd the holder opened; the child maps it.
    Framebuffer(RawFd),
}

/// What the renderer draws into: The VT lock's framebuffer here, the X11
/// lock window in the X11 binary.
pub trait Output {
    /// The rectangle the panels center on.
    fn rect(&self) -> (i32, i32, i32, i32);
    /// Paint all of it `bg` before the first panel; a window has a
    /// background of its own.
    fn clear(&mut self, _bg: (f64, f64, f64)) {}
    /// Put a panel's RGB24 rows, `stride` bytes apart, at `rect`.
    fn put(&mut self, data: &[u8], stride: usize, rect: (i32, i32, i32, i32)) -> Result<(), String>;
    /// An fd the renderer polls for the output's events, or -1.
    fn fd(&self) -> RawFd {
        -1
    }
    /// The output's pending events read; true when the panels must be
    /// drawn again.
    fn exposed(&mut self) -> Result<bool, String> {
        Ok(false)
    }
}

/// The VT lock's framebuffer, mapped before the sandbox, so drawing into it
/// takes no syscall.
pub struct Framebuffer {
    _file: std::fs::File,
    map: memmap2::MmapMut,
    info: fb::Info,
}

impl Framebuffer {
    pub fn open(fd: RawFd) -> Result<Self, String> {
        let info = fb::info(fd).ok_or("not a 32-bit XRGB framebuffer")?;
        let file = unsafe { std::fs::File::from_raw_fd(fd) };
        let map = unsafe { memmap2::MmapOptions::new().len(info.len).map_mut(&file) }
            .map_err(|e| format!("map the framebuffer: {}", e))?;
        Ok(Framebuffer { _file: file, map, info })
    }
}

impl Output for Framebuffer {
    fn rect(&self) -> (i32, i32, i32, i32) {
        (0, 0, self.info.width, self.info.height)
    }

    // The framebuffer still shows the console.
    fn clear(&mut self, bg: (f64, f64, f64)) {
        let to = |c: f64| (c * 255.0).round() as u32;
        let pixel = ((to(bg.0) << 16) | (to(bg.1) << 8) | to(bg.2)).to_ne_bytes();
        for row in 0..self.info.height as usize {
            let start = self.info.offset + row * self.info.stride;
            for px in self.map[start..start + self.info.width as usize * 4].chunks_exact_mut(4) {
                px.copy_from_slice(&pixel);
            }
        }
    }

    fn put(&mut self, data: &[u8], stride: usize, rect: (i32, i32, i32, i32)) -> Result<(), String> {
        blit(&mut self.map, &self.info, data, stride, rect);
        Ok(())
    }
}

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
    out: Box<dyn Output>,
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

/// The renderer: `open` makes its output once it is known not to run as
/// root.
pub fn run(sock: RawFd, open: impl FnOnce() -> Result<Box<dyn Output>, String>, sandbox: bool) -> i32 {
    if unsafe { libc::geteuid() } == 0 {
        eprintln!("[gordian_knot ui] refusing to render as root");
        return 1;
    }
    let mut p = match open().and_then(Panels::new) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("[gordian_knot ui] {}", e);
            return 1;
        }
    };
    p.clear();
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

    let xfd = p.out.fd();
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

        let exposed = match p.out.exposed() {
            Ok(exposed) => exposed,
            Err(e) => {
                eprintln!("[gordian_knot ui] {}", e);
                return 1;
            }
        };
        if exposed {
            if let Err(e) = p.draw_sys().and_then(|_| p.draw_usr()) {
                eprintln!("[gordian_knot ui] {}", e);
                return 1;
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
    fn new(out: Box<dyn Output>) -> Result<Panels, String> {
        let paths = config::Paths::init()?;
        let mut cfg = config::load(&paths)?;
        if cfg.gordian_knot.accent.is_empty() {
            cfg.gordian_knot.accent = cfg.appearance.accent.clone();
        }
        let gk = &cfg.gordian_knot;
        let (mx, my, mw, mh) = out.rect();

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
            out,
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

    fn clear(&mut self) {
        self.out.clear(self.bg);
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
        let src_stride = region.surface.stride() as usize;
        let mut put = Ok(());
        let out = &mut self.out;
        let _ = region.surface.with_data(|data| {
            put = out.put(data, src_stride, (rx, ry, rw, rh));
        });
        put
    }
}

// Copy a region's RGB24 rows into the XRGB framebuffer at (x, y), clipped to
// the screen.
fn blit(map: &mut [u8], info: &fb::Info, data: &[u8], src_stride: usize, (x, y, w, h): (i32, i32, i32, i32)) {
    let (x0, x1) = (x.max(0), (x + w).min(info.width));
    if x0 >= x1 {
        return;
    }
    let bytes = (x1 - x0) as usize * 4;
    for row in 0..h {
        let dy = y + row;
        if dy < 0 || dy >= info.height {
            continue;
        }
        let src = row as usize * src_stride + (x0 - x) as usize * 4;
        let dst = info.offset + dy as usize * info.stride + x0 as usize * 4;
        map[dst..dst + bytes].copy_from_slice(&data[src..src + bytes]);
    }
}

// The holder's handle on its UI child: Spawned, told the state, restarted
// if it dies.
pub struct UiChild {
    exe: std::ffi::CString,
    ident: Ident,
    target: Target,
    sandbox: bool,
    pid: i32,
    sock: OwnedFd,
    spawned_at: Instant,
}

impl UiChild {
    pub fn spawn(ident: &Ident, target: Target, sandbox: bool) -> Result<UiChild, String> {
        // Resolved here, as root: The child is non-dumpable after its
        // setuid change, so its own /proc/self/exe is not readable to it.
        let exe = std::fs::read_link("/proc/self/exe").map_err(|e| format!("/proc/self/exe: {}", e))?;
        let exe = std::ffi::CString::new(exe.as_os_str().as_encoded_bytes()).map_err(|e| e.to_string())?;
        let (pid, sock) = start(&exe, ident, target, sandbox)?;
        Ok(UiChild { exe, ident: ident.clone(), target, sandbox, pid, sock, spawned_at: Instant::now() })
    }

    pub fn sock(&self) -> RawFd {
        self.sock.as_raw_fd()
    }

    pub fn wait_painted(&mut self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        while let Some(left) = deadline.checked_duration_since(Instant::now()) {
            let mut pfd = libc::pollfd { fd: self.sock(), events: libc::POLLIN, revents: 0 };
            if unsafe { libc::poll(&mut pfd, 1, left.as_millis().max(1) as i32) } <= 0 {
                return false;
            }
            let mut b = 0u8;
            let n = unsafe { libc::recv(self.sock(), &mut b as *mut u8 as *mut libc::c_void, 1, 0) };
            if n <= 0 {
                return false;
            }
            if b == b'P' {
                return true;
            }
        }
        false
    }

    pub fn send(&self, dots: usize, busy: bool, status: &str) {
        let msg = encode_state(dots, busy, status);
        unsafe {
            libc::send(
                self.sock(),
                msg.as_ptr() as *const libc::c_void,
                msg.len(),
                libc::MSG_NOSIGNAL | libc::MSG_DONTWAIT,
            )
        };
    }

    // Reap a dead UI child and start another. True when a new one started
    // (the caller resends the current state).
    pub fn supervise(&mut self) -> bool {
        let mut status = 0;
        let gone = unsafe { libc::waitpid(self.pid, &mut status, libc::WNOHANG) } == self.pid;
        if !gone || self.spawned_at.elapsed() < RESPAWN_INTERVAL {
            return false;
        }
        eprintln!("[gordian_knot] UI child exited; the lock holds, restarting it");
        match start(&self.exe, &self.ident, self.target, self.sandbox) {
            Ok((pid, sock)) => {
                self.pid = pid;
                self.sock = sock;
                self.spawned_at = Instant::now();
                true
            }
            Err(e) => {
                eprintln!("[gordian_knot] UI restart failed: {}", e);
                self.spawned_at = Instant::now();
                false
            }
        }
    }

    // SIGKILL: The renderer holds nothing to save, and an unlock must not
    // wait on a child that ignores a gentler signal.
    pub fn stop(&mut self) {
        unsafe {
            libc::kill(self.pid, libc::SIGKILL);
            libc::waitpid(self.pid, std::ptr::null_mut(), 0);
        }
    }
}

// Fork, drop to the user with NO_NEW_PRIVS, and re-exec this binary as the
// UI with the socket on fd 3 (and a framebuffer on fd 4) and nothing else
// inherited: `--ui 3 WINDOW` or `--ui-fb 3 4`.
fn start(exe: &std::ffi::CStr, ident: &Ident, target: Target, sandbox: bool) -> Result<(i32, OwnedFd), String> {
    let mut sv = [0i32; 2];
    if unsafe { libc::socketpair(libc::AF_UNIX, libc::SOCK_SEQPACKET | libc::SOCK_CLOEXEC, 0, sv.as_mut_ptr()) } != 0 {
        return Err(format!("socketpair: {}", std::io::Error::last_os_error()));
    }
    let (ours, theirs) = unsafe { (OwnedFd::from_raw_fd(sv[0]), OwnedFd::from_raw_fd(sv[1])) };

    // Everything allocated before fork: The child only makes syscalls.
    let (win_arg, fb_fd) = match target {
        Target::Window(win) => (Some(std::ffi::CString::new(win.to_string()).unwrap()), -1),
        Target::Framebuffer(fd) => (None, fd),
    };
    let mut argv: Vec<&std::ffi::CStr> = match &win_arg {
        Some(win) => vec![c"gordian_knot", c"--ui", c"3", win],
        None => vec![c"gordian_knot", c"--ui-fb", c"3", c"4"],
    };
    if !sandbox {
        argv.push(c"--no-sandbox");
    }
    let mut argv_ptrs: Vec<*const libc::c_char> = argv.iter().map(|a| a.as_ptr()).collect();
    argv_ptrs.push(std::ptr::null());
    let oom = c"/proc/self/oom_score_adj";

    match unsafe { libc::fork() } {
        -1 => Err(format!("fork: {}", std::io::Error::last_os_error())),
        0 => unsafe {
            // The holder's -1000 is not the renderer's to keep.
            let fd = libc::open(oom.as_ptr(), libc::O_WRONLY);
            if fd >= 0 {
                libc::write(fd, b"0".as_ptr() as *const libc::c_void, 1);
                libc::close(fd);
            }
            libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL);
            // Lift both fds clear of 3 and 4 first, so neither move clobbers
            // the other; dup2 then leaves the targets without CLOEXEC.
            let sock = libc::fcntl(theirs.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 20);
            let fbuf = if fb_fd >= 0 { libc::fcntl(fb_fd, libc::F_DUPFD_CLOEXEC, 20) } else { -1 };
            let moved = sock >= 0
                && libc::dup2(sock, 3) == 3
                && (fb_fd < 0 || (fbuf >= 0 && libc::dup2(fbuf, 4) == 4));
            if !moved {
                libc::_exit(126);
            }
            let keep = if fb_fd >= 0 { 5u32 } else { 4u32 };
            libc::syscall(libc::SYS_close_range, keep, u32::MAX, 0u32);
            if libc::setresgid(ident.gid, ident.gid, ident.gid) != 0
                || libc::setresuid(ident.uid, ident.uid, ident.uid) != 0
                || libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0
            {
                libc::_exit(126);
            }
            libc::execv(exe.as_ptr(), argv_ptrs.as_ptr());
            libc::_exit(127);
        },
        pid => {
            drop(theirs);
            Ok((pid, ours))
        }
    }
}
