// Idle-watcher daemon. Runs as a persistent user process (systemd user
// service). Polls XScreenSaver every 5 seconds; when idle_ms exceeds the
// configured threshold AND no D-Bus inhibitor holds "idle", spawns
// `gordian_knot` (this same binary, without --daemon) to perform the lock.
//
// Rationale: forking a fresh child per lock keeps the daemon tiny and means
// each lock session gets its own sandboxed process lifecycle. The daemon
// itself never touches PAM, never grabs input, never draws.

use std::ffi::CString;
use std::process::Command;
use std::time::{Duration, Instant};

use x11rb::connection::Connection;
use x11rb::protocol::screensaver::ConnectionExt as _;
use x11rb::rust_connection::RustConnection;

use crate::gordian_knot::inhibit;

const POLL_INTERVAL: Duration = Duration::from_secs(5);

// `threshold_seconds` is the resolved idle threshold from the config.
// The caller picks input.idle.lock_seconds when set, else falls back
// to the legacy gordian_knot.idle_timeout_seconds. We don't know
// which source was authoritative -- doesn't matter for the daemon's
// job; it just polls and triggers.
pub fn run(threshold_seconds: u64, self_binary: &str) -> i32 {
    let (conn, screen_num) = match RustConnection::connect(None) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("[gordian_knot-daemon] x11 connect: {}", e);
            return 1;
        }
    };
    let root = conn.setup().roots[screen_num].root;

    // Verify the ScreenSaver extension is present. If not, we have no way
    // to measure idle time and the daemon is useless -- bail.
    let qv = match conn.screensaver_query_version(1, 0) {
        Ok(c) => c.reply().ok(),
        Err(_) => None,
    };
    if qv.is_none() {
        eprintln!("[gordian_knot-daemon] ScreenSaver X extension unavailable");
        return 1;
    }

    let threshold_ms = threshold_seconds.saturating_mul(1000);
    let mut inhibitor = inhibit::Watcher::new();

    eprintln!(
        "[gordian_knot-daemon] watching idle; threshold {}s, poll {}s",
        threshold_seconds,
        POLL_INTERVAL.as_secs()
    );

    let mut last_lock_at: Option<Instant> = None;

    loop {
        std::thread::sleep(POLL_INTERVAL);

        let info = match conn.screensaver_query_info(root) {
            Ok(c) => c.reply().ok(),
            Err(_) => None,
        };
        let Some(info) = info else {
            continue;
        };
        let idle_ms = info.ms_since_user_input as u64;

        // Don't re-fire while a previous lock session is still active.
        if let Some(t) = last_lock_at {
            if t.elapsed() < Duration::from_secs(2) {
                continue;
            }
        }

        if idle_ms < threshold_ms {
            continue;
        }
        if inhibitor.is_idle_inhibited() {
            continue;
        }

        eprintln!(
            "[gordian_knot-daemon] idle {}s >= {}s, triggering lock",
            idle_ms / 1000,
            threshold_seconds
        );
        last_lock_at = Some(Instant::now());
        if let Err(e) = spawn_locker(self_binary) {
            eprintln!("[gordian_knot-daemon] spawn failed: {}", e);
        }
    }
}

fn spawn_locker(self_binary: &str) -> Result<(), String> {
    let _ = CString::new(self_binary).map_err(|e| e.to_string())?;
    // We deliberately do NOT wait on the child. The locker owns its own
    // lifecycle (grab, prompt, PAM, unmap). We want to return to polling
    // immediately, and the 2-second dedup above prevents re-trigger.
    Command::new(self_binary)
        .spawn()
        .map(|_| ())
        .map_err(|e| e.to_string())
}
