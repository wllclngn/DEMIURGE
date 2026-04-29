// Cursor auto-hide. unclutter-style behavior built into the WM:
// when [cursor] auto_hide = true, the cursor hides after
// auto_hide_seconds of no pointer motion and reappears the moment
// the user moves the mouse. auto_hide_seconds = 0 with auto_hide
// = true gives "always hidden except while moving" -- a strict
// keyboard-forward setup where the pointer is invisible whenever
// it's stationary.
//
// Detection mechanism: poll XQueryPointer every 100ms via a dedicated
// timerfd in main.rs::run. This is what unclutter has done since the
// 80s and it Just Works -- 100ms is below the perceptual threshold
// for "did the cursor lag when I reached for it", and the per-tick
// cost is negligible (one round-trip query, one i16/i16 compare).
//
// We deliberately don't use XInput2 raw events. They'd give
// per-event accuracy but the additional protocol surface and the
// XInput2 cookie machinery isn't worth it for a 100ms-vs-event
// tradeoff that's invisible in practice.

use std::time::{Duration, Instant};

use x11rb::connection::Connection;
use x11rb::protocol::xfixes::ConnectionExt as XfixesExt;
use x11rb::protocol::xproto::{ConnectionExt as XprotoExt, Window};
use x11rb::rust_connection::RustConnection;

const POLL_INTERVAL_MS: u64 = 100;

pub struct CursorState {
    pub auto_hide: bool,
    pub auto_hide_seconds: u32,
    // Whether the cursor is currently hidden via XFixes. Tracked
    // explicitly because the XFixes hide/show calls are reference-
    // counted on the server side -- calling hide twice in a row
    // requires two show calls to undo. We never want to double-call.
    pub hidden: bool,
    // Last observed pointer position (root coords) and the wall-clock
    // instant we last saw it change. The hide trigger is "elapsed
    // since last_motion >= auto_hide_seconds".
    pub last_pos: (i16, i16),
    pub last_motion: Instant,
}

impl CursorState {
    pub fn new(auto_hide: bool, auto_hide_seconds: u32) -> Self {
        Self {
            auto_hide,
            auto_hide_seconds,
            hidden: false,
            last_pos: (0, 0),
            last_motion: Instant::now(),
        }
    }
}

// One-shot at Wm::init: announce the XFixes version we support so
// the server enables the per-client extension state. Skipping this
// makes hide/show silently no-op on some server impls.
pub fn init_xfixes(conn: &RustConnection) -> Result<(), String> {
    conn.xfixes_query_version(5, 0)
        .map_err(|e| format!("xfixes_query_version: {}", e))?
        .reply()
        .map_err(|e| format!("xfixes_query_version reply: {}", e))?;
    Ok(())
}

pub fn hide(conn: &RustConnection, root: Window) {
    let _ = conn.xfixes_hide_cursor(root);
    let _ = conn.flush();
}

pub fn show(conn: &RustConnection, root: Window) {
    let _ = conn.xfixes_show_cursor(root);
    let _ = conn.flush();
}

// Setup the timerfd that drives cursor_tick. Returns -1 on failure;
// main.rs::run still polls() the fd, libc::poll just ignores entries
// with fd < 0 so we degrade silently to "auto_hide doesn't work" if
// timerfd_create fails, rather than killing the whole WM.
pub fn setup_timerfd() -> i32 {
    unsafe {
        let fd = libc::timerfd_create(
            libc::CLOCK_MONOTONIC,
            libc::TFD_NONBLOCK | libc::TFD_CLOEXEC,
        );
        if fd < 0 {
            eprintln!("[demiurge] cursor timerfd_create failed; auto_hide disabled");
            return -1;
        }
        let interval_ns = (POLL_INTERVAL_MS * 1_000_000) as i64;
        let spec = libc::itimerspec {
            it_interval: libc::timespec {
                tv_sec: 0,
                tv_nsec: interval_ns,
            },
            it_value: libc::timespec {
                tv_sec: 0,
                tv_nsec: interval_ns,
            },
        };
        if libc::timerfd_settime(fd, 0, &spec, std::ptr::null_mut()) < 0 {
            eprintln!("[demiurge] cursor timerfd_settime failed");
            libc::close(fd);
            return -1;
        }
        fd
    }
}

// Drain the timerfd. Returns the expiration count (typically 1; can
// be higher if the WM was busy). Caller doesn't care how many ticks
// fired -- one cursor_tick per wakeup is sufficient.
pub fn drain_timerfd(fd: i32) {
    if fd < 0 {
        return;
    }
    let mut buf = [0u8; 8];
    unsafe {
        libc::read(fd, buf.as_mut_ptr() as *mut libc::c_void, 8);
    }
}

// Apply a "hide / show" decision based on current pointer position
// versus the last observed one. Called every POLL_INTERVAL_MS from
// the main event loop. Returns nothing -- the cursor state is owned
// by Wm.
pub fn tick(
    state: &mut CursorState,
    conn: &RustConnection,
    root: Window,
    drag_active: bool,
) {
    if !state.auto_hide {
        return;
    }

    // Force-show during drag operations so the user can actually
    // see what they're doing. Reset the motion timestamp so we
    // don't immediately hide again the instant the drag releases.
    if drag_active {
        if state.hidden {
            show(conn, root);
            state.hidden = false;
        }
        state.last_motion = Instant::now();
        return;
    }

    let pointer = match conn.query_pointer(root).ok().and_then(|c| c.reply().ok()) {
        Some(p) => p,
        None => return,
    };
    let pos = (pointer.root_x, pointer.root_y);

    if pos != state.last_pos {
        // Motion detected.
        state.last_pos = pos;
        state.last_motion = Instant::now();
        if state.hidden {
            show(conn, root);
            state.hidden = false;
        }
        return;
    }

    // No motion this tick. Hide if we're past the timeout.
    if !state.hidden {
        let timeout = Duration::from_secs(state.auto_hide_seconds as u64);
        if state.last_motion.elapsed() >= timeout {
            hide(conn, root);
            state.hidden = true;
        }
    }
}

// Called by reload_config when the auto_hide / auto_hide_seconds
// settings change. New auto_hide = false: force-show if currently
// hidden. New auto_hide_seconds: reset the motion timer so the new
// timeout kicks in from "now" rather than dragging the previous
// timeout.
pub fn reload(
    state: &mut CursorState,
    conn: &RustConnection,
    root: Window,
    new_auto_hide: bool,
    new_auto_hide_seconds: u32,
) {
    if !new_auto_hide && state.hidden {
        show(conn, root);
        state.hidden = false;
    }
    state.auto_hide = new_auto_hide;
    state.auto_hide_seconds = new_auto_hide_seconds;
    state.last_motion = Instant::now();
}
