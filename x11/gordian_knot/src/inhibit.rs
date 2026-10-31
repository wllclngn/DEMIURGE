// D-Bus inhibit watcher for the idle daemon.
//
// Queries org.freedesktop.login1.Manager.ListInhibitors periodically and
// reports whether any inhibitor blocks idle. Video players (mpv with
// --keep-open, Firefox during fullscreen playback, Steam in-game) take
// out these inhibit locks; respecting them keeps the auto-lock from
// interrupting movies and games.
//
// Implemented via zbus's blocking API -- the daemon makes at most one D-Bus
// call every POLL_INTERVAL, so async runtime overhead is overkill.

use std::time::{Duration, Instant};

use zbus::blocking::Connection;

const RECHECK_INTERVAL: Duration = Duration::from_secs(5);

pub struct Watcher {
    conn: Option<Connection>,
    last_check: Option<Instant>,
    cached: bool,
}

impl Watcher {
    pub fn new() -> Self {
        Self {
            conn: Connection::system().ok(),
            last_check: None,
            cached: false,
        }
    }

    // Returns true if any active inhibitor includes "idle" in its what field.
    // Result is cached for RECHECK_INTERVAL; successive calls within the
    // window return the stored value without hitting D-Bus.
    pub fn is_idle_inhibited(&mut self) -> bool {
        if let Some(t) = self.last_check {
            if t.elapsed() < RECHECK_INTERVAL {
                return self.cached;
            }
        }
        self.last_check = Some(Instant::now());

        let Some(conn) = self.conn.as_ref() else {
            self.cached = false;
            return false;
        };

        // Signature of ListInhibitors return value: a(ssssuu)
        //   array of (what, who, why, mode, uid, pid)
        let reply = match conn.call_method(
            Some("org.freedesktop.login1"),
            "/org/freedesktop/login1",
            Some("org.freedesktop.login1.Manager"),
            "ListInhibitors",
            &(),
        ) {
            Ok(r) => r,
            Err(_) => {
                self.cached = false;
                return false;
            }
        };

        // The body() is a Vec of tuples; if anyone mentions "idle" in
        // `what`, we respect it. We don't filter by mode ("block" vs
        // "delay") -- any idle inhibitor is enough.
        let body = reply.body();
        let inhibitors: Vec<(String, String, String, String, u32, u32)> =
            match body.deserialize() {
                Ok(v) => v,
                Err(_) => {
                    self.cached = false;
                    return false;
                }
            };
        self.cached = inhibitors.iter().any(|(what, ..)| what.contains("idle"));
        self.cached
    }
}
