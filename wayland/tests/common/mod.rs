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
//     the test (Xorg in dev, Xvfb in CI)
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
use std::time::{Duration, Instant};

use wayland_client::Connection;

const STARTUP_TIMEOUT: Duration = Duration::from_secs(10);
const POLL_INTERVAL: Duration = Duration::from_millis(50);

pub struct WlTest {
    child: Option<Child>,
    // Held alive for the test's lifetime. tempdir auto-removes on drop.
    _runtime_dir: tempfile::TempDir,
    pub socket_path: PathBuf,
    pub socket_name: String,
}

impl WlTest {
    /// Spawn demiurge-wl in a fresh test environment, with no user
    /// config. demiurge-wl's config::load returns Config::default()
    /// when no config.toml is present at $HOME/.config/demiurge/.
    pub fn new() -> Self {
        Self::spawn(None)
    }

    /// Same as [`new`] but writes the given TOML to
    /// $HOME/.config/demiurge/config.toml inside the harness's tempdir
    /// before spawning. Lets a test pin a specific layout, tag set,
    /// or master_ratio.
    pub fn with_config(config_toml: &str) -> Self {
        Self::spawn(Some(config_toml))
    }

    fn spawn(config_toml: Option<&str>) -> Self {
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

        let bin = env!("CARGO_BIN_EXE_demiurge-wl");
        let child = Command::new(bin)
            .env("XDG_RUNTIME_DIR", runtime_dir.path())
            // Point HOME at the tempdir so config::Paths::init resolves
            // to {tempdir}/.config/demiurge/config.toml -- which doesn't
            // exist, so config::load returns Config::default(). Tests
            // assert against documented defaults.
            .env("HOME", runtime_dir.path())
            // demiurge-wl's main.rs sets WAYLAND_DISPLAY itself to its
            // chosen socket name; clearing it here makes sure the
            // child doesn't try to connect to a host Wayland server.
            .env_remove("WAYLAND_DISPLAY")
            // Suppress the autospawn-a-client convenience that main.rs
            // exposes for manual sanity runs.
            .env_remove("DEMIURGE_WL_AUTOSPAWN")
            // Don't spawn Xwayland in tests -- avoids dependency on
            // /usr/bin/Xwayland being installed in CI / sandbox
            // environments. main.rs honors this env var.
            .env("DEMIURGE_WL_NO_XWAYLAND", "1")
            .stdout(Stdio::null())
            // Discard log spam by default. Flip to Stdio::inherit() when
            // debugging a test failure.
            .stderr(Stdio::null())
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
}

impl Drop for WlTest {
    fn drop(&mut self) {
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
