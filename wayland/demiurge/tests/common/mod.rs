// Integration-test harness for demiurge-wl. Each WlTest:
//
//   - allocates a private XDG_RUNTIME_DIR (tempdir mode 0700) so
//     concurrent tests don't fight over the wayland-N namespace
//   - overrides $HOME to that same tempdir so demiurge-wl's
//     config::Paths::init() resolves to a non-existent config and
//     falls back to Config::default(). Tests can therefore assert
//     against documented default behavior without depending on the
//     dev machine's ~/.config/demiurge/config.toml.
//   - spawns demiurge-wl as a child, inheriting the host's DISPLAY
//     so winit can nest its host window in whatever X server runs
//     the test (Xorg in dev, Xvfb in CI). DEMIURGE_WL_TEST_BIN names
//     another compositor binary instead: The QEMU guest runs the same
//     suite against demiurge-wl-tty on the real screen.
//   - polls for the wayland-1 socket file to appear (auto-pick is
//     deterministic when XDG_RUNTIME_DIR is empty)
//   - connects a wayland-client `Connection` directly via UnixStream,
//     bypassing env vars so parallel tests don't race
//   - tears down the child on Drop (SIGKILL via Child::kill); even on
//     test panic the cleanup runs
//
// The Wayland equivalent of EWMH-on-root inspection (used by the X11
// wm_smoke harness) doesn't exist -- compositors don't expose
// internal state to clients. So tests observe behavior from the
// client side: globals advertised, xdg_surface configure cycle,
// keyboard focus, surface roles, frame callbacks. That's the right
// scope for an integration harness anyway -- it's testing the
// protocol contract demiurge-wl exposes, not its internal
// bookkeeping.

#![allow(dead_code)]

use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::{PoisonError, RwLock, RwLockReadGuard, RwLockWriteGuard};
use std::time::{Duration, Instant};

use wayland_client::Connection;

const STARTUP_TIMEOUT: Duration = Duration::from_secs(10);
const POLL_INTERVAL: Duration = Duration::from_millis(50);

// The Wayland session the tests run in, its socket by absolute path; None
// outside one.
fn host_wayland() -> Option<PathBuf> {
    let name = PathBuf::from(std::env::var_os("WAYLAND_DISPLAY")?);
    let socket = if name.is_absolute() { name } else { PathBuf::from(std::env::var_os("XDG_RUNTIME_DIR")?).join(name) };
    socket.exists().then_some(socket)
}

// Tests run beside each other, each holding this shared; a test that
// measures time holds it alone, so no other test's compositor starts or
// draws while it does. Beside the 24 others, a timed update's first frame
// came 71 to 95 ms after its time, every CPU busy as they started.
static TURN: RwLock<()> = RwLock::new(());

pub struct WlTest {
    child: Option<Child>,
    // Held alive for the test's lifetime. tempdir auto-removes on drop.
    _runtime_dir: tempfile::TempDir,
    pub socket_path: PathBuf,
    pub socket_name: String,
    // The test's turn, let go last, once its compositor is gone.
    _shared: Option<RwLockReadGuard<'static, ()>>,
    _alone: Option<RwLockWriteGuard<'static, ()>>,
}

impl WlTest {
    /// Spawn demiurge-wl in a fresh test environment, with no user
    /// config. demiurge-wl's config::load returns Config::default()
    /// when no config.toml is present at $HOME/.config/demiurge/.
    pub fn new() -> Self {
        Self::spawn(None, false, false)
    }

    /// Same as [`new`], for a test that measures time: It runs alone.
    pub fn alone() -> Self {
        Self::spawn(None, false, true)
    }

    /// Same as [`new`] but writes the given TOML to
    /// $HOME/.config/demiurge/config.toml inside the harness's tempdir
    /// before spawning. Lets a test pin a specific layout, tag set,
    /// or master_ratio.
    pub fn with_config(config_toml: &str) -> Self {
        Self::spawn(Some(config_toml), false, false)
    }

    /// Same as [`with_config`], with XWayland spawned as in a session.
    pub fn with_xwayland(config_toml: &str) -> Self {
        Self::spawn(Some(config_toml), true, false)
    }

    /// The compositor's process id.
    pub fn pid(&self) -> u32 {
        self.child.as_ref().expect("compositor running").id()
    }

    fn spawn(config_toml: Option<&str>, xwayland: bool, alone: bool) -> Self {
        // A test that panicked while holding its turn poisons the lock;
        // the turn itself is still good.
        let (_shared, _alone) = if alone {
            (None, Some(TURN.write().unwrap_or_else(PoisonError::into_inner)))
        } else {
            (Some(TURN.read().unwrap_or_else(PoisonError::into_inner)), None)
        };
        let runtime_dir = tempfile::tempdir().expect("create XDG_RUNTIME_DIR tempdir");
        std::fs::set_permissions(
            runtime_dir.path(),
            std::fs::Permissions::from_mode(0o700),
        )
        .expect("chmod 0700 XDG_RUNTIME_DIR");

        if let Some(toml) = config_toml {
            let config_dir = runtime_dir.path().join(".config").join("demiurge");
            std::fs::create_dir_all(&config_dir)
                .expect("create $HOME/.config/demiurge in tempdir");
            std::fs::write(config_dir.join("config.toml"), toml)
                .expect("write config.toml in tempdir");
        }

        let log = std::fs::File::create(runtime_dir.path().join("demiurge-wl.log")).expect("create the compositor's log");
        let bin = std::env::var("DEMIURGE_WL_TEST_BIN")
            .unwrap_or_else(|_| env!("CARGO_BIN_EXE_demiurge-wl").to_string());
        let mut command = Command::new(bin);
        // The compositor's window nests on the host: A Wayland session's
        // socket, by its absolute path since the compositor's runtime
        // directory is the tempdir, where NVIDIA's EGL has no X11 window
        // surface on Xwayland; else X11's DISPLAY. Its clients get its own
        // socket, which it names for them.
        match host_wayland() {
            Some(socket) => command.env("WAYLAND_DISPLAY", socket),
            None => command.env_remove("WAYLAND_DISPLAY"),
        };
        // Don't spawn Xwayland in tests that don't ask for it -- avoids
        // depending on /usr/bin/Xwayland being installed. The compositor
        // honors this env var; one that asks for it gets it even where the
        // suite itself runs with the variable set, as in the QEMU guest.
        if xwayland {
            command.env_remove("DEMIURGE_WL_NO_XWAYLAND");
        } else {
            command.env("DEMIURGE_WL_NO_XWAYLAND", "1");
        }
        let child = command
            .env("XDG_RUNTIME_DIR", runtime_dir.path())
            // Point HOME at the tempdir so config::Paths::init resolves
            // to {tempdir}/.config/demiurge/config.toml -- which doesn't
            // exist, so config::load returns Config::default(). Tests
            // assert against documented defaults.
            .env("HOME", runtime_dir.path())
            // Suppress the autospawn-a-client convenience that main.rs
            // exposes for manual sanity runs.
            .env_remove("DEMIURGE_WL_AUTOSPAWN")
            .stdout(Stdio::null())
            // The log goes with the tempdir; a test reads it with log().
            .stderr(log)
            .spawn()
            .expect(
                "spawn demiurge-wl -- ensure `cargo test` populates \
                 CARGO_BIN_EXE_demiurge-wl",
            );

        // ListeningSocketSource::new_auto picks the first available
        // wayland-N name; with a fresh empty XDG_RUNTIME_DIR that's
        // wayland-1 deterministically. Poll the file into existence.
        let socket_name = "wayland-1".to_string();
        let socket_path = runtime_dir.path().join(&socket_name);
        let deadline = Instant::now() + STARTUP_TIMEOUT;
        while !socket_path.exists() {
            if Instant::now() >= deadline {
                panic!(
                    "demiurge-wl socket {} did not appear within {:?}",
                    socket_path.display(),
                    STARTUP_TIMEOUT,
                );
            }
            std::thread::sleep(POLL_INTERVAL);
        }

        Self {
            child: Some(child),
            _runtime_dir: runtime_dir,
            socket_path,
            socket_name,
            _shared,
            _alone,
        }
    }

    /// Connect a wayland-client Connection to the running demiurge-wl
    /// via UnixStream. Bypasses env vars so concurrent tests don't
    /// race on $WAYLAND_DISPLAY / $XDG_RUNTIME_DIR.
    pub fn connect(&self) -> Connection {
        let stream = UnixStream::connect(&self.socket_path)
            .expect("UnixStream connect to demiurge-wl socket");
        Connection::from_socket(stream).expect("wayland-client Connection::from_socket")
    }

    /// What the compositor has logged so far.
    pub fn log(&self) -> String {
        std::fs::read_to_string(self._runtime_dir.path().join("demiurge-wl.log")).unwrap_or_default()
    }

    /// Wait for the compositor to log a line containing `needle`; the
    /// line, or None once `timeout` passes.
    pub fn wait_for_log(&self, needle: &str, timeout: Duration) -> Option<String> {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(line) = self.log().lines().find(|l| l.contains(needle)) {
                return Some(line.to_string());
            }
            if Instant::now() >= deadline {
                return None;
            }
            std::thread::sleep(POLL_INTERVAL);
        }
    }

    /// The display of the XWayland the compositor spawned, once it is
    /// ready, from Xwayland's own command line.
    pub fn xwayland_display(&self) -> String {
        self.wait_for_log("XWayland ready", Duration::from_secs(10))
            .unwrap_or_else(|| panic!("XWayland never came up:\n{}", self.log()));
        let xwayland = children(self.pid())
            .into_iter()
            .find(|(_, comm, _)| comm == "Xwayland")
            .expect("the compositor spawned no Xwayland");
        let cmdline = std::fs::read(format!("/proc/{}/cmdline", xwayland.0)).expect("Xwayland's cmdline");
        cmdline
            .split(|&b| b == 0)
            .map(String::from_utf8_lossy)
            .find(|arg| arg.starts_with(':'))
            .expect("Xwayland's display argument")
            .to_string()
    }

    /// The runtime directory the compositor and its clients share.
    pub fn runtime_dir(&self) -> &std::path::Path {
        self._runtime_dir.path()
    }
}

/// The children of `pid`: Each one's pid, command name and state.
pub fn children(pid: u32) -> Vec<(u32, String, char)> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir("/proc").into_iter().flatten().flatten() {
        let Ok(stat) = std::fs::read_to_string(entry.path().join("stat")) else {
            continue;
        };
        // pid (comm) state ppid ...; comm can hold spaces and parens.
        let (Some(open), Some(close)) = (stat.find('('), stat.rfind(')')) else {
            continue;
        };
        let mut rest = stat[close + 1..].split_whitespace();
        let (Some(state), Some(ppid)) = (rest.next(), rest.next()) else {
            continue;
        };
        if ppid.parse() == Ok(pid) {
            let child = stat[..open].trim().parse().unwrap_or(0);
            out.push((child, stat[open + 1..close].to_string(), state.chars().next().unwrap_or('?')));
        }
    }
    out
}

impl Drop for WlTest {
    fn drop(&mut self) {
        // A failed test shows what the compositor said before its tempdir,
        // and the log in it, goes.
        if std::thread::panicking() {
            eprintln!("demiurge-wl's log:\n{}", self.log());
        }
        if let Some(mut child) = self.child.take() {
            // SIGKILL via Child::kill -- a test harness doesn't need a
            // graceful shutdown handshake, and avoiding libc keeps the
            // dev-deps surface small. The tempdir auto-removes on drop
            // even if the compositor didn't flush.
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}
