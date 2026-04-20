// VT-based lock path: acquire a fresh Linux console, lock VT switching,
// prompt for password, verify via PAM.
//
// Entry points:
//   acquire()    -- parent (root). Opens /dev/console, VT_OPENQRY a fresh
//                   TTY, VT_ACTIVATE, VT_LOCKSWITCH. Returns a VtSession
//                   carrying the fds the child needs.
//   release()    -- parent. Restores VT switching, reactivates the prev VT,
//                   VT_DISALLOCATE, closes fds. Idempotent.
//   prompt_loop  -- child (unprivileged after drop). Reads password from
//                   tty_fd, drives PAM, returns an exit code.

use std::io;
use std::io::Write;
use std::os::fd::RawFd;

use crate::gordian_knot::pam;

const CONSOLE: &str = "/dev/console";

// Linux VT ioctls (from linux/vt.h). libc doesn't expose the constants --
// these numeric values are stable kernel ABI.
const VT_OPENQRY: libc::c_ulong = 0x5600;
const VT_GETSTATE: libc::c_ulong = 0x5603;
const VT_ACTIVATE: libc::c_ulong = 0x5606;
const VT_WAITACTIVE: libc::c_ulong = 0x5607;
const VT_DISALLOCATE: libc::c_ulong = 0x5608;
const VT_LOCKSWITCH: libc::c_ulong = 0x560B;
const VT_UNLOCKSWITCH: libc::c_ulong = 0x560C;

#[repr(C)]
#[derive(Default, Copy, Clone)]
struct VtStat {
    v_active: u16,
    v_signal: u16,
    v_state: u16,
}

pub struct VtSession {
    pub console_fd: RawFd,
    pub tty_fd: RawFd,
    pub tty_num: i32,
    pub prev_vt: i32,
    pub switch_locked: bool,
    pub term_orig: Option<libc::termios>,
}

impl VtSession {
    fn new() -> Self {
        Self {
            console_fd: -1,
            tty_fd: -1,
            tty_num: -1,
            prev_vt: -1,
            switch_locked: false,
            term_orig: None,
        }
    }
}

fn errno() -> i32 {
    unsafe { *libc::__errno_location() }
}

pub fn acquire() -> Result<VtSession, String> {
    unsafe {
        let mut s = VtSession::new();

        let cpath = std::ffi::CString::new(CONSOLE).unwrap();
        s.console_fd = libc::open(cpath.as_ptr(), libc::O_RDWR);
        if s.console_fd < 0 {
            return Err(format!("open {}: errno {} (run as root?)", CONSOLE, errno()));
        }

        let mut stat = VtStat::default();
        if libc::ioctl(s.console_fd, VT_GETSTATE, &mut stat) < 0 {
            return Err(format!("VT_GETSTATE: errno {}", errno()));
        }
        s.prev_vt = stat.v_active as i32;

        let mut new_vt: i32 = -1;
        if libc::ioctl(s.console_fd, VT_OPENQRY, &mut new_vt) < 0 || new_vt < 0 {
            return Err(format!("VT_OPENQRY: errno {}", errno()));
        }
        s.tty_num = new_vt;

        let tty_path = format!("/dev/tty{}", new_vt);
        let ctty = std::ffi::CString::new(tty_path.as_str()).unwrap();
        s.tty_fd = libc::open(ctty.as_ptr(), libc::O_RDWR);
        if s.tty_fd < 0 {
            return Err(format!("open {}: errno {}", tty_path, errno()));
        }

        if libc::ioctl(s.console_fd, VT_ACTIVATE, new_vt) < 0 {
            return Err(format!("VT_ACTIVATE: errno {}", errno()));
        }
        if libc::ioctl(s.console_fd, VT_WAITACTIVE, new_vt) < 0 {
            return Err(format!("VT_WAITACTIVE: errno {}", errno()));
        }
        if libc::ioctl(s.console_fd, VT_LOCKSWITCH, 1) < 0 {
            return Err(format!("VT_LOCKSWITCH: errno {}", errno()));
        }
        s.switch_locked = true;

        // Capture original termios so we can restore on exit.
        let mut t: libc::termios = std::mem::zeroed();
        if libc::tcgetattr(s.tty_fd, &mut t) == 0 {
            s.term_orig = Some(t);
        }

        Ok(s)
    }
}

// Idempotent cleanup. Called from the parent after waitpid on the child
// (or from a crash handler if anything goes wrong).
pub fn release(s: &mut VtSession) {
    unsafe {
        if let Some(orig) = s.term_orig {
            if s.tty_fd >= 0 {
                libc::tcsetattr(s.tty_fd, libc::TCSANOW, &orig);
            }
        }
        if s.switch_locked && s.console_fd >= 0 {
            libc::ioctl(s.console_fd, VT_UNLOCKSWITCH, 1);
            s.switch_locked = false;
        }
        if s.prev_vt > 0 && s.console_fd >= 0 {
            libc::ioctl(s.console_fd, VT_ACTIVATE, s.prev_vt);
            libc::ioctl(s.console_fd, VT_WAITACTIVE, s.prev_vt);
        }
        if s.tty_fd >= 0 {
            libc::close(s.tty_fd);
            s.tty_fd = -1;
        }
        if s.tty_num > 0 && s.console_fd >= 0 {
            libc::ioctl(s.console_fd, VT_DISALLOCATE, s.tty_num);
            s.tty_num = -1;
        }
        if s.console_fd >= 0 {
            libc::close(s.console_fd);
            s.console_fd = -1;
        }
    }
}

// Put the terminal into raw-ish mode: no echo, no canonical line buffering,
// no signal generation from Ctrl+C. Saves original in the VtSession.
fn set_prompt_termios(tty_fd: RawFd) {
    unsafe {
        let mut t: libc::termios = std::mem::zeroed();
        if libc::tcgetattr(tty_fd, &mut t) != 0 {
            return;
        }
        t.c_iflag &= !libc::IXON;
        t.c_lflag &= !(libc::ECHO | libc::ICANON | libc::ISIG);
        libc::tcsetattr(tty_fd, libc::TCSANOW, &t);
    }
}

// Child's entry point: draw the text UI, read password, call PAM. Loops
// on auth failure until success or SIGTERM. Returns an exit code.
//
// Text UI is *text* -- drawn with terminal escape sequences, no Cairo.
// The X11 path handles the pretty-panel rendering; VT stays console-style.
pub fn prompt_loop(tty_fd: RawFd, user: &str, prompt: &str) -> i32 {
    set_prompt_termios(tty_fd);

    unsafe {
        // Route stdio to the TTY so println / eprintln hit the right surface.
        libc::dup2(tty_fd, 0);
        libc::dup2(tty_fd, 1);
        libc::dup2(tty_fd, 2);
    }

    let mut out = io::stdout();

    loop {
        // Clear screen + home + draw the SYSTEM stanza + USER stanza.
        let screen = render_text_ui(user, prompt);
        let _ = out.write_all(screen.as_bytes());
        let _ = out.flush();

        let password = match read_password_line(tty_fd) {
            Some(p) => p,
            None => return 1, // Eof / read error -- bail, parent will release.
        };

        match pam::authenticate(user, &password) {
            Ok(()) => return 0,
            Err(e) => {
                let _ = writeln!(out, "\r\n{}\r", e);
                let _ = out.flush();
                // Brief pause so the message is readable before the redraw.
                unsafe {
                    let ts = libc::timespec { tv_sec: 1, tv_nsec: 500_000_000 };
                    libc::nanosleep(&ts, std::ptr::null_mut());
                }
                // Don't bail on AuthError::MaxTries here; PAM's own faillock
                // already throttles. Looping forever matches physlock.
                let _ = e;
            }
        }
    }
}

fn render_text_ui(user: &str, prompt: &str) -> String {
    use crate::gordian_knot::sysinfo;
    let mut s = String::with_capacity(1024);
    // Clear + home.
    s.push_str("\x1b[2J\x1b[H");
    s.push_str("\r\n");
    s.push_str(&format!("  SYSTEM\r\n"));
    s.push_str(&format!("    HOSTNAME  {}\r\n", sysinfo::hostname()));
    s.push_str(&format!("    KERNEL    {}\r\n", sysinfo::kernel_release()));
    s.push_str(&format!("    DATE      {}\r\n", sysinfo::current_date()));
    s.push_str(&format!("    TIME      {}\r\n", sysinfo::current_time()));
    s.push_str(&format!("    UPTIME    {}\r\n", sysinfo::uptime_formatted()));
    s.push_str("\r\n");
    s.push_str(&format!("  USER\r\n"));
    s.push_str(&format!("    USER      {}\r\n", user));
    s.push_str(&format!("    {}  ", prompt));
    s
}

fn read_password_line(tty_fd: RawFd) -> Option<String> {
    let mut buf = String::new();
    let mut one = [0u8; 1];
    let mut out = io::stdout();
    loop {
        let n = unsafe {
            libc::read(tty_fd, one.as_mut_ptr() as *mut libc::c_void, 1)
        };
        if n <= 0 {
            return None;
        }
        let ch = one[0];
        match ch {
            b'\r' | b'\n' => {
                let _ = out.write_all(b"\r\n");
                let _ = out.flush();
                return Some(buf);
            }
            0x7f | 0x08 => {
                // DEL / BS
                if !buf.is_empty() {
                    buf.pop();
                    let _ = out.write_all(b"\x08 \x08");
                    let _ = out.flush();
                }
            }
            0x15 => {
                // Ctrl+U: clear line.
                for _ in 0..buf.chars().count() {
                    let _ = out.write_all(b"\x08 \x08");
                }
                buf.clear();
                let _ = out.flush();
            }
            ch if ch >= 0x20 && ch < 0x7f => {
                buf.push(ch as char);
                let _ = out.write_all("\u{25cf}".as_bytes());
                let _ = out.flush();
            }
            _ => {}
        }
    }
}
