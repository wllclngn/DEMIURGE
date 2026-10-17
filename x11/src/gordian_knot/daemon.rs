// Idle watcher + lock-before-suspend. Runs as the user (the dispatcher
// drops privilege first), started by demiurge itself when
// [input.idle].lock_seconds is set, so it lives exactly as long as the
// session.
//
// Idle: Polls XScreenSaver every 5 seconds; past the threshold, with no
// logind "idle" inhibitor, it spawns `gordian_knot` (this binary, which
// becomes the root holder through its setuid bit). It keeps the Child and
// spawns no second locker while one lives.
//
// Suspend: A second thread holds a logind sleep *delay* inhibitor. On
// PrepareForSleep(true) it starts a locker (unless one runs), waits for it
// to report the lock is up (one byte on the GK_READY_FD pipe, written once
// the grab or the VT is held), then releases the inhibitor so the suspend
// proceeds; on PrepareForSleep(false) it takes a fresh one. The machine
// never sleeps unlocked while the daemon runs.

use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::process::CommandExt;
use std::process::{Child, Command};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use x11rb::connection::Connection;
use x11rb::protocol::screensaver::ConnectionExt as _;
use x11rb::rust_connection::RustConnection;

use crate::gordian_knot::inhibit;

const POLL_INTERVAL: Duration = Duration::from_secs(5);
const READY_TIMEOUT: Duration = Duration::from_secs(3);
const READY_FD: i32 = 3;

pub struct Locker {
    bin: String,
    child: Option<Child>,
}

impl Locker {
    pub fn new(bin: &str) -> Locker {
        Locker { bin: bin.to_string(), child: None }
    }

    pub fn alive(&mut self) -> bool {
        match self.child.as_mut().map(|c| c.try_wait()) {
            Some(Ok(None)) => true,
            Some(_) => {
                self.child = None;
                false
            }
            None => false,
        }
    }

    // Start a locker unless one runs. Returns the read end of its
    // readiness pipe when one was started.
    pub fn lock(&mut self) -> Option<OwnedFd> {
        if self.alive() {
            return None;
        }
        let mut fds = [0i32; 2];
        if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) } != 0 {
            eprintln!("[gordian_knot-daemon] pipe: {}", std::io::Error::last_os_error());
            return None;
        }
        let (rd, wr) = unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) };
        let wr_raw = wr.as_raw_fd();
        let mut cmd = Command::new(&self.bin);
        cmd.env("GK_READY_FD", READY_FD.to_string());
        unsafe {
            cmd.pre_exec(move || {
                let ok = if wr_raw == READY_FD {
                    libc::fcntl(READY_FD, libc::F_SETFD, 0) == 0
                } else {
                    libc::dup2(wr_raw, READY_FD) == READY_FD
                };
                if ok { Ok(()) } else { Err(std::io::Error::last_os_error()) }
            });
        }
        match cmd.spawn() {
            Ok(child) => {
                eprintln!("[gordian_knot-daemon] locker started (pid {})", child.id());
                self.child = Some(child);
                drop(wr);
                Some(rd)
            }
            Err(e) => {
                eprintln!("[gordian_knot-daemon] spawn failed: {}", e);
                None
            }
        }
    }
}

pub fn run(threshold_seconds: u64, self_binary: &str) -> i32 {
    let (conn, screen_num) = match RustConnection::connect(None) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("[gordian_knot-daemon] x11 connect: {}", e);
            return 1;
        }
    };
    let root = conn.setup().roots[screen_num].root;
    let has_saver = conn
        .screensaver_query_version(1, 0)
        .ok()
        .and_then(|c| c.reply().ok())
        .is_some();
    if !has_saver {
        eprintln!("[gordian_knot-daemon] ScreenSaver X extension unavailable");
        return 1;
    }

    let locker = Arc::new(Mutex::new(Locker::new(self_binary)));
    {
        let locker = Arc::clone(&locker);
        std::thread::spawn(move || suspend_watch(locker));
    }

    let threshold_ms = threshold_seconds.saturating_mul(1000);
    let mut inhibitor = inhibit::Watcher::new();
    eprintln!(
        "[gordian_knot-daemon] watching idle; threshold {}s, poll {}s",
        threshold_seconds,
        POLL_INTERVAL.as_secs()
    );

    loop {
        std::thread::sleep(POLL_INTERVAL);
        let Some(info) = conn.screensaver_query_info(root).ok().and_then(|c| c.reply().ok()) else {
            continue;
        };
        let idle_ms = info.ms_since_user_input as u64;
        if threshold_ms == 0 || idle_ms < threshold_ms {
            continue;
        }
        let mut l = locker.lock().unwrap_or_else(|p| p.into_inner());
        if l.alive() || inhibitor.is_idle_inhibited() {
            continue;
        }
        eprintln!("[gordian_knot-daemon] idle {}s >= {}s, locking", idle_ms / 1000, threshold_seconds);
        drop(l.lock());
    }
}

fn suspend_watch(locker: Arc<Mutex<Locker>>) {
    let conn = match zbus::blocking::Connection::system() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("[gordian_knot-daemon] system bus: {}; no lock-before-suspend", e);
            return;
        }
    };
    let proxy = match zbus::blocking::Proxy::new(
        &conn,
        "org.freedesktop.login1",
        "/org/freedesktop/login1",
        "org.freedesktop.login1.Manager",
    ) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("[gordian_knot-daemon] logind: {}; no lock-before-suspend", e);
            return;
        }
    };
    let signals = match proxy.receive_signal("PrepareForSleep") {
        Ok(s) => s,
        Err(e) => {
            eprintln!("[gordian_knot-daemon] PrepareForSleep: {}; no lock-before-suspend", e);
            return;
        }
    };
    let mut held = take_inhibitor(&proxy);
    for msg in signals {
        let going_down: bool = msg.body().deserialize().unwrap_or(false);
        if going_down {
            let ready = locker.lock().unwrap_or_else(|p| p.into_inner()).lock();
            if let Some(rd) = ready {
                let mut pfd = libc::pollfd { fd: rd.as_raw_fd(), events: libc::POLLIN, revents: 0 };
                if unsafe { libc::poll(&mut pfd, 1, READY_TIMEOUT.as_millis() as i32) } <= 0 {
                    eprintln!("[gordian_knot-daemon] locker not ready before suspend");
                }
            }
            held = None;
        } else {
            held = take_inhibitor(&proxy);
        }
    }
    drop(held);
}

fn take_inhibitor(proxy: &zbus::blocking::Proxy<'_>) -> Option<zbus::zvariant::OwnedFd> {
    match proxy.call::<_, _, zbus::zvariant::OwnedFd>(
        "Inhibit",
        &("sleep", "gordian_knot", "Lock the screen before suspend", "delay"),
    ) {
        Ok(fd) => Some(fd),
        Err(e) => {
            eprintln!("[gordian_knot-daemon] sleep inhibitor: {}", e);
            None
        }
    }
}
