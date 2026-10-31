// Harness shared by the end-to-end tests: Headless X servers, the real
// cherrypie binary, and window and property helpers.
#![allow(dead_code)]

use std::io::{BufRead, BufReader, Read};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use x11rb::connection::Connection;
use x11rb::protocol::xproto::*;
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as _;

pub const TIMEOUT: Duration = Duration::from_secs(10);
pub const SCREEN_W: u32 = 1280;
pub const SCREEN_H: u32 = 720;

pub struct Kill(pub Child);

impl Drop for Kill {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

pub fn start_xvfb() -> (Kill, String) {
    start_server("Xvfb", &["-screen", "0", &format!("{}x{}x24", SCREEN_W, SCREEN_H), "-nolisten", "tcp"], None)
}

// A resizeable Xephyr nested in its own Xvfb, so RandR mode changes are
// possible and no window manager on the user's display can touch it.
pub fn start_resizeable() -> (Kill, Kill, String) {
    let (host, host_display) = start_xvfb();
    let (nested, display) = start_server(
        "Xephyr",
        &["-screen", &format!("{}x{}", SCREEN_W, SCREEN_H), "-resizeable", "-noreset", "-ac"],
        Some(&host_display),
    );
    (host, nested, display)
}

pub fn start_server(bin: &str, args: &[&str], host_display: Option<&str>) -> (Kill, String) {
    let mut fds = [0i32; 2];
    assert_eq!(unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) }, 0);
    let read_fd = unsafe { OwnedFd::from_raw_fd(fds[0]) };
    let write_fd = unsafe { OwnedFd::from_raw_fd(fds[1]) };
    let write_raw = write_fd.as_raw_fd();

    let mut cmd = Command::new(bin);
    cmd.args(args)
        .args(["-displayfd", &write_raw.to_string()])
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if let Some(host) = host_display {
        cmd.env("DISPLAY", host);
    }
    unsafe {
        cmd.pre_exec(move || {
            let flags = libc::fcntl(write_raw, libc::F_GETFD);
            if flags < 0 || libc::fcntl(write_raw, libc::F_SETFD, flags & !libc::FD_CLOEXEC) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let child = Kill(cmd.spawn().expect("spawn the X server (xorg-server-xvfb, xorg-server-xephyr)"));
    drop(write_fd);

    let mut file = std::fs::File::from(read_fd);
    let mut buf = Vec::new();
    let mut chunk = [0u8; 16];
    let deadline = Instant::now() + TIMEOUT;
    while !buf.contains(&b'\n') {
        let left = deadline.checked_duration_since(Instant::now()).expect("displayfd timeout");
        let mut pfd = libc::pollfd { fd: file.as_raw_fd(), events: libc::POLLIN, revents: 0 };
        assert!(unsafe { libc::poll(&mut pfd, 1, left.as_millis().max(1) as i32) } > 0, "X server silent");
        let n = file.read(&mut chunk).expect("read displayfd");
        assert!(n > 0, "the X server exited before reporting a display");
        buf.extend_from_slice(&chunk[..n]);
    }
    let num = std::str::from_utf8(&buf).unwrap().trim().to_string();
    (child, format!(":{}", num))
}

// The real binary with its own XDG_RUNTIME_DIR, so its socket and instance
// lock never meet the user's.
pub struct Cherrypie {
    _child: Kill,
    pub runtime: tempfile::TempDir,
    pub display: String,
}

impl Cherrypie {
    pub fn socket(&self) -> std::path::PathBuf {
        let tag: String = self.display.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '_' }).collect();
        self.runtime.path().join(format!("cherrypie-{tag}.sock"))
    }
}

pub fn start_cherrypie(display: &str, config: &Path, learn_after: u64) -> Cherrypie {
    let runtime = tempfile::tempdir().unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_cherrypie"))
        .arg("--config")
        .arg(config)
        .args(["--learn-after", &learn_after.to_string()])
        .env("XDG_RUNTIME_DIR", runtime.path())
        .env("DISPLAY", display)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn cherrypie");
    let stderr = child.stderr.take().unwrap();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
            if line.contains("daemon started") {
                let _ = tx.send(());
            }
        }
    });
    rx.recv_timeout(TIMEOUT).expect("cherrypie never reported 'daemon started'");
    Cherrypie { _child: Kill(child), runtime, display: display.to_string() }
}

pub fn wait_until(mut ok: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + TIMEOUT;
    while Instant::now() < deadline {
        if ok() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    false
}

pub fn intern(conn: &RustConnection, name: &str) -> Atom {
    conn.intern_atom(false, name.as_bytes()).unwrap().reply().unwrap().atom
}

pub fn client_window(conn: &RustConnection, root: Window, class: &str, transient_for: Option<Window>) -> Window {
    window_with(conn, root, class, Some(class), transient_for, (0, 0, 300, 200))
}

pub fn window_with(
    conn: &RustConnection,
    root: Window,
    class: &str,
    title: Option<&str>,
    transient_for: Option<Window>,
    (x, y, w, h): (i16, i16, u16, u16),
) -> Window {
    let win = conn.generate_id().unwrap();
    conn.create_window(0, win, root, x, y, w, h, 0, WindowClass::INPUT_OUTPUT, 0, &CreateWindowAux::new())
        .unwrap();
    let wm_class = format!("{}\0{}\0", class.to_lowercase(), class);
    conn.change_property8(PropMode::REPLACE, win, AtomEnum::WM_CLASS, AtomEnum::STRING, wm_class.as_bytes())
        .unwrap();
    if let Some(title) = title {
        conn.change_property8(PropMode::REPLACE, win, AtomEnum::WM_NAME, AtomEnum::STRING, title.as_bytes())
            .unwrap();
    }
    if let Some(parent) = transient_for {
        conn.change_property32(PropMode::REPLACE, win, AtomEnum::WM_TRANSIENT_FOR, AtomEnum::WINDOW, &[parent])
            .unwrap();
    }
    conn.map_window(win).unwrap();
    win
}

pub fn publish_clients(conn: &RustConnection, root: Window, list_atom: Atom, clients: &[Window]) {
    conn.change_property32(PropMode::REPLACE, root, list_atom, AtomEnum::WINDOW, clients).unwrap();
    conn.flush().unwrap();
}

pub fn geometry(conn: &RustConnection, win: Window) -> (i16, i16, u16, u16) {
    let g = conn.get_geometry(win).unwrap().reply().unwrap();
    (g.x, g.y, g.width, g.height)
}

pub fn config_text(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_default()
}
