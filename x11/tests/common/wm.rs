// Integration-test harness for DEMIURGE. Each WmTest owns:
//
//   - a private Xephyr nested X server on its own DISPLAY
//   - a demiurge process running inside that server
//   - an independent x11rb connection used by the test for inspection
//     and synthetic input
//
// Display number contention is avoided by Xephyr's -displayfd flag,
// which makes Xephyr pick an unused display itself and write the chosen
// number to the given file descriptor. No /tmp/.X{N}-lock probing, no
// race against other test binaries.
//
// Drop tears down demiurge first (so it sees its connection close
// cleanly rather than the X server vanishing under it), then Xephyr,
// then the temp config dir. If a test panics, Drop still runs.

use std::io::Read;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use x11rb::connection::Connection;
use x11rb::protocol::xproto::*;
use x11rb::rust_connection::RustConnection;

const STARTUP_TIMEOUT: Duration = Duration::from_secs(5);
const POLL_INTERVAL: Duration = Duration::from_millis(50);

pub struct WmTest {
    pub display: String,
    // The private Xvfb a resizeable Xephyr runs inside (new_resizeable).
    host: Option<Child>,
    xephyr: Child,
    demiurge: Option<Child>,
    config_dir: Option<PathBuf>,
    pub conn: RustConnection,
    pub root: Window,
    pub atoms: TestAtoms,
}

// Atoms the test side needs to inspect EWMH state. Interned once on
// connect so individual assertions don't pay the round-trip each time.
// Field names mirror the EWMH spec verbatim; snake-case lint is off
// because matching the spec literally is more readable than
// _net_supporting_wm_check.
#[allow(non_snake_case)]
pub struct TestAtoms {
    pub _NET_SUPPORTING_WM_CHECK: Atom,
    pub _NET_WM_NAME: Atom,
    pub _NET_NUMBER_OF_DESKTOPS: Atom,
    pub _NET_DESKTOP_NAMES: Atom,
    pub _NET_CURRENT_DESKTOP: Atom,
    pub _NET_CLIENT_LIST: Atom,
    pub _NET_ACTIVE_WINDOW: Atom,
    pub _NET_WM_DESKTOP: Atom,
    pub _NET_WORKAREA: Atom,
    pub _NET_DESKTOP_GEOMETRY: Atom,
    pub UTF8_STRING: Atom,
}

impl WmTest {
    /// Spawn Xephyr on a fresh display, connect, intern atoms. Does not
    /// start demiurge -- callers do that via start_demiurge so the test
    /// can write a per-test config first.
    pub fn new() -> Self {
        Self::start(None)
    }

    /// A resizeable Xephyr inside its own Xvfb: RandR mode changes work
    /// (xrandr -s), and no window manager on the user's display can
    /// resize the Xephyr window under the test.
    pub fn new_resizeable() -> Self {
        let (host, display) = spawn_server("Xvfb", &["-screen", "0", "1920x1200x24", "-nolisten", "tcp"], None);
        let mut t = Self::start(Some(&display));
        t.host = Some(host);
        t
    }

    fn start(host_display: Option<&str>) -> Self {
        let mut args = vec!["-screen", "1280x720", "-noreset", "-ac"];
        if host_display.is_some() {
            args.push("-resizeable");
        }
        let (xephyr, display) = spawn_server("Xephyr", &args, host_display);

        // Connect to the new server. Xephyr may need a moment between
        // writing -displayfd and accepting connections; poll-with-timeout.
        let (conn, root) = connect_with_timeout(&display, STARTUP_TIMEOUT)
            .expect("could not connect to Xephyr after startup");

        let atoms = TestAtoms::intern(&conn);

        Self {
            display,
            host: None,
            xephyr,
            demiurge: None,
            config_dir: None,
            conn,
            root,
            atoms,
        }
    }

    /// Write the given config to a fresh tempdir and start demiurge
    /// against it. Blocks until _NET_SUPPORTING_WM_CHECK is set on the
    /// root, which is demiurge's "fully initialized" signal.
    pub fn start_demiurge(&mut self, config_toml: &str) {
        let dir = std::env::temp_dir().join(format!(
            "demiurge-test-{}-{}",
            std::process::id(),
            self.display.trim_start_matches(':'),
        ));
        std::fs::create_dir_all(&dir).expect("mkdir tempdir");
        let config_path = dir.join("config.toml");
        std::fs::write(&config_path, config_toml).expect("write config");
        self.config_dir = Some(dir);

        let bin = env!("CARGO_BIN_EXE_demiurge");
        let demiurge = Command::new(bin)
            .arg("-c")
            .arg(&config_path)
            .env("DISPLAY", &self.display)
            // Strip XCURSOR_THEME etc. so the test inherits a clean env;
            // the WM will export its own.
            .env_remove("XCURSOR_THEME")
            .env_remove("XCURSOR_SIZE")
            .stdout(Stdio::null())
            // DEMIURGE_TEST_STDERR=1 passes the WM's log through for a
            // failing test (run it with --nocapture).
            .stderr(if std::env::var_os("DEMIURGE_TEST_STDERR").is_some() {
                Stdio::inherit()
            } else {
                Stdio::null()
            })
            .spawn()
            .expect("spawn demiurge");
        self.demiurge = Some(demiurge);

        wait_for_wm(&self.conn, self.root, &self.atoms, STARTUP_TIMEOUT)
            .expect("demiurge did not finish initializing within timeout");
    }

    pub fn demiurge_pid(&self) -> u32 {
        self.demiurge.as_ref().expect("demiurge not started").id()
    }

    /// Create + map a basic InputOutput window on the test connection
    /// and flush. Returns the X window ID. Demiurge sees the MapRequest
    /// and runs it through manage().
    pub fn spawn_window(&self, w: u16, h: u16) -> Window {
        let win = self.conn.generate_id().expect("generate_id");
        let depth = self.conn.setup().roots[0].root_depth;
        self.conn
            .create_window(
                depth,
                win,
                self.root,
                0,
                0,
                w,
                h,
                0,
                WindowClass::INPUT_OUTPUT,
                x11rb::COPY_FROM_PARENT,
                &CreateWindowAux::new()
                    .background_pixel(self.conn.setup().roots[0].white_pixel)
                    .event_mask(EventMask::EXPOSURE),
            )
            .expect("create_window");
        self.conn.map_window(win).expect("map_window");
        self.conn.flush().expect("flush");
        win
    }

    /// Read _NET_SUPPORTING_WM_CHECK and follow the pointer to the
    /// check window. Returns None if the property is absent.
    pub fn supporting_wm_check(&self) -> Option<Window> {
        prop_window(&self.conn, self.root, self.atoms._NET_SUPPORTING_WM_CHECK)
    }

    /// Returns the UTF-8 _NET_WM_NAME on the given window, or None.
    pub fn wm_name(&self, window: Window) -> Option<String> {
        let reply = self
            .conn
            .get_property(
                false,
                window,
                self.atoms._NET_WM_NAME,
                self.atoms.UTF8_STRING,
                0,
                64,
            )
            .ok()?
            .reply()
            .ok()?;
        if reply.value.is_empty() {
            return None;
        }
        Some(String::from_utf8_lossy(&reply.value).into_owned())
    }

    pub fn number_of_desktops(&self) -> Option<u32> {
        prop_u32(&self.conn, self.root, self.atoms._NET_NUMBER_OF_DESKTOPS)
    }

    pub fn current_desktop(&self) -> Option<u32> {
        prop_u32(&self.conn, self.root, self.atoms._NET_CURRENT_DESKTOP)
    }

    pub fn desktop_names(&self) -> Vec<String> {
        let reply = match self
            .conn
            .get_property(
                false,
                self.root,
                self.atoms._NET_DESKTOP_NAMES,
                self.atoms.UTF8_STRING,
                0,
                1024,
            )
            .and_then(|c| Ok(c.reply()))
        {
            Ok(Ok(r)) => r,
            _ => return Vec::new(),
        };
        reply
            .value
            .split(|&b| b == 0)
            .filter(|s| !s.is_empty())
            .map(|s| String::from_utf8_lossy(s).into_owned())
            .collect()
    }

    pub fn workarea(&self) -> Vec<u32> {
        prop_u32_vec(&self.conn, self.root, self.atoms._NET_WORKAREA)
    }

    pub fn desktop_geometry(&self) -> Vec<u32> {
        prop_u32_vec(&self.conn, self.root, self.atoms._NET_DESKTOP_GEOMETRY)
    }

    pub fn client_list(&self) -> Vec<Window> {
        prop_window_vec(&self.conn, self.root, self.atoms._NET_CLIENT_LIST)
    }

    pub fn active_window(&self) -> Option<Window> {
        let v = prop_window_vec(&self.conn, self.root, self.atoms._NET_ACTIVE_WINDOW);
        v.first().copied().filter(|&w| w != 0)
    }

    /// Send a `_NET_CURRENT_DESKTOP` client message to the root with
    /// the given tag index. demiurge's handle_client_message routes
    /// this to view_tag(tag).
    pub fn request_view_tag(&self, tag: u32) {
        let data = ClientMessageData::from([tag, 0u32, 0, 0, 0]);
        let event = ClientMessageEvent::new(
            32,
            self.root,
            self.atoms._NET_CURRENT_DESKTOP,
            data,
        );
        // EWMH says these go to root with substructure_redirect|notify
        // so the WM (running as the root's substructure manager) sees them.
        let _ = self.conn.send_event(
            false,
            self.root,
            EventMask::SUBSTRUCTURE_REDIRECT | EventMask::SUBSTRUCTURE_NOTIFY,
            event,
        );
        let _ = self.conn.flush();
    }

    /// Send a `_NET_WM_DESKTOP` client message asking the WM to move
    /// `window` to `tag`. handle_client_message routes this to
    /// move_to_tag(tag) with the message's window as focus context.
    pub fn request_move_to_tag(&self, window: Window, tag: u32) {
        let data = ClientMessageData::from([tag, 0u32, 0, 0, 0]);
        let event = ClientMessageEvent::new(
            32,
            window,
            self.atoms._NET_WM_DESKTOP,
            data,
        );
        let _ = self.conn.send_event(
            false,
            self.root,
            EventMask::SUBSTRUCTURE_REDIRECT | EventMask::SUBSTRUCTURE_NOTIFY,
            event,
        );
        let _ = self.conn.flush();
    }

    /// Read `_NET_WM_DESKTOP` on the given window.
    pub fn wm_desktop(&self, window: Window) -> Option<u32> {
        prop_u32(&self.conn, window, self.atoms._NET_WM_DESKTOP)
    }

    /// Get the X11 map state of a window. Used by tests to verify a
    /// view_tag actually unmapped clients on the previously-visible
    /// tag (and re-mapped them when switched back).
    pub fn map_state(&self, window: Window) -> Option<MapState> {
        let reply = self
            .conn
            .get_window_attributes(window)
            .ok()?
            .reply()
            .ok()?;
        Some(reply.map_state)
    }

    /// Block until `predicate` returns true or `timeout` elapses.
    /// Used to wait for asynchronous WM state transitions (e.g. a
    /// MapRequest landing in _NET_CLIENT_LIST after spawn_window).
    pub fn wait_for(&self, mut predicate: impl FnMut(&Self) -> bool, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        loop {
            if predicate(self) {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(POLL_INTERVAL);
        }
    }
}

// Start an X server that reports its display through -displayfd, with
// DISPLAY set to `host_display` for a nested server.
fn spawn_server(bin: &str, args: &[&str], host_display: Option<&str>) -> (Child, String) {
    // Pipe the server's chosen display number back to us. It writes ASCII
    // digits + '\n' to the write end and closes it; we read from our end.
    let (read_fd, write_fd) = make_pipe();
    let write_raw = write_fd.as_raw_fd();

    let mut cmd = Command::new(bin);
    cmd.args(args)
        .args(["-displayfd", &write_raw.to_string()])
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if let Some(host) = host_display {
        cmd.env("DISPLAY", host);
    }
    // CLOEXEC is cleared in the child only, between fork and exec. Clearing
    // it in the parent let every server a parallel test spawned meanwhile
    // inherit this write end, so the read below never saw EOF.
    unsafe {
        cmd.pre_exec(move || {
            let flags = libc::fcntl(write_raw, libc::F_GETFD);
            if flags < 0 || libc::fcntl(write_raw, libc::F_SETFD, flags & !libc::FD_CLOEXEC) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let child = cmd
        .spawn()
        .unwrap_or_else(|e| panic!("spawn {bin}: {e} (xorg-server-xephyr, xorg-server-xvfb)"));

    // Drop the parent's write end so the read returns EOF when the server
    // closes its copy.
    drop(write_fd);

    let display_num = read_displayfd(read_fd, STARTUP_TIMEOUT)
        .unwrap_or_else(|| panic!("{bin} did not report a display number within timeout"));
    (child, format!(":{}", display_num))
}

impl Drop for WmTest {
    fn drop(&mut self) {
        if let Some(mut d) = self.demiurge.take() {
            // SIGTERM -> let signalfd path do clean shutdown if possible.
            unsafe {
                libc::kill(d.id() as libc::pid_t, libc::SIGTERM);
            }
            // Give it a beat to clean up; then force-kill if still alive.
            let deadline = Instant::now() + Duration::from_millis(500);
            loop {
                match d.try_wait() {
                    Ok(Some(_)) => break,
                    _ if Instant::now() >= deadline => {
                        let _ = d.kill();
                        let _ = d.wait();
                        break;
                    }
                    _ => std::thread::sleep(Duration::from_millis(20)),
                }
            }
        }
        let _ = self.xephyr.kill();
        let _ = self.xephyr.wait();
        if let Some(mut host) = self.host.take() {
            let _ = host.kill();
            let _ = host.wait();
        }
        if let Some(dir) = self.config_dir.take() {
            let _ = std::fs::remove_dir_all(dir);
        }
    }
}

impl TestAtoms {
    fn intern(conn: &RustConnection) -> Self {
        let one = |name: &str| -> Atom {
            conn.intern_atom(false, name.as_bytes())
                .expect("intern")
                .reply()
                .expect("intern reply")
                .atom
        };
        Self {
            _NET_SUPPORTING_WM_CHECK: one("_NET_SUPPORTING_WM_CHECK"),
            _NET_WM_NAME: one("_NET_WM_NAME"),
            _NET_NUMBER_OF_DESKTOPS: one("_NET_NUMBER_OF_DESKTOPS"),
            _NET_DESKTOP_NAMES: one("_NET_DESKTOP_NAMES"),
            _NET_CURRENT_DESKTOP: one("_NET_CURRENT_DESKTOP"),
            _NET_CLIENT_LIST: one("_NET_CLIENT_LIST"),
            _NET_ACTIVE_WINDOW: one("_NET_ACTIVE_WINDOW"),
            _NET_WM_DESKTOP: one("_NET_WM_DESKTOP"),
            _NET_WORKAREA: one("_NET_WORKAREA"),
            _NET_DESKTOP_GEOMETRY: one("_NET_DESKTOP_GEOMETRY"),
            UTF8_STRING: one("UTF8_STRING"),
        }
    }
}

// --- internals ---

// Create a pipe with CLOEXEC on both ends; the spawn clears it on the
// write end inside the child only.
fn make_pipe() -> (OwnedFd, OwnedFd) {
    let mut fds = [0i32; 2];
    let rc = unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) };
    assert!(rc == 0, "pipe2 failed: {}", std::io::Error::last_os_error());
    let read_fd = unsafe { OwnedFd::from_raw_fd(fds[0]) };
    let write_fd = unsafe { OwnedFd::from_raw_fd(fds[1]) };
    (read_fd, write_fd)
}

// Read the displayfd pipe until Xephyr's "<digits>\n" arrives, polling
// against the deadline so a silent Xephyr fails the test instead of
// hanging it. EOF before a newline means Xephyr died.
fn read_displayfd(read_fd: OwnedFd, timeout: Duration) -> Option<u32> {
    let deadline = Instant::now() + timeout;
    let mut file = std::fs::File::from(read_fd);
    let mut buf = Vec::with_capacity(16);
    let mut chunk = [0u8; 16];
    while !buf.contains(&b'\n') {
        let left = deadline.checked_duration_since(Instant::now())?;
        let mut pfd = libc::pollfd { fd: file.as_raw_fd(), events: libc::POLLIN, revents: 0 };
        let ready = unsafe { libc::poll(&mut pfd, 1, left.as_millis().max(1) as i32) };
        if ready <= 0 {
            if ready < 0 && std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return None;
        }
        match file.read(&mut chunk) {
            Ok(0) | Err(_) => return None,
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
        }
    }
    std::str::from_utf8(&buf).ok()?.trim().parse::<u32>().ok()
}

// Xephyr accepts connections roughly when -displayfd fires, but a
// retry loop costs nothing and saves the rare "raced the listen()"
// false negative.
fn connect_with_timeout(display: &str, timeout: Duration) -> Option<(RustConnection, Window)> {
    let deadline = Instant::now() + timeout;
    loop {
        match RustConnection::connect(Some(display)) {
            Ok((conn, _screen)) => {
                let root = conn.setup().roots[0].root;
                return Some((conn, root));
            }
            Err(_) if Instant::now() < deadline => {
                std::thread::sleep(POLL_INTERVAL);
            }
            Err(_) => return None,
        }
    }
}

// Demiurge is "ready" when it has set _NET_SUPPORTING_WM_CHECK on the
// root and the check window's _NET_WM_NAME is "demiurge". This proves
// the EWMH bootstrap completed, not just that the process is alive.
fn wait_for_wm(
    conn: &RustConnection,
    root: Window,
    atoms: &TestAtoms,
    timeout: Duration,
) -> Result<(), String> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(check) = prop_window(conn, root, atoms._NET_SUPPORTING_WM_CHECK) {
            if let Ok(reply) = conn.get_property(
                false,
                check,
                atoms._NET_WM_NAME,
                atoms.UTF8_STRING,
                0,
                64,
            ) {
                if let Ok(r) = reply.reply() {
                    let name = String::from_utf8_lossy(&r.value);
                    if name.starts_with("demiurge") {
                        return Ok(());
                    }
                }
            }
        }
        if Instant::now() >= deadline {
            return Err("WM check window never appeared".into());
        }
        std::thread::sleep(POLL_INTERVAL);
    }
}

fn prop_window(conn: &RustConnection, window: Window, atom: Atom) -> Option<Window> {
    let reply = conn
        .get_property(false, window, atom, AtomEnum::WINDOW, 0, 1)
        .ok()?
        .reply()
        .ok()?;
    reply.value32().and_then(|mut it| it.next()).filter(|&w| w != 0)
}

fn prop_u32(conn: &RustConnection, window: Window, atom: Atom) -> Option<u32> {
    let reply = conn
        .get_property(false, window, atom, AtomEnum::CARDINAL, 0, 1)
        .ok()?
        .reply()
        .ok()?;
    reply.value32().and_then(|mut it| it.next())
}

fn prop_u32_vec(conn: &RustConnection, window: Window, atom: Atom) -> Vec<u32> {
    let reply = match conn
        .get_property(false, window, atom, AtomEnum::CARDINAL, 0, 1024)
        .and_then(|c| Ok(c.reply()))
    {
        Ok(Ok(r)) => r,
        _ => return Vec::new(),
    };
    reply.value32().map(|it| it.collect()).unwrap_or_default()
}

fn prop_window_vec(conn: &RustConnection, window: Window, atom: Atom) -> Vec<Window> {
    let reply = match conn
        .get_property(false, window, atom, AtomEnum::WINDOW, 0, 1024)
        .and_then(|c| Ok(c.reply()))
    {
        Ok(Ok(r)) => r,
        _ => return Vec::new(),
    };
    reply.value32().map(|it| it.collect()).unwrap_or_default()
}

// --- canned configs ---

/// Minimal config for tests: three tags, default font, no startup spawns.
pub fn minimal_config() -> &'static str {
    r#"
[general]
tags = ["A", "B", "C"]
default_layout = "floating"

[bar]
height = 24
font = "Noto Sans 9"

[startup]
commands = []
"#
}
