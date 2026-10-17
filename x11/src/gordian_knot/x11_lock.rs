// X11 in-session lock: The holder.
//
// Runs as root (real uid 0, so the user's processes cannot signal it),
// never draws and never loads a font. It owns the fullscreen
// override-redirect lock window and the keyboard + pointer grab, decodes
// keys with xkbcommon into the mlocked Secret, and runs PAM in a forked
// child per attempt. Drawing is the UI child's (ui.rs): Unprivileged,
// sandboxed, told only how many characters were typed.
//
// Order is the point:
//   1. connect, create + map the lock window
//   2. key decoder and password buffer ready
//   3. spawn the UI child and wait for "painted" (its first frame accepted)
//   4. only then take the grab
// A locker that cannot show its prompt never holds input. Anything that
// fails before the grab is LockError::Unavailable, and the dispatcher may
// fall back to the VT lock; once the grab is held there is no fallback.
//
// A UI child that dies leaves the grab held and the window mapped; the
// holder respawns it (at most once a second). A PAM module that crashes
// costs one attempt, not the lock.
//
// Limitation of any X11 in-session lock: Other clients can watch XInput2
// raw key events while the grab is held. The VT lock gives X no input.

use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::time::{Duration, Instant};

use x11rb::connection::Connection;
use x11rb::protocol::xproto::*;
use x11rb::protocol::Event;
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as _;

use crate::config::GordianKnot as Cfg;
use crate::gordian_knot::keyinput::{Decoder, KeyAction};
use crate::gordian_knot::privsep::{self, Ident};
use crate::gordian_knot::secret::Secret;
use crate::gordian_knot::{pam, ui};
use crate::torrentius;

const PAINT_TIMEOUT: Duration = Duration::from_secs(10);
const RESPAWN_INTERVAL: Duration = Duration::from_secs(1);

pub enum LockError {
    // Nothing was locked; a fallback is safe.
    Unavailable(String),
    // The lock was up and failed.
    Failed(String),
}

impl std::fmt::Display for LockError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LockError::Unavailable(s) => write!(f, "X11 lock unavailable: {}", s),
            LockError::Failed(s) => write!(f, "X11 lock failed: {}", s),
        }
    }
}

pub fn lock(cfg: &Cfg, ident: &Ident, sandbox: bool) -> Result<(), LockError> {
    let unavailable = |e: String| LockError::Unavailable(e);
    let (conn, screen_num) =
        RustConnection::connect(None).map_err(|e| unavailable(format!("connect: {}", e)))?;
    let screen = &conn.setup().roots[screen_num];
    let (root, depth) = (screen.root, screen.root_depth);
    let (w, h) = (screen.width_in_pixels, screen.height_in_pixels);

    let win = conn.generate_id().map_err(|e| unavailable(e.to_string()))?;
    conn.create_window(
        depth,
        win,
        root,
        0,
        0,
        w,
        h,
        0,
        WindowClass::INPUT_OUTPUT,
        0,
        &CreateWindowAux::new()
            .override_redirect(1)
            .background_pixel(torrentius::hex_to_pixel(&cfg.bg))
            .event_mask(
                EventMask::KEY_PRESS
                    | EventMask::STRUCTURE_NOTIFY
                    // Re-raise if another client maps over the lock: X11
                    // cannot prevent a cover, only answer it at once.
                    | EventMask::VISIBILITY_CHANGE,
            ),
    )
    .map_err(|e| unavailable(format!("create_window: {}", e)))?;
    let destroy = |conn: &RustConnection| {
        let _ = conn.destroy_window(win);
        let _ = conn.flush();
    };
    let _ = conn.map_window(win);
    let _ = conn.configure_window(win, &ConfigureWindowAux::new().stack_mode(StackMode::ABOVE));
    let _ = conn.flush();

    let Some(decoder) = Decoder::from_server(&conn, root) else {
        destroy(&conn);
        return Err(unavailable("no XKB keymap for this server".into()));
    };
    let secret = match Secret::new() {
        Ok(s) => s,
        Err(e) => {
            destroy(&conn);
            return Err(unavailable(format!("password buffer: {}", e)));
        }
    };

    let mut ui = match UiChild::spawn(ident, win, sandbox) {
        Ok(u) => u,
        Err(e) => {
            destroy(&conn);
            return Err(unavailable(e));
        }
    };
    if !ui.wait_painted(PAINT_TIMEOUT) {
        ui.stop();
        destroy(&conn);
        return Err(unavailable("the lock screen never painted".into()));
    }
    if !grab_input(&conn, win) {
        ui.stop();
        destroy(&conn);
        return Err(unavailable("keyboard/pointer grab failed".into()));
    }
    privsep::signal_ready();

    let result = event_loop(&conn, win, root, decoder, secret, &mut ui, ident);

    let _ = conn.ungrab_keyboard(x11rb::CURRENT_TIME);
    let _ = conn.ungrab_pointer(x11rb::CURRENT_TIME);
    ui.stop();
    destroy(&conn);
    result
}

fn grab_input(conn: &RustConnection, win: Window) -> bool {
    // 50 tries x 100 ms: Enough to outlast another client releasing a grab.
    for _ in 0..50 {
        let kb = conn
            .grab_keyboard(false, win, x11rb::CURRENT_TIME, GrabMode::ASYNC, GrabMode::ASYNC)
            .ok()
            .and_then(|c| c.reply().ok())
            .is_some_and(|r| r.status == GrabStatus::SUCCESS);
        let ptr = kb
            && conn
                .grab_pointer(
                    false,
                    win,
                    EventMask::NO_EVENT,
                    GrabMode::ASYNC,
                    GrabMode::ASYNC,
                    x11rb::NONE,
                    x11rb::NONE,
                    x11rb::CURRENT_TIME,
                )
                .ok()
                .and_then(|c| c.reply().ok())
                .is_some_and(|r| r.status == GrabStatus::SUCCESS);
        if kb && ptr {
            return true;
        }
        if kb {
            let _ = conn.ungrab_keyboard(x11rb::CURRENT_TIME);
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    false
}

fn event_loop(
    conn: &RustConnection,
    win: Window,
    root: Window,
    mut decoder: Decoder,
    mut secret: Secret,
    ui: &mut UiChild,
    ident: &Ident,
) -> Result<(), LockError> {
    let failed = |e: String| LockError::Failed(e);
    let mut status = String::new();
    ui.send(secret.char_count(), false, &status);

    loop {
        while let Some(ev) = conn.poll_for_event().map_err(|e| failed(e.to_string()))? {
            match ev {
                Event::KeyPress(kp) => {
                    match decoder.decode(&kp) {
                        KeyAction::Submit if secret.is_empty() => {
                            status = "Type your password and press Enter.".into();
                        }
                        KeyAction::Submit => {
                            ui.send(secret.char_count(), true, "");
                            let attempt = pam::authenticate_forked(&ident.name, &mut secret);
                            secret.clear();
                            for m in &attempt.messages {
                                eprintln!("[gordian_knot] pam: {}", m);
                            }
                            match attempt.result {
                                Ok(()) => return Ok(()),
                                Err(e) => {
                                    eprintln!("[gordian_knot] attempt failed: {}", e);
                                    status = attempt.messages.last().cloned().unwrap_or_else(|| e.to_string());
                                }
                            }
                            discard_queued_events(conn, win)?;
                        }
                        KeyAction::Backspace => secret.pop_char(),
                        KeyAction::Clear => {
                            secret.clear();
                            status.clear();
                        }
                        KeyAction::Append(s) => {
                            if !secret.push(s.as_bytes()) {
                                status = "Password too long.".into();
                            }
                        }
                        KeyAction::Ignore => continue,
                    }
                    ui.send(secret.char_count(), false, &status);
                }
                Event::VisibilityNotify(v) if v.state != Visibility::UNOBSCURED => {
                    let _ = conn.configure_window(win, &ConfigureWindowAux::new().stack_mode(StackMode::ABOVE));
                }
                Event::MappingNotify(_) => {
                    if let Some(d) = Decoder::from_server(conn, root) {
                        decoder = d;
                    }
                }
                _ => {}
            }
        }
        let _ = conn.flush();

        if ui.supervise() {
            ui.send(secret.char_count(), false, &status);
        }

        let mut fds = [
            libc::pollfd { fd: conn.stream().as_raw_fd(), events: libc::POLLIN, revents: 0 },
            libc::pollfd { fd: ui.sock(), events: libc::POLLIN, revents: 0 },
        ];
        unsafe { libc::poll(fds.as_mut_ptr(), 2, 500) };
    }
}

// Keys typed while PAM was deciding were typed blind: Drop everything the
// server queued meanwhile (sync() pulls it all in), then re-raise, since a
// VisibilityNotify among the dropped events would otherwise go unanswered.
fn discard_queued_events(conn: &RustConnection, win: Window) -> Result<(), LockError> {
    conn.sync().map_err(|e| LockError::Failed(e.to_string()))?;
    while conn.poll_for_event().map_err(|e| LockError::Failed(e.to_string()))?.is_some() {}
    let _ = conn.configure_window(win, &ConfigureWindowAux::new().stack_mode(StackMode::ABOVE));
    Ok(())
}

struct UiChild {
    exe: std::ffi::CString,
    ident: Ident,
    win: Window,
    sandbox: bool,
    pid: i32,
    sock: OwnedFd,
    spawned_at: Instant,
}

impl UiChild {
    fn spawn(ident: &Ident, win: Window, sandbox: bool) -> Result<UiChild, String> {
        // Resolved here, as root: The child is non-dumpable after its
        // setuid change, so its own /proc/self/exe is not readable to it.
        let exe = std::fs::read_link("/proc/self/exe").map_err(|e| format!("/proc/self/exe: {}", e))?;
        let exe = std::ffi::CString::new(exe.as_os_str().as_encoded_bytes()).map_err(|e| e.to_string())?;
        let (pid, sock) = start(&exe, ident, win, sandbox)?;
        Ok(UiChild { exe, ident: ident.clone(), win, sandbox, pid, sock, spawned_at: Instant::now() })
    }

    fn sock(&self) -> RawFd {
        self.sock.as_raw_fd()
    }

    fn wait_painted(&mut self, timeout: Duration) -> bool {
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

    fn send(&self, dots: usize, busy: bool, status: &str) {
        let msg = ui::encode_state(dots, busy, status);
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
    fn supervise(&mut self) -> bool {
        let mut status = 0;
        let gone = unsafe { libc::waitpid(self.pid, &mut status, libc::WNOHANG) } == self.pid;
        if !gone || self.spawned_at.elapsed() < RESPAWN_INTERVAL {
            return false;
        }
        eprintln!("[gordian_knot] UI child exited; the lock holds, restarting it");
        match start(&self.exe, &self.ident, self.win, self.sandbox) {
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

    fn stop(&mut self) {
        unsafe {
            libc::kill(self.pid, libc::SIGTERM);
            libc::waitpid(self.pid, std::ptr::null_mut(), 0);
        }
    }
}

// Fork, drop to the user with NO_NEW_PRIVS, and re-exec this binary as the
// UI (`--ui 3 WINDOW`) with the socket on fd 3 and nothing else inherited.
fn start(exe: &std::ffi::CStr, ident: &Ident, win: Window, sandbox: bool) -> Result<(i32, OwnedFd), String> {
    let mut sv = [0i32; 2];
    if unsafe { libc::socketpair(libc::AF_UNIX, libc::SOCK_SEQPACKET | libc::SOCK_CLOEXEC, 0, sv.as_mut_ptr()) } != 0 {
        return Err(format!("socketpair: {}", std::io::Error::last_os_error()));
    }
    let (ours, theirs) = unsafe { (OwnedFd::from_raw_fd(sv[0]), OwnedFd::from_raw_fd(sv[1])) };

    // Everything allocated before fork: The child only makes syscalls.
    let win_arg = std::ffi::CString::new(win.to_string()).unwrap();
    let mut argv: Vec<&std::ffi::CStr> = vec![c"gordian_knot", c"--ui", c"3", &win_arg];
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
            // dup2 onto itself would keep CLOEXEC, so fd 3 is cleared directly.
            let moved = if theirs.as_raw_fd() == 3 {
                libc::fcntl(3, libc::F_SETFD, 0) == 0
            } else {
                libc::dup2(theirs.as_raw_fd(), 3) == 3
            };
            if !moved {
                libc::_exit(126);
            }
            libc::syscall(libc::SYS_close_range, 4u32, u32::MAX, 0u32);
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
