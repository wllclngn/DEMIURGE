// Idle / power-save / screensaver application. Replaces the
// `xset s <timeout>` and `xset dpms <s> <s> <s>` lines that
// historically lived in DEMIURGE startup.commands.
//
// Two protocols touched:
//   - DPMS extension for monitor power management (standby ->
//     suspend -> off ladder).
//   - Core xproto SetScreenSaver for the X server's own
//     screensaver (separate from DPMS; controls screen blanking
//     and the X SS timer that GORDIAN KNOT polls).
//
// All values come from [input.idle] in the config. Each "0"
// means "leave the server default in place" -- DEMIURGE doesn't
// own that timer. Non-zero overrides.

use x11rb::connection::Connection;
use x11rb::protocol::dpms::ConnectionExt as DpmsExt;
use x11rb::protocol::xproto::{Blanking, ConnectionExt as XprotoExt, Exposures};
use x11rb::rust_connection::RustConnection;

// One-shot at Wm::init: announce the DPMS version we support so the
// server enables the per-client extension state. Skipping this is
// mostly fine on modern X11 but ensures the per-client extension
// dispatch table is set up.
pub fn init_dpms(conn: &RustConnection) -> Result<(), String> {
    conn.dpms_get_version(1, 1)
        .map_err(|e| format!("dpms_get_version: {}", e))?
        .reply()
        .map_err(|e| format!("dpms_get_version reply: {}", e))?;
    Ok(())
}

// Apply DPMS standby/suspend/off timeouts. Any field == 0 keeps the
// existing server value for that field (we read the current values
// first and only override the non-zero ones). DPMS is enabled
// whenever any field is non-zero; if all three are zero, we leave
// DPMS state alone (don't enable, don't disable).
pub fn apply_dpms(
    conn: &RustConnection,
    standby_seconds: u32,
    suspend_seconds: u32,
    off_seconds: u32,
) {
    if standby_seconds == 0 && suspend_seconds == 0 && off_seconds == 0 {
        return;
    }

    // Read current timeouts so we preserve any field the user left
    // at 0. dpms_get_timeouts may fail if the extension isn't
    // available -- treat as "no current values" and proceed with
    // whatever the user did set.
    let (cur_standby, cur_suspend, cur_off) = match conn
        .dpms_get_timeouts()
        .ok()
        .and_then(|c| c.reply().ok())
    {
        Some(r) => (r.standby_timeout, r.suspend_timeout, r.off_timeout),
        None => (0u16, 0u16, 0u16),
    };

    let standby = if standby_seconds > 0 {
        standby_seconds.min(u16::MAX as u32) as u16
    } else {
        cur_standby
    };
    let suspend = if suspend_seconds > 0 {
        suspend_seconds.min(u16::MAX as u32) as u16
    } else {
        cur_suspend
    };
    let off = if off_seconds > 0 {
        off_seconds.min(u16::MAX as u32) as u16
    } else {
        cur_off
    };

    let _ = conn.dpms_set_timeouts(standby, suspend, off);
    let _ = conn.dpms_enable();
    let _ = conn.flush();
}

// Apply X11 screensaver timeout. Distinct from DPMS -- this is the
// server's blanking / screen-cleared timer, also driven by the same
// XScreenSaver extension that GORDIAN KNOT's daemon polls for idle
// detection.
//
// timeout_seconds == 0 leaves the server default in place.
//
// SetScreenSaver's args: timeout (i16, seconds), interval (i16,
// seconds, only used in screen-cycle mode -- 0 disables), and two
// hint flags. We set Blanking::PREFER_BLANKING so the server clears
// the screen rather than running a built-in pattern, and
// Exposures::DEFAULT to leave the expose-event behavior alone.
pub fn apply_screensaver(conn: &RustConnection, timeout_seconds: u32) {
    if timeout_seconds == 0 {
        return;
    }
    let timeout = timeout_seconds.min(i16::MAX as u32) as i16;
    let _ = conn.set_screen_saver(
        timeout,
        0,
        Blanking::PREFERRED,
        Exposures::DEFAULT,
    );
    let _ = conn.flush();
}
