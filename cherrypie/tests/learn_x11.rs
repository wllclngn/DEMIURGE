// End to end: The real cherrypie binary against a headless Xvfb, with this
// test standing in for the window manager by publishing _NET_CLIENT_LIST.
// Verifies a missing config is created with one learned rule per class once
// a window of it stays open, that the learned defaults are applied to the
// window, that transient, untitled, short-lived and fullscreen windows are
// never learned, that placement respects the work area, and that monitor
// changes are followed.

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

const TIMEOUT: Duration = Duration::from_secs(10);
const SCREEN_W: u32 = 1280;
const SCREEN_H: u32 = 720;

struct Kill(Child);

impl Drop for Kill {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn start_xvfb() -> (Kill, String) {
    start_server("Xvfb", &["-screen", "0", &format!("{}x{}x24", SCREEN_W, SCREEN_H), "-nolisten", "tcp"], None)
}

// A resizeable Xephyr nested in its own Xvfb, so RandR mode changes are
// possible and no window manager on the user's display can touch it.
fn start_resizeable() -> (Kill, Kill, String) {
    let (host, host_display) = start_xvfb();
    let (nested, display) = start_server(
        "Xephyr",
        &["-screen", &format!("{}x{}", SCREEN_W, SCREEN_H), "-resizeable", "-noreset", "-ac"],
        Some(&host_display),
    );
    (host, nested, display)
}

fn start_server(bin: &str, args: &[&str], host_display: Option<&str>) -> (Kill, String) {
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

fn start_cherrypie(display: &str, config: &Path, learn_after: u64) -> Kill {
    let mut child = Command::new(env!("CARGO_BIN_EXE_cherrypie"))
        .arg("--config")
        .arg(config)
        .args(["--learn-after", &learn_after.to_string()])
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
    Kill(child)
}

fn wait_until(mut ok: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + TIMEOUT;
    while Instant::now() < deadline {
        if ok() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    false
}

fn intern(conn: &RustConnection, name: &str) -> Atom {
    conn.intern_atom(false, name.as_bytes()).unwrap().reply().unwrap().atom
}

fn client_window(conn: &RustConnection, root: Window, class: &str, transient_for: Option<Window>) -> Window {
    window_with(conn, root, class, Some(class), transient_for, (0, 0, 300, 200))
}

fn window_with(
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

fn publish_clients(conn: &RustConnection, root: Window, list_atom: Atom, clients: &[Window]) {
    conn.change_property32(PropMode::REPLACE, root, list_atom, AtomEnum::WINDOW, clients).unwrap();
    conn.flush().unwrap();
}

fn geometry(conn: &RustConnection, win: Window) -> (i16, i16, u16, u16) {
    let g = conn.get_geometry(win).unwrap().reply().unwrap();
    (g.x, g.y, g.width, g.height)
}

#[test]
fn learns_one_rule_per_class_and_applies_it() {
    let (_xvfb, display) = start_xvfb();
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("config.toml");

    let (conn, screen) = RustConnection::connect(Some(&display)).unwrap();
    let root = conn.setup().roots[screen].root;
    let list_atom = intern(&conn, "_NET_CLIENT_LIST");
    publish_clients(&conn, root, list_atom, &[]);

    let _cherrypie = start_cherrypie(&display, &config, 1);
    assert!(!config.exists(), "no window yet, so no rule and no file");

    // A second daemon on the same display bows out, so no class is learned twice.
    let second = Command::new(env!("CARGO_BIN_EXE_cherrypie"))
        .arg("--config")
        .arg(&config)
        .env("DISPLAY", &display)
        .output()
        .expect("run a second cherrypie");
    assert!(second.status.success(), "second instance: {:?}", second.status);
    assert!(String::from_utf8_lossy(&second.stderr).contains("already running"));

    let first = client_window(&conn, root, "LearnTest", None);
    publish_clients(&conn, root, list_atom, &[first]);

    // 60% x 70% of 1280x720, centered.
    let (w, h) = ((SCREEN_W * 60 / 100) as u16, (SCREEN_H * 70 / 100) as u16);
    let (x, y) = (((SCREEN_W as u16 - w) / 2) as i16, ((SCREEN_H as u16 - h) / 2) as i16);
    assert!(
        wait_until(|| geometry(&conn, first) == (x, y, w, h)),
        "learned defaults not applied: {:?}",
        geometry(&conn, first)
    );
    assert!(wait_until(|| std::fs::read_to_string(&config).is_ok_and(|s| s.contains("'^LearnTest$'"))));

    // A second window of the class matches the learned rule.
    let second = client_window(&conn, root, "LearnTest", None);
    publish_clients(&conn, root, list_atom, &[first, second]);
    assert!(wait_until(|| geometry(&conn, second) == (x, y, w, h)));

    // A transient window is a dialog: Never learned, never moved.
    let dialog = client_window(&conn, root, "DialogTest", Some(first));
    publish_clients(&conn, root, list_atom, &[first, second, dialog]);
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(geometry(&conn, dialog), (0, 0, 300, 200));

    let body = std::fs::read_to_string(&config).unwrap();
    assert_eq!(body.matches("[[rule]]").count(), 1, "one rule per class:\n{body}");
    assert!(!body.contains("DialogTest"));
}

fn config_text(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_default()
}

#[test]
fn short_lived_and_untitled_windows_are_not_learned() {
    let (_xvfb, display) = start_xvfb();
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("config.toml");
    let (conn, screen) = RustConnection::connect(Some(&display)).unwrap();
    let root = conn.setup().roots[screen].root;
    let list_atom = intern(&conn, "_NET_CLIENT_LIST");
    publish_clients(&conn, root, list_atom, &[]);
    let _cherrypie = start_cherrypie(&display, &config, 2);

    let stays = client_window(&conn, root, "Stays", None);
    let blink = client_window(&conn, root, "Blink", None);
    let untitled = window_with(&conn, root, "Untitled", None, None, (0, 0, 300, 200));
    publish_clients(&conn, root, list_atom, &[stays, blink, untitled]);

    // The defaults apply at once, written or not.
    assert!(wait_until(|| geometry(&conn, blink).2 == (SCREEN_W * 60 / 100) as u16));
    conn.destroy_window(blink).unwrap();
    publish_clients(&conn, root, list_atom, &[stays, untitled]);

    assert!(wait_until(|| config_text(&config).contains("'^Stays$'")));
    std::thread::sleep(Duration::from_millis(500));
    let body = config_text(&config);
    assert!(!body.contains("Blink"), "a window that closed inside the interval was learned:\n{body}");
    assert!(!body.contains("Untitled"), "an untitled window was learned:\n{body}");
    assert_eq!(geometry(&conn, untitled), (0, 0, 300, 200));
}

#[test]
fn fullscreen_windows_keep_their_geometry() {
    let (_xvfb, display) = start_xvfb();
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("config.toml");
    std::fs::write(&config, "[[rule]]\nclass = \"Game\"\nposition = \"center\"\nsize = [\"50%\", \"50%\"]\n").unwrap();
    let (conn, screen) = RustConnection::connect(Some(&display)).unwrap();
    let root = conn.setup().roots[screen].root;
    let list_atom = intern(&conn, "_NET_CLIENT_LIST");
    publish_clients(&conn, root, list_atom, &[]);
    let _cherrypie = start_cherrypie(&display, &config, 1);

    // A matched window in the fullscreen state, and an unmatched one that
    // simply covers the screen.
    let full = (0, 0, SCREEN_W as u16, SCREEN_H as u16);
    let game = window_with(&conn, root, "Game", Some("Game"), None, full);
    let state = intern(&conn, "_NET_WM_STATE");
    let fullscreen = intern(&conn, "_NET_WM_STATE_FULLSCREEN");
    conn.change_property32(PropMode::REPLACE, game, state, AtomEnum::ATOM, &[fullscreen]).unwrap();
    let movie = window_with(&conn, root, "Movie", Some("Movie"), None, full);
    publish_clients(&conn, root, list_atom, &[game, movie]);

    std::thread::sleep(Duration::from_millis(2500));
    assert_eq!(geometry(&conn, game), full);
    assert_eq!(geometry(&conn, movie), full);
    assert!(!config_text(&config).contains("Movie"), "a fullscreen window was learned");
}

#[test]
fn placement_uses_the_work_area_below_the_bar() {
    let (_xvfb, display) = start_xvfb();
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("config.toml");
    let (conn, screen) = RustConnection::connect(Some(&display)).unwrap();
    let root = conn.setup().roots[screen].root;
    let list_atom = intern(&conn, "_NET_CLIENT_LIST");
    let workarea = intern(&conn, "_NET_WORKAREA");
    let desktop = intern(&conn, "_NET_CURRENT_DESKTOP");
    conn.change_property32(PropMode::REPLACE, root, workarea, AtomEnum::CARDINAL, &[0, 24, SCREEN_W, SCREEN_H - 24])
        .unwrap();
    conn.change_property32(PropMode::REPLACE, root, desktop, AtomEnum::CARDINAL, &[0]).unwrap();
    publish_clients(&conn, root, list_atom, &[]);
    let _cherrypie = start_cherrypie(&display, &config, 1);

    let win = client_window(&conn, root, "Below", None);
    publish_clients(&conn, root, list_atom, &[win]);
    // 60% x 70% of the 1280x696 work area, centered inside it.
    let (w, h) = (768u16, 487u16);
    let want = (((SCREEN_W as u16 - w) / 2) as i16, 24 + ((SCREEN_H as u16 - 24 - h) / 2) as i16, w, h);
    assert!(wait_until(|| geometry(&conn, win) == want), "got {:?}, want {:?}", geometry(&conn, win), want);
}

#[test]
fn monitor_changes_are_followed() {
    let (_host, _xephyr, display) = start_resizeable();
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("config.toml");
    let (conn, screen) = RustConnection::connect(Some(&display)).unwrap();
    let root = conn.setup().roots[screen].root;
    let list_atom = intern(&conn, "_NET_CLIENT_LIST");
    publish_clients(&conn, root, list_atom, &[]);
    let _cherrypie = start_cherrypie(&display, &config, 1);

    let status = Command::new("xrandr").args(["-display", &display, "-s", "1024x768"]).status().unwrap();
    assert!(status.success());
    std::thread::sleep(Duration::from_millis(500));

    let win = client_window(&conn, root, "Resized", None);
    publish_clients(&conn, root, list_atom, &[win]);
    // 60% x 70% of the new 1024x768 mode, centered.
    let want = (205, 115, 614, 538);
    assert!(wait_until(|| geometry(&conn, win) == want), "got {:?}, want {:?}", geometry(&conn, win), want);
}
