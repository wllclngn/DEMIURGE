// VT lock: Acquire a fresh Linux console, lock VT switching, prompt for the
// password on it, verify through PAM.
//
// The holder runs as root (real uid 0) for the whole lock: It owns the VT
// ioctls, reads the password from the tty into the mlocked Secret, and
// runs PAM in a forked child per attempt (pam::authenticate_forked), so a
// crashing PAM module costs one attempt and VT_LOCKSWITCH is never left
// set by a dead process. NO_NEW_PRIVS is never set here: It would make the
// kernel ignore unix_chkpwd's setuid bit and fail every password.
//
// The VT ioctls go through /dev/tty0 (the foreground console), falling
// back to /dev/console, which is a serial port wherever console=ttyS0.
//
// The text-mode helpers at the bottom serve --login's greetd greeter.

use std::io::{self, Write};
use std::os::fd::RawFd;

use crate::gordian_knot::privsep::{self, Ident};
use crate::gordian_knot::secret::Secret;
use crate::gordian_knot::{pam, sysinfo};

const CONSOLES: [&str; 2] = ["/dev/tty0", "/dev/console"];

// Linux VT ioctls (linux/vt.h); stable kernel ABI.
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
        Self { console_fd: -1, tty_fd: -1, tty_num: -1, prev_vt: -1, switch_locked: false, term_orig: None }
    }
}

fn errno() -> i32 {
    io::Error::last_os_error().raw_os_error().unwrap_or(0)
}

// A console fd that answers VT_GETSTATE, with the current VT.
fn open_console() -> Result<(RawFd, i32), String> {
    let mut last = String::new();
    for path in CONSOLES {
        let c = std::ffi::CString::new(path).unwrap();
        let fd = unsafe { libc::open(c.as_ptr(), libc::O_RDWR | libc::O_NOCTTY | libc::O_CLOEXEC) };
        if fd < 0 {
            last = format!("open {}: errno {}", path, errno());
            continue;
        }
        let mut stat = VtStat::default();
        if unsafe { libc::ioctl(fd, VT_GETSTATE, &mut stat) } == 0 {
            return Ok((fd, stat.v_active as i32));
        }
        last = format!("VT_GETSTATE on {}: errno {}", path, errno());
        unsafe { libc::close(fd) };
    }
    Err(last)
}

pub fn acquire() -> Result<VtSession, String> {
    let mut s = VtSession::new();
    let (fd, active) = open_console()?;
    s.console_fd = fd;
    s.prev_vt = active;
    unsafe {
        let mut new_vt: i32 = -1;
        if libc::ioctl(s.console_fd, VT_OPENQRY, &mut new_vt) < 0 || new_vt < 0 {
            let e = format!("VT_OPENQRY: errno {}", errno());
            release(&mut s);
            return Err(e);
        }
        s.tty_num = new_vt;
        let path = std::ffi::CString::new(format!("/dev/tty{}", new_vt)).unwrap();
        s.tty_fd = libc::open(path.as_ptr(), libc::O_RDWR | libc::O_NOCTTY | libc::O_CLOEXEC);
        if s.tty_fd < 0 {
            let e = format!("open /dev/tty{}: errno {}", new_vt, errno());
            release(&mut s);
            return Err(e);
        }
        if libc::ioctl(s.console_fd, VT_ACTIVATE, new_vt) < 0
            || libc::ioctl(s.console_fd, VT_WAITACTIVE, new_vt) < 0
        {
            let e = format!("VT_ACTIVATE {}: errno {}", new_vt, errno());
            release(&mut s);
            return Err(e);
        }
        if libc::ioctl(s.console_fd, VT_LOCKSWITCH, 1) < 0 {
            let e = format!("VT_LOCKSWITCH: errno {}", errno());
            release(&mut s);
            return Err(e);
        }
        s.switch_locked = true;
        let mut t: libc::termios = std::mem::zeroed();
        if libc::tcgetattr(s.tty_fd, &mut t) == 0 {
            s.term_orig = Some(t);
        }
    }
    Ok(s)
}

// Idempotent cleanup: Restore the tty, unlock switching, return to the
// previous VT, free the one we took.
pub fn release(s: &mut VtSession) {
    unsafe {
        if let Some(orig) = s.term_orig.take() {
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

// The whole VT lock: Acquire, prompt until PAM accepts, release. 0 on
// unlock, 1 if the VT could not be taken.
pub fn lock(prompt: &str, ident: &Ident) -> i32 {
    let mut secret = match Secret::new() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("[gordian_knot] password buffer: {}", e);
            return 1;
        }
    };
    let mut session = match acquire() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("[gordian_knot] VT acquire: {}", e);
            return 1;
        }
    };
    privsep::signal_ready();
    let code = prompt_loop(session.tty_fd, ident, prompt, &mut secret);
    release(&mut session);
    code
}

fn tty_write(fd: RawFd, bytes: &[u8]) {
    let mut off = 0;
    while off < bytes.len() {
        let n = unsafe { libc::write(fd, bytes[off..].as_ptr() as *const libc::c_void, bytes.len() - off) };
        if n <= 0 {
            return;
        }
        off += n as usize;
    }
}

fn prompt_loop(tty: RawFd, ident: &Ident, prompt: &str, secret: &mut Secret) -> i32 {
    set_prompt_termios(tty);
    let mut status = String::new();
    loop {
        tty_write(tty, render_text_ui(&ident.name, prompt, &status).as_bytes());
        if !read_secret_line(tty, secret) {
            // The tty went away under us; nothing to read a password from.
            secret.clear();
            return 1;
        }
        if secret.is_empty() {
            status = "Type your password and press Enter.".into();
            continue;
        }
        tty_write(tty, b"\r\n    verifying...\r\n");
        let attempt = pam::authenticate_forked(&ident.name, secret);
        secret.clear();
        for m in &attempt.messages {
            eprintln!("[gordian_knot] pam: {}", m);
        }
        match attempt.result {
            Ok(()) => return 0,
            Err(e) => {
                eprintln!("[gordian_knot] attempt failed: {}", e);
                status = attempt.messages.last().cloned().unwrap_or_else(|| e.to_string());
                // Typed-ahead input was typed blind.
                unsafe { libc::tcflush(tty, libc::TCIFLUSH) };
            }
        }
    }
}

// Raw-ish mode: No echo, no line buffering, no signals from Ctrl+C.
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

// Read a line into `secret`, echoing a dot per character. UTF-8 bytes go in
// whole; BS/DEL drops a character, Ctrl+U clears. False on EOF / error.
fn read_secret_line(tty: RawFd, secret: &mut Secret) -> bool {
    secret.clear();
    let mut b = [0u8; 1];
    loop {
        let n = unsafe { libc::read(tty, b.as_mut_ptr() as *mut libc::c_void, 1) };
        if n <= 0 {
            return false;
        }
        match b[0] {
            b'\r' | b'\n' => return true,
            0x7f | 0x08 => {
                if !secret.is_empty() {
                    secret.pop_char();
                    tty_write(tty, b"\x08 \x08");
                }
            }
            0x15 => {
                for _ in 0..secret.char_count() {
                    tty_write(tty, b"\x08 \x08");
                }
                secret.clear();
            }
            c if c >= 0x20 && c != 0x7f => {
                let starts_char = c & 0xC0 != 0x80;
                if secret.push(&[c]) && starts_char {
                    tty_write(tty, "\u{25cf}".as_bytes());
                }
            }
            _ => {}
        }
    }
}

fn render_text_ui(user: &str, prompt: &str, status: &str) -> String {
    let mut s = String::with_capacity(1024);
    s.push_str("\x1b[2J\x1b[H\r\n");
    s.push_str("  SYSTEM\r\n");
    s.push_str(&format!("    HOSTNAME  {}\r\n", sysinfo::hostname()));
    s.push_str(&format!("    KERNEL    {}\r\n", sysinfo::kernel_release()));
    s.push_str(&format!("    DATE      {}\r\n", sysinfo::current_date()));
    s.push_str(&format!("    TIME      {}\r\n", sysinfo::current_time()));
    s.push_str(&format!("    UPTIME    {}\r\n", sysinfo::uptime_formatted()));
    s.push_str("\r\n  USER\r\n");
    s.push_str(&format!("    USER      {}\r\n", user));
    if !status.is_empty() {
        s.push_str(&format!("    STATUS    {}\r\n", status));
    }
    s.push_str(&format!("    {}  ", prompt));
    s
}

// --login (greetd greeter) helpers below: Text UI on the stdin tty greetd
// attached, with the username prompt above the password.

pub(crate) fn render_login_ui(username_so_far: &str, prompt_label: &str, status: &str) -> String {
    let mut s = String::with_capacity(1024);
    s.push_str("\x1b[2J\x1b[H");
    s.push_str("\r\n");
    s.push_str("  SYSTEM\r\n");
    s.push_str(&format!("    HOSTNAME  {}\r\n", sysinfo::hostname()));
    s.push_str(&format!("    KERNEL    {}\r\n", sysinfo::kernel_release()));
    s.push_str(&format!("    DATE      {}\r\n", sysinfo::current_date()));
    s.push_str(&format!("    TIME      {}\r\n", sysinfo::current_time()));
    s.push_str(&format!("    UPTIME    {}\r\n", sysinfo::uptime_formatted()));
    s.push_str("\r\n");
    s.push_str("  LOGIN\r\n");
    s.push_str(&format!("    USERNAME  {}\r\n", username_so_far));
    if !status.is_empty() {
        s.push_str(&format!("    STATUS    {}\r\n", status));
    }
    s.push_str(&format!("    {}  ", prompt_label));
    s
}

// Read a line from `tty_fd`, echoing each character (visible mode), for the
// username prompt and greetd's "visible" auth messages. None on EOF.
pub(crate) fn read_visible_line(tty_fd: RawFd) -> Option<String> {
    read_line(tty_fd, false)
}

// The masked counterpart, for greetd's "secret" auth messages.
pub(crate) fn read_password_line_pub(tty_fd: RawFd) -> Option<String> {
    read_line(tty_fd, true)
}

pub(crate) fn set_prompt_termios_pub(tty_fd: RawFd) {
    set_prompt_termios(tty_fd);
}

fn read_line(tty_fd: RawFd, masked: bool) -> Option<String> {
    let mut buf = String::new();
    let mut one = [0u8; 1];
    let mut out = io::stdout();
    loop {
        let n = unsafe { libc::read(tty_fd, one.as_mut_ptr() as *mut libc::c_void, 1) };
        if n <= 0 {
            return None;
        }
        match one[0] {
            b'\r' | b'\n' => {
                let _ = out.write_all(b"\r\n");
                let _ = out.flush();
                return Some(buf);
            }
            0x7f | 0x08 => {
                if buf.pop().is_some() {
                    let _ = out.write_all(b"\x08 \x08");
                    let _ = out.flush();
                }
            }
            0x15 => {
                for _ in 0..buf.chars().count() {
                    let _ = out.write_all(b"\x08 \x08");
                }
                buf.clear();
                let _ = out.flush();
            }
            ch if (0x20..0x7f).contains(&ch) => {
                buf.push(ch as char);
                if masked {
                    let _ = out.write_all("\u{25cf}".as_bytes());
                } else {
                    let _ = out.write_all(&[ch]);
                }
                let _ = out.flush();
            }
            _ => {}
        }
    }
}
